/**
 * Workspace path safety helpers.
 *
 * Ported from Rust's `backend/daemon/src/tools/workspace.rs:182-278`
 * (`resolve_roots`, `resolve_path`, `resolve_list_path`) and
 * `backend/daemon/src/memory/deferred_edits.rs:52-107`
 * (`normalize_workspace_path`, `normalize_protected_path`,
 * `is_prompt_visible_path`).
 *
 * Every workspace tool that takes a `path` argument routes it through
 * `resolveWorkspacePath` first — that's the single source of truth for:
 *   - rejecting `..` traversal and absolute paths,
 *   - tolerating the `workspace/` and `memory/` display prefixes,
 *   - canonicalizing and verifying the result is inside the workspace.
 */

import fs from "node:fs";
import path from "node:path";

import { ToolError } from "./registry.ts";

/** Top-level workspace files whose content is read into the system prompt. */
const PROTECTED_PATHS = [
  "SOUL.md",
  "USER.md",
  "AGENTS.md",
  "TOOLS.md",
  "HEARTBEAT.md",
] as const;

/** Prompt-visible memory index — also editable but deferred to compaction. */
const MEMORY_INDEX_FILE = "MEMORY.md";

/**
 * Strip leading `/`, `./`, and `workspace/` segments repeatedly until none
 * apply. Mirrors `normalize_workspace_path` in deferred_edits.rs — a single
 * pass misses inputs like `workspace/./SOUL.md`.
 */
function normalizeWorkspacePath(p: string): string {
  let normalized = p.trim().replace(/\\/g, "/");
  // eslint-disable-next-line no-constant-condition
  while (true) {
    const before = normalized.length;
    while (normalized.startsWith("/")) normalized = normalized.slice(1);
    while (normalized.startsWith("./")) normalized = normalized.slice(2);
    if (normalized.startsWith("workspace/")) {
      normalized = normalized.slice("workspace/".length);
    }
    if (normalized.length === before) break;
  }
  return normalized;
}

/**
 * Return the canonical name if `path` resolves to one of the protected
 * top-level workspace files (SOUL/USER/AGENTS/TOOLS/HEARTBEAT).
 */
export function normalizeProtectedPath(p: string): string | undefined {
  const normalized = normalizeWorkspacePath(p);
  return PROTECTED_PATHS.includes(normalized as (typeof PROTECTED_PATHS)[number])
    ? normalized
    : undefined;
}

export function isProtectedPath(p: string): boolean {
  return normalizeProtectedPath(p) !== undefined;
}

/**
 * Return the canonical name if `path` is prompt-visible — protected file
 * OR the memory index. Used by the dispatch layer to flag write/edit
 * results as `deferred_until_compaction: true` and by delete to refuse
 * destruction.
 */
export function normalizePromptVisiblePath(p: string): string | undefined {
  const normalized = normalizeWorkspacePath(p);
  const protectedHit = normalizeProtectedPath(normalized);
  if (protectedHit !== undefined) return protectedHit;
  if (normalized === MEMORY_INDEX_FILE) return MEMORY_INDEX_FILE;
  return undefined;
}

export function isPromptVisiblePath(p: string): boolean {
  return normalizePromptVisiblePath(p) !== undefined;
}

/**
 * Split a relative path into the (base, stripped) pair Rust's
 * `resolve_roots` produces:
 *   - `"workspace"`         → (workspaceDir, "")
 *   - `"workspace/notes"`   → (workspaceDir, "notes")
 *   - `"memory"`            → (workspaceDir/memory, "")
 *   - `"memory/people/x"`   → (workspaceDir/memory, "people/x")
 *   - `"notes/foo.md"`      → (workspaceDir, "notes/foo.md")
 */
