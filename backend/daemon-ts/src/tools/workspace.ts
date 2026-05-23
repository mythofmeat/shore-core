/**
 * Workspace filesystem tools — read, write, edit, list_files, delete,
 * file_search, exec.
 *
 * Ported from `backend/daemon/src/tools/workspace.rs`. Two renames from
 * Rust: `search` → `file_search` (the rest of the names match).
 *
 * Path safety lives entirely in `paths.ts`. The workspace handlers route
 * every user-supplied path through `resolvePath` / `resolveListPath`
 * before touching the filesystem.
 *
 * Exec sandbox shape (verbatim from workspace.rs):
 *   - Shell-words splitter (no shell invocation — `cmd1; cmd2` parses as
 *     a single token list, fails the allowlist).
 *   - Allowlist limited to specific binaries; commands with `/` or `\\`
 *     in the first token are rejected outright (no `/usr/bin/git`).
 *   - Every positional arg that "looks like a path" is canonicalized
 *     and must stay inside the workspace.
 */

import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

import { queueDeferredEdit } from "../memory/deferred_edits.ts";

import {
  displayPathFor,
  isPromptVisiblePath,
  normalizeProtectedPath,
  normalizePromptVisiblePath,
  resolveListPath,
  resolvePath,
} from "./paths.ts";
import type { ToolContext, ToolHandler } from "./registry.ts";
import { ToolError } from "./registry.ts";

// ---------------------------------------------------------------------------
// Descriptions (from prompts/tools/workspace/*.md, sans trailing newline)
// ---------------------------------------------------------------------------

export const READ_DESCRIPTION =
  "Read the contents of a file. Returns the file content as text; use offset and limit for large files.";

export const WRITE_DESCRIPTION =
  "Write or overwrite a file. Parent directories are created automatically. Overwrites without confirmation.";

export const EDIT_DESCRIPTION =
  "Edit an existing file by replacing specific text. Each replacement must match old_string exactly, including whitespace and newlines.";

export const LIST_FILES_DESCRIPTION =
  "List files and directories under a path. Returns each entry's name, type, and size. Use this when you're looking for files by name, date, or directory structure — `file_search` is for fuzzy content matching, not exact-name lookups.";

export const FILE_SEARCH_DESCRIPTION =
  "Find files matching a query. Uses hybrid ranking (semantic + lexical) by default so paraphrased queries still find the right file; pass `mode: \"lexical\"` for substring-only or `mode: \"vector\"` for pure semantic similarity. Best for fuzzy concept questions (\"anything about my dog?\") rather than structural lookups by date or filename — use `list_files` for those. Returns paths, line numbers, and short excerpts. Treat results as discovery and follow up with `read` on the top files for full context.";

export const DELETE_DESCRIPTION =
  "Move a file to a trash folder. The file is moved out of your workspace into a timestamped trash folder, not permanently erased. Refuses prompt-visible files (SOUL.md, USER.md, AGENTS.md, TOOLS.md, HEARTBEAT.md, MEMORY.md) and directories.";

export const EXEC_DESCRIPTION =
  "Run an allowlisted host command. The command string is parsed into argv and executed directly; shell features like pipes, redirects, command substitution, and `;` chaining are not supported. Use this for search, git, and build/test commands when a file tool is awkward.";

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

const SEARCH_DEFAULT_MAX_RESULTS = 20;
const SEARCH_MAX_RESULTS = 100;
const SEARCH_EXCERPT_CHARS = 1200;

function requirePathField(input: unknown): string {
  const obj = (input ?? {}) as Record<string, unknown>;
  const v = obj["path"];
  if (typeof v !== "string") {
    throw new ToolError("InvalidArgs", "missing required field: path");
  }
  return v;
}

function requireStringField(input: unknown, field: string): string {
  const obj = (input ?? {}) as Record<string, unknown>;
  const v = obj[field];
  if (typeof v !== "string") {
    throw new ToolError("InvalidArgs", `missing required field: ${field}`);
  }
  return v;
}

/**
 * Decorate a write/edit result for prompt-visible files AND queue the
 * deferred edit. The queue entry is what `applyDeferredEdits` consumes at
 * the next compaction boundary to refresh the active prompt snapshot.
 *
 * No-op for paths that aren't prompt-visible (returns base unchanged).
 * Queue-write failures are logged but don't fail the tool call — the
 * file edit already succeeded, and the operator can recover by editing
 * MEMORY.md/etc. directly. Mirrors Rust's `ContextToolContext::defer_edit`
 * warn-and-continue behavior.
 */
