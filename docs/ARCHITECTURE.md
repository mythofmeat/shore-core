# Shore V2 — Architecture Plan

## 1. Design Goals

1. **Discrete, modular services** with hard boundaries and a formalized wire
   protocol — no service needs to understand another's internals.
2. **Each service is small enough for an LLM to fully comprehend** in a single
   context window read (~2-5K LOC per service, ~500 LOC per module within the
   daemon).
3. **Rust core** — compiler-enforced correctness, no runtime dependencies for
   the daemon, CLI, and bridges.
4. **Native LLM providers** — the `shore-llm-client` crate implements all
   provider integrations (Anthropic, OpenAI-compat, Gemini, DeepSeek, ZhipuAI, Z.AI, NanoGPT)
   directly in Rust using `reqwest`. No separate process or TypeScript runtime.
5. **Separate binaries** for daemon, CLI/TUI, and each bridge —
   independent development, deployment, and restart.
6. **Build it right from the start** — incorporate planned redesigns (interiority,
   private conversations) into the V2 architecture rather than porting V1 bugs.

---

## 2. System Overview

```
                            ┌──────────────────────────────────────┐
                            │         shore-daemon (Rust)           │
                            │                                       │
┌──────────────┐  SWP/Unix  │  ┌───────────┐  ┌───────────────┐    │
│  shore       │───────────▶│  │  Server    │──│  Engine        │   │
│  (CLI/TUI)   │◀───────────│  │  (accept,  │  │  (prompt asm,  │   │
└──────────────┘            │  │   route)   │  │   tool loop,   │   │
                            │  └───────────┘  │   commands)     │   │
┌──────────────┐  SWP/Unix  │                  └──┬─────┬───────┘   │
│  shore-mx    │───────────▶│                     │     │           │
│  (bridge)    │◀───────────│  ┌─────────────┐    │     │           │
└──────────────┘            │  │ Autonomy     │◀──┘     │           │
                            │  │ (interiority,│         │           │
                            │  │  cache)      │    ┌────▼───────┐   │
                            │  └─────────────┘    │ LLM Client  │   │
                            │                     │ (reqwest,    │──── LLM APIs
                            │                     │  Anthropic,  │   │
                            │                     │  OpenAI,     │   │
                            │  ┌───────────────┐  │  Gemini,     │   │
                            │  │  Memory        │  │  etc.)       │   │
                            │  │  (SQLite, RAG, │  └─────────────┘   │
                            │  │   LanceDB)     │                    │
                            │  └───────────────┘                    │
                            └──────────────────────────────────────┘
```

**Four binaries at launch** (more bridges/clients added later):

| Binary | Language | Role |
|--------|----------|------|
| `shore-daemon` | Rust | Persistent daemon — engine, memory, autonomy, tool loop, LLM providers |
| `shore` | Rust | CLI — stateless commands |
| `shore-tui` | Rust | TUI — persistent connection, full terminal UI |
| `shore-matrix` | Rust | Matrix bridge (includes Synapse management) |

Telegram and Discord bridges are **deferred** — Matrix is the only required
platform integration for V2 launch.

---

## 3. Shore Wire Protocol (SWP)

### 3.1 Transport & Framing

- **Transport:** TCP
- **Framing:** Newline-delimited JSON (JSON-Lines). Each message is a single
  JSON object followed by `\n`. Content newlines are JSON-escaped.
- **Encoding:** UTF-8
- **Max message size:** 16 MB
- **Keepalive:** Server sends `ping` every 30s on TCP connections.

### 3.2 Connection Lifecycle

```
Client                          Server
  │                               │
  │──── connect ─────────────────▶│
  │                               │
  │◀──── hello ──────────────────│  (protocol version, server info)
  │                               │
  │──── hello ──────────────────▶│  (client info, capabilities)
  │                               │
  │◀──── history ────────────────│  (active messages + config snapshot)
  │                               │
  │     ... normal operation ...  │
  │                               │
  │◀──── shutdown ───────────────│  (server going down)
  │                               │
```

### 3.3 Message Envelope

Every message has this shape:

```json
{
  "type": "<message_type>",
  "v": 1,
  "rid": "optional-request-id",
  ...fields specific to type
}
```

- `v` — protocol version. Clients and servers must reject unsupported versions.
- `rid` — request ID. Client-generated, opaque string. The server echoes `rid`
  on every response message (including streaming chunks and push messages)
  that was triggered by that request. Push messages not tied to a request
  (interiority, status_change) have `rid: null`. This enables:
  - **Multiplexing** — TUI can send a status command while a message streams
  - **Tracing** — follow a request through structured logs across all services
  - **Debugging** — correlate "which request caused this error?"

### 3.4 Client → Server Messages

Three first-class message types plus a generic command envelope.

#### `hello` — Client identification (sent once after connect)
```json
{
  "type": "hello",
  "v": 1,
  "client_type": "tui" | "cli" | "bridge",
  "client_name": "shore-tui",
  "capabilities": ["streaming"]
}
```

#### `message` — Send a user message
```json
{
  "type": "message",
  "rid": "msg_01",
  "text": "...",
  "stream": true,
  "images": [],
  "image_data": [{"filename": "photo.jpg", "data": "<base64>"}],
  "absence_seconds": null
}
```
All fields except `type` and `text` are optional. `image_data` (base64-encoded)
is preferred over `images` (filesystem paths); both are accepted for backwards
compatibility.

#### `regen` — Regenerate last response
```json
{
  "type": "regen",
  "rid": "regen_01",
  "stream": true,
  "guidance": null
}
```

#### `command` — Execute a server command
```json
{
  "type": "command",
  "rid": "cmd_01",
  "name": "switch_character",
  "args": { "name": "alice" }
}
```

All server-side operations that don't involve streaming responses go through
this single envelope. See §3.7 for the complete command reference.

### 3.5 Server → Client Messages

#### Connection & Lifecycle

| Type | When | Key Fields |
|------|------|------------|
| `hello` | After client connects | `v`, `server_name`, `characters[]` |
| `history` | After handshake; after any state change (switch, new chat, toggle, edit, delete) | `messages[]`, `config` |
| `shutdown` | Server stopping | — |
| `ping` | Periodic keepalive (TCP) | — |

`history` is the workhorse — any time the client's view of the world should
change, the server re-sends full state. Simple to implement, impossible to
desync.

#### Message Object

Every message in `history.messages[]`, `stream_end`, and `new_message` uses
this flat struct. One shape everywhere — no polymorphism, no optional subtype
variants.

```rust
struct Message {
    msg_id:    String,                // stable ID for edit/delete/swipe refs
    role:      Role,                  // "user" | "assistant" | "system"
    content:   String,                // rendered text content
    images:    Vec<ImageRef>,         // empty vec when none
    alt_index: Option<u32>,           // swipe: current variant (0-based), null if no alternatives
    alt_count: Option<u32>,           // swipe: total variants, null if no alternatives
    timestamp: String,                // ISO 8601
}

struct ImageRef {
    path:    String,                  // filesystem path to image
    caption: Option<String>,
    data:    Option<String>,          // base64-encoded bytes (wire only, stripped on disk)
}
```

`msg_id` is a server-assigned opaque string. Clients use it verbatim in `edit`
and `delete` commands — never parse or construct IDs.

#### Request Responses

| Type | When | Key Fields |
|------|------|------------|
| `command_output` | Command result | `name`, `data` |
| `error` | Any error | `code`, `message` |

#### Streaming

All `message` and `regen` responses use the streaming path — there is no
separate non-streaming response type. Short replies are just
`stream_start` → `stream_end` with no intermediate chunks. One code path
for clients.

