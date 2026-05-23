/**
 * Deferred edits + active prompt snapshot.
 *
 * Port of `backend/daemon/src/memory/deferred_edits.rs`.
 *
 * Edits to "prompt-visible" workspace files (SOUL.md, USER.md, AGENTS.md,
 * TOOLS.md, HEARTBEAT.md, MEMORY.md) are visible on the filesystem
 * immediately but only become *prompt-active* after the next compaction
 * boundary. The snapshot under `<characterDataDir>/active_prompt/` is the
 * version the system prompt reads from; `apply_deferred_edits` refreshes
 * the snapshot and clears the queue.
 *
 * `tools/paths.ts` already owns the normalization helpers
 * (`normalizeProtectedPath`, `normalizePromptVisiblePath`, etc.) — we
 * re-export them here so the conceptual home (`memory/`) matches the
 * Rust layout without duplicating the implementation.
 */

import fs from "node:fs";
import path from "node:path";

import {
  isPromptVisiblePath,
  isProtectedPath,
  normalizePromptVisiblePath,
  normalizeProtectedPath,
} from "../tools/paths.ts";

// ---------------------------------------------------------------------------
// Re-exports — single source of truth lives in tools/paths.ts
// ---------------------------------------------------------------------------

export {
  isPromptVisiblePath,
  isProtectedPath,
  normalizePromptVisiblePath,
  normalizeProtectedPath,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

export const SOUL_FILE = "SOUL.md";
export const USER_FILE = "USER.md";
export const AGENTS_FILE = "AGENTS.md";
export const TOOLS_FILE = "TOOLS.md";
export const HEARTBEAT_FILE = "HEARTBEAT.md";

/** Top-level prompt-visible workspace files (no MEMORY.md — that's the index). */
export const PROTECTED_PATHS = [
  SOUL_FILE,
  USER_FILE,
  AGENTS_FILE,
  TOOLS_FILE,
  HEARTBEAT_FILE,
] as const;

/** Active-prompt snapshot directory under the character data dir. */
const ACTIVE_PROMPT_DIR = "active_prompt";

export const MEMORY_INDEX_FILE = "MEMORY.md";
export const MEMORY_INDEX_DEFERRED_PATH = "MEMORY.md";

const LEGACY_RECENT_MEMORY_SNAPSHOT = "RECENT_MEMORY.md";

const DEFAULT_TOOLS_GUIDANCE = `# TOOLS

Use tools when they materially help.

- Read files before editing them.
- Search memory files before guessing facts about the user or past events.
- Prefer concise, direct tool use over busywork.
`;

const DEFAULT_HEARTBEAT_GUIDANCE = `# HEARTBEAT

- Use this private turn however seems useful.
- You may use tools, schedule the next wake, or send the user a message.
- If nothing needs action, respond HEARTBEAT_OK.
`;

const DEFERRED_EDITS_QUEUE = "deferred_edits.jsonl";

// ---------------------------------------------------------------------------
// Path helpers — inlined `shore_config::character_*` for self-containment
// ---------------------------------------------------------------------------

/** `<configDir>/characters/<name>/`. */
export function characterConfigDir(configDir: string, charName: string): string {
  return path.join(configDir, "characters", charName);
}

/** `<configDir>/characters/<name>/workspace/`. */
export function characterWorkspaceDir(configDir: string, charName: string): string {
  return path.join(characterConfigDir(configDir, charName), "workspace");
}

/** `<configDir>/characters/<name>/workspace/<file>`. */
export function characterWorkspaceFile(
  configDir: string,
  charName: string,
  file: string,
): string {
  return path.join(characterWorkspaceDir(configDir, charName), file);
}

/** `<configDir>/characters/<name>/workspace/memory/`. */
export function characterMemoryDir(configDir: string, charName: string): string {
  return path.join(characterWorkspaceDir(configDir, charName), "memory");
}

// ---------------------------------------------------------------------------
// Active prompt snapshot
// ---------------------------------------------------------------------------

export function activePromptDir(characterDataDir: string): string {
  return path.join(characterDataDir, ACTIVE_PROMPT_DIR);
}

export function activePromptFile(characterDataDir: string, name: string): string {
  return path.join(activePromptDir(characterDataDir), name);
}

export function memoryIndexPath(configDir: string, charName: string): string {
  return path.join(characterWorkspaceDir(configDir, charName), MEMORY_INDEX_FILE);
}

/**
 * Load the prompt-visible memory index. If the active snapshot file
 * exists, that's the answer (even if empty → `undefined`) — an empty
 * snapshot is the sentinel that says "canonical edit is pending
 * compaction, don't surface it yet." Falls back to canonical only when
 * the snapshot doesn't exist at all.
 */
export function loadMemoryIndex(
  characterDataDir: string,
  configDir: string,
  charName: string,
): string | undefined {
  const active = activePromptFile(characterDataDir, MEMORY_INDEX_FILE);
  if (fs.existsSync(active)) return readNonEmpty(active);
  return readNonEmpty(memoryIndexPath(configDir, charName));
}

export function loadCanonicalMemoryIndex(
  configDir: string,
  charName: string,
): string | undefined {
  return readNonEmpty(memoryIndexPath(configDir, charName));
}

export function loadActivePromptFile(
  characterDataDir: string,
  name: string,
): string | undefined {
  return readNonEmpty(activePromptFile(characterDataDir, name));
}

function readNonEmpty(file: string): string | undefined {
  try {
    const content = fs.readFileSync(file, "utf8");
    if (content.trim().length === 0) return undefined;
    return content;
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw e;
  }
}

// ---------------------------------------------------------------------------
// Deferred edit queue
// ---------------------------------------------------------------------------

/**
 * Append a queue entry for a prompt-visible workspace file. No-op on
 * non-prompt-visible paths so callers can pass through any edit path.
 *
 * Creates the queue file and, for MEMORY.md, a zero-byte sentinel under
 * the active-prompt directory so subsequent `ensure_active_prompt_snapshot`
 * calls don't re-seed it from canonical (the canonical version is now
 * pending activation).
 */
export function queueDeferredEdit(characterDataDir: string, p: string): void {
  const normalized = normalizePromptVisiblePath(p);
  if (normalized === undefined) return;

  fs.mkdirSync(characterDataDir, { recursive: true });
  if (normalized === MEMORY_INDEX_DEFERRED_PATH) {
    ensureDeferredMemoryIndexSentinel(characterDataDir);
  }

  const queuePath = path.join(characterDataDir, DEFERRED_EDITS_QUEUE);
  const line =
    JSON.stringify({
      path: normalized,
      timestamp: nowRfc3339Local(),
    }) + "\n";
  fs.appendFileSync(queuePath, line, "utf8");
}

/**
 * Wrapper for the common case of queueing a MEMORY.md refresh — keeps
 * compaction sites parallel to the Rust API.
 */
export function noteMemoryIndexDeferred(characterDataDir: string): void {
  queueDeferredEdit(characterDataDir, MEMORY_INDEX_DEFERRED_PATH);
}

/**
 * Return the deduped set of prompt-visible paths waiting for activation.
 * Malformed queue lines are skipped silently — mirrors Rust's tolerant
 * `serde_json::from_str` loop.
 */
export function pendingDeferredEditPaths(characterDataDir: string): string[] {
  const queuePath = path.join(characterDataDir, DEFERRED_EDITS_QUEUE);
  let content: string;
  try {
    content = fs.readFileSync(queuePath, "utf8");
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return [];
    throw e;
  }

  const seen = new Set<string>();
  for (const line of content.split("\n")) {
    const trimmed = line.trim();
    if (trimmed.length === 0) continue;
    let parsed: unknown;
    try {
      parsed = JSON.parse(trimmed);
    } catch {
      continue;
    }
    const obj = parsed as Record<string, unknown>;
    const raw = obj["path"];
    if (typeof raw !== "string") continue;
    const normalized = normalizePromptVisiblePath(raw);
    if (normalized !== undefined) seen.add(normalized);
  }
  return [...seen].sort();
}

/**
 * Refresh every protected file in the active snapshot from the canonical
 * workspace, then clear the queue. This is the compaction-boundary
 * activation step.
 */
export function applyDeferredEdits(
  characterDataDir: string,
  configDir: string,
  charName: string,
): void {
  refreshActivePromptSnapshot(characterDataDir, configDir, charName);

  const queuePath = path.join(characterDataDir, DEFERRED_EDITS_QUEUE);
  if (fs.existsSync(queuePath)) {
    fs.rmSync(queuePath);
  }
}

// ---------------------------------------------------------------------------
// Workspace bootstrap + snapshot seeding
// ---------------------------------------------------------------------------

/**
 * Ensure the character workspace layout exists and migrate any legacy
 * bootstrap files / memory directories into it. Idempotent: existing
 * files are left alone.
 *
 * Migrations performed:
 *   - characters/<C>/character.md → workspace/SOUL.md
 *   - characters/<C>/user.md      → workspace/USER.md
 *   - characters/<C>/prompts/system.md → workspace/AGENTS.md
 *   - <configDir>/user.md         → workspace/USER.md (only if absent)
 *   - <dataDir>/<C>/memories/**   → workspace/memory/**
 *
 * Default TOOLS.md and HEARTBEAT.md are written if absent.
 */
export function ensureCharacterWorkspace(
  characterDataDir: string,
  configDir: string,
  charName: string,
): void {
  const charConfigDir = characterConfigDir(configDir, charName);
  const workspaceDir = characterWorkspaceDir(configDir, charName);
  const memoryDir = characterMemoryDir(configDir, charName);

  fs.mkdirSync(workspaceDir, { recursive: true });
  fs.mkdirSync(memoryDir, { recursive: true });

  migrateLegacyFile(
    path.join(charConfigDir, "character.md"),
    path.join(workspaceDir, SOUL_FILE),
  );
  migrateLegacyFile(
    path.join(charConfigDir, "user.md"),
    path.join(workspaceDir, USER_FILE),
  );
  migrateLegacyFile(
    path.join(charConfigDir, "prompts", "system.md"),
    path.join(workspaceDir, AGENTS_FILE),
  );

  const globalUser = path.join(configDir, "user.md");
  const workspaceUser = path.join(workspaceDir, USER_FILE);
  if (fs.existsSync(globalUser) && !fs.existsSync(workspaceUser)) {
    fs.copyFileSync(globalUser, workspaceUser);
  }

  writeDefaultIfMissing(path.join(workspaceDir, TOOLS_FILE), DEFAULT_TOOLS_GUIDANCE);
  writeDefaultIfMissing(
    path.join(workspaceDir, HEARTBEAT_FILE),
    DEFAULT_HEARTBEAT_GUIDANCE,
  );

  const legacyMemories = path.join(characterDataDir, "memories");
  if (fs.existsSync(legacyMemories)) {
    copyTreeIfMissing(legacyMemories, memoryDir);
  }
}

/**
 * Seed the active-prompt snapshot from the canonical workspace. Existing
 * snapshot files are left untouched (this is the gate that keeps edits
 * deferred). Also cleans up the legacy `RECENT_MEMORY.md` snapshot from
 * the pre-rename era.
 */
export function ensureActivePromptSnapshot(
  characterDataDir: string,
  configDir: string,
  charName: string,
): void {
  ensureCharacterWorkspace(characterDataDir, configDir, charName);

  const activeDir = activePromptDir(characterDataDir);
  fs.mkdirSync(activeDir, { recursive: true });

  for (const name of PROTECTED_PATHS) {
    copyPromptVisibleFile(characterDataDir, configDir, charName, name, true);
  }
  copyPromptVisibleFile(
    characterDataDir,
    configDir,
    charName,
    MEMORY_INDEX_DEFERRED_PATH,
    true,
  );

  const legacy = path.join(activeDir, LEGACY_RECENT_MEMORY_SNAPSHOT);
  if (fs.existsSync(legacy)) fs.rmSync(legacy);
}

/**
 * Refresh every protected file (and MEMORY.md) in the active snapshot
 * from the canonical workspace. Called at the compaction boundary by
 * `applyDeferredEdits`.
 */
export function refreshActivePromptSnapshot(
  characterDataDir: string,
  configDir: string,
  charName: string,
): void {
  ensureCharacterWorkspace(characterDataDir, configDir, charName);
  for (const name of PROTECTED_PATHS) {
    copyPromptVisibleFile(characterDataDir, configDir, charName, name, false);
  }
  copyPromptVisibleFile(
    characterDataDir,
    configDir,
    charName,
    MEMORY_INDEX_DEFERRED_PATH,
    false,
  );
}

// ---------------------------------------------------------------------------
// Internal — file ops
// ---------------------------------------------------------------------------

function canonicalPromptVisibleFile(
  configDir: string,
  charName: string,
  p: string,
): string {
  if (p === MEMORY_INDEX_DEFERRED_PATH) {
    return memoryIndexPath(configDir, charName);
  }
  return characterWorkspaceFile(configDir, charName, p);
}

function activePromptSnapshotName(p: string): string {
  return p === MEMORY_INDEX_DEFERRED_PATH ? MEMORY_INDEX_FILE : p;
}

/**
 * Copy a canonical prompt-visible file into the active snapshot. With
 * `seedOnly`, an existing snapshot file is left intact (this is what
 * keeps deferred edits from leaking into the prompt). Without it
 * (refresh mode), the snapshot is overwritten; if the canonical file
 * has been deleted, the snapshot is removed too.
 */
function copyPromptVisibleFile(
  characterDataDir: string,
  configDir: string,
  charName: string,
  p: string,
  seedOnly: boolean,
): void {
  const activeDir = activePromptDir(characterDataDir);
  fs.mkdirSync(activeDir, { recursive: true });

  const src = canonicalPromptVisibleFile(configDir, charName, p);
  const dst = path.join(activeDir, activePromptSnapshotName(p));

  if (seedOnly && fs.existsSync(dst)) return;

  if (fs.existsSync(src)) {
    fs.copyFileSync(src, dst);
  } else if (!seedOnly && fs.existsSync(dst)) {
    fs.rmSync(dst);
  }
}

/**
 * Drop a zero-byte sentinel at active_prompt/MEMORY.md so subsequent
 * `ensureActivePromptSnapshot` calls (which run in seed-only mode) won't
 * pull the unactivated canonical version into the snapshot. Idempotent.
 */
function ensureDeferredMemoryIndexSentinel(characterDataDir: string): void {
  const dst = activePromptFile(characterDataDir, MEMORY_INDEX_FILE);
  if (fs.existsSync(dst)) return;
  fs.mkdirSync(path.dirname(dst), { recursive: true });
  fs.writeFileSync(dst, "");
}

function migrateLegacyFile(src: string, dst: string): void {
  if (fs.existsSync(src) && !fs.existsSync(dst)) {
    fs.mkdirSync(path.dirname(dst), { recursive: true });
    fs.copyFileSync(src, dst);
  }
}

function writeDefaultIfMissing(file: string, content: string): void {
  if (fs.existsSync(file)) return;
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, content);
}

