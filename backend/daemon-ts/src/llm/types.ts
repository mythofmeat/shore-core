/**
 * Provider-agnostic types for the LLM call boundary.
 *
 * We use Anthropic-style content blocks as the canonical in-process
 * representation because:
 *   1. Our on-disk format (`active.jsonl`) already stores blocks in this
 *      shape — see `src/engine/types.ts`.
 *   2. Anthropic is the picky one about block ordering (thinking →
 *      tool_use → tool_result → thinking → text); preserving its exact
 *      shape end-to-end is how we kill the cache regression.
 *   3. Converting Anthropic blocks → OpenAI messages is straightforward
 *      and contained inside the OpenAI adapter.
 *
 * The "thinking" block's `signature` is opaque bytes from Anthropic.
 * Replay across turns is verbatim — we never inspect, regenerate, or
 * normalize it. (This was a recurring Rust bug surface.)
 */

import type { ContentBlock, ImageRef } from "../engine/types.ts";

export interface ToolDef {
  name: string;
  description: string;
  /** JSON Schema for the tool's input. */
  inputSchema: Record<string, unknown>;
}

/** One turn in the message array sent to the provider.
 *
 * `system` is allowed mid-history for heartbeat recaps and compaction
 * prompts. Adapters handle it differently — Anthropic must wrap it in
 * `<system_instruction>` user blocks (the Messages API rejects raw
 * `role:"system"` in the messages array), while OpenAI passes it through
 * natively. See providers/anthropic.ts::convertInlineSystemMessages.
 */
export interface TurnMessage {
  role: "user" | "assistant" | "system";
  content: ContentBlock[];
  /**
   * Images to prepend to the turn's content when building the
   * provider-specific message. Stored separately (matching Rust's
   * `Message.images`) so they're not entangled with the cache-stable
   * `content_blocks` array. Each provider wraps these into its native
   * image block shape — see `providers/anthropic.ts` and
   * `providers/openai.ts`.
   */
  images?: ImageRef[];
}

export interface ThinkingConfig {
  /** When false, request goes out without any thinking config. */
  enabled: boolean;
  /** Anthropic budget_tokens (when reasoning_effort is not "adaptive"). */
  budgetTokens?: number;
  /**
   * Anthropic reasoning_effort. "adaptive" enables Claude's adaptive
   * thinking (no fixed budget); other values map to fixed budgets.
   * "low" | "medium" | "high" | "xhigh" | "max" | "adaptive".
   */
  effort?: string;
}

export interface SystemPromptBlock {
  type: "text";
  text: string;
  _label?: string;
}

export interface ChatRequest {
  /** System prompt as text blocks, or a legacy single string from tests/background callers. */
  system: string | SystemPromptBlock[];
  messages: TurnMessage[];
  tools: ToolDef[];
  thinking: ThinkingConfig;
  /** Empty string disables caching; otherwise a TTL like "1h" / "5m". */
  cacheTtl: string;
  /** Model id sent on the wire (e.g. "anthropic/claude-haiku-4.5"). */
  modelId: string;
  /** Bearer credential — adapter knows how to apply it (api-key vs Authorization). */
  apiKey: string;
  /** Provider base URL override (e.g. https://openrouter.ai/api/v1). */
  baseUrl?: string;
  maxTokens: number;
  temperature?: number;
  topP?: number;
  /** AbortSignal for cancelling the generation mid-stream. */
  signal?: AbortSignal;
  /** Optional cache-forensics sink used by Anthropic request construction. */
  cacheForensics?: CacheForensicsSink;
  forensicCharacter?: string;
  forensicRid?: string;
}

export interface CacheForensicsSink {
  nextCallId(): number;
  logRequest(entry: {
    callId: number;
    character?: string;
    model: string;
    msgCount: number;
    msgBreakpoints: number[];
    sysBreakpoints: number[];
    sysBlocks: number;
    prefixHash: string;
    hasExistingMarkers: boolean;
    cacheEnabled: boolean;
    rid?: string;
  }): void;
}

export interface UsageStats {
  inputTokens: number;
  outputTokens: number;
  cacheReadInputTokens: number;
  cacheCreationInputTokens: number;
}

export type ChatEvent =
  | { kind: "text_delta"; text: string }
  | { kind: "thinking_delta"; text: string }
  | { kind: "tool_use_start"; id: string; name: string }
  | { kind: "tool_use_input_delta"; id: string; partial_json: string }
  | { kind: "tool_use_done"; id: string }
  | {
      kind: "done";
      /** Final assistant content blocks, in order. */
      content: ContentBlock[];
      /** "end_turn" | "tool_use" | "max_tokens" | "stop_sequence" | "refusal" */
      stopReason: string;
      usage: UsageStats;
    };

/**
 * Resolved result of a non-streaming provider call. Same shape as the
 * payload of the streaming `{kind: "done"}` event so a non-streaming
 * caller can be a drop-in replacement when no token-by-token UI is
 * needed (background tasks like compaction, dreaming, heartbeat, plus
 * any future "no-stream" chat mode).
 */
export interface GenerateResult {
  content: ContentBlock[];
  stopReason: string;
  usage: UsageStats;
}

export interface ProviderClient {
  /** Async iterator over streaming events. Caller must consume until "done". */
  stream(req: ChatRequest): AsyncIterable<ChatEvent>;
  /**
   * Single-shot non-streaming call. Sends the request without
   * `stream: true` on the wire (Anthropic returns a JSON Message,
   * OpenAI-compatible returns a single ChatCompletion). Use for
   * background tasks that don't need progressive output and where
   * matching the non-streaming wire shape matters (cache-prefix
   * parity, ledger accounting, etc.).
   */
  generate(req: ChatRequest): Promise<GenerateResult>;
}
