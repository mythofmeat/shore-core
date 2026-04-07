# Architecture Review — Session 1: Foundation Crates

**Date:** 2026-04-06
**Reviewer:** GLM-5.1
**Scope:** All crates except `shore-daemon`. `shore-gui` and `shore-matrix` excluded as unfinished stubs.

---

## PHASE 1: STRUCTURAL INVENTORY

### shore-protocol

**Purpose:** Defines the Shore Wire Protocol (SWP) — all message types, shared data types, error codes, and the tool-loop message merge algorithm. A pure data crate with no behavior beyond serialization.

**Public API surface:** 5 enums (`Role`, `ContentBlock`, `ErrorCode`, `ClientMessage`, `ServerMessage`), 28 structs (Message, StreamMetadata, TokenCounts, ClientHello, etc.), 5 public functions (`merge_tool_loop_messages`, `derive_content_from_blocks`, `derive_content_from_blocks_with`, `Message::normalize`, `Message::serialize_for_storage`), 1 constant (`SWP_V1`).

**Internal structure:** Flat module layout — `lib.rs`, `types.rs`, `client_msg.rs`, `server_msg.rs`, `error.rs`, `merge.rs`. No sub-modules.

**Dependencies:** `serde`, `serde_json` only. No internal crate dependencies. This is the leaf node of the dependency graph.

**Size/complexity:** ~1,300 LOC source + ~970 LOC tests (including golden-file fixtures). A substantial data-definition crate with a non-trivial merge algorithm (~700 LOC including tests).

### shore-config

**Purpose:** Loads, validates, and resolves configuration from TOML files. Handles XDG directory resolution, model catalog construction with provider defaults cascading, character discovery, and prompt template resolution.

**Public API surface:** 3 error types (`ConfigError`, `CatalogError`), ~40 config structs (`AppConfig`, `DaemonConfig`, `DefaultsConfig`, `BehaviorConfig`, `MemoryConfig`, `ToolToggles`, etc.), `ModelCatalog`, `ResolvedModel`, `Sdk`, `LoadedConfig`, `ShoreDirs`, plus ~15 public functions for loading and resolution.

**Internal structure:** 3 files: `lib.rs` (loading/validation/XDG), `app.rs` (all config structs), `models.rs` (model catalog + provider resolution).

**Dependencies:** `serde`, `serde_json`, `toml`, `thiserror`, `tracing`, `dirs`, `dotenvy`. No internal crate dependencies.

**Size/complexity:** ~3,300 LOC source + ~600 LOC tests. A substantial library.

### shore-diagnostics

**Purpose:** In-memory ring buffers for API call, tool execution, and error observability. JSON serialization for diagnostic output.

**Public API surface:** `RingBuffer<T>`, `Diagnostics`, `ApiCallEntry`, `ToolCallEntry`, `ErrorEntry`, `truncate_summary`.

**Internal structure:** Single file (`lib.rs`, 268 lines).

**Dependencies:** `serde`, `serde_json`. No internal crate dependencies.

**Size/complexity:** Thin wrapper. 268 LOC total including tests.

### shore-llm-client

**Purpose:** Native Rust LLM provider integrations (Anthropic, OpenAI-compatible, Gemini, Z.AI). Streaming via SSE, request building, response normalization, retry logic, embeddings, and image generation.

**Public API surface:** `LlmClient`, `LlmError` (7 variants), `LlmRequest`, `StreamEvent`, `StreamResult`, `GenerateResponse`, `StreamConsumer`, `CacheContext`, `RetryPolicy`, `RetryDecision`, `Usage`, `Timing`, `ImageGenerateParams`, `ImageGenerateResponse`, plus `should_retry_error`, `should_retry_refusal`, `is_refusal`.

**Internal structure:** `lib.rs` (client + request building), `types.rs` (all types), `stream.rs` (stream consumer + cache invalidation), `retry.rs` (retry/refusal logic), `providers/` (mod.rs dispatch + 5 provider modules + SSE parser + stream helpers).

**Dependencies:** `shore-protocol`, `shore-config`, `tokio`, `serde`, `serde_json`, `chrono`, `tracing`, `thiserror`, `reqwest`, `futures-util`.

**Size/complexity:** ~5,800 LOC source. The largest foundation crate by a significant margin. The largest single file is `anthropic.rs` at 1,472 lines.

### shore-ledger

**Purpose:** SQLite-backed append-only ledger for LLM API call recording with cost tracking, per-character Anthropic cache state tracking, and usage aggregation queries.

**Public API surface:** `Ledger`, `LedgerClient` (wraps `LlmClient`), `LedgerStream` (wraps streaming), `PricingEngine`, `CacheTracker`, `CallType`, `CallRow`, `QueryFilter`, `UsageSummary`, `CostBreakdown`, plus query functions.

**Internal structure:** 6 modules: `ledger.rs` (SQLite), `pricing.rs` (OpenRouter pricing), `cache_tracker.rs` (state machine), `client.rs` (LedgerClient wrapper), `stream.rs` (LedgerStream), `query.rs` (aggregation).

