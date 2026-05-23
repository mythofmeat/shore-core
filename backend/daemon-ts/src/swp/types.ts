/**
 * SWP protocol types.
 *
 * These mirror `core/protocol/src/{client_msg,server_msg,types}.rs`. The
 * Rust types are the source of truth; if they diverge, the Rust definitions
 * win. We re-declare them in TS so we don't depend on the Rust crate at
 * runtime.
 *
 * Frame format: one JSON object per line (newline-delimited), `type` field
 * is the tag. Protocol version is 1.
 */

export const SWP_V1 = 1;

/** Maximum wire frame size (bytes). Mirrors `MAX_WIRE_MESSAGE_SIZE`. */
export const MAX_WIRE_MESSAGE_SIZE = 128 * 1024 * 1024;

// ── Client → Server ─────────────────────────────────────────────────

export interface ClientHello {
  type: "hello";
  client_type: string;
  client_name: string;
  capabilities?: string[];
  character?: string;
}

export interface ClientMessageBody {
  type: "message";
  rid?: string;
  text: string;
  stream?: boolean;
  images?: string[];
  image_data?: Array<{ filename: string; data: string }>;
  absence_seconds?: number;
  overrides?: { temperature?: number; top_p?: number; thinking_budget?: number };
}

export interface ClientRegen {
  type: "regen";
  rid?: string;
  stream?: boolean;
  guidance?: string;
}

export interface ClientCommand {
  type: "command";
  rid?: string;
  name: string;
  args?: unknown;
}

export interface ClientCancel {
  type: "cancel";
}

export type ClientMessage = ClientHello | ClientMessageBody | ClientRegen | ClientCommand | ClientCancel;

// ── Server → Client ─────────────────────────────────────────────────

import type { CharacterInfo } from "../characters/registry.ts";

export interface ServerHello {
  type: "hello";
  v: number;
  server_name: string;
  characters?: CharacterInfo[];
}

/**
 * Per `core/protocol/src/server_msg.rs`:
 *   - `config` is always emitted (default `null`); the Rust daemon populates
 *     it with `{active_model, private}`.
 *   - `active_start` skip-if-zero.
 *   - `selected_character` skip-if-none.
 *   - `revision` always emitted (default 0).
 */
export interface ServerHistory {
  type: "history";
  rid?: string;
  messages: unknown[];
  active_start?: number;
  config: unknown;
  selected_character?: string;
  revision: number;
}

export interface ServerShutdown {
  type: "shutdown";
}

export interface ServerPing {
  type: "ping";
}

export interface ServerError {
  type: "error";
  rid?: string;
  code: string;
  message: string;
}

/** Per-generation token counts. Mirrors `core/protocol/src/types.rs::TokenCounts`. */
export interface TokenCounts {
  input: number;
  output: number;
  cache_read: number;
  cache_write: number;
}

/** Timing for a generation. Mirrors `core/protocol/src/types.rs::TimingInfo`. */
export interface TimingInfo {
  total_ms: number;
  ttft_ms: number;
}

export interface StreamMetadata {
  tokens: TokenCounts;
  timing: TimingInfo;
  model: string;
}

export interface ServerStreamStart {
  type: "stream_start";
  rid?: string;
  regen?: boolean;
}

export interface ServerStreamChunk {
  type: "stream_chunk";
  rid?: string;
  text: string;
  /** "text" | "thinking" — defaults to "text" wire-side. */
  content_type?: string;
}

/** One StreamEnd per LLM turn. `is_final=true` marks the terminal one. */
export interface ServerStreamEnd {
  type: "stream_end";
  rid?: string;
  msg_id?: string;
  revision?: number;
  content: string;
  metadata: StreamMetadata;
  finish_reason?: string;
  is_final?: boolean;
}

export interface ServerToolCall {
  type: "tool_call";
  rid?: string;
  tool_id: string;
  tool_name: string;
  input: unknown;
}

export interface ServerToolResult {
  type: "tool_result";
  rid?: string;
  tool_id: string;
  tool_name: string;
  output: string;
  is_error?: boolean;
}

export interface ServerNewMessage {
  type: "new_message";
  revision?: number;
  character?: string;
  origin?: "user_input" | "assistant_reply" | "autonomous";
  // Message fields are flattened into this frame (matches Rust's
  // #[serde(flatten)] on NewMessage.message).
  msg_id: string;
  role: "user" | "assistant" | "system";
  content: string;
  images: unknown[];
  content_blocks: unknown[];
  timestamp: string;
}

export type ServerMessage =
  | ServerHello
  | ServerHistory
  | ServerShutdown
  | ServerPing
  | ServerError
  | ServerStreamStart
  | ServerStreamChunk
  | ServerStreamEnd
  | ServerToolCall
  | ServerToolResult
  | ServerNewMessage;
