# Ralph Progress Log

This file tracks progress across iterations. Agents update this file
after each iteration and it's included in prompts for context.

## Codebase Patterns (Study These First)

- Workspace uses `resolver = "2"` in root Cargo.toml
- Library crates (`shore-protocol`, `shore-client`) use `src/lib.rs`; binary crates use `src/main.rs`
- `shore-llm` is a standalone TypeScript package outside the Cargo workspace
- Protocol types use `#[serde(tag = "type", rename_all = "snake_case")]` for JSON-Lines framing
- `NewMessage` uses `#[serde(flatten)]` to inline Message fields into the envelope
- `StreamChunk.content_type` defaults to `"text"` via `#[serde(default = "...")]`
- `tokio::io::duplex` + `SWPConnection::from_raw_stream()` is the pattern for mock-server tests — no real sockets needed
- `Box<dyn AsyncReadWrite>` unifies Unix/TCP transports; manual `Debug` impl needed since trait objects aren't `Debug`
- `shore-llm` tests use Unix socket via `node:http` `createServer` + `server.listen(socketPath)` — no external test HTTP client needed
- `tsconfig.json` excludes `**/*.test.ts` so test files don't compile into `dist/`

---

## 2026-03-25 - US-001
- What was implemented: Full repo scaffolding with Cargo workspace, 6 Rust crates, TypeScript shore-llm package, docs/ and examples/ directories
- Files changed:
  - `.gitignore` — Rust + Node + IDE ignores
  - `Cargo.toml` — workspace root with all 6 Rust members
  - `shore-protocol/` — library crate (Cargo.toml + src/lib.rs)
  - `shore-client/` — library crate (Cargo.toml + src/lib.rs)
  - `shore-daemon/` — binary crate (Cargo.toml + src/main.rs)
  - `shore-cli/` — binary crate (Cargo.toml + src/main.rs)
  - `shore-tui/` — binary crate (Cargo.toml + src/main.rs)
  - `shore-matrix/` — binary crate (Cargo.toml + src/main.rs)
  - `shore-llm/` — package.json, tsconfig.json, src/index.ts
  - `docs/SHORE-V2-ARCHITECTURE.md` — copied from root ARCHITECTURE.md
  - `examples/config.toml` — example daemon config
  - `examples/models.toml` — example model definitions
- **Learnings:**
  - Empty `src/lib.rs` files are valid for workspace compilation — no placeholder code needed
  - Cargo workspace with `resolver = "2"` compiles, tests, and lints cleanly with zero-content crates
---

## 2026-03-25 - US-002
- What was implemented: All SWP protocol message types as serde-serializable Rust structs in shore-protocol
- Files changed:
  - `shore-protocol/Cargo.toml` — added serde + serde_json dependencies
  - `shore-protocol/src/lib.rs` — module declarations, SWP_V1 constant, 28 unit tests
  - `shore-protocol/src/types.rs` — Role, ImageRef, Message, TokenCounts, TimingInfo, StreamMetadata, ConversationInfo, CharacterInfo
  - `shore-protocol/src/client_msg.rs` — ClientHello, ClientMessageBody, Regen, Command, ClientMessage enum
  - `shore-protocol/src/server_msg.rs` — ServerHello, History, Shutdown, Ping, CommandOutput, Error, StreamStart, StreamChunk, StreamEnd, Phase, NewMessage, ToolCall, ToolResult, SendImage, CacheWarning, ServerMessage enum
  - `shore-protocol/src/error.rs` — ErrorCode enum with 7 variants
- **Learnings:**
  - `#[serde(flatten)]` on NewMessage inlines the Message fields into the tagged envelope so `msg_id`, `role`, etc. appear at top level alongside `"type": "new_message"`
  - `#[serde(default = "fn_name")]` works well for default string values like content_type
  - `#[serde(skip_serializing_if = "Option::is_none")]` keeps JSON clean for optional fields (alt_index, alt_count, caption, guidance)
  - All 28 round-trip tests pass; cargo build, test, clippy all clean
---

## 2026-03-25 - US-003
- What was implemented: Full SWP client library with connection management, daemon discovery, and stream handling
- Files changed:
  - `shore-client/Cargo.toml` — added shore-protocol, tokio, serde, serde_json, thiserror dependencies
  - `shore-client/src/lib.rs` — module declarations, re-exports, 16 unit tests (handshake, send/recv, stream assembly, discovery, callbacks)
  - `shore-client/src/connection.rs` — SWPConnection struct with Unix socket + TCP transport, JSON-Lines framing, handshake (connect + connect_raw), send_message/send_regen/send_command convenience methods, from_raw_stream for testing
  - `shore-client/src/discovery.rs` — Read $XDG_RUNTIME_DIR/shore/instances.json, find socket for config path, fallback to default socket
  - `shore-client/src/stream.rs` — StreamHandler that tracks stream_start/chunk/end sequences, assembles chunks, StreamCallbacks trait for user-provided hooks
  - `shore-client/src/error.rs` — ClientError enum with Connect, Disconnected, Protocol, Discovery, Serialize, Deserialize, Io variants
- **Learnings:**
  - `tokio::io::duplex(8192)` creates a bidirectional in-memory stream perfect for mock-server tests without real sockets
  - `tokio::io::join(reader, sink())` / `join(empty(), writer)` adapts split halves into `AsyncRead + AsyncWrite` for `Box<dyn>` erasure
  - `Box<dyn Trait>` for trait objects with `AsyncRead + AsyncWrite + Send + Unpin` requires a manual `Debug` impl since trait objects don't derive Debug
  - `thiserror v2` works cleanly with `#[source]` for wrapping `io::Error` and `serde_json::Error`
  - `tokio::io::split()` works on any `AsyncRead + AsyncWrite` — used for `connect_raw` with duplex streams in tests
  - All 44 tests pass (16 client + 28 protocol); cargo build, test, clippy all clean