**Dependencies:** `shore-llm-client`, `shore-config`, `rusqlite`, `reqwest`, `tokio`, `tracing`, `chrono`, `serde`, `serde_json`.

**Size/complexity:** ~2,300 LOC source. A substantial, well-scoped library.

### shore-client

**Purpose:** SWP client library — connection management, handshake, message framing, streaming assembly, daemon instance discovery, terminal image protocol detection, and auto-reconnect connection manager.

**Public API surface:** `SWPConnection`, `ServerAddr`, `StreamHandler`, `StreamCallbacks`, `ClientError`, `spawn_connection`, `ConnEvent`, `ConnCommand`, `discover`, `discover_or_default`, `detect_protocol`, `ImageProtocol`, `load_client_config`, `ClientConfig`.

**Internal structure:** 7 modules: `connection.rs`, `stream.rs`, `client_config.rs`, `discovery.rs`, `image_protocol.rs`, `conn_manager.rs`, `error.rs`.

**Dependencies:** `shore-protocol`, `shore-config`, `tokio`, `serde`, `serde_json`, `toml`, `thiserror`, `base64`, `libc`.

**Size/complexity:** ~1,650 LOC source. A moderate library.

### shore-cli

**Purpose:** Stateful CLI binary. Connects to daemon, sends SWP commands, renders formatted output. Supports ~30 commands, streaming responses with spinners, terminal image rendering, shell completions, and interactive memory shell.

**Public API surface:** `Cli` (clap parser), `CliCommand`, various output formatting functions, `execute()`.

**Internal structure:** `main.rs`, `cli.rs` (arg parsing + SWP mapping), `run.rs` (command dispatch), `state.rs` (active character persistence), `images.rs` (terminal image rendering), `output/` (4 modules: styling, spinner, commands, transcript).

**Dependencies:** `shore-protocol`, `shore-config`, `shore-client`, `clap`, `tokio`, `serde_json`, `chrono`, `crossterm`, `base64`, `tempfile`.

**Size/complexity:** ~5,000 LOC source (~3,400 excluding tests). A substantial CLI application.

### shore-tui

**Purpose:** Terminal UI binary (ratatui). Persistent SWP connection, scrollable conversation view, vim-like input modes, command palette, kitty/iTerm2 image rendering, markdown rendering, fullscreen image viewer.

**Public API surface:** None (binary crate). Key internal types: `App`, `InputState`, `StreamState`, `ConversationEntry`, `InputMode`, `Action`.

**Internal structure:** `main.rs` (event loop + server message dispatch), `app.rs` (state), `connection.rs` (thin wrapper), `input.rs` (keyboard handling), `images.rs` (kitty protocol), `ui.rs` (rendering), `markdown.rs` (lightweight converter), `bin/kitty_diag.rs` (diagnostic binary).

**Dependencies:** `shore-protocol`, `shore-client`, `clap`, `tokio`, `ratatui`, `crossterm`, `serde_json`, `base64`, `unicode-width`, `libc`, `image`.

**Size/complexity:** ~5,600 LOC source (~3,800 excluding tests). The most complex client-facing crate.

### Cross-crate observations

**Dependency graph (compile-time):**
```
shore-protocol  <- shore-client  <- shore-cli
                <- shore-llm-client <- shore-ledger
shore-config   <- shore-client
                <- shore-llm-client <- shore-ledger
                <- shore-cli
                <- shore-ledger

shore-protocol <- shore-llm-client <- shore-ledger
shore-diagnostics (standalone, consumed only by shore-daemon)
```

**Shared types across crate boundaries:**
- `shore-protocol` types (`Message`, `ContentBlock`, `ServerMessage`, `ClientMessage`, etc.) are used by every crate except `shore-diagnostics`
- `shore-config` types (`ResolvedModel`, `Sdk`, `AppConfig`) flow into `shore-llm-client`, `shore-ledger`, and the two client binaries
- `shore-llm-client` types (`LlmClient`, `LlmError`, `Usage`, `Timing`) flow into `shore-ledger`

**Protocol/contract location:** All inter-component contracts are in `shore-protocol` — the SWP message types. The daemon is the server; CLI/TUI/bridges are clients. The contract is well-isolated in a single crate.

---

## PHASE 2: EXECUTION PATH TRACING

### 2.1 Configuration Flow

**Loading:**
1. `shore-config::load_config(config_path: Option<&Path>)` (`lib.rs:174`)
2. `ShoreDirs::resolve()` — XDG directories with Shore-specific env var overrides (`lib.rs:94`)
3. `.env` loading from config dir via `dotenvy` (`lib.rs:195`)
4. Raw TOML table parsing: `config.toml` -> `include` files (deep merge) -> `conf.d/*.toml` (deep merge) (`lib.rs:204-249`)
5. Model sections (`chat`, `tools`, `embedding`, `image_generation`) extracted before `AppConfig` deserialization to avoid `deny_unknown_fields` rejection (`lib.rs:259-290`)
6. `ModelCatalog::from_sections()` — per-provider hardcoded defaults -> TOML scalar cascade -> per-model resolution (`models.rs:292`)
7. `validate_config()` — jitter range, model reference checks (`lib.rs:432`)