function decorateForPromptVisible(
  pathStr: string,
  base: Record<string, unknown>,
  ctx: ToolContext,
): Record<string, unknown> {
  const deferredPath = normalizePromptVisiblePath(pathStr);
  if (deferredPath === undefined) return base;

  if (ctx.characterDataDir.length > 0) {
    try {
      queueDeferredEdit(ctx.characterDataDir, pathStr);
    } catch (e) {
      console.warn(
        `[deferred_edits] failed to queue ${pathStr}: ${(e as Error).message}`,
      );
    }
  }

  const out = { ...base };
  out["prompt_visible_file"] = true;
  if (normalizeProtectedPath(pathStr) !== undefined) {
    out["protected_file"] = true;
  }
  out["deferred_until_compaction"] = true;
  out["deferred_path"] = deferredPath;
  out["prompt_reload_required"] = true;
  return out;
}

// ---------------------------------------------------------------------------
// read
// ---------------------------------------------------------------------------

export const readHandler: ToolHandler = {
  name: "read",
  description: READ_DESCRIPTION,
  inputSchema: {
    type: "object",
    properties: {
      path: {
        type: "string",
        description: "Relative path within your workspace.",
      },
      offset: {
        type: "number",
        description: "Line number to start reading from (1-based). Optional.",
      },
      limit: {
        type: "number",
        description: "Maximum number of lines to read. Optional.",
      },
    },
    required: ["path"],
  },
  async execute(input: unknown, ctx: ToolContext): Promise<string> {
    const pathStr = requirePathField(input);
    const obj = input as Record<string, unknown>;
    const resolved = resolvePath(ctx.workspaceDir, pathStr);

    let stat: fs.Stats;
    try {
      stat = fs.statSync(resolved);
    } catch (e) {
      const code = (e as NodeJS.ErrnoException).code;
      if (code === "ENOENT") {
        throw new ToolError("Io", `file not found: ${pathStr}`);
      }
      throw new ToolError("Io", (e as Error).message);
    }
    if (!stat.isFile()) {
      throw new ToolError("InvalidArgs", `${pathStr} is not a file`);
    }

    const content = fs.readFileSync(resolved, "utf8");
    const lines = content.split("\n");
    const totalLines = lines.length;

    const rawOffset = obj["offset"];
    const offset = Math.max(
      0,
      Math.min(
        totalLines,
        typeof rawOffset === "number" && Number.isFinite(rawOffset)
          ? Math.floor(rawOffset) - 1
          : 0,
      ),
    );
    const rawLimit = obj["limit"];
    const limit =
      typeof rawLimit === "number" && Number.isFinite(rawLimit)
        ? Math.max(0, Math.floor(rawLimit))
        : totalLines;
    const end = Math.min(offset + limit, totalLines);
    const selected = lines.slice(offset, end).join("\n");

    const result: Record<string, unknown> = {
      path: pathStr,
      content: selected,
      total_lines: totalLines,
    };
    if (offset > 0 || end < totalLines) {
      result["offset"] = offset + 1;
      result["returned_lines"] = end - offset;
      if (end < totalLines) {
        result["note"] = `Showing lines ${offset + 1}–${end} of ${totalLines}. Use offset=${end + 1} to continue.`;
      }
    }
    return JSON.stringify(result);
  },
};

// ---------------------------------------------------------------------------
// write
// ---------------------------------------------------------------------------

export const writeHandler: ToolHandler = {
  name: "write",
  description: WRITE_DESCRIPTION,
  inputSchema: {
    type: "object",
    properties: {
      path: {
        type: "string",
        description: "Relative path within your workspace.",
      },
      content: { type: "string", description: "Full content to write." },
    },
    required: ["path", "content"],
  },
  async execute(input: unknown, ctx: ToolContext): Promise<string> {
    const pathStr = requirePathField(input);
    const content = requireStringField(input, "content");
    const resolved = resolvePath(ctx.workspaceDir, pathStr);

    const parent = path.dirname(resolved);
    fs.mkdirSync(parent, { recursive: true });
    fs.writeFileSync(resolved, content);

    const base: Record<string, unknown> = {
      path: pathStr,
      bytes_written: Buffer.byteLength(content, "utf8"),
    };
    return JSON.stringify(decorateForPromptVisible(pathStr, base, ctx));
  },
};