| Type | When | Key Fields |
|------|------|------------|
| `stream_start` | Begin streaming | `regen` (bool) |
| `stream_chunk` | Partial content | `text`, `content_type` |
| `stream_end` | Done streaming | `content`, `metadata` |

`stream_chunk.content_type` is `"text"` (default) or `"thinking"`. Clients
that don't support thinking display can ignore chunks where
`content_type == "thinking"`.

`stream_end.metadata` has a fixed shape:

```json
{
  "tokens": {
    "input": 1234,
    "output": 567,
    "cache_read": 890,
    "cache_write": 0
  },
  "timing": {
    "total_ms": 2340,
    "ttft_ms": 450
  },
  "model": "claude-haiku-4-5-20251001"
}
```

All fields are integers except `model` (string). Provider-specific token fields
(`cache_read`, `cache_write`) are `0` for providers that don't support caching.

#### Push (unsolicited, `rid: null`)

| Type | When | Key Fields |
|------|------|------------|
| `phase` | Generation phase change | `phase`, `model` |
| `new_message` | Autonomous message arrived | full `Message` object |
| `tool_call` | Tool invoked during generation | `tool_id`, `tool_name`, `input` (JSON object) |
| `tool_result` | Tool completed | `tool_id`, `tool_name`, `output`, `is_error` |
| `send_image` | Server-generated image ready | `path`, `caption?`, `data?` (base64) |
| `cache_warning` | Unexpected cache invalidation | `expected_tokens`, `message` |

`phase` values: `"thinking"`, `"text_generation"`, `"tool_use"`. Clients use
these to show generation state (e.g. "thinking..." spinner).