**Configurable vs. hardcoded defaults:**
- Hardcoded: provider API key env var names, base URLs, default SDK per provider (`models.rs:472-523`), base provider field defaults (temp=1.0, max_tokens=8192, max_context=200k) (`models.rs:462`)
- Configurable via TOML: every field in `AppConfig` and nested structs, model parameters, all behavioral knobs
- Defaults: `AppConfig::default()` provides sensible values for all optional fields

**Propagation:** `LoadedConfig` is passed to consumers. `shore-cli` and `shore-tui` use `shore-config` only for `config_dir()`/`runtime_dir()` paths and the client config loader — they don't load the daemon config directly.

### 2.2 Client-Server Protocol

**What `shore-protocol` defines:**
- `ClientMessage` enum (5 variants: Hello, Message, Regen, Command, Cancel) — internally tagged by `"type"` field
- `ServerMessage` enum (15 variants: Hello, History, Shutdown, Ping, CommandOutput, Error, StreamStart/Chunk/End, Phase, NewMessage, ToolCall, ToolResult, SendImage, CacheWarning)
- `ErrorCode` enum (7 variants)
- Core types: `Message`, `ContentBlock` (5 variants), `Role`, `ImageRef`, `StreamMetadata`, `TokenCounts`, `TimingInfo`
- `merge_tool_loop_messages()` — collapses daemon-side tool-loop messages into client-displayable single turns

**How CLI/TUI use it:**

*CLI* (`shore-cli/src/run.rs`):
1. `SWPConnection::connect(addr, "cli", "shore-cli", character)` -> 3-step handshake
2. Send `ClientMessage::Message` / `ClientMessage::Command` via `conn.send()`
3. Receive `ServerMessage` variants via `conn.recv()` in a loop
4. Dispatch to output formatters: `recv_streaming_response()` for messages, `recv_command_data()` for commands

*TUI* (`shore-tui/src/main.rs`):
1. `spawn_connection()` creates a background task with auto-reconnect (`conn_manager.rs:43`)
2. Background task handles handshake, returns `ConnEvent::Connected` with hello + history
3. `handle_server_message()` matches all `ServerMessage` variants, mutates `App` state
4. Rendering loop redraws from `App` state every 16ms

**Serialization format:** Newline-delimited JSON (JSON-Lines). Each message is a single JSON object followed by `\n`. Example:
```json
{"type":"stream_chunk","text":"Hello","content_type":"text"}
{"type":"stream_end","content":"Hello!","metadata":{"tokens":{"input":1234,"output":56,"cache_read":890,"cache_write":0},"timing":{"total_ms":450,"ttft_ms":200},"model":"claude-sonnet-4-6"},"finish_reason":"end_turn"}
```

### 2.3 LLM Client Abstraction

**Abstraction mechanism:** Dispatch-by-string, NOT a trait. `LlmRequest.provider` string (set via `Sdk::as_provider_str()`) routes to the appropriate provider module (`providers/mod.rs:33-67`):
- `"anthropic"` -> `anthropic::stream/generate`
- `"openai"` | `"deepseek"` | `"zhipuai"` | `"xai"` | `"nanogpt"` -> `openai::stream/generate`
- `"gemini"` -> `gemini::stream/generate`
- `"zai"` -> `zai::stream/generate`

**Models/providers supported:** Anthropic (native), OpenAI, DeepSeek, ZhipuAI, xAI, nanoGPT, OpenRouter (all via OpenAI-compatible handler), Gemini, Z.AI.

**Request building:** `LlmClient::build_request()` (`lib.rs:83`) takes a `ResolvedModel` + messages + system + tools + provider_options, resolves API key from env var, constructs an `LlmRequest` with all provider-specific options embedded.

**Streaming architecture:** Each provider spawns a `tokio::spawn` background task that reads SSE from the HTTP response, translates events to NDJSON `StreamEvent` lines, and writes to a `DuplexStream`. `StreamConsumer::consume()` reads from the other end of the DuplexStream, broadcasts `ServerMessage` variants to SWP clients, and accumulates a `StreamResult`.

**Cost tracking at this layer:** Only raw token counts (`Usage` struct: `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_creation_tokens`). No dollar-cost calculation. Dollar costs are computed in `shore-ledger` via `PricingEngine::calculate_cost()` using cached OpenRouter pricing.

**Embedding pipeline:** `LlmClient::embed()` -> `providers::embed()` -> `openai::embed()` — all providers route through the OpenAI-compatible `/embeddings` endpoint. No Anthropic or Gemini-specific embedding path.

### 2.4 User Input to Daemon Boundary