// ---------------------------------------------------------------------------
// edit
// ---------------------------------------------------------------------------

export const editHandler: ToolHandler = {
  name: "edit",
  description: EDIT_DESCRIPTION,
  inputSchema: {
    type: "object",
    properties: {
      path: {
        type: "string",
        description: "Relative path within your workspace.",
      },
      edits: {
        type: "array",
        description: "List of replacements to apply in order.",
        items: {
          type: "object",
          properties: {
            old_string: {
              type: "string",
              description:
                "Exact text to find and replace. Must match whitespace and newlines precisely.",
            },
            new_string: {
              type: "string",
              description: "Text to replace old_string with.",
            },
          },
          required: ["old_string", "new_string"],
        },
      },
    },
    required: ["path", "edits"],
  },
  async execute(input: unknown, ctx: ToolContext): Promise<string> {
    const pathStr = requirePathField(input);
    const obj = input as Record<string, unknown>;
    const editsRaw = obj["edits"];
    if (!Array.isArray(editsRaw) || editsRaw.length === 0) {
      throw new ToolError("InvalidArgs", "missing or empty 'edits' array");
    }

    const resolved = resolvePath(ctx.workspaceDir, pathStr);
    if (!fs.existsSync(resolved)) {
      throw new ToolError("Io", `file not found: ${pathStr}`);
    }
    let content = fs.readFileSync(resolved, "utf8");
    let replacementsMade = 0;

    for (const e of editsRaw) {
      const editRec = (e ?? {}) as Record<string, unknown>;
      const oldStr = editRec["old_string"];
      const newStr = editRec["new_string"];
      if (typeof oldStr !== "string") {
        throw new ToolError("InvalidArgs", "each edit must have 'old_string'");
      }
      if (typeof newStr !== "string") {
        throw new ToolError("InvalidArgs", "each edit must have 'new_string'");
      }
      if (oldStr.length === 0) {
        throw new ToolError("InvalidArgs", "old_string must not be empty");
      }
      if (!content.includes(oldStr)) {
        const snippetLimit = 800;
        const chars = [...content];
        const snippet =
          chars.length <= snippetLimit
            ? content
            : `${chars.slice(0, snippetLimit).join("")}\n... (truncated)`;
        throw new ToolError(
          "InvalidArgs",
          `Could not find the exact text in ${pathStr}.\nCurrent file contents:\n${snippet}`,
        );
      }
      // Replace ALL occurrences.
      let count = 0;
      let idx = content.indexOf(oldStr);
      while (idx >= 0) {
        count += 1;
        idx = content.indexOf(oldStr, idx + oldStr.length);
      }
      content = content.split(oldStr).join(newStr);
      replacementsMade += count;
    }

    fs.writeFileSync(resolved, content);

    const base: Record<string, unknown> = {
      path: pathStr,
      replacements_made: replacementsMade,
    };
    return JSON.stringify(decorateForPromptVisible(pathStr, base, ctx));
  },
};

// ---------------------------------------------------------------------------
// list_files
// ---------------------------------------------------------------------------

export const listFilesHandler: ToolHandler = {
  name: "list_files",
  description: LIST_FILES_DESCRIPTION,
  inputSchema: {
    type: "object",
    properties: {
      path: {
        type: "string",
        description:
          "Relative directory path within your workspace. Omit for workspace root.",
      },
    },
    required: [],
  },
  async execute(input: unknown, ctx: ToolContext): Promise<string> {
    const obj = (input ?? {}) as Record<string, unknown>;
    const rawPath = obj["path"];
    const relative = typeof rawPath === "string" ? rawPath : undefined;
    const dir = resolveListPath(ctx.workspaceDir, relative);

    if (!fs.existsSync(dir)) {
      return JSON.stringify({
        entries: [],
        note: "directory does not exist yet",
      });
    }
    let stat: fs.Stats;
    try {
      stat = fs.statSync(dir);
    } catch (e) {
      throw new ToolError("Io", (e as Error).message);
    }
    if (!stat.isDirectory()) {
      throw new ToolError(
        "InvalidArgs",
        `${relative ?? "."} is not a directory`,
      );
    }

    const entries: Array<{ name: string; type: string; size: number }> = [];
    for (const name of fs.readdirSync(dir)) {
      const full = path.join(dir, name);
      let entryStat: fs.Stats;
      try {
        entryStat = fs.statSync(full);
      } catch {
        continue;
      }
      entries.push({
        name,
        type: entryStat.isDirectory() ? "directory" : "file",
        size: entryStat.size,
      });
    }
    entries.sort((a, b) => a.name.localeCompare(b.name));
    return JSON.stringify({ entries });
  },
};

