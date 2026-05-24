/**
 * Tool registry — handler interface, context, error type, and the
 * single-source-of-truth `defaultRegistry()` builder.
 *
 * Ported from `backend/daemon/src/tools/mod.rs`. The TS port differs in
 * two ways:
 *   1. `ToolHandler.execute` takes a `ToolContext` parameter instead of
 *      reaching into globals — the Rust dispatch passes `&dyn ToolContext`
 *      through, we pass it through `runToolLoop`.
 *   2. Tool descriptions are inline string constants; the Rust impl uses
 *      `include_prompt!("../../prompts/tools/.../*.md")` at compile time.
 *      Both forms strip a single trailing newline (see prompts.rs:23) so
 *      cache keys stay byte-stable — our constants simply omit the trailing
 *      newline. If you touch a description, change it in both places (or
 *      port the Rust prompt files to assets and load them at startup).
 *
 * Two name changes from Rust:
 *   - `search`         → `file_search`
 *   - `search_history` → `conversation_search`
 *
 * The Rust names were too easily conflated by both humans and the model.
 */

import type { ConversationEngine } from "../engine/engine.ts";
import { renderTemplate } from "../engine/prompt.ts";
import type { Embedder } from "../llm/embed.ts";

import {
  activityHeatmapHandler,
  ACTIVITY_HEATMAP_DESCRIPTION,
} from "./activity.ts";
import {
  checkTimeHandler,
  rollDiceHandler,
  setNextWakeHandler,
  CHECK_TIME_DESCRIPTION,
  ROLL_DICE_DESCRIPTION,
  SET_NEXT_WAKE_DESCRIPTION,
} from "./basic.ts";
import {
  generateImageHandler,
  GENERATE_IMAGE_DESCRIPTION,
} from "./images.ts";
import {
  conversationSearchHandler,
  CONVERSATION_SEARCH_DESCRIPTION,
} from "./history.ts";
import { fetchUrlHandler, webSearchHandler, FETCH_URL_DESCRIPTION, WEB_SEARCH_DESCRIPTION } from "./web.ts";
import {
  deleteHandler,
  editHandler,
  execHandler,
  fileSearchHandler,
  listFilesHandler,
  readHandler,
  writeHandler,
  DELETE_DESCRIPTION,
  EDIT_DESCRIPTION,
  EXEC_DESCRIPTION,
  FILE_SEARCH_DESCRIPTION,
  LIST_FILES_DESCRIPTION,
  READ_DESCRIPTION,
  WRITE_DESCRIPTION,
} from "./workspace.ts";

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

export type ToolErrorKind = "InvalidArgs" | "NotImplemented" | "Io" | "Http";

/**
 * Tool error sentinel. `kind` mirrors the Rust `ToolError` variants;
 * the dispatch layer converts these into `tool_result is_error: true`
 * blocks. Use `throw new ToolError("InvalidArgs", "...")` from a handler.
 */
export class ToolError extends Error {
  public readonly kind: ToolErrorKind;
  constructor(kind: ToolErrorKind, message: string) {
    super(formatToolErrorMessage(kind, message));
    this.kind = kind;
    this.name = "ToolError";
  }
}

function formatToolErrorMessage(kind: ToolErrorKind, message: string): string {
  switch (kind) {
    case "InvalidArgs":
      return `invalid args: ${message}`;
    case "NotImplemented":
      return `${message}: not yet implemented`;
    case "Io":
      return `io: ${message}`;
    case "Http":
      return `http: ${message}`;
  }
}

// ---------------------------------------------------------------------------
// Tool toggle config
// ---------------------------------------------------------------------------

/**
 * Per-tool enable/disable from the config's `[tools]` table. A missing
 * entry defaults to enabled. Mirrors `ToolToggles::is_enabled` in Rust.
 *
 * Legacy memory toggles (`memory`, `memory_read`, `memory_write`) are
 * silently ignored so old configs don't accidentally disable the
 * workspace tools — see `legacy_memory_toggles_do_not_gate_tools` in
 * tools/mod.rs.
 */
export interface ToolToggles {
  isEnabled(name: string): boolean;
}

export function defaultToggles(): ToolToggles {
  return { isEnabled: () => true };
}

export function togglesFromConfig(raw: unknown): ToolToggles {
  if (typeof raw !== "object" || raw === null) return defaultToggles();
  const obj = raw as Record<string, unknown>;
  return {
    isEnabled(name: string): boolean {
      const v = obj[name];
      if (typeof v === "boolean") return v;
      return true;
    },
  };
}

// ---------------------------------------------------------------------------
// Tool context — dependency injection passed through `runToolLoop`
// ---------------------------------------------------------------------------