**CLI path:**
1. User types `shore send "hello" -i photo.jpg`
2. `clap::Parser::parse()` -> `Cli { command: Send { message: ["hello"], images: ["photo.jpg"] } }`
3. `run::execute(cli)` -> `resolve_addr(cli)` -> `discover_or_default()` -> `ServerAddr`
4. `SWPConnection::connect(addr, "cli", "shore-cli", character)` -> 3-step handshake
5. Text resolution: CLI args joined -> stdin (if piped) -> `$EDITOR` temp file (`run.rs:45-51`)
6. Images: file paths read from disk, base64-encoded into `ImageUpload` structs (`connection.rs:200-236`)
7. `conn.send_message_full(text, true, images, overrides)` -> sends `ClientMessage::Message`
8. `recv_streaming_response(conn)` -> reads `StreamStart`/`StreamChunk`/`StreamEnd`/`ToolCall`/`ToolResult`/`Phase` messages, renders to terminal

**TUI path:**
1. User types in insert mode, presses Enter
2. `input::handle_insert_mode()` -> `Action::Send(ClientMessage::Message)` (`input.rs:262`)
3. App inserts optimistic user echo into `entries` (`main.rs:267-278`)
4. `cmd_tx.send(ConnCommand::Send(msg)).await` -> connection manager sends to daemon (`main.rs:284`)
5. `handle_server_message()` processes `StreamStart`/`StreamChunk`/`StreamEnd` -> mutates `App.stream` state
6. Rendering loop draws from `App` state every 16ms

**Transformations:**
- CLI: text joining -> image base64 -> JSON-Lines framing
- TUI: keystroke -> input buffer -> message construction -> optimistic echo -> JSON-Lines framing
- Both: `StreamHandler`/`recv_streaming_response` reassembles chunked stream into displayable content

### 2.5 Error Handling Patterns

**Path 1: LLM API error -> CLI user**

1. `reqwest` returns HTTP 429 -> `LlmError::HttpStatus { status: 429, body }` (`shore-llm-client/src/providers/mod.rs:14`)
2. Daemon catches `LlmError`, sends `ServerMessage::Error(Error { code: ProviderError, message })` to client (crosses daemon boundary — out of scope to verify)
3. CLI receives `ServerMessage::Error` in `recv_streaming_response()` (`run.rs:596`) -> returns `Err`
4. `run::execute()` catches the error, calls `output::print_error(&e)` (`main.rs:36`) -> prints to stderr

**Path 2: Config parse error -> user**

1. `load_config()` reads `config.toml`, `toml::from_str()` fails -> `ConfigError::ParseApp(source)` (`shore-config/src/lib.rs:19`)
2. Caller (daemon startup) displays the error — out of scope to trace further

**Path 3: Connection failure -> TUI user**

1. `SWPConnection::open()` fails (Unix socket not found) -> `ClientError::Connect("...")` (`shore-client/src/connection.rs:55`)
2. `conn_manager::connection_loop` catches the error -> sends `ConnEvent::Disconnected(reason)` (`conn_manager.rs:126`)
3. `handle_conn_event()` in TUI sets `app.connection_status = ConnectionStatus::Disconnected` (`main.rs:408`)
4. Status bar renders the disconnected state in `ui::draw_input`

**Error representation across boundaries:**
- `shore-llm-client` -> `LlmError` (thiserror, 7 variants) — typed
- `shore-client` -> `ClientError` (thiserror, 6 variants) — typed
- `shore-config` -> `ConfigError` / `CatalogError` (thiserror) — typed
- `shore-protocol` -> `ErrorCode` (serde enum, wire format only)
- `shore-ledger` -> No error type. Uses `Box<dyn Error>` and raw `rusqlite::Error`
- `shore-cli` -> `Box<dyn std::error::Error>` everywhere in `run.rs`
- **No `From` impls connecting error types across crate boundaries.**

---

## PHASE 3: SEMI-FORMAL ANALYSIS

### 3.1 Abstraction Quality

---

**FINDING 1:** The SSE parser has an O(n^2) allocation pattern on the hottest code path in the entire application.

```
PREMISES:
- P1: shore-llm-client/src/providers/sse.rs:37-38 — every SSE line extraction creates
  two new String allocations: one for the extracted line, one for
  self.buf = self.buf[newline_pos + 1..].to_string() which copies the entire
  remaining buffer.
- P2: A typical LLM streaming response contains 50-200+ SSE events, each triggering
  this code path.

TRACED PATH:
- Step 1: LlmClient::stream_raw() spawns a background task calling
  provider-specific stream() (lib.rs:163)
- Step 2: Provider calls read_sse_events() which calls SseParser::feed() on every
  HTTP chunk (sse.rs:88)
- Step 3: Inside feed(), the while let Some(newline_pos) loop calls
  self.buf[newline_pos + 1..].to_string() on every line (sse.rs:38)

CONCLUSION:
For a 10KB SSE chunk containing 50 lines, this produces ~1,275 string copies
(sum of 50+49+48+...). This is a scalability concern — the allocation pressure
scales quadratically with lines-per-chunk, and streaming is the primary
interaction mode. String::drain(..=newline_pos) would reduce this to O(n) total.

CONFIDENCE: HIGH
SEVERITY: SIGNIFICANT

WHAT I COULD NOT VERIFY:
Actual performance profiling data. The impact depends on response length and
chunk frequency. Short responses may not be noticeably affected.
```