// ---------------------------------------------------------------------------
// delete
// ---------------------------------------------------------------------------

export const deleteHandler: ToolHandler = {
  name: "delete",
  description: DELETE_DESCRIPTION,
  inputSchema: {
    type: "object",
    properties: {
      path: {
        type: "string",
        description:
          "Relative path to the file to remove, within your workspace.",
      },
    },
    required: ["path"],
  },
  async execute(input: unknown, ctx: ToolContext): Promise<string> {
    const pathStr = requirePathField(input);
    if (isPromptVisiblePath(pathStr)) {
      throw new ToolError(
        "InvalidArgs",
        `${pathStr} is a prompt-visible file and cannot be deleted`,
      );
    }
    if (ctx.characterDataDir.length === 0) {
      throw new ToolError(
        "InvalidArgs",
        "character data directory not configured",
      );
    }
    const resolved = resolvePath(ctx.workspaceDir, pathStr);
    let stat: fs.Stats;
    try {
      stat = fs.statSync(resolved);
    } catch (e) {
      const code = (e as NodeJS.ErrnoException).code;
      if (code === "ENOENT") {
        throw new ToolError("Io", `file not found: ${pathStr}`);
      }
      throw new ToolError("Io", (e as Error).message);
    }
    if (!stat.isFile()) {
      throw new ToolError(
        "InvalidArgs",
        `${pathStr} is not a file (delete only operates on regular files)`,
      );
    }

    const workspaceRoot = ctx.workspaceDir;
    const relativeUnderWorkspace = path.relative(workspaceRoot, resolved);
    const stamp = trashStampUtc(new Date());
    const trashRoot = path.join(ctx.characterDataDir, "trash", stamp);
    const trashTarget = path.join(
      trashRoot,
      relativeUnderWorkspace.length > 0
        ? relativeUnderWorkspace
        : path.basename(resolved),
    );
    fs.mkdirSync(path.dirname(trashTarget), { recursive: true });

    try {
      fs.renameSync(resolved, trashTarget);
    } catch (renameErr) {
      // Cross-device rename fails with EXDEV. Fall back to copy + remove.
      try {
        fs.copyFileSync(resolved, trashTarget);
      } catch (copyErr) {
        throw new ToolError(
          "Io",
          `could not move file to trash (rename: ${(renameErr as Error).message}, copy fallback: ${(copyErr as Error).message})`,
        );
      }
      try {
        fs.rmSync(resolved);
      } catch (removeErr) {
        throw new ToolError(
          "Io",
          `could not remove original after copy: ${(removeErr as Error).message}`,
        );
      }
    }

    // Display path: relative to the data dir's parent, forward-slashed.
    const dataDirParent = path.dirname(ctx.characterDataDir);
    let trashedDisplay = trashTarget;
    if (dataDirParent.length > 0) {
      const rel = path.relative(dataDirParent, trashTarget);
      trashedDisplay = rel.length > 0 ? rel : trashTarget;
    }
    return JSON.stringify({
      path: pathStr,
      deleted: true,
      trashed_to: trashedDisplay.replace(/\\/g, "/"),
    });
  },
};

function trashStampUtc(d: Date): string {
  const pad = (n: number, w = 2): string => String(n).padStart(w, "0");
  const ms = String(d.getUTCMilliseconds()).padStart(3, "0");
  return (
    `${d.getUTCFullYear()}${pad(d.getUTCMonth() + 1)}${pad(d.getUTCDate())}T` +
    `${pad(d.getUTCHours())}${pad(d.getUTCMinutes())}${pad(d.getUTCSeconds())}${ms}Z`
  );
}

// ---------------------------------------------------------------------------
// file_search
// ---------------------------------------------------------------------------