---

## 2026-03-25 - US-004
- What was implemented: Integration test suite with golden-file JSON fixtures for all SWP message types
- Files changed:
  - `shore-protocol/tests/golden_json.rs` — 44 integration tests covering:
    - Golden-file fixtures for every server message type (ServerHello, History, Shutdown, Ping, CommandOutput, Error, StreamStart, StreamChunk, StreamEnd, Phase, NewMessage, ToolCall, ToolResult, SendImage, CacheWarning)
    - Golden-file fixtures for every client message type (ClientHello, ClientMessageBody, Regen, Command)
    - Standalone type fixtures (Message, StreamMetadata)
    - Forward compatibility: unknown fields silently ignored (7 tests)
    - Missing optional fields → None/default (10 tests)
    - Protocol version mismatch → ProtocolError (2 tests)
    - All ErrorCode variants and Role variants golden (2 tests)
- **Learnings:**
  - `assert_golden<T>` helper pattern: deserialize fixture, re-serialize, compare JSON values — handles field ordering differences automatically since serde_json::Value equality is order-independent
  - Serde's default behavior (without `deny_unknown_fields`) already provides forward compatibility — unknown fields are silently ignored during deserialization
  - `#[serde(tag = "type")]` tagged enums also silently ignore unknown fields in inner structs
  - `#[serde(flatten)]` with NewMessage works correctly in golden tests — flattened Message fields appear at top level alongside `"type": "new_message"`
---

## 2026-03-25 - US-005
- What was implemented: shore-llm HTTP server scaffold with health endpoint, structured logging, and route dispatch
- Files changed:
  - `shore-llm/package.json` — added build/start/test scripts, pino + @types/node deps, vitest + typescript + pino-pretty devDeps
  - `shore-llm/tsconfig.json` — added exclude for test files
  - `shore-llm/src/index.ts` — HTTP server listening on Unix socket (path via CLI arg or SHORE_LLM_SOCKET env var), graceful shutdown on SIGTERM/SIGINT
  - `shore-llm/src/router.ts` — route dispatch for /v1/health (200), /v1/generate, /v1/stream, /v1/embed, /v1/image/generate (501 stubs); 404 for unmatched routes; 400 for invalid JSON POST bodies
  - `shore-llm/src/logger.ts` — pino logger with `service: "shore-llm"` base field, `childWithRid()` for X-Request-ID propagation
  - `shore-llm/src/router.test.ts` — 10 tests: health endpoint (200 + content-type), 404 handling (unknown path + wrong method), invalid JSON (400), stub endpoints (4x 501), X-Request-ID propagation
- **Learnings:**
  - Node.js `http.createServer` + `server.listen(socketPath)` works directly for Unix socket servers — no Express or Fastify needed for simple routing
  - `vitest run` works out of the box with TypeScript ESM (`"type": "module"`) — no additional config needed
  - Pino child loggers via `logger.child({ rid })` cleanly propagate request-scoped fields without middleware
  - `npm run build` requires `typescript` as a devDependency — the scaffold from US-001 didn't include it
  - `tsconfig.json` `exclude: ["src/**/*.test.ts"]` prevents test files from being compiled into `dist/`
- Anthropic SDK streaming returns `AsyncIterable<RawMessageStreamEvent>` — use `for await` to consume events
- Tool use JSON accumulates via `input_json_delta` events across `content_block_delta`, then parse on `content_block_stop`
---

## 2026-03-25 - US-006
- What was implemented: Anthropic provider for shore-llm with generate and streaming endpoints
- Files changed:
  - `shore-llm/package.json` — added `@anthropic-ai/sdk` dependency
  - `shore-llm/src/providers/anthropic.ts` — full Anthropic provider: request translation (cache_control_depth, thinking/budget_tokens, temperature, top_p, tools, system), response normalization (content, content_blocks, finish_reason, usage with cache tokens, timing), streaming with ndjson events (start, text, thinking, tool_use, done)
  - `shore-llm/src/router.ts` — wired `/v1/generate` and `/v1/stream` to Anthropic provider (replacing 501 stubs), added unsupported provider validation
  - `shore-llm/src/router.test.ts` — updated stub tests to reflect live endpoints, added unsupported provider rejection tests
  - `shore-llm/src/providers/anthropic.test.ts` — 22 unit tests with mocked SDK client covering: buildCreateParams (basic, system, tools, temperature, top_p, thinking, cache_control_depth, stream flag), generate (text, cache tokens, null cache tokens, timing, tool_use, thinking, SDK params), stream (text events, thinking events, tool_use with accumulated JSON, ndjson content-type, cache tokens in done)
- **Learnings:**
  - Anthropic SDK's `client.messages.create()` with `stream: true` returns an `AsyncIterable<RawMessageStreamEvent>` — consume with `for await`
  - Tool use input arrives as incremental `input_json_delta` events with `partial_json` strings that must be concatenated and parsed on `content_block_stop`
  - `cache_read_input_tokens` and `cache_creation_input_tokens` may be `null` in SDK response — normalize to `0` for the shore-llm response
  - `MessageDeltaUsage` (from `message_delta` events) has cumulative `output_tokens` — use it to update the running usage total
  - Cache control must be applied per content-block: string content must be converted to `TextBlockParam[]` form to attach `cache_control: { type: "ephemeral" }`
  - Using `Record<string, unknown>` for params building requires `as unknown as` double cast due to strict TypeScript overlap checks
---