---

**FINDING 2:** `shore-ledger` has no typed error type, using `Box<dyn Error>` for its public API.

```
PREMISES:
- P1: LedgerClient::new() returns Result<Self, Box<dyn std::error::Error>>
  (shore-ledger/src/client.rs:162)
- P2: PricingEngine::fetch_pricing() returns Result<_, Box<dyn Error + Send + Sync>>
  (shore-ledger/src/pricing.rs:129)
- P3: All other crates with public error APIs use thiserror-derived types
  (LlmError, ClientError, ConfigError).

TRACED PATH:
- Step 1: LedgerClient::new() calls Ledger::open(path) which can return
  rusqlite::Error
- Step 2: The error is boxed: ? converts via Box<dyn Error>
- Step 3: The caller (daemon startup) receives a Box<dyn Error> and can only
  display it, not match on specific failure modes (DB locked, permission denied,
  schema corruption, etc.)

CONCLUSION:
This is a maintainability concern. If the daemon needs to handle specific ledger
failures differently (e.g., retry on lock, fail fast on corruption), it cannot
currently do so without downcasting or string matching.

CONFIDENCE: HIGH
SEVERITY: MINOR

WHAT I COULD NOT VERIFY:
Whether the daemon actually needs to discriminate between ledger error types
in practice.
```

---

**FINDING 3:** The `shore-llm-client` provider abstraction uses string dispatch instead of a trait, and `ResolvedModel` fields are duplicated into `LlmRequest` fields.

```
PREMISES:
- P1: providers/mod.rs:33-67 matches on request.provider string to dispatch to
  provider modules
- P2: LlmRequest struct has 13 fields including raw serde_json::Value fields for
  messages, system, and tools (types.rs:8-52)
- P3: ResolvedModel (22 fields) is mapped to LlmRequest (13 fields) in
  build_request() (lib.rs:83-160), with some fields duplicated (provider_key
  separate from provider, model_id vs model)

TRACED PATH:
- Step 1: LlmClient::build_request(model: &ResolvedModel, ...) resolves API key
  from env, maps Sdk -> provider string, copies fields
- Step 2: Provider modules receive &LlmRequest and dispatch based on
  request.provider string
- Step 3: Within OpenAI handler, request.provider_key is checked for
  OpenRouter-specific logic

CONCLUSION:
The string dispatch works but has two downsides: (a) adding a new provider
requires modifying mod.rs and is not compiler-guided, and (b) the
ResolvedModel -> LlmRequest mapping is a wide manual field copy. The LlmRequest
struct uses serde_json::Value for messages/tools/system rather than typed
structures, which means malformed request construction is only caught at runtime.
This is an architectural concern — the abstraction is functional but not type-safe
at the boundary.

CONFIDENCE: HIGH
SEVERITY: OBSERVATION

WHAT I COULD NOT VERIFY:
Whether the serde_json::Value approach was chosen to accommodate provider-specific
message format differences (e.g., Anthropic's content blocks vs OpenAI's format),
which would make full typing impractical.
```

---

### 3.2 Protocol Soundness

**FINDING 4:** The SWP protocol has no mechanism for clients to detect or negotiate capabilities beyond `"streaming"`.

```
PREMISES:
- P1: ClientHello.capabilities is hardcoded to vec!["streaming"] in both CLI and
  TUI (connection.rs:125, shore-tui/src/connection.rs:12)
- P2: ServerHello has no capabilities field — only v, server_name, characters
- P3: The ServerMessage enum has 15 variants and will grow (e.g., deferred
  commands in ARCHITECTURE.md section 3.8)

TRACED PATH:
- Step 1: Client sends ClientMessage::Hello with capabilities: ["streaming"]
- Step 2: Server receives it — but there is no code in any client crate that
  checks the server's capabilities
- Step 3: Server sends messages like CacheWarning, Phase, SendImage — clients
  must handle all variants

CONCLUSION:
If the daemon sends a new message variant that an older client doesn't recognize,
serde's default behavior is to silently ignore unknown fields (no
deny_unknown_fields on ServerMessage). This means old clients will silently drop
new message types rather than erroring. The forward-compatibility tests
(tests/golden_json.rs:584-706) confirm unknown fields are tolerated. However,
there is no way for a client to signal "I don't understand message type X" or for
the server to avoid sending it. This is an architectural concern for future
protocol evolution.

CONFIDENCE: HIGH
SEVERITY: OBSERVATION

WHAT I COULD NOT VERIFY:
Whether the daemon already checks client capabilities before sending optional
message types.
```