function findCaseInsensitiveMatch(
  line: string,
  queryLower: string,
): { start: number; end: number } | undefined {
  const lower = line.toLowerCase();
  const idx = lower.indexOf(queryLower);
  if (idx < 0) return undefined;
  // The lowercased length can differ from the original (e.g. ẞ → ß).
  // Walk the original by code-point so the returned offsets index the
  // *original* string correctly.
  const folded_end = idx + queryLower.length;
  let foldedPos = 0;
  let originalStart: number | undefined;
  let originalEnd: number | undefined;
  let i = 0;
  while (i < line.length) {
    const cp = line.codePointAt(i)!;
    const ch = String.fromCodePoint(cp);
    const charLen = ch.length;
    const charFoldedStart = foldedPos;
    const folded = ch.toLowerCase();
    foldedPos += folded.length;
    const charFoldedEnd = foldedPos;

    if (charFoldedEnd > idx && charFoldedStart < folded_end) {
      if (originalStart === undefined) originalStart = i;
      originalEnd = i + charLen;
      if (charFoldedEnd >= folded_end) break;
    }
    i += charLen;
  }
  return {
    start: originalStart ?? 0,
    end: originalEnd ?? line.length,
  };
}

function excerptLine(line: string, matchStart: number, matchEnd: number): string {
  const trimmedStart = line.replace(/^\s+/, "");
  const leadingTrimmedBytes = line.length - trimmedStart.length;
  const trimmed = trimmedStart.replace(/\s+$/, "");

  const mStart = Math.min(
    Math.max(0, matchStart - leadingTrimmedBytes),
    trimmed.length,
  );
  const mEnd = Math.max(
    mStart,
    Math.min(trimmed.length, Math.max(0, matchEnd - leadingTrimmedBytes)),
  );
  const matchChars = [...trimmed.slice(mStart, mEnd)].length;
  const availableBefore = [...trimmed.slice(0, mStart)].length;
  const availableAfter = [...trimmed.slice(mEnd)].length;
  const contextChars = Math.max(0, SEARCH_EXCERPT_CHARS - matchChars);

  let beforeChars = Math.min(Math.floor(contextChars / 2), availableBefore);
  let afterChars = Math.min(contextChars - beforeChars, availableAfter);
  const unusedAfter = Math.max(0, contextChars - beforeChars - afterChars);
  if (unusedAfter > 0) {
    beforeChars += Math.min(availableBefore - beforeChars, unusedAfter);
  }
  const unusedBefore = Math.max(0, contextChars - beforeChars - afterChars);
  if (unusedBefore > 0) {
    afterChars += Math.min(availableAfter - afterChars, unusedBefore);
  }

  const excerptStart = byteIndexBeforeChars(trimmed, mStart, beforeChars);
  const excerptEnd = byteIndexAfterChars(trimmed, mEnd, afterChars);
  let out = "";
  if (excerptStart > 0) out += "...";
  out += trimmed.slice(excerptStart, excerptEnd);
  if (excerptEnd < trimmed.length) out += "...";
  return out;
}

function byteIndexBeforeChars(s: string, end: number, count: number): number {
  let start = end;
  for (let i = 0; i < count; i++) {
    if (start === 0) return 0;
    // Walk back one code point.
    let cpStart = start - 1;
    if (cpStart > 0 && s.charCodeAt(cpStart) >= 0xdc00 && s.charCodeAt(cpStart) <= 0xdfff) {
      cpStart -= 1;
    }
    start = cpStart;
  }
  return start;
}

function byteIndexAfterChars(s: string, start: number, count: number): number {
  let end = start;
  for (let i = 0; i < count; i++) {
    if (end >= s.length) return s.length;
    const code = s.charCodeAt(end);
    end += code >= 0xd800 && code <= 0xdbff ? 2 : 1;
  }
  return Math.min(end, s.length);
}

interface SearchEntry {
  path: string;
  mtimeMs: number;
}