`tool_call.input` is always a JSON object (the tool's input parameters), never
a string.

### 3.6 Error Codes

```
PROTOCOL_ERROR     — malformed message, unknown type, version mismatch
INVALID_REQUEST    — missing required field, bad argument
NOT_FOUND          — unknown character, conversation, message
BUSY               — engine already processing a request
PROVIDER_ERROR     — LLM API failure
TIMEOUT            — request timed out
INTERNAL_ERROR     — unexpected server error
```

### 3.7 Command Reference

18 flat commands. Naming convention: `verb_noun` for actions on a specific
thing, bare verb/noun when unambiguous.

#### Conversation

| Command | Args | Description |
|---------|------|-------------|
| `send` | `text`, `images[]?` | Send a message (CLI shorthand — maps to `message` protocol type) |
| `regen` | `guidance?` | Regenerate (CLI shorthand — maps to `regen` protocol type) |
| `swipe` | `target?` (prev/next/N) | Navigate response candidates; next is default, regens at end of stack |
| `log` | `count?` | Show conversation history |
| `edit` | `ref`, `content?` | Edit a message |
| `delete` | `refs` | Delete message(s) |

#### Navigation

| Command | Args | Description |
|---------|------|-------------|
| `list_characters` | — | List all characters |
| `switch_character` | `name` | Switch active character → server re-sends `history` |
| `list_chats` | — | List conversations (shows `[private]` badge) |
| `switch_chat` | `id` | Open a conversation → server re-sends `history` |
| `new_chat` | `title?` | Start a new conversation |

#### State

| Command | Args | Description |
|---------|------|-------------|
| `status` | — | System state + token counts |
| `list_models` | — | List available model profiles |
| `switch_model` | `name?` | No arg = show current; with arg = switch model |
| `memory` | `query?` | No arg = status; with arg = search memories |
| `toggle_private` | — | Toggle private mode on active conversation → server re-sends `history` |
| `compact` | `dry_run?` | Trigger compaction |
| `collate` | `full?` | Run 5-phase collation pipeline (backfill → collate → tidy → normalize → decay). `full=true` loops until stable |
| `memory_purge` | `older_than?` | Delete old superseded entries (default 30d) |
| `toggle_autonomy` | — | Toggle autonomy pause/resume |
| `config` | `section?` | Show effective configuration |

Commands that change client-visible state (`switch_character`, `switch_chat`,
`new_chat`, `toggle_private`, `edit`, `delete`) trigger a `history` push so
clients stay in sync without parsing command output.

### 3.8 Deferred Commands

These commands exist in V1 but are deferred to post-launch. The generic
`command` envelope means adding them later requires zero protocol changes.

| Command | Reason for deferral |
|---------|-------------------|
| `chat_fork` | Nice to have, not essential |
| `chat_search` | Nice to have |
| `msg_insert` | Rare operation |
| `msg_detach` | Edge case |
| ~~`memory_collate`~~ | ~~Runs automatically~~ (implemented as `collate` command + auto-trigger) |
| `memory_reindex` | Maintenance op |
| `memory_import` | Add later |
| `memory_shell` | Interactive memory sessions (requires additional protocol messages) |
| `images_list` | Add when image tools ship |
| `images_import` | Add when image tools ship |

### 3.9 Protocol Crate

A shared Rust crate (`shore-protocol`) defines all message types as
serde-serializable structs and enums. This crate is a dependency of every
binary in the workspace.

---

## 4. Service Specifications

### 4.1 shore-daemon

**Language:** Rust
**Async runtime:** Tokio
**Binary:** `shore-daemon`

#### Responsibilities

| Subsystem | Description | Key Crates |
|-----------|-------------|------------|
| **Server** | Accept connections (Unix/TCP), route requests, broadcast push messages | `tokio`, `serde_json` |
| **Engine** | Per-character conversation state machine: prompt assembly, tool loop, message persistence | — |
| **LLM Client** | Native provider integrations via `shore-llm-client` crate (Anthropic, OpenAI-compat, Gemini, DeepSeek, ZhipuAI, Z.AI) | `reqwest` |
| **Memory** | SQLite database (entries, entities, flags, changelog), CRUD operations | `rusqlite` |
| **RAG** | Vector search (LanceDB) + BM25 keyword retrieval + embedding via HTTP | `lancedb`, custom BM25 |
| **Compaction** | Conversation → memory entries (via LLM). Proactive idle timer fires at `idle_trigger_minutes` after last activity — no waiting for next user message. | — |
| **Collation** | 5-phase memory pipeline: timestamp backfill → collate (merge) → tidy (split) → normalize entities → confidence decay. Embedding-driven clustering groups related entries before LLM calls. `collated_at` watermark tracks processing state. | — |
| **Interiority** | Timer-based autonomous turns with full tool access (see §13.1) | — |
| **Cache Keepalive** | Anthropic prompt cache TTL refresh pings | — |
| **Activity Tracker** | Session tempo, hour histograms, engagement scoring | — |
| **Config** | TOML loading (config.toml + models.toml), validation, defaults | `toml`, `serde` |
| **Image Resize** | Format-aware image resizing for LLM API limits; XDG disk cache with SHA-256 keys; async pre-warm before prompt assembly | `fast_image_resize`, `image`, `sha2` |
| **Commands** | Command handlers dispatched by name+subcmd | — |
| **Registry** | Instance registry in `$XDG_RUNTIME_DIR` with file locking | — |

The daemon's LLM provider integrations live in the `shore-llm-client` crate,
which implements direct HTTP calls to each provider's API using `reqwest`.
There is no separate LLM proxy process.

#### Internal Module Layout

```
shore-daemon/
├── src/
│   ├── main.rs
│   ├── server/
│   │   ├── mod.rs              # Server struct, accept loop, client tracking
│   │   ├── handler.rs          # Request → command routing
│   │   └── registry.rs         # Instance registry (write)
│   │
│   ├── engine/
│   │   ├── mod.rs              # ConversationEngine (state machine)
│   │   ├── prompt.rs           # Prompt assembly pipeline
│   │   ├── messages.rs         # Message CRUD (append, edit, delete, swipe)
│   │   ├── conversations.rs    # Conversation lifecycle (new, switch, fork, archive, private)
│   │   └── tools.rs            # Tool use agentic loop
│   │
│   ├── llm_client/
│   │   ├── mod.rs              # Client using shore-llm-client crate
│   │   ├── types.rs            # Request/response types
│   │   ├── retry.rs            # Application-level retry (refusal detection, model fallback)
│   │   └── stream.rs           # Streaming response consumer
│   │
│   ├── memory/
│   │   ├── mod.rs              # Memory manager (high-level operations)
│   │   ├── db.rs               # SQLite schema, CRUD, migrations
│   │   ├── rag.rs              # RAG pipeline (vector + BM25 + scoring)
│   │   ├── vectorstore.rs      # LanceDB integration
│   │   ├── compaction.rs       # Conversation → entries (library)
│   │   ├── compaction_impls.rs # Production CompactionLlm/VectorIndexer/ConversationManager
│   │   ├── collation.rs        # 4-phase dedup pipeline (library)
│   │   ├── collation_impls.rs  # Production CollationLlm (JSON parsing)
│   │   ├── agent.rs            # Memory agent (with caller identity awareness)
│   │   ├── search.rs           # Full-text search
│   │   └── importer.rs         # File import to entries
│   │
│   ├── autonomy/
│   │   ├── mod.rs              # Master autonomy controller
│   │   ├── interiority.rs      # Interiority clock (dual-deadline timer + dormancy)
│   │   └── activity.rs         # Activity tracker (tempo, histograms)
│   │
│   ├── config/
│   │   ├── mod.rs              # Config loading, resolution, validation
│   │   ├── app.rs              # AppConfig struct (all sections)
│   │   └── models.rs           # ModelConfig struct (models.toml)
│   │
│   ├── handler/
│   │   ├── mod.rs              # Message handling: prompt assembly, image encoding, cache warm-up
│   │   ├── images.rs           # Image ingestion, content block encoding
│   │   ├── resize.rs           # Format-aware resize pipeline, XDG disk cache
│   │   ├── generation.rs       # LLM response generation
│   │   └── persistence.rs      # Message persistence
│   │
│   ├── commands/
│   │   ├── mod.rs              # Command dispatch table (18 flat commands, see §3.7)
│   │   ├── navigation.rs       # list_characters, switch_character, list_chats, switch_chat, new_chat
│   │   ├── conversation.rs     # swipe, log, edit, delete
│   │   └── state.rs            # status, list_models, switch_model, memory, toggle_private, compact, collate, toggle_autonomy, config
│   │
│   └── types.rs                # Shared daemon-internal types
```

No file should exceed ~1000 LOC. If it does, split it.

### 4.1b shore-daemon-server

SWP (Shore Wire Protocol) server and instance registry. Handles Unix socket + TCP listeners,
client handshake, message routing, and broadcast. Also provides the daemon instance registry
for service discovery. Depends on `shore-protocol` and `shore-config` only.

### 4.2 shore (CLI)

**Language:** Rust
**Binary:** `shore`

| Feature | Description |
|---------|-------------|
| CLI parsing | Subcommand routing (`shore <cmd> [args]`) |
| Socket client | Connect to daemon via SWP, handle streaming + push messages |
| Instance discovery | Read registry, auto-find socket for config path |
| Formatting | Rich terminal output (colors, markdown rendering) |
| Tab completion | Shell completions for fish/bash/zsh |
| Inline images | Render images in terminal via kitty/iTerm2 graphics protocols |
| Desktop notify | `notify-send` integration |

Stateless — every command connects to the daemon, sends a request, prints the
response, and exits. No daemon logic. Pure SWP client.

### 4.2b shore-tui (TUI)

**Language:** Rust (ratatui)
**Binary:** `shore-tui`

| Feature | Description |
|---------|-------------|
| Conversation view | Scrollable message log with markdown rendering |
| Input | Multiline text input, editor mode |
| Status bar | Character, model, token count, private mode indicator |
| Persistent connection | Long-lived SWP session with streaming + push handling |
| Inline images | kitty/iTerm2 graphics protocol rendering |

Maintains a persistent SWP connection. Shares `shore-client` with the CLI but
is a separate binary with its own UI layer. Both the CLI and TUI are pure SWP
clients — they can be developed independently.

### 4.3 shore-matrix

**Language:** Rust (`matrix-sdk`)

#### Responsibilities

- Connect to daemon as SWP client (`client_type: "bridge"`)
- Matrix client with E2E encryption (matrix-sdk handles crypto natively)
- SAS key verification (auto-trust allowed user)
- Room management, auto-join
- Image buffering (collect images until next text message)
- Command handling (`!` prefix)
- Synapse subprocess management (optional, for embedded mode):
  - Config generation, health checking
  - Admin account creation
  - Character account provisioning (register, create room, avatar sync)
- Reconnection to daemon with backoff

#### Configuration

Receives most config from daemon via SWP `hello` exchange. Bridge only needs:
- Daemon address (auto-discovered via registry, or `--addr` flag)
- For external Matrix: access_token, homeserver_url, device_id (env/flags)
- For embedded Synapse: admin credentials (env/flags)

### 4.4 shore-llm-client (LLM Provider Crate)

**Language:** Rust
**Type:** Library crate (workspace member)

#### Purpose

`shore-llm-client` implements all LLM provider integrations natively in Rust.
The daemon calls it as a library — there is no separate process, no IPC, and
no TypeScript runtime.

#### Responsibilities

| What | Description |
|------|-------------|
| Provider implementations | Anthropic, OpenAI-compat, Gemini, DeepSeek, OpenRouter, ZhipuAI, Z.AI, NanoGPT |
| Streaming | Async streaming via `reqwest` + SSE parsing |
| Provider retries | 429 rate limits, 503 transient errors |
| Response normalization | Every provider's response mapped to a common format |
| Timing + metrics | Reports latency, time-to-first-token, cache hit status per call |
| Embeddings | Text embedding via OpenAI-compatible endpoints |
| Image generation | DALL-E, Flux, Gemini image generation |

The crate is zero-config. The daemon owns all configuration and passes the
fully resolved model profile (provider, model name, API key, base URL, and
all provider-specific options) in every call.

### 4.5 Future Bridges (deferred)

| Bridge | Language | Status |
|--------|----------|--------|
| `shore-telegram` | Rust (teloxide) | Deferred — add after V2 launch |
| `shore-discord` | Rust (serenity + poise) | Deferred — add after V2 launch |

These follow the same bridge pattern as shore-matrix. The SWP protocol is
designed so adding a new bridge requires zero daemon changes.

---

## 5. Service Management

The daemon can optionally manage companion services (e.g., the Matrix bridge)
as child processes via the `[services]` config section.

### 5.1 Config

```toml
[services]
# Bridges are optional
matrix = { command = "shore-matrix", socket = "matrix.sock", enabled = false }
```

LLM provider calls are handled natively by the `shore-llm-client` crate
within the daemon process — no external LLM service is needed.

### 5.2 Behavior

For each enabled service:
1. **Spawn** the process as a child of the daemon
2. **Health check** — poll the socket/health endpoint (1s interval, 30s timeout)
3. **Mark ready** when health check passes
4. **Monitor** — if the process exits unexpectedly:
   - Log the exit code
   - Wait 1s, then restart (exponential backoff up to 30s on repeated failures)
   - Cap at 5 restart attempts, then log an error and mark the service as failed
5. **Shutdown** — on daemon exit, send SIGTERM to all children, wait 10s, SIGKILL

Bridges are non-blocking — the daemon runs fine without them.

---

## 6. Structured Logging

All services emit structured JSON logs to stderr. Every log entry includes:

```json
{
  "ts": "2026-03-25T14:30:00.123Z",
  "level": "info",
  "service": "shore-daemon",
  "rid": "msg_01",
  "msg": "LLM call completed",
  "provider": "anthropic",
  "model": "claude-sonnet-4-6",
  "input_tokens": 1234,
  "output_tokens": 567,
  "latency_ms": 2340
}
```

- `service` — which process emitted the log
- `rid` — request ID from the SWP message that triggered this work (propagated
  through LLM calls via an `X-Request-ID` header). Enables tracing a
  user message through daemon → LLM provider and back.
- Rust uses `tracing` + `tracing-subscriber` with human-readable formatting

---

## 7. Filesystem Layout

Three XDG directories, each with a clear purpose.

### 7.1 Config — User-Edited Files

```
$XDG_CONFIG_HOME/shore/            (~/.config/shore/)
├── config.toml                    # Global configuration
├── models.toml                    # Model profiles
├── user.md                        # Default user definition
├── prompts/                       # Default prompt templates
│   ├── system.md
│   ├── post_session.md
│   ├── social_need.md
│   ├── deferred.md
│   ├── compact.md
│   ├── collate.md
│   ├── tidy.md
│   └── normalize_entities.md
└── characters/
    └── {character}/
        ├── character.md           # Character definition
        ├── user.md                # Optional — overrides global user.md
        └── prompts/               # Optional — per-character prompt overrides
            └── system.md          # Overrides default prompts/system.md
```

**Prompt resolution order** (first found wins):
1. `characters/{character}/prompts/{template}.md`
2. `prompts/{template}.md`
3. Built-in default (shipped with the binary install)

**User definition resolution:**
1. `characters/{character}/user.md` (if exists)
2. `user.md` (global default)

### 7.2 Data — Program-Managed, Persistent

```
$XDG_DATA_HOME/shore/              (~/.local/share/shore/)
├── prompts.manifest.json          # Tracks stock vs user-modified templates
└── {character}/
    ├── memory/
    │   ├── memory.db              # SQLite (entries, entities, flags, changelog)
    │   ├── vectorstore/           # LanceDB index
    │   ├── recap.md               # Rolling narrative recap (generated)
    │   └── changelog.md           # Audit trail (generated, configurable path)
    ├── conversations/
    │   ├── manifest.json          # Conversation index (includes private flag)
    │   └── {conv_id}.jsonl        # Message history
    ├── images/
    │   ├── generated/             # AI-generated images
    │   └── received/              # Images from user or chat platforms
    └── matrix/
        ├── provision.json         # Provisioning state (user_id, token, room_id)
        └── crypto_store/          # E2E encryption keys (matrix-sdk managed)
```

Everything under `{character}/` is self-contained. You can back up, move, or
delete a character's data by operating on a single directory.

Matrix bridge state lives under the character it belongs to, not in a separate
top-level `matrix/` directory.

### 7.3 Runtime — Ephemeral, Gone on Reboot

```
$XDG_RUNTIME_DIR/shore/            (/run/user/{uid}/shore/)
├── shore.sock                     # Daemon SWP socket
├── instances.json                 # Instance registry
└── instances.lock                 # File lock for concurrent access
```

---

## 8. Configuration

### 8.1 config.toml (Daemon)

Loaded by daemon on startup. Key changes from V1:

- `[behavior.autonomy]` section replaces scattered autonomy knobs:
  - `enabled` (bool)
- `[behavior.autonomy.interiority]` — interiority config (interval, jitter, max idle ticks)
- `[memory.compaction]` — compaction triggers
- `[memory.collation]` — collation settings
- `[connections.matrix]` — replaces `matrix_external` and `matrix_embedded`
  (single section, mode determined by config present)
- `[connections.telegram]` and `[connections.discord]` — reserved for future use

### 8.2 models.toml (Daemon)

Unchanged from V1 structure.

### 8.3 Bridge Configuration

Bridges need exactly two things to start:
1. **How to find the daemon** — TCP address (auto-discovered via registry) or
   explicit `--addr` flag
2. **Platform credentials** — access token / bot token (env var or flag)

Everything else comes from the daemon via the SWP `hello` exchange.

### 8.4 Client Configuration (`client.toml`)

Clients (CLI, TUI, bridges) can set a default server address in
`$XDG_CONFIG_HOME/shore/client.toml`. This is loaded by `shore-client`
independently of the daemon's `config.toml` — the two files share a directory
but use separate code paths.

```toml
default_address = "100.64.0.1:7320"
```

**Address resolution order:**

1. `--addr` CLI flag (explicit address)
2. `client.toml` `default_address`
3. Instance discovery (`instances.json`, optionally filtered by `--config` ID)
4. Default: `127.0.0.1:7320`

The file is optional. If missing or unparseable, resolution falls through to
instance discovery. On the daemon machine, omit `client.toml` (or leave
`default_address` unset) to use instance discovery as before.

---

## 9. SQLite Schema

Carried forward from V1 with no changes.

### Tables

**entries** — Primary memory store
| Column | Type | Description |
|--------|------|-------------|
| id | TEXT PK | `YYYYMMDD_HHMMSS_N` |
| memory_type | TEXT | `episodic` / `semantic` |
| source | TEXT | `summary` / `import` / ... |
| reason | TEXT | `compaction` / `collation` / `tidy_split` / ... |
| status | TEXT | `active` / `protected` / `superseded` |
| canonical | BOOL | Is this a canonical (merged) entry? |
| confidence | REAL | 0.0–1.0 |
| summary_text | TEXT | Newline-joined content |
| topic_tags | TEXT | Comma-separated |
| topic_key | TEXT | Inferred topic cluster |
| start_timestamp | TEXT | ISO timestamp |
| end_timestamp | TEXT | ISO timestamp |
| message_count | INT | Messages condensed |
| source_entry_ids | TEXT | Comma-separated parent IDs |
| related_entry_ids | TEXT | Comma-separated linked IDs |
| superseded_by | TEXT | Replacement entry ID |
| created_at | TEXT | ISO timestamp |
| updated_at | TEXT | ISO timestamp |
| entry_type | TEXT | `""` / `"image"` |
| image_path | TEXT | Filesystem path if image |
| collated_at | TEXT | Last collation pipeline timestamp (empty = never) |

**entities** — Entity registry (entity_id INT PK, name TEXT UNIQUE NOCASE,
type TEXT, description TEXT, created_at TEXT, updated_at TEXT)

**entry_entities** — Many-to-many (entry_id TEXT, entity_id INT)

**changelog** — Audit log (changelog_id INT PK, operation TEXT, description
TEXT, timestamp TEXT)

**changelog_entries** / **changelog_entities** — Junction tables

**flags** — Issue tracking (flag_id INT PK, entry_id TEXT FK, flag_type TEXT,
reason TEXT, resolved_at TEXT, resolution TEXT, created_at TEXT)

**collation_skip** — ~~Optimization~~ Legacy table, no longer used by collation pipeline (replaced by `collated_at` column on entries)

---

## 10. Complete Feature Map

### Daemon (shore-daemon)

| Feature | V1 File(s) | V2 Module | Notes |
|---------|------------|-----------|-------|
| Conversation state machine | engine.py | engine/mod.rs | |
| Prompt assembly | engine_prompt.py | engine/prompt.rs | No interiority context injection |
| Message CRUD | engine_messages.py | engine/messages.rs | |
| Conversation lifecycle | engine_conversations.py, conversations.py | engine/conversations.rs | Adds private flag |
| Tool use loop | tool_use.py | engine/tools.rs | |
| LLM client | providers/*.py | llm_client/mod.rs | Uses shore-llm-client crate |
| Refusal detection + fallback | llm_retry.py | llm_client/retry.rs | Application-level (daemon-side) |
| Anthropic provider | providers/anthropic.py | **shore-llm-client** | |
| Gemini provider | providers/gemini.py | **shore-llm-client** | |
| OpenAI-compat provider | providers/openai_compatible.py | **shore-llm-client** | |
| OpenRouter provider | providers/openrouter.py | **shore-llm-client** | |
| ZhipuAI provider | providers/zhipuai.py | **shore-llm-client** | |
| Provider trait + factory | providers/_base.py, _factory.py | **shore-llm-client** | |
| Provider-level retry | llm_retry.py | **shore-llm-client** (reqwest) | 429, 503, transient |
| Model selector | model_selector.py | engine/mod.rs (inline) | |
| Memory DB (SQLite) | db.py | memory/db.rs | |
| RAG pipeline | rag.py | memory/rag.rs | |
| Vector store (LanceDB) | vectorstore.py | memory/vectorstore.rs | |
| Compaction | compaction.py | memory/compaction.rs | Skips private conversations |
| Collation (4 phases) | collation.py | memory/collation.rs | |
| Memory manager | memory.py | memory/mod.rs | |
| Memory agent | memory_agent.py | memory/agent.rs | Fix: caller identity awareness |
| Full-text search | search.py | memory/search.rs | |
| File importer | importer.py | memory/importer.rs | |
| Heartbeat scheduler | heartbeat.py | autonomy/interiority.rs | **Replaced** by interiority system (see §13.1) |
| Cache keepalive | cache_keepalive.py | autonomy/interiority.rs | **Merged** into unified interiority (see §13.1) |
| Activity tracker | activity_tracker.py | autonomy/activity.rs | Fix: lower data threshold |
| Server | server.py | server/mod.rs | |
| Command dispatch | commands.py | commands/*.rs | 18 flat commands (see §3.7) |
| Config loading | config.py, config_schema.py | config/*.rs | New autonomy section |
| Instance registry | registry.py | server/registry.rs | |

### CLI (shore)

| Feature | V1 File(s) | V2 Location |
|---------|------------|-------------|
| CLI entry + routing | cli/__init__.py, _base.py, _commands.py | shore-cli: main.rs + subcommand modules |
| Tab completion | cli/_completers.py | shore-cli: completions subcommand |
| Socket client | cli/_connect.py, client.py | shore-client crate (shared) |
| Desktop notifications | interfaces/notifier.py | shore-cli: notify module |
| Formatting | rendering.py, formatting.py | shore-cli: output module |

### TUI (shore-tui)

| Feature | V1 File(s) | V2 Location |
|---------|------------|-------------|
| TUI | (new) | shore-tui: app module (ratatui) |
| Text editor | interfaces/editor.py | shore-tui: input module |
| Log viewport | interfaces/log_follow.py | shore-tui: viewport module |
| Inline images | (new) | shore-tui: image module (kitty/iTerm2) |

### Bridges

| Feature | V1 File(s) | V2 Binary |
|---------|------------|-----------|
| Matrix bot | interfaces/matrix.py | shore-matrix |
| Matrix provisioning | interfaces/matrix_provision.py | shore-matrix |
| Synapse management | synapse_manager.py | shore-matrix |

### Removed

| V1 Feature | Disposition |
|------------|-------------|
| **Interiority scheduler** (interiority.py) | **Removed** — journal and story generation cut entirely |
| **Interiority generation** (engine_interiority.py) | **Removed** |
| **Interiority data dir** (journal/, stories/) | **Removed** |
| **Interiority config** ([autonomy.interiority]) | **Removed** |
| Telegram bot (interfaces/telegram.py) | **Deferred** to post-launch |
| Discord bot (interfaces/discord.py) | **Deferred** to post-launch |
| ChatInterface protocol (interfaces/__init__.py) | Replaced by SWP |
| BaseBotInterface (interfaces/_base.py) | Replaced by SWP client lib |
| InProcessClient | Replaced by SWP client lib |
| Relay server (relay.py) | Eliminated — daemon has native TCP |
| result_types.py | Absorbed into shore-protocol crate |
| analytics.py | Removed (not useful) |

---

## 11. Prompt Templates & Definitions

Templates are plain text/markdown files loaded by the daemon.

**Resolution order** (first found wins):
1. Per-character override: `characters/{character}/prompts/system.md`
2. Global override: `prompts/system.md`
3. Built-in default: shipped with the binary install

Templates are **not** compiled into the binary. They live on the filesystem so
they can be reviewed and edited without rebuilding. The daemon ships with a
default template directory that is installed alongside the binary.

**Templates (V2):**

| Template | Purpose |
|----------|---------|
| `system.md` | Base system prompt |
| `compact.md` | Compaction prompt |
| `collate.md` | Collation merge prompt |
| `tidy.md` | Collation tidy-split prompt |
| `normalize_entities.md` | Entity normalization prompt |
| `post_session.md` | **New** — post-session probe (character chooses time or declines) |
| `deferred.md` | **New** — deferred message delivery (character recalls why they chose this time) |
| `social_need.md` | **New** — spontaneous social-need probe (no scheduling instructions) |

**Removed templates:** `heartbeat.md` (replaced by post_session/deferred/social_need split), `nudge.md`, `journal.md`, `story.md`

### 11.1 Template Upgrade Management

Default templates will change between versions. The daemon must update stock
templates without clobbering user edits. A manifest at
`$XDG_DATA_HOME/shore/prompts.manifest.json` tracks the state:

```json
{
  "version": 1,
  "templates": {
    "system.md": { "hash": "sha256:abc123...", "updated_at": "2026-03-25T..." },
    "compact.md": { "hash": "sha256:def456...", "updated_at": "2026-03-25T..." }
  }
}
```

**On startup, for each default template:**

| File exists? | In manifest? | Hash matches? | Action |
|:---:|:---:|:---:|---|
| No | — | — | Write default, record hash |
| Yes | Yes | Yes | User hasn't touched it → overwrite with new default, update hash |
| Yes | Yes | No | User modified it → leave it alone |
| Yes | No | — | Pre-manifest file → treat as user-managed, don't touch |

Per-character prompt overrides (`characters/{character}/prompts/`) are always
user-managed and never tracked by the manifest.

---

## 12. Tools

The tool use system stays entirely within the daemon. Tools available to the
LLM during conversation:

| Tool | Description | Dependencies |
|------|-------------|-------------|
| `memory` | Semantic search + save | Memory/RAG subsystem |
| `send_image` | Send image from memory | Filesystem |
| `list_images` | List image memories (optional `query` for semantic search via RAG, top-32) | Memory DB + RAG |
| `recall_image` | View image at full resolution | Filesystem |
| `generate_image` | DALL-E 3 / Flux generation | HTTP (OpenAI-compat endpoint) |
| `web_search` | Search the web | HTTP (Tavily API) |
| `fetch_url` | Read a webpage | HTTP |
| `roll_dice` | Dice notation (2d6+3) | Pure computation |
| `check_time` | Current date/time | System clock |
| `activity_heatmap` | User's message patterns | Activity tracker |

**Private conversation behavior:** When a conversation is private, memory write
tools are excluded from the tool list. Memory read tools (RAG) are also
suppressed. Other tools (web, images, dice, time) remain available.

**Memory agent identity fix:** The memory agent must be told whether its caller
is `{{char}}` (during agentic tool calls) or `{{user}}` (during interactive
`memory shell` sessions). V1 bug: the agent couldn't resolve first-person
pronouns in character queries.

---

## 13. Autonomy Subsystems

### 13.1 Interiority — Autonomous Character Turns

The V1 heartbeat (5-state probability machine) is replaced by the unified
interiority system. Characters get periodic "turns to self" — full agentic
turns with the same tool set as normal conversation, running a real multi-turn
tool loop within each tick. Cache refresh is unified into the same timer — no
separate keepalive system.

#### Design

```
                                   set_next_wake()
                                   (character tool)
            ┌─────────┐  tick()    ┌──────────────┐     │
            │  Active  │─────────▶│  RunTick      │──▶ tool loop ──▶ schedule()
            └────┬────┘          └──────────────┘   (up to 6 iter)
                 │
          abandonment guard:
          ticks_without_user >= 3
          OR silent >= 48h
                 │
            ┌────▼──────┐
            │ Abandoned   │  next_wake_at = None
            └────┬──────┘  cache_keepalive.set_next_wake(None)
                 │
           user message
                 │
            ┌────▼────┐
            │  Active  │  next_wake_at = max(existing, now + min_wake)
            └─────────┘  ticks_without_user = 0

   Separate subsystem (independent cycle):
   CacheKeepalive ──▶ 59min ping ──▶ max_tokens=1 cache refresh
   (only fires if next_wake is within 18h break-even window)
```

#### Deadline-Based Clock

`InteriorityClock` is a pure deadline holder. The character drives its own
cadence via `set_next_wake` during interiority ticks. The clock holds the
deadline, applies bounds (1h–48h), and stops ticking when the abandonment
guard trips.

If the character doesn't call `set_next_wake`, the clock falls back to
`default_interval` (from `interval_secs` config).

#### Cache Keepalive

`CacheKeepalive` is a separate subsystem with its own 59-minute ping cycle.
It only fires pings when a future wake is scheduled AND the wake is within
the 18-hour break-even window (cost of pings vs. re-caching the full prompt).
Guard-trip propagation: when the abandonment guard clears `next_wake_at`,
it also clears `CacheKeepalive::next_wake`.

#### Recap System

Characters write first-person notes via `<recap>` tags during interiority
ticks (last-wins semantics, same as `<sendMessage>`). Entries are stored in
`recaps.jsonl` per character via `RecapStore`. During prompt assembly,
`trim_messages` injects recap markers alongside time-gap markers so the
character sees its own notes from between conversations.

#### States

| State | Description |
|-------|-------------|
| `Active` | Character has a scheduled wake time. Full interiority ticks fire when the deadline arrives. Cache keepalive pings fire independently. |
| `Abandoned` | `ticks_without_user >= max_idle_ticks` OR `time_since_last_user >= max_silent_secs`. No further ticks. Wakes on next user message. |

#### Tool Loop (`execute_unified_tick`)

Each interiority tick clones the last conversation request, appends the
interiority prompt as a user message, then runs a real multi-turn tool loop:
`generate()` → extract tool_use → dispatch tools → feed results back →
`generate()` again, up to `min(max_iterations, 6)` iterations.

Tool loop messages are **ephemeral** — they do not persist to `active.jsonl`
or mutate the cached `last_request`. Only `<sendMessage>` content (if the
character chooses to message the user) gets persisted to the conversation log.
All tool activity is logged to the interiority ring buffer, visible via
`shore log --heartbeat`.

The first `generate()` call uses `CallType::Interiority`; subsequent calls
in the same tick use `CallType::ToolLoop` for cost tracking.

#### Key Properties

- **Self-scheduling**: Characters control their own cadence via `set_next_wake`
  tool. The clock enforces [1h, 48h] bounds.
- **Real tool loop**: The character sees tool results within the same tick,
  enabling genuine exploration (web search → read results → compose message).
- **Identical tool set**: Preserves Anthropic prompt cache — system prompt and
  tool definitions are identical to normal conversation (plus `set_next_wake`).
- **Ephemeral loop messages**: Tool loop messages don't pollute the conversation
  log. The conversation only sees autonomous messages the character sends.
- **Decoupled cache keepalive**: Cache pings are a separate subsystem with
  break-even economics. Guard-trip propagation stops pings when the character
  is abandoned.
- **Recap continuity**: `<recap>` tags provide cross-tick memory that survives
  compaction. Injected into conversation history at time-gap boundaries.

#### Config

```toml
[behavior.autonomy.interiority]
enabled = true           # default: true
interval_secs = 7200     # default: 3600 (1 hour) — fallback when character doesn't set_next_wake
max_idle_ticks = 3       # abandon after 3 ticks with no user
max_silent_secs = 172800 # 48h wall-clock silence guard
min_wake_secs = 3600     # floor for on_user_message deadline (1h default)
```

#### Persisted State (v4)

State is saved to `autonomy.json` per character. Version bumped from 3→4.
Fields: `ticks_without_user` (u32), `next_wake_at` (RFC3339, optional),
`last_user_at` (RFC3339, optional). Instant recovery on restart converts
RFC3339 back to `Instant` via delta from wall clock. V3 state files are
silently discarded (fresh start).

### 13.2 Cache Invalidation Safeguard

An unexpected prompt cache invalidation means the entire prompt is re-sent
uncached — expensive on long conversations. The daemon detects and warns.

**Detection:** After every LLM response, check `cache_read_tokens` in the
usage data. If `cache_read_tokens == 0` and we expected cache hits (conversation
has >1 turn), it's an unexpected invalidation.

**Expected invalidations** (no warning):
- First message after compaction (new conversation = new cache prefix)
- First message after daemon restart (cache expired during downtime)

**On unexpected invalidation:**
1. Log as `ERROR` in structured logs (visible in `journalctl`)
2. Push `cache_warning` event to connected clients (see §3.5 Push events)
3. Include `expected_tokens` (estimated cached prompt size) and a human-readable
   `message` explaining the cost impact

**Config:**
```toml
[advanced]
cache_invalidation_warnings = true   # default: true, opt-out
```

Implementation: one check in the LLM response handler, one push event, one
config key. No state machine — just compare actual vs. expected and warn.

### 13.3 Activity Tracker

Carried forward with fixes:

**Fix:** Lower the minimum data thresholds. V1 required 20 messages across
7 days for heatmap — too high. V2 thresholds (see Constants Reference above):
adaptive timing at ≥5 msgs / ≥2 days, heatmap at ≥20 msgs / ≥7 days.

**Tracks:** per-message timestamps (monotonic + wall clock + weekday)
**Computes:** engagement score (0.6 × consistency + 0.4 × tempo), weekday-aware
hour histogram, peak/trough hours (filtered to current weekday, falls back to
global with <5 events), session tempo, z-score anomaly
**Session boundary:** 30 min gap (`SESSION_GAP`)
**Cache:** stats recomputed at most every 60s (`STATS_CACHE_TTL`)

---

## 14. Private Conversations (NEW FEATURE)

Conversations can be marked as **private**, fully isolating them from the
memory subsystem. Private conversations do not create, read, or modify memory
database entries.

### Behavior

| Aspect | Public (default) | Private |
|--------|-----------------|---------|
| Compaction | Normal | Skipped (auto and manual) |
| RAG injection | Normal | Suppressed |
| Recap | Injected | Suppressed |
| Memory write tools | Available | Excluded from tool list |
| Memory read tools | Available | Excluded from tool list |
| Other tools | Available | Available |

### Manifest Change

The conversation manifest gains a `private` boolean field:

```json
{
  "id": "conv_...",
  "created_at": "...",
  "last_activity": "...",
  "message_count": 0,
  "title": "...",
  "archived": false,
  "private": false
}
```

Missing field defaults to `false` (backwards compatible with V1 data).

### Commands

- `chat private` — toggle private flag on active conversation
- `chat list` — shows `[private]` badge on private conversations

### UI

- TUI status bar shows `[private]` indicator when active conversation is private
- Status line updates immediately on toggle

---

## 15. Crate / Package Structure

### 15.1 Repo Topology

Single monorepo. Organized by **program**, not by language — every top-level
`shore-*` directory is a buildable component. Rust components share a Cargo
workspace; non-Rust components have their own build systems.

```
shore/                              # Git root
├── shore-protocol/                 # Rust lib — shared SWP types
│   ├── src/
│   │   ├── lib.rs
│   │   ├── client_msg.rs          # Client → Server message types
│   │   ├── server_msg.rs          # Server → Client message types
│   │   ├── types.rs               # Shared types (Message, ConversationInfo, etc.)
│   │   └── error.rs               # Error codes
│   └── Cargo.toml
│
├── shore-client/                   # Rust lib — SWP client library
│   ├── src/
│   │   ├── lib.rs
│   │   ├── connection.rs          # Unix/TCP connection management
│   │   ├── discovery.rs           # Instance registry lookup
│   │   └── stream.rs              # Streaming response handler
│   └── Cargo.toml
│
├── shore-daemon/                   # Rust binary (see §4.1 for modules)
│   ├── src/
│   └── Cargo.toml
│
├── shore-daemon-server/            # Rust lib — SWP server, registry (see §4.1b)
│   ├── src/
│   └── Cargo.toml
│
├── shore-cli/                      # Rust binary — CLI
│   ├── src/
│   └── Cargo.toml
│
├── shore-tui/                      # Rust binary — TUI (ratatui)
│   ├── src/
│   └── Cargo.toml
│
├── shore-matrix/                   # Rust binary — Matrix bridge + Synapse
│   ├── src/
│   │   ├── main.rs
│   │   ├── bot.rs                 # Matrix client + handlers
│   │   ├── crypto.rs              # E2E encryption helpers
│   │   ├── provision.rs           # Character provisioning
│   │   ├── synapse.rs             # Synapse subprocess management
│   │   └── format.rs              # HTML formatting
│   └── Cargo.toml
│
├── shore-llm-client/               # Rust lib — native LLM provider integrations
│   ├── src/
│   │   └── lib.rs                 # Provider trait, implementations, streaming
│   └── Cargo.toml
│
├── Cargo.toml                      # Workspace root
├── docs/
├── contrib/                        # Live test scripts
└── examples/                       # Example config.toml, models.toml
```

Adding future components is just another top-level directory:

```
├── shore-gui/                      # Future: Tauri, Electron, etc.
├── shore-telegram/                 # Future: Rust binary
├── shore-discord/                  # Future: Rust binary
├── shore-plugins/                  # Future: Python plugin host
```

**Cargo workspace** (`Cargo.toml` at root):

```toml
[workspace]
members = [
    "shore-protocol",
    "shore-client",
    "shore-config",
    "shore-llm-client",
    "shore-daemon",
    "shore-daemon-server",
    "shore-cli",
    "shore-tui",
    "shore-matrix",
]
resolver = "2"
```

### 15.2 Why Monorepo

Protocol changes touch multiple components. In a monorepo, updating the
protocol, daemon, and clients is a single atomic commit. No cross-repo PRs,
no version pinning, no "which repo has the bug?" The SWP protocol crate is a
workspace dependency — all Rust binaries always build against the same
protocol version.

Non-Rust clients (future GUI, plugins) implement the protocol from the spec
in `docs/` — they don't depend on the Rust crate.

### 15.3 Dependency Graph

```
Rust (compile-time):
  shore-protocol    ← shore-client  ← shore-cli
                                     ← shore-tui
                                     ← shore-matrix
  shore-llm-client  ← shore-daemon
  shore-config      ← shore-daemon
  shore-protocol    ← shore-daemon
  shore-config      ← shore-daemon-server
  shore-protocol    ← shore-daemon-server
  shore-daemon-server ← shore-daemon

Runtime:
  shore-daemon ──HTTPS──▶ LLM APIs (Anthropic, OpenAI, Gemini, etc.)
```

`cargo build --release` produces four Rust binaries: `shore-daemon`, `shore`
(CLI), `shore-tui`, `shore-matrix`.

---

## 16. What Changes vs. V1

| Aspect | V1 | V2 |
|--------|-----|-----|
| Language | Python (everything) | Rust (everything) |
| LLM providers | Python SDKs in daemon process | Native Rust implementations in `shore-llm-client` crate |
| Daemon binary | `uv run shore serve` | `shore-daemon` |
| CLI binary | Same as daemon | `shore` (separate binary) |
| Bridge architecture | In-process plugins | Out-of-process via SWP |
| Wire protocol | Ad-hoc JSON-lines | Formalized SWP with version, handshake, 18 flat commands |
| Commands | Nested groups (`chat list`, `model switch`, ...) | Flat `verb_noun` names (`list_chats`, `switch_model`, ...) |
| Server responses | Streaming + non-streaming paths | Always-stream (one code path for clients) |
| State sync | Clients parse command output | Server pushes `history` on any state change |
| Heartbeat | 3-mode (session/inter-session/deferred) | Interiority system (timer + agentic turns) |
| Session idle | Measured from last user message | `max(last_user, last_assistant)` + 3-min floor |
| Compaction trigger | Reactive (on next user message) | Proactive background timer |
| Interiority | Journal + story generation | **Redesigned** — autonomous turns with full tool access |
| Private conversations | Not supported | Full memory isolation |
| Relay server | Separate process | Eliminated — native TCP on daemon |
| Chat platforms | Telegram, Discord, Matrix (all in-process) | Matrix only at launch (out-of-process) |
| Template upgrades | Manual | Manifest-tracked (auto-update stock, preserve user edits) |
| Vector store | LanceDB Python SDK | LanceDB Rust SDK (native) |
| E2E encryption | matrix-nio + libolm | matrix-sdk native crypto |
| Inline images | Not supported | kitty/iTerm2 protocol rendering |
| Activity heatmap | Global histogram only | Weekday-aware, per-day filtering |
| Memory agent | Confused by first-person pronouns | Caller identity awareness |

---

## 17. Migration Strategy

Since this is a ground-up rewrite (not an incremental port), the strategy is
to build V2 in parallel while V1 continues to run. Data files (SQLite,
JSONL, config) are compatible — V2 reads V1 data.

### Phase 1: Foundation
- [ ] Create Cargo workspace
- [ ] `shore-protocol` crate: all SWP message types, shared types, error codes
- [ ] `shore-client` crate: connection management, instance discovery, streaming
- [ ] Integration tests: validate SWP JSON matches V1 protocol

### Phase 2: LLM Providers + Daemon Core
- [ ] `shore-llm-client`: native Rust provider implementations (Anthropic, Gemini, OpenAI-compat, OpenRouter, ZhipuAI)
- [ ] Streaming via reqwest + SSE parsing
- [ ] `shore-daemon`: server (accept, route, broadcast), config loading
- [ ] Engine: message lifecycle, prompt assembly, persistence
- [ ] `llm_client` module: uses shore-llm-client crate
- [ ] Tool use loop (daemon calls LLM → gets tool_use → executes tool → repeats)
- [ ] Command dispatch (basic set: send, regen, log, status, tokens, model)
- [ ] **Milestone: daemon can hold a basic conversation**

### Phase 3: Memory & RAG
- [ ] SQLite layer (rusqlite, full schema)
- [ ] LanceDB integration (Rust native)
- [ ] BM25 implementation
- [ ] RAG pipeline (vector + BM25 + lifecycle scoring)
- [ ] Compaction (with private conversation awareness)
- [ ] Collation (4 phases)
- [ ] Memory agent (with caller identity fix)
- [ ] Remaining commands (memory, chat, compact, etc.)
- [ ] **Milestone: full memory system working**

### Phase 4: Autonomy
- [ ] Activity tracker (with lowered threshold)
- [x] Interiority system (replaces heartbeat)
- [ ] Cache keepalive
- [ ] Autonomy commands (pause, resume, status)
- [ ] **Milestone: character reaches out autonomously**

### Phase 5: CLI/TUI
- [ ] `shore` binary: CLI subcommands (stateless request-response)
- [ ] TUI mode (ratatui, persistent connection)
- [ ] Shell completions, editor, log viewport
- [ ] Private conversation UI (toggle, status badge, chat list badge)
- [ ] **Milestone: full replacement for V1 CLI**

### Phase 6: Matrix Bridge
- [ ] `shore-matrix`: matrix-sdk client, E2E crypto
- [ ] Command handling, image buffering, typing indicators
- [ ] Synapse management (subprocess, provisioning)
- [ ] **Milestone: characters live on Matrix**

### Phase 7: Polish & Retire V1
- [ ] Data migration validation (V1 SQLite/JSONL → V2 seamless)
- [ ] Config migration guide (V1 config.toml → V2)
- [ ] Remove Python codebase
- [ ] **Milestone: V1 retired, single `cargo build --release`**

**Each phase produces a testable artifact.**

---

## 18. Future Features (Not in V2 Scope)

These are noted for architectural awareness — V2 should not block them, but
does not implement them.

### Group Chats (characters messaging each other)
Characters can choose to message another character during interiority ticks.
Messages work like an inbox/outbox — the response happens during the
*recipient's* interiority tick. User can observe and participate in group chats.
**Architectural impact:** daemon needs inter-character message routing. The SWP
protocol doesn't need changes (this is all daemon-internal).

### Beets Integration (music library)
Character can query the user's music library (what they've listened to, ratings,
play counts). Likely implemented as a new tool.
**Architectural impact:** new tool in engine/tools.rs, new config section,
beets library query via subprocess or API.

### Video Input (Gemini)
Periodic clip capture from OBS, sent to Gemini standard API for character
reactions. Deferred until audio+video becomes practical across providers.
**Architectural impact:** new tool or interface, Gemini-specific.

### Telegram Bridge
Standard bridge pattern. `shore-telegram` binary using teloxide.

### Discord Bridge
Standard bridge pattern. `shore-discord` binary using serenity + poise.

---

## 19. Resolved Decisions

1. **Synapse management** — Fully integrated into `shore-matrix`. No separate
   helper script.

2. **Prompt templates** — Loaded from filesystem at a well-known path. Not
   compiled into the binary. Makes them easier to review and edit.

3. **Config hot-reload** — Low priority. Runtime overrides (via commands) are
   supported, but persisting changes requires editing the config file. V1
   already has hot-reload, so this is a known-good pattern to carry forward
   eventually.

4. **Social need curve tuning** — Deferred to implementation. Tuning knob, not
   architectural.

5. **Activity tracker threshold** — The real bug is that V1 fails to detect
   sufficient data even when the user has far exceeded the threshold. Root
   cause TBD during implementation — likely a data accounting bug, not a
   threshold problem.

## 20. Async Generation Architecture (2026-03-31)

### Handler Concurrency Model

Previously, `MessageHandler::run()` processed all messages sequentially from a single
`RoutedMessage` channel. Both Commands and Engine messages (Message/Regen) shared this
channel, so a long LLM stream would block `shore status` and other commands.

**Current model:**

- **Commands** (`shore status`, `shore log`, etc.) are processed inline by the handler
  loop — they never do LLM I/O and return in microseconds.

- **Engine messages** (Message/Regen) are spawned as independent `tokio` tasks via
  `tokio::spawn`. The handler loop returns immediately after spawning and can process
  the next message (usually a command).

- A `GenContext` struct (Clone-able, Arc-backed) passes shared state to generation
  tasks: registry, llm_client, push_tx, autonomy, session_tokens, diagnostics, and
  the `is_first_after_restart` / `has_seen_cache_read` atomic flags.

- `CharacterRegistry.engines` stores `Arc<tokio::sync::Mutex<ConversationEngine>>`.
  The registry lock is held only briefly (to look up or create an engine Arc); the
  engine lock is independent and only held for brief mutations (message append/delete).
  It is never held during LLM streaming.

### Lock Ordering

To prevent deadlocks:
1. Never hold registry lock while waiting for engine lock.
2. Never hold engine lock across an `await` point in generation tasks.
   (The engine lock IS held across awaits in the `dispatch` command handler, which
   uses `tokio::sync::Mutex` for correctness. This is intentional — commands are
   sequential and user-initiated.)

### Concurrency Guarantees

- `shore status` always responds immediately, even during active generation.
- Multiple characters can generate in parallel (separate engine locks).
- Session token counts are updated atomically via `Arc<std::sync::Mutex<SessionTokens>>`.
- Per-character serialization of mutations (append/delete/edit) is enforced by the
  engine's tokio Mutex — generating and editing the same character's history at the
  same time will serialize, not corrupt.

---

## 10. Token Usage Ledger (`shore-ledger`)

A dedicated crate for persistent LLM usage tracking, cost calculation, and cache health monitoring.

### Architecture

```
shore-daemon ──▶ shore-ledger ──▶ shore-llm-client
shore-cli    ──▶ shore-ledger (query only)
```

### Components

- **LedgerClient** — wraps `LlmClient`, consumes it so the raw client is inaccessible.
  Every `generate()` and `stream_raw()` call automatically records to the ledger.
- **LedgerStream** — wraps the streaming reader. Must be `finalize()`d after consumption;
  Drop impl warns if finalization was skipped.
- **Ledger** — SQLite database at `$XDG_DATA_HOME/shore/ledger.db` with `calls` and `pricing` tables.
- **CacheTracker** — per-character state machine (Cold/Warm) for Anthropic prompt cache.
  Detects anomalies: unexpected reads (cold but got cache hit) and unexpected writes
  (warm but cache_read decreased). Anomalies fire `tracing::error!` and notifications.
- **PricingEngine** — fetches per-model pricing from OpenRouter's API, caches in DB.
  Applies 4x multiplier for Anthropic 1h cache TTL writes.
- **Query module** — aggregation, filtering, and TSV/CSV export for the CLI.

### Data Flow

1. Daemon constructs `LedgerClient::new(llm_client, db_path)` at startup
2. Every LLM call goes through `LedgerClient` with `CallType` + character name
3. On response: usage recorded to SQLite, cache tracker updated, cost calculated
4. CLI queries the DB directly via `shore usage` (no daemon connection needed)

---

## Test Architecture

### shore-test-harness

Dev-only crate providing integration test infrastructure. Not published.

- **MockLlmServer** — wraps `wiremock::MockServer`, serves canned Anthropic SSE streams
- **TestHarness** — boots real daemon stack in-process, connects SWP client, provides send/collect helpers
- **CrashedHarness** — simulates crash/reboot for recovery testing
- **TestConfigBuilder** — builds `LoadedConfig` pointing at mock server

Integration tests in `shore-daemon/tests/integration_*.rs` use the harness.

**Data flow in tests:**
```
SWPConnection → Server → MessageHandler → LlmClient → reqwest → MockServer (wiremock)
```

All components are real except the HTTP responses from the LLM provider.