---

**FINDING 5:** The `rid` (request ID) field is defined in the protocol but not propagated through the LLM call chain in the visible crates.

```
PREMISES:
- P1: ClientMessageBody.rid is an Option<String> used for request correlation
  (client_msg.rs:40)
- P2: LlmClient::stream_raw() and LlmClient::generate() take _rid parameter but
  prefix it with underscore — it is unused (lib.rs:163, 182)
- P3: ARCHITECTURE.md section 6 states rid should be propagated via X-Request-ID
  HTTP header for tracing

TRACED PATH:
- Step 1: CLI generates rid via uuid_v4() (nanosecond timestamp)
  (connection.rs:308)
- Step 2: Client sends ClientMessage::Message with rid
- Step 3: Daemon receives it (out of scope) — would need to extract rid and pass
  to LLM call
- Step 4: LlmClient::stream_raw(request, _rid) — _rid is never used (lib.rs:163)

CONCLUSION:
The rid parameter is plumbed through the LLM client API but explicitly ignored
(underscore prefix). This means LLM API calls do NOT include X-Request-ID headers,
and structured logs from the LLM layer cannot be correlated back to specific user
requests. The tracing feature described in the architecture doc is not implemented
at this layer.

CONFIDENCE: HIGH
SEVERITY: SIGNIFICANT

WHAT I COULD NOT VERIFY:
Whether the daemon passes rid through to LlmClient calls at all (daemon code is
out of scope). Even if it does, the _rid parameter is currently unused in
shore-llm-client.
```

---

### 3.3 Error Handling Consistency

**FINDING 6:** Mutex poisoning panics are possible in production code paths.

```
PREMISES:
- P1: shore-ledger/src/ledger.rs:101,144,158,173 — self.conn.lock().unwrap()
  called on every DB operation
- P2: shore-ledger/src/pricing.rs:86,116,227 — self.memory_cache.lock().unwrap()
  called on every pricing lookup
- P3: shore-cli/src/output/spinner.rs:64,79,104,109,115,125 —
  self.state.lock().unwrap() called on every spinner update
- P4: shore-ledger/src/client.rs:71,176,388,389,419 —
  cache_trackers.lock().unwrap() called on every LLM call recording

TRACED PATH:
- Step 1: Any thread panics while holding the ledger mutex (e.g., a rusqlite
  panic from a malformed SQL query)
- Step 2: Next call to self.conn.lock().unwrap() panics again with PoisonError
- Step 3: If this happens on the daemon's main task, the entire process crashes

CONCLUSION:
This is a correctness concern. If any thread panics while holding a Mutex
guarding the ledger or cache tracker, all subsequent accesses to that resource
will panic. The standard mitigation is .lock().unwrap_or_else(|e| e.into_inner())
which recovers from poisoning. For the ledger specifically, this may be acceptable
(a poisoned DB connection is likely unrecoverable), but the spinner and cache
tracker locks should be more resilient.

CONFIDENCE: HIGH
SEVERITY: SIGNIFICANT

WHAT I COULD NOT VERIFY:
Whether rusqlite operations can panic (they generally return Result), which would
determine the practical likelihood of mutex poisoning.
```

---

**FINDING 7:** The CLI uses `Box<dyn Error>` for all error propagation, losing type information.

```
PREMISES:
- P1: Every public function in shore-cli/src/run.rs returns
  Result<_, Box<dyn std::error::Error>> (lines 11, 274, 308, 333, 375, 399,
  475, 499, 507, 547, 619)
- P2: The only error display is print_error(&e) which calls Display::fmt on a
  trait object (main.rs:36)
- P3: shore-client has a well-typed ClientError enum that gets boxed into
  dyn Error at the run.rs boundary

TRACED PATH:
- Step 1: SWPConnection::connect() returns Result<_, ClientError>
- Step 2: run::execute() catches it and propagates as Box<dyn Error> (run.rs:33)
- Step 3: main() catches and calls print_error(&e) — loses the specific error
  variant

CONCLUSION:
This is a maintainability concern. The CLI cannot programmatically distinguish
between connection failures, protocol errors, and deserialization errors at the
top level. For a CLI that exits after each command, this is acceptable (the user
sees the error message). But it prevents structured exit codes or conditional
retry logic.

CONFIDENCE: HIGH
SEVERITY: MINOR

WHAT I COULD NOT VERIFY:
Whether structured exit codes were ever considered for the CLI.
```

---

### 3.4 Cost Management

**FINDING 8:** The `LedgerStream` type uses a `Drop` guard to detect unrecorded API calls, but the drop implementation only logs — it cannot retroactively record.