async function enumerateFiles(
  root: string,
  retrievalConfig: { max_file_bytes: number },
): Promise<{ candidates: SearchEntry[]; skipped: number }> {
  const candidates: SearchEntry[] = [];
  let skipped = 0;
  const pending: string[] = [root];
  while (pending.length > 0) {
    const here = pending.pop()!;
    let stat: fs.Stats;
    try {
      stat = fs.lstatSync(here);
    } catch {
      continue;
    }
    if (stat.isSymbolicLink()) continue;
    if (stat.isDirectory()) {
      let names: string[];
      try {
        names = fs.readdirSync(here);
      } catch {
        continue;
      }
      for (const n of names) pending.push(path.join(here, n));
      continue;
    }
    if (!stat.isFile()) continue;
    if (stat.size > retrievalConfig.max_file_bytes) {
      skipped += 1;
      continue;
    }
    candidates.push({ path: here, mtimeMs: stat.mtimeMs });
  }
  candidates.sort((a, b) => {
    if (a.mtimeMs !== b.mtimeMs) return b.mtimeMs - a.mtimeMs;
    return a.path.localeCompare(b.path);
  });
  return { candidates, skipped };
}

export const fileSearchHandler: ToolHandler = {
  name: "file_search",
  description: FILE_SEARCH_DESCRIPTION,
  inputSchema: {
    type: "object",
    properties: {
      query: {
        type: "string",
        description:
          "Keyword, phrase, or natural-language description to search for.",
      },
      mode: {
        type: "string",
        enum: ["hybrid", "lexical", "vector"],
        description:
          'Ranking mode. `hybrid` (default) blends semantic similarity with substring matching. `lexical` is case-insensitive substring only, ordered by file recency. `vector` is pure semantic similarity.',
      },
      path: {
        type: "string",
        description:
          "Optional relative path to scope the search to a subtree. Works in all modes — hybrid/vector queries are filtered to this subtree after ranking against the workspace-wide embedding index.",
      },
      max_results: {
        type: "number",
        description: "Maximum matches to return. Defaults to 20, maximum 100.",
      },
    },
    required: ["query"],
  },
  async execute(input: unknown, ctx: ToolContext): Promise<string> {
    const obj = (input ?? {}) as Record<string, unknown>;
    const rawQuery = obj["query"];
    if (typeof rawQuery !== "string") {
      throw new ToolError("InvalidArgs", "missing required field: query");
    }
    const query = rawQuery.trim();
    if (query.length === 0) {
      throw new ToolError("InvalidArgs", "query must not be empty");
    }
    const queryLower = query.toLowerCase();

    const rawMode = obj["mode"];
    let mode: "hybrid" | "lexical" | "vector" = "hybrid";
    if (rawMode !== undefined) {
      if (typeof rawMode !== "string") {
        throw new ToolError("InvalidArgs", "search `mode` must be a string");
      }
      if (rawMode === "hybrid" || rawMode === "lexical" || rawMode === "vector") {
        mode = rawMode;
      } else {
        throw new ToolError(
          "InvalidArgs",
          `unknown search mode '${rawMode}'; expected hybrid, lexical, or vector`,
        );
      }
    }

    const rawMaxResults = obj["max_results"];
    const maxResults =
      typeof rawMaxResults === "number" && Number.isFinite(rawMaxResults)
        ? Math.min(SEARCH_MAX_RESULTS, Math.max(1, Math.floor(rawMaxResults)))
        : SEARCH_DEFAULT_MAX_RESULTS;

    if (ctx.workspaceDir.length === 0) {
      throw new ToolError("InvalidArgs", "workspace not configured");
    }
    const rawPath = obj["path"];
    const root = resolveListPath(
      ctx.workspaceDir,
      typeof rawPath === "string" ? rawPath : undefined,
    );

    if (!fs.existsSync(root)) {
      return JSON.stringify({
        query,
        results: [],
        count: 0,
        note: "path does not exist",
      });
    }

    const { candidates, skipped: initialSkipped } = await enumerateFiles(
      root,
      ctx.retrievalConfig,
    );
    let skipped = initialSkipped;

    const results: Array<Record<string, unknown>> = [];
    const filesSummary: Array<{ path: string; hits: number }> = [];
    let searchedFiles = 0;

    outer: for (const c of candidates) {
      let content: string;
      try {
        content = fs.readFileSync(c.path, "utf8");
      } catch {
        continue;
      }
      // Heuristic for binary content matching Rust's `String::from_utf8`
      // rejection: if the read produced a replacement character at a
      // position that doesn't trace back to a valid UTF-8 sequence, treat
      // as binary. fs.readFileSync with 'utf8' replaces invalid bytes
      // with U+FFFD, so we use that as the marker.
      if (content.includes("�")) {
        skipped += 1;
        continue;
      }
      searchedFiles += 1;
      const display = displayPathFor(ctx.workspaceDir, c.path);
      const lines = content.split("\n");
      let fileHits = 0;
      for (let lineIdx = 0; lineIdx < lines.length; lineIdx++) {
        const m = findCaseInsensitiveMatch(lines[lineIdx]!, queryLower);
        if (m === undefined) continue;
        results.push({
          path: display,
          line: lineIdx + 1,
          excerpt: excerptLine(lines[lineIdx]!, m.start, m.end),
        });
        fileHits += 1;
        if (results.length >= maxResults) {
          if (fileHits > 0) filesSummary.push({ path: display, hits: fileHits });
          break outer;
        }
      }
      if (fileHits > 0) filesSummary.push({ path: display, hits: fileHits });
    }

    const count = results.length;
    const response: Record<string, unknown> = {
      query,
      results,
      count,
      searched_files: searchedFiles,
      skipped_binary_or_large: skipped,
      mode: "lexical",
    };
    if (count > 0) {
      response["files"] = filesSummary;
      response["note"] =
        "These are line-level excerpts, ordered by file recency. Call `read` on the top file paths to see surrounding context — excerpts almost never contain the full answer, and one file often references others worth reading too.";
    }
    // hybrid/vector modes are gracefully degraded: until the Phase 6
    // embedder lands, we fall back to lexical and flag it.
    if (mode !== "lexical") {
      response["semantic_unavailable"] = "embedder not configured";
    }
    return JSON.stringify(response);
  },
};