/**
 * Search-tool configuration. Read from the `[search]` table in config.toml.
 * Defaults match Rust's `SearchConfig::default()`.
 */
export interface SearchConfig {
  api_key_env: string;
  max_results: number;
  search_depth: string;
  include_answer: boolean;
}

export function defaultSearchConfig(): SearchConfig {
  return {
    api_key_env: "TAVILY_API_KEY",
    max_results: 5,
    search_depth: "basic",
    include_answer: true,
  };
}

/**
 * Image generation configuration. Read from `[image_generation]` /
 * equivalent in config.toml. Optional — when absent, `generate_image`
 * returns an Io error.
 */
export interface ImageGenConfig {
  provider: string;
  model_id: string;
  api_key: string;
  base_url?: string;
  size: string;
  quality?: string;
  aspect_ratio?: string;
  image_size?: string;
}

export type RetrievalMode = "auto" | "lexical" | "hybrid";
export type RetrievalBinaryMode = "skip" | "metadata" | "try_embed";

/**
 * Workspace retrieval configuration. Defaults mirror Rust's
 * `RetrievalConfig::default()`.
 */
export interface RetrievalConfig {
  mode: RetrievalMode;
  max_file_bytes: number;
  max_indexed_files: number;
  max_total_indexed_bytes: number;
  max_embed_chars_per_file: number;
  binary: RetrievalBinaryMode;
}

export function defaultRetrievalConfig(): RetrievalConfig {
  return {
    mode: "auto",
    max_file_bytes: 2 * 1024 * 1024,
    max_indexed_files: 50_000,
    max_total_indexed_bytes: 1024 * 1024 * 1024,
    max_embed_chars_per_file: 4_000,
    binary: "skip",
  };
}

export function normalizeRetrievalConfig(
  raw: Partial<RetrievalConfig> | undefined,
): RetrievalConfig {
  const defaults = defaultRetrievalConfig();
  if (raw === undefined) return defaults;
  return {
    mode: raw.mode ?? defaults.mode,
    max_file_bytes: raw.max_file_bytes ?? defaults.max_file_bytes,
    max_indexed_files: raw.max_indexed_files ?? defaults.max_indexed_files,
    max_total_indexed_bytes:
      raw.max_total_indexed_bytes ?? defaults.max_total_indexed_bytes,
    max_embed_chars_per_file:
      raw.max_embed_chars_per_file ?? defaults.max_embed_chars_per_file,
    binary: raw.binary ?? defaults.binary,
  };
}

/**
 * Hook signature for scheduling the next heartbeat tick. Returns `undefined`
 * when the caller is not in a heartbeat context — that's the signal
 * `set_next_wake` uses to refuse the call.
 */
export type ScheduleNextWake = (
  hoursFromNow: number,
  reason: string,
) => Promise<Record<string, unknown>> | Record<string, unknown>;

/**
 * Per-character activity stats hook, surfaced by `activity_heatmap`.
 * Returns `undefined` when the autonomy subsystem isn't wired up (Phase
 * 8) — that's the empty-heatmap path.
 */
export interface ActivityStats {
  hourHistogram: number[]; // length 24
  hourClassifications: Array<"peak" | "trough" | "normal">; // length 24
  hasSufficientHeatmap: boolean;
  engagementScore: number;
  sessionsPerDay: number;
  turnCount: number;
}

export type ActivityStatsHook = (
  characterName: string,
) => ActivityStats | undefined;

export interface ToolContext {
  /** Character whose history/workspace this dispatch belongs to. */
  characterName: string;
  /** `$CONFIG_DIR/characters/<name>/`. */
  characterConfigDir: string;
  /** `$DATA_DIR/characters/<name>/` — trash/, segments/, active.jsonl. */
  characterDataDir: string;
  /** `<characterConfigDir>/workspace`. */
  workspaceDir: string;
  /** `$CONFIG_DIR` itself — for `.env` lookups in web_search. */
  configDir: string;
  /** `$DATA_DIR/images/<character>` — where generate_image saves files. */
  imageDir: string;
  /** Engine handle for history/segment access (conversation_search). */
  engine: ConversationEngine;
  /** Web/search/image-gen / retrieval config slices. */
  searchConfig: SearchConfig;
  imageGenConfig?: ImageGenConfig;
  retrievalConfig: RetrievalConfig;
  /** Optional embedding provider + cache path for hybrid/vector file_search. */
  embedder?: Embedder;
  workspaceIndexPath?: string;
  /** Heartbeat-only hook for `set_next_wake`. Undefined during user turns. */
  scheduleNextWake?: ScheduleNextWake;
  /** Autonomy-stats hook for `activity_heatmap`. Undefined until Phase 8. */
  activityStats?: ActivityStatsHook;
}