function copyTreeIfMissing(src: string, dst: string): void {
  const stat = fs.statSync(src);
  if (stat.isFile()) {
    if (!fs.existsSync(dst)) {
      fs.mkdirSync(path.dirname(dst), { recursive: true });
      fs.copyFileSync(src, dst);
    }
    return;
  }
  fs.mkdirSync(dst, { recursive: true });
  for (const entry of fs.readdirSync(src)) {
    const childSrc = path.join(src, entry);
    const childDst = path.join(dst, entry);
    if (fs.statSync(childSrc).isDirectory()) {
      copyTreeIfMissing(childSrc, childDst);
    } else if (!fs.existsSync(childDst)) {
      fs.copyFileSync(childSrc, childDst);
    }
  }
}

function nowRfc3339Local(): string {
  const d = new Date();
  const pad = (n: number, w = 2): string => String(n).padStart(w, "0");
  const tzOffsetMin = -d.getTimezoneOffset();
  const sign = tzOffsetMin >= 0 ? "+" : "-";
  const absMin = Math.abs(tzOffsetMin);
  const offHours = Math.floor(absMin / 60);
  const offMins = absMin % 60;
  const offsetStr = `${sign}${pad(offHours)}:${pad(offMins)}`;
  return (
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T` +
    `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}` +
    offsetStr
  );
}