// ---------------------------------------------------------------------------
// exec
// ---------------------------------------------------------------------------

/** Default allowed commands — verbatim from workspace.rs:1134-1164. */
const DEFAULT_ALLOWLIST = new Set<string>([
  "ls",
  "cat",
  "rg",
  "git",
  "wc",
  "pwd",
  "sort",
  "uniq",
  "dirname",
  "basename",
  "file",
  "stat",
  "du",
  "df",
  "which",
  "whoami",
  "date",
  "tree",
  "fd",
  "cargo",
  "rustc",
  "rustfmt",
  "clippy",
  "rust-analyzer",
  "npm",
  "pnpm",
  "yarn",
  "make",
  "cmake",
]);

/**
 * Minimal POSIX shell-words splitter — single quotes preserve verbatim,
 * double quotes interpret `\"` and `\\`, backslash outside quotes
 * escapes the next char, whitespace separates tokens. Unclosed quotes
 * throw. Sufficient to replace the Rust `shell_words::split` call.
 */
export function shellSplit(s: string): string[] {
  const tokens: string[] = [];
  let i = 0;
  while (i < s.length) {
    const ch = s[i]!;
    if (ch === " " || ch === "\t" || ch === "\n") {
      i += 1;
      continue;
    }
    let token = "";
    let inSingle = false;
    let inDouble = false;
    while (i < s.length) {
      const c = s[i]!;
      if (!inSingle && !inDouble && (c === " " || c === "\t" || c === "\n")) {
        break;
      }
      if (!inDouble && c === "'") {
        inSingle = !inSingle;
        i += 1;
        continue;
      }
      if (!inSingle && c === '"') {
        inDouble = !inDouble;
        i += 1;
        continue;
      }
      if (!inSingle && c === "\\") {
        // backslash outside single quotes
        if (i + 1 >= s.length) {
          throw new Error("trailing backslash");
        }
        const nxt = s[i + 1]!;
        if (inDouble && nxt !== '"' && nxt !== "\\" && nxt !== "$" && nxt !== "`" && nxt !== "\n") {
          // In double quotes, backslash only escapes specific chars;
          // otherwise both backslash and the char are preserved.
          token += "\\" + nxt;
        } else {
          token += nxt;
        }
        i += 2;
        continue;
      }
      token += c;
      i += 1;
    }
    if (inSingle || inDouble) {
      throw new Error("unclosed quote");
    }
    tokens.push(token);
  }
  return tokens;
}

function parseCommand(command: string): string[] {
  let argv: string[];
  try {
    argv = shellSplit(command);
  } catch (e) {
    throw new ToolError(
      "InvalidArgs",
      `invalid command line: ${(e as Error).message}`,
    );
  }
  if (argv.length === 0) {
    throw new ToolError("InvalidArgs", "command is empty");
  }
  return argv;
}