// ---------------------------------------------------------------------------
// Handler interface and registry
// ---------------------------------------------------------------------------

export interface ToolHandler {
  name: string;
  description: string;
  inputSchema: Record<string, unknown>;
  execute(input: unknown, ctx: ToolContext): Promise<string>;
}

export class ToolRegistry {
  private readonly tools = new Map<string, ToolHandler>();

  register(handler: ToolHandler): void {
    this.tools.set(handler.name, handler);
  }

  get(name: string): ToolHandler | undefined {
    return this.tools.get(name);
  }

  list(): ToolHandler[] {
    return [...this.tools.values()];
  }
}

// ---------------------------------------------------------------------------
// Default registry — the full 15-tool surface
// ---------------------------------------------------------------------------

export interface DefaultRegistryOptions {
  /** Per-tool toggles (typically from the `[tools]` config table). */
  toggles?: ToolToggles;
  /** True for character-private conversations — drops conversation_search and exec. */
  isPrivate?: boolean;
  /** Character name for {{char}} substitution in tool descriptions. */
  characterName: string;
  /** Display name for {{user}} substitution in tool descriptions. */
  displayName: string;
}

/**
 * Build the standard 15-tool registry, filtered for the current privacy
 * mode + config toggles. Tool descriptions are rendered with the
 * supplied {{char}} / {{user}} before registration so the model gets
 * concrete names, not literal placeholders.
 */
export function defaultRegistry(opts: DefaultRegistryOptions): ToolRegistry {
  const reg = new ToolRegistry();
  const toggles = opts.toggles ?? defaultToggles();
  const vars: Record<string, string> = {
    char: opts.characterName,
    character_name: opts.characterName,
    user: opts.displayName,
  };
  const render = (s: string): string => renderTemplate(s, vars);

  // Names mirror Rust's available_tools filter at mod.rs:175-185 plus
  // the two renames.
  const candidates: Array<{ handler: ToolHandler; description: string }> = [
    { handler: readHandler, description: READ_DESCRIPTION },
    { handler: writeHandler, description: WRITE_DESCRIPTION },
    { handler: editHandler, description: EDIT_DESCRIPTION },
    { handler: listFilesHandler, description: LIST_FILES_DESCRIPTION },
    { handler: fileSearchHandler, description: FILE_SEARCH_DESCRIPTION },
    { handler: deleteHandler, description: DELETE_DESCRIPTION },
    { handler: execHandler, description: EXEC_DESCRIPTION },
    { handler: checkTimeHandler, description: CHECK_TIME_DESCRIPTION },
    { handler: rollDiceHandler, description: ROLL_DICE_DESCRIPTION },
    { handler: setNextWakeHandler, description: SET_NEXT_WAKE_DESCRIPTION },
    { handler: webSearchHandler, description: WEB_SEARCH_DESCRIPTION },
    { handler: fetchUrlHandler, description: FETCH_URL_DESCRIPTION },
    {
      handler: conversationSearchHandler,
      description: CONVERSATION_SEARCH_DESCRIPTION,
    },
    {
      handler: activityHeatmapHandler,
      description: ACTIVITY_HEATMAP_DESCRIPTION,
    },
    { handler: generateImageHandler, description: GENERATE_IMAGE_DESCRIPTION },
  ];

  for (const { handler, description } of candidates) {
    if (opts.isPrivate === true && isPrivateGated(handler.name)) continue;
    if (!toggles.isEnabled(handler.name)) continue;
    // Re-emit with the rendered description so {{char}}/{{user}} don't
    // ship as literals in the schema.
    reg.register({ ...handler, description: render(description) });
  }
  return reg;
}

function isPrivateGated(name: string): boolean {
  // Mirrors mod.rs:179 — drop these two in private (character-internal) mode.
  return name === "conversation_search" || name === "exec";
}

// ---------------------------------------------------------------------------
// Re-exports so callers can directly import individual handlers in tests
// ---------------------------------------------------------------------------

export {
  readHandler,
  writeHandler,
  editHandler,
  listFilesHandler,
  fileSearchHandler,
  deleteHandler,
  execHandler,
} from "./workspace.ts";
export { checkTimeHandler, rollDiceHandler, setNextWakeHandler } from "./basic.ts";
export { webSearchHandler, fetchUrlHandler } from "./web.ts";
export { conversationSearchHandler } from "./history.ts";
export { activityHeatmapHandler } from "./activity.ts";
export { generateImageHandler } from "./images.ts";
