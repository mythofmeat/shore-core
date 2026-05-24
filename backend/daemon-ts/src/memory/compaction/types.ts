/**
 * Compaction types — shared data types and trait-equivalent interfaces.
 *
 * Port of `backend/daemon/src/memory/compaction/types.rs`. Mirrors the Rust
 * surface 1:1; Rust's `CompactionConfig` is re-exported from `shore-config`,
 * but the TS daemon's config loader doesn't yet expose it, so the type lives
 * here with the same field shape and defaults as `core/config/src/app.rs`.
 */

import type { ChatRequest } from "../../llm/types.ts";

import type { MemoryFileOp } from "./parser.ts";

// ---------------------------------------------------------------------------
// CompactionConfig — mirrors `core/config/src/app.rs::CompactionConfig`
// ---------------------------------------------------------------------------

export interface CompactionConfig {
  enabled: boolean;
  /** Idle seconds before compaction triggers. */
  idleTriggerSecs: number;
  minTurns: number;
  maxTurns: number;
  /** 0 disables the token-based trigger. */
  maxContextTokens: number;
  keepRecentTurns: number;
}

export const DEFAULT_COMPACTION_CONFIG: CompactionConfig = Object.freeze({
  enabled: true,
  idleTriggerSecs: 1800,
  minTurns: 8,
  maxTurns: 16,
  maxContextTokens: 200_000,
  keepRecentTurns: 2,
});

// ---------------------------------------------------------------------------
// ConversationMessage — input shape for compaction
// ---------------------------------------------------------------------------

export interface ConversationMessage {
  role: string;
  content: string;
  timestamp: string;
  /**
   * True when this is a user-role message whose content_blocks are all
   * ToolResult blocks — a tool-loop intermediate, not a real user turn.
   */
  isToolResultOnly: boolean;
}

// ---------------------------------------------------------------------------
// Outcomes
// ---------------------------------------------------------------------------

export interface CompactionResult {
  memoryFilesWritten: string[];
  conversationId: string;
  newConversationId: string;
  messageCount: number;
  compactedTurns: number;
  retainedCount: number;
  retainedTurns: number;
  markdownPaths: string[];
}

export interface DryRunResult {
  wouldWriteFiles: number;
  fileOpsPreview: MemoryFileOp[];
  messageCount: number;
  compactedTurns: number;
  retainedCount: number;
  retainedTurns: number;
  markdownPreview: string[];
}

export type CompactionOutcome =
  | { kind: "compacted"; result: CompactionResult }
  | { kind: "dryRun"; result: DryRunResult };

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

export type CompactionErrorKind =
  | "llm"
  | "parse"
  | "privateConversation"
  | "insufficientMessages"
  | "conversationManager"
  | "markdownStore";

export class CompactionError extends Error {
  constructor(public readonly kind: CompactionErrorKind, message: string) {
    super(message);
    this.name = "CompactionError";
  }
}

// ---------------------------------------------------------------------------
// RetentionParams — input to the conversation manager's archive_and_retain
// ---------------------------------------------------------------------------

export interface RetentionParams {
  /** Number of messages to keep from the end of active.jsonl. */
  keepLastN: number;
  /**
   * Pre-read content of active.jsonl at the time messages were parsed.
   * Eliminates the TOCTOU race where the file could change between
   * message analysis and the archive-and-retain write.
   */
  activeContent: string;
}

// ---------------------------------------------------------------------------
// Trait-equivalent interfaces for external dependencies
// ---------------------------------------------------------------------------

/**
 * LLM client for compaction.
 *
 * `messages` is the pre-built role/content array (e.g. the compacted slice
 * with a trailing "save your memory" user turn). The implementation sends
 * it to the provider and returns the raw response text.
 *
 * `cachedRequest` is provided when the live chat's cached request prefix
 * should be reused (preserving the Anthropic prompt cache for compaction
 * itself). The TS port currently leaves this hook in place but the
 * background runner does not yet thread a cached request through — the
 * autonomy manager that tracks the most-recent chat request isn't ported
 * yet. Implementations should accept `undefined` and fall back to the
 * fresh path.
 */
export interface CompactionLlm {
  summarize(
    system: string,
    messages: Array<{ role: "user" | "assistant"; content: string }>,
    cachedRequest: ChatRequest | undefined,
  ): Promise<string>;
}

/** Archive old messages and retain recent ones in active.jsonl. */
export interface ConversationManager {
  archiveAndRetain(
    conversationId: string,
    params: RetentionParams,
  ): Promise<string>;
}