function isCommandAllowed(argv: string[]): boolean {
  const first = argv[0];
  if (first === undefined) return false;
  if (first.includes("/") || first.includes("\\")) return false;
  return DEFAULT_ALLOWLIST.has(first);
}

function isPathLikeArg(arg: string): boolean {
  if (arg.length === 0 || arg === "-" || arg === "--") return false;
  if (
    arg.startsWith("/") ||
    arg.startsWith("\\") ||
    arg.startsWith("./") ||
    arg.startsWith("../") ||
    arg.startsWith("~/") ||
    arg.startsWith("~\\")
  ) {
    return true;
  }
  if (arg === "." || arg === "..") return true;
  if (arg.includes("/") || arg.includes("\\")) return true;
  if (arg.startsWith("file:")) return true;
  return false;
}

function validateExecPathArg(workspaceDir: string, arg: string): void {
  // Component check: `..`, absolute markers.
  const parts = arg.split(/[\\/]+/);
  for (const p of parts) {
    if (p === "..") {
      throw new ToolError(
        "InvalidArgs",
        `exec argument escapes workspace: ${arg}`,
      );
    }
  }
  if (path.isAbsolute(arg)) {
    throw new ToolError(
      "InvalidArgs",
      `exec argument uses an absolute path: ${arg}`,
    );
  }
  // Confinement check via resolvePath (canonicalize + inside-workspace).
  resolvePath(workspaceDir, arg);
}

function validateExecArgs(workspaceDir: string, argv: string[]): void {
  if (workspaceDir.length === 0) {
    throw new ToolError("InvalidArgs", "workspace not configured");
  }
  for (let i = 1; i < argv.length; i++) {
    const arg = argv[i]!;
    if (arg.startsWith("file:")) {
      throw new ToolError(
        "InvalidArgs",
        `exec argument uses a file URL: ${arg}`,
      );
    }
    const eqIdx = arg.indexOf("=");
    if (eqIdx >= 0) {
      const value = arg.slice(eqIdx + 1);
      if (isPathLikeArg(value)) validateExecPathArg(workspaceDir, value);
    }
    if (isPathLikeArg(arg)) validateExecPathArg(workspaceDir, arg);
  }
}

export const execHandler: ToolHandler = {
  name: "exec",
  description: EXEC_DESCRIPTION,
  inputSchema: {
    type: "object",
    properties: {
      command: { type: "string", description: "Shell command to execute." },
      workdir: {
        type: "string",
        description:
          "Working directory for the command (relative to workspace root). Optional.",
      },
    },
    required: ["command"],
  },
  async execute(input: unknown, ctx: ToolContext): Promise<string> {
    const obj = (input ?? {}) as Record<string, unknown>;
    const command = obj["command"];
    if (typeof command !== "string") {
      throw new ToolError("InvalidArgs", "missing required field: command");
    }
    const argv = parseCommand(command);
    if (!isCommandAllowed(argv)) {
      throw new ToolError(
        "InvalidArgs",
        `command '${argv[0]}' is not in the allowlist`,
      );
    }
    validateExecArgs(ctx.workspaceDir, argv);

    const rawWorkdir = obj["workdir"];
    let cwd = ctx.workspaceDir;
    if (typeof rawWorkdir === "string" && rawWorkdir.length > 0) {
      cwd = resolvePath(ctx.workspaceDir, rawWorkdir);
    }

    return await new Promise<string>((resolve, reject) => {
      const child = spawn(argv[0]!, argv.slice(1), {
        cwd,
        // shell:false (default) — load-bearing per the no-shell-invocation
        // requirement in AGENTS.md.
        shell: false,
        stdio: ["ignore", "pipe", "pipe"],
      });
      let stdout = "";
      let stderr = "";
      child.stdout.on("data", (chunk: Buffer) => {
        stdout += chunk.toString("utf8");
      });
      child.stderr.on("data", (chunk: Buffer) => {
        stderr += chunk.toString("utf8");
      });
      child.on("error", (err) => {
        reject(new ToolError("Io", err.message));
      });
      child.on("close", (code) => {
        resolve(
          JSON.stringify({
            command,
            exit_code: code,
            stdout,
            stderr,
          }),
        );
      });
    });
  },
};