export function resolveRoots(
  workspaceDir: string,
  relative: string,
): { base: string; stripped: string } {
  if (workspaceDir.length === 0) {
    throw new ToolError("InvalidArgs", "workspace not configured");
  }
  const trimmed = relative.trim();
  if (trimmed.length === 0) {
    throw new ToolError("InvalidArgs", "path is empty");
  }
  if (trimmed === "workspace") return { base: workspaceDir, stripped: "" };
  if (trimmed.startsWith("workspace/")) {
    return { base: workspaceDir, stripped: trimmed.slice("workspace/".length) };
  }
  if (trimmed === "memory") {
    return { base: path.join(workspaceDir, "memory"), stripped: "" };
  }
  if (trimmed.startsWith("memory/")) {
    return {
      base: path.join(workspaceDir, "memory"),
      stripped: trimmed.slice("memory/".length),
    };
  }
  return { base: workspaceDir, stripped: trimmed };
}

/**
 * Resolve a relative workspace path to an absolute filesystem path.
 *
 * Rejects:
 *   - empty paths after prefix stripping,
 *   - `..` traversal segments,
 *   - absolute paths,
 *   - resolved paths that canonicalize outside the workspace (catches symlink escape).
 *
 * Throws `ToolError("InvalidArgs", ...)` on rejection.
 */
export function resolvePath(workspaceDir: string, relative: string): string {
  const { base, stripped } = resolveRoots(workspaceDir, relative);
  if (stripped.length === 0) {
    throw new ToolError("InvalidArgs", "path is empty");
  }

  const segments = stripped.split(/[\\/]+/).filter((s) => s.length > 0);
  for (const seg of segments) {
    if (seg === "..") {
      throw new ToolError("InvalidArgs", "path traversal (..) is not allowed");
    }
  }
  if (path.isAbsolute(stripped)) {
    throw new ToolError("InvalidArgs", "absolute paths are not allowed");
  }

  const resolved = path.join(base, stripped);

  // Symlink-escape check: if the resolved path exists, canonicalize it
  // and verify the result is still inside the workspace. If it doesn't
  // exist yet (common for write), walk up to the nearest existing
  // ancestor and check that.
  let canonicalBase: string;
  try {
    canonicalBase = fs.realpathSync(base);
  } catch {
    // Base doesn't exist yet (write creates it) — defer the check.
    return resolved;
  }

  try {
    const canonical = fs.realpathSync(resolved);
    if (!isInside(canonical, canonicalBase)) {
      throw new ToolError("InvalidArgs", "resolved path escapes workspace");
    }
    return resolved;
  } catch (e) {
    if (e instanceof ToolError) throw e;
    // Path doesn't exist yet — walk up to the nearest existing ancestor.
    let ancestor = resolved;
    while (true) {
      const parent = path.dirname(ancestor);
      if (parent === ancestor) break;
      try {
        const canonicalParent = fs.realpathSync(parent);
        if (!isInside(canonicalParent, canonicalBase)) {
          throw new ToolError("InvalidArgs", "resolved path escapes workspace");
        }
        break;
      } catch (inner) {
        if (inner instanceof ToolError) throw inner;
        ancestor = parent;
      }
    }
    return resolved;
  }
}

/**
 * Resolve a directory path for `list_files` — accepts an absent/empty/
 * `"."` argument as "workspace root", otherwise delegates to
 * `resolvePath`. Mirrors Rust's `resolve_list_path`.
 */
export function resolveListPath(
  workspaceDir: string,
  relative: string | undefined,
): string {
  if (workspaceDir.length === 0) {
    throw new ToolError("InvalidArgs", "workspace not configured");
  }
  if (relative === undefined || relative === "" || relative === ".") {
    return workspaceDir;
  }
  const { base, stripped } = resolveRoots(workspaceDir, relative);
  if (stripped.length === 0) return base;
  return resolvePath(workspaceDir, relative);
}

/**
 * Return the display string used by `file_search` / hybrid-search scope
 * filters: a path relative to the workspace, forward-slashed. Mirrors
 * `display_path_for` in workspace.rs.
 */
export function displayPathFor(workspaceDir: string, absPath: string): string {
  const rel = path.relative(workspaceDir, absPath);
  if (rel.startsWith("..")) return absPath.replace(/\\/g, "/");
  return rel.replace(/\\/g, "/");
}

function isInside(candidate: string, root: string): boolean {
  if (candidate === root) return true;
  const rootWithSep = root.endsWith(path.sep) ? root : root + path.sep;
  return candidate.startsWith(rootWithSep);
}