```
PREMISES:
- P1: LedgerStream has a finalized: bool field, and Drop::drop checks it
  (stream.rs:105-117)
- P2: If finalized is false on drop, it logs tracing::error!("LedgerStream
  dropped without finalize — API call was NOT recorded")
- P3: LedgerStream::finalize() calls record_call() which inserts into the SQLite
  ledger (stream.rs:83)

TRACED PATH:
- Step 1: LedgerClient::stream_raw() creates a LedgerStream (client.rs:240)
- Step 2: Caller must call .finalize(&result) after reading the stream
- Step 3: If the caller drops the LedgerStream without finalizing (e.g., due to
  an error or early return), the API call cost is lost forever

CONCLUSION:
This is a correctness concern for cost tracking. The Drop guard provides a safety
net (you'll see an error log), but the actual token usage and cost for that call
are permanently lost. In a streaming context where errors can cause early drops,
this could lead to incomplete cost records. The LedgerClient design (consuming
LlmClient) structurally prevents unlogged calls, but the streaming path has this
gap.

CONFIDENCE: HIGH
SEVERITY: MINOR

WHAT I COULD NOT VERIFY:
How frequently streams are dropped without finalization in practice (depends on
daemon error handling, which is out of scope).
```

---

**FINDING 9:** The TUI rebuilds the entire conversation history after every streaming response, including re-transmitting all images.

```
PREMISES:
- P1: On StreamEnd, the TUI sends a log command to fetch the full conversation
  (main.rs:613-617)
- P2: The log response handler clears app.entries and re-expands every message
  from scratch (main.rs:729-735)
- P3: transmit_entry_images() re-transmits ALL conversation images to kitty after
  every rebuild (main.rs:519)

TRACED PATH:
- Step 1: handle_server_message(StreamEnd) triggers conn.send_command("log", ...)
  (main.rs:613-617)
- Step 2: handle_server_message(CommandOutput("log")) clears all entries and
  re-expands (main.rs:729-735)
- Step 3: transmit_entry_images(&mut app) re-transmits every image via kitty
  protocol (main.rs:519)

CONCLUSION:
This is a scalability concern. For long conversations with images, every
streaming response triggers an O(n) history rebuild + O(k) image re-transmission
(where k = total images, not just new ones). The architecture doc (section 3.5)
states that history is re-sent after state changes, but the TUI's response —
clearing and rebuilding everything rather than incrementally updating — is
unnecessarily expensive. This will become noticeable with long conversations.

CONFIDENCE: HIGH
SEVERITY: SIGNIFICANT

WHAT I COULD NOT VERIFY:
Whether the daemon sends a History push after StreamEnd that would make the TUI's
log command redundant.
```

---

### 3.5 Rust Idiom

**FINDING 10:** The `MemoryDB` unsafe `Sync` impl in the daemon relies on a runtime invariant that is not compiler-enforced.

```
PREMISES:
- P1: shore-daemon/src/memory/db.rs:222 — unsafe impl Sync for MemoryDB {}
  wraps rusqlite::Connection (Send but not Sync)
- P2: The safety comment states "only used from a single tokio task at a time"
- P3: This unsafe is in the daemon crate, which is out of scope, but the pattern
  is visible in the dependency chain

TRACED PATH:
- Step 1: MemoryDB wraps a rusqlite::Connection inside Arc<MemoryDB>
- Step 2: If Arc::clone is shared between tasks, the Sync impl allows it
- Step 3: Concurrent SQLite access from multiple tasks would be undefined behavior

CONCLUSION:
This is a correctness concern. The safety argument depends on a runtime discipline
("we only call it from one task") that is not enforced by the type system. A
Mutex<Connection> or tokio::task::spawn_blocking would make the invariant
compile-time safe. Flagged for the daemon review session.

CONFIDENCE: HIGH
SEVERITY: CRITICAL (but in out-of-scope crate)

WHAT I COULD NOT VERIFY:
The daemon's actual usage pattern — whether the invariant holds in practice.
```

---

**FINDING 11:** All `unsafe` blocks in in-scope crates are justified and low-risk.

```
PREMISES:
- P1: shore-tui/src/images.rs:55-56 — std::mem::zeroed() for libc::winsize +
  libc::ioctl for terminal pixel size query
- P2: shore-tui/src/bin/kitty_diag.rs:404,416-417 — libc::poll and ioctl for
  diagnostic binary
- P3: shore-client/src/image_protocol.rs:133 — libc::poll for kitty graphics
  protocol probe

TRACED PATH:
- Step 1: ImageCache::new() calls query_cell_size() which uses libc::ioctl
  (images.rs:49)
- Step 2: zeroed() initializes a libc::winsize struct (4 u16 fields) — safe for
  zeroed initialization
- Step 3: ioctl return value is checked before reading the struct

CONCLUSION:
These are standard FFI patterns for terminal control. The zeroed() initialization
is safe for the C structs involved. The poll() calls have bounded timeouts. No
concerns.

CONFIDENCE: HIGH
SEVERITY: OBSERVATION

WHAT I COULD NOT VERIFY: None.
```

---

## PHASE 4: SYNTHESIS

### 4.1 Foundation Assessment

