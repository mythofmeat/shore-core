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

export interface CharacterInfo {
  name: string;
  // Other fields exist; we'll add them as needed in later phases.
}

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

// Many more (StreamStart, StreamChunk, StreamEnd, ToolCall, ToolResult,
// CommandOutput, SendImage, etc.) — added as phases progress.

export type ServerMessage =
  | ServerHello
  | ServerHistory
  | ServerShutdown
  | ServerPing
  | ServerError;