The foundation crates are well-structured, clearly separated by responsibility, and demonstrate strong architectural discipline. `shore-protocol` is a clean, leaf dependency with no behavioral coupling — it defines the contract and nothing else. The serialization strategy (internally-tagged JSON enums with `serde`) is consistent, forward-compatible, and thoroughly tested with golden-file fixtures and round-trip tests. `shore-config` handles a genuinely complex configuration problem (provider defaults cascading, TOML layering, XDG resolution) with clear separation between the loading/validation pipeline and the config struct definitions. `shore-llm-client` is the most complex foundation crate, and its provider dispatch pattern — while not using traits — is pragmatic: each provider module is self-contained, making it easy to understand and modify one provider without affecting others.

The two areas where the foundations show their age are the streaming hot path (the SSE parser's quadratic allocation pattern and per-chunk String allocations) and the error type architecture (no cross-crate `From` impls, `Box<dyn Error>` in the ledger and CLI). These are not correctness bugs — the code works correctly today — but they represent scalability and maintainability debt that will compound as the codebase grows. The TUI's full-history-rebuild-after-every-stream pattern is the most immediately impactful performance concern. The protocol itself is sound: the JSON-Lines framing, handshake sequence, and forward-compatible serde strategy are well-designed. The test coverage across all crates is strong — golden-file fixtures, round-trip tests, integration tests with mock servers, and comprehensive unit tests for edge cases.

### 4.2 Top 5 Findings (Ranked by Severity)

1. **[SIGNIFICANT]** The SSE parser has an O(n^2) allocation pattern on the hottest code path — every SSE line re-creates the entire remaining buffer (`sse.rs:38`). (Finding 1)

2. **[SIGNIFICANT]** The `rid` (request ID) parameter is plumbed through the LLM client API but explicitly ignored (`_rid`), meaning LLM calls cannot be traced back to user requests as the architecture doc specifies. (Finding 5)

3. **[SIGNIFICANT]** The TUI rebuilds the entire conversation history and re-transmits all images after every streaming response — an O(n) operation that will degrade with conversation length. (Finding 9)

4. **[SIGNIFICANT]** Mutex poisoning panics are possible in production — `.lock().unwrap()` is used in the ledger (6 sites), pricing engine (4 sites), and CLI spinner (6 sites). (Finding 6)

5. **[OBSERVATION]** The protocol has no server-to-client capability negotiation — old clients will silently drop new message types rather than erroring, which is forward-compatible but makes protocol evolution opaque. (Finding 4)

### 4.3 Questions for the Developer

1. **Is the `rid` propagation intended to be implemented later?** The `_rid` parameter exists in `LlmClient::stream_raw()` and `generate()` but is unused. The architecture doc (section 6) specifies `X-Request-ID` header propagation. Is this deferred or a gap?

2. **Does the daemon send a `History` push after `StreamEnd`?** The TUI's response to `StreamEnd` is to fetch the full log again. If the daemon already pushes `History` after state changes, this is redundant work. If not, why not?

3. **What is the expected conversation length?** The TUI's full-rebuild pattern is viable for short conversations but would be problematic for conversations with hundreds of messages and dozens of images.

4. **Is the `MemoryDB` unsafe `Sync` impl a known, accepted risk?** The safety invariant (single-task usage) is documented but not compiler-enforced. Is this intentional for performance, or would wrapping in `Mutex` be acceptable?

5. **Are there plans for structured exit codes in the CLI?** Currently all errors are boxed into `Box<dyn Error>` and the CLI returns a generic `ExitCode::FAILURE`. Structured exit codes would help automation and scripting.

6. **Why does `shore-ledger` not have a typed error enum?** Was this a deliberate choice (simplicity) or an oversight?

### 4.4 Findings to Carry Forward (Session 2: `shore-daemon`)

1. **The daemon's `unsafe impl Sync for MemoryDB`** — must verify the single-task invariant holds in all code paths. (`shore-daemon/src/memory/db.rs:222`)

2. **The `rid` propagation gap** — does the daemon extract `rid` from incoming `ClientMessage` and pass it through to `LlmClient` calls? If so, the `_rid` parameter in `shore-llm-client` needs to be activated. If not, the daemon's tracing story is incomplete.

3. **The `History` push after state changes** — ARCHITECTURE.md section 3.5 states "history is re-sent after any state change." Need to verify the daemon implements this, and if so, whether the TUI's redundant `log` fetch after `StreamEnd` is unnecessary.

4. **The `NewMessage` push for autonomous messages** — the TUI has deduplication logic (`main.rs:627-686`) with a "dominated check" and "optimistic echo replacement." Need to verify the daemon's `NewMessage` timing to understand if race conditions are possible.

5. **The `LedgerStream` finalization contract** — does the daemon always call `.finalize()` on every `LedgerStream`, including error paths? The Drop guard logs but doesn't record.

6. **The `merge_tool_loop_messages()` in `shore-protocol`** — this runs client-side. Need to verify the daemon sends the raw (un-merged) tool-loop messages, and that the merge algorithm handles all daemon-side tool-loop patterns correctly.
