# Shore Wire Protocol (SWP)

Reference for implementing a Shore client. Covers transport, framing, the
handshake, every client→server frame, every server→client frame, and every
command currently dispatched by `shore-daemon`. Authoritative source: the
`shore-protocol` crate (`core/protocol/src/`) and the daemon command
dispatcher (`backend/daemon/src/commands/mod.rs`).

## 1. Transport

- **Type:** plain TCP. No TLS, no authentication.
- **Default listen address:** `127.0.0.1:7320` (`[daemon].addr` in
  `config.toml`).
- **Peer ACL:** the daemon may carry a `[daemon].allowed_hosts` IP allowlist.
  An empty list means allow all peers. This is not auth — it is a coarse
  drop-the-connection check on `peer.ip()` after `accept()`.
- **Non-loopback policy:** when the daemon binds to anything other than
  `127.0.0.1` / `::1` / `localhost`, startup requires an explicit
  acknowledgement (`[daemon].unsafe_allow_remote_access = true`) and ideally an
  allowlist. Clients should not describe shore as authenticated or
  encrypted.

### Daemon discovery

A running daemon writes itself into
`$SHORE_RUNTIME_DIR/shore/instances.json` (falling back to
`$XDG_RUNTIME_DIR/shore/instances.json`, then platform defaults). The file
is a JSON array; each element has the shape:

```json
{
  "id": "default",
  "addr": "127.0.0.1:7320",
  "pid": 12345,
  "data_dir": "/home/user/.local/share/shore",
  "config_dir": "/home/user/.config/shore"
}
```

Clients are expected to:

1. Honour a user-supplied address (CLI flag, `SHORE_ADDR`, or
   `client.toml`'s `default_address`) if present.
2. Otherwise read `instances.json`, prune entries whose `pid` is no longer
   alive, and pick the first match (optionally filtered by `id` or
   `config_dir`).
3. Fall back to `127.0.0.1:7320` if no registry exists.

A corrupt `instances.json` must surface an error rather than silently
defaulting.

## 2. Framing

- **Newline-delimited JSON.** Each frame is one UTF-8 JSON object followed
  by a single `\n`. No length prefix, no trailing whitespace required.
- **Max frame size:** `MAX_WIRE_MESSAGE_SIZE = 128 * 1024 * 1024` bytes
  (128 MiB). The daemon disconnects clients that exceed this; clients
  should reject server frames that exceed it (the bundled
  `swp-client` raises `ClientError::Protocol("...exceeds maximum size...")`).
  Sizing accommodates inline base64 images.
- **Encoding:** JSON values use `serde_json` round-tripping. Fields marked
  `skip_serializing_if = "Option::is_none"` are omitted when null; defaulted
  fields are tolerated on read (`#[serde(default)]`).
- **Tagging:** both directions use an externally tagged enum with the
  `"type"` discriminator (snake_case variant names). Clients **must skip** an
  unrecognized server→client `"type"` rather than erroring the connection — see
  the forward-compatibility rule in §3. (A server may still reject an unknown
  client→server `"type"` it cannot act on.)

## 3. Protocol version

```text
SWP_V1 = 1
```

The version is currently advertised by the server in `ServerHello.v`. The
bundled client refuses to proceed when `v != 1`. There is no negotiation
step — clients pin to `1`.

The wire version stays at `1` across **additive** changes: new optional
fields on existing frames (defaulted, so older clients ignore them) and new
server→client frame `type`s. Because the version does not bump for these,
**a client must tolerate an unrecognized `type` by skipping that frame**, not
by erroring the connection. A client that hard-matches every known `type` and
treats anything else as a fatal error will break the moment a newer daemon
emits a frame it predates (e.g. a newly added warning). The bundled client
deserializes any unknown `type` into a benign `ServerMessage::Unknown`
sentinel that its read loop ignores; client authors in other languages should
do the equivalent. The version only bumps for a **breaking** change (a removed
or restructured frame older clients cannot safely ignore).

## 4. Connection lifecycle

```
client                                    daemon
  │   tcp connect ──────────────────────▶  │
  │   ◀────────────────────  ServerHello   │   (step 1)
  │   ClientHello   ─────────────────────▶ │   (step 2)
  │   ◀─────────────────────  History      │   (step 3, rid = None)
  │                                        │
  │     ── steady state ──                 │
  │   Message/Regen/Command/Cancel  ────▶  │
  │   ◀──── StreamStart/Chunk/End …        │
  │   ◀──── CommandOutput / Error          │
  │   ◀──── NewMessage / History / push    │
  │   ◀──── Ping (every 30s)               │
  │                                        │
  │     ── shutdown ──                     │
  │   ◀──── Shutdown                       │
  │   tcp close                            │
```

Notes:

- The server speaks first. A client that sends `ClientHello` before
  receiving `ServerHello` is fine — frames cross on the wire — but it must
  still consume the server hello.
- A second `ClientHello` after handshake yields `Error{ code: protocol_error,
  message: "Duplicate hello" }`.
- The server sends `Ping` roughly every 30 seconds. Clients should treat
  it as a keepalive; no response is required.
- `Shutdown` is broadcast immediately before the listener closes; clients
  should treat it as an EOF signal.
- When *all* clients disconnect, the daemon cancels any in-flight
  generation for those sessions.

## 5. Request IDs (`rid`)

- All request-shaped client frames carry an optional `rid` string (the
  bundled client uses `rid_<nanos>_<seq>`; UUIDs are also fine — the daemon
  never inspects the value).
- The daemon echoes the originating `rid` on request-scoped responses
  (`StreamStart`, `StreamChunk`, `StreamEnd`, `Phase`, `ToolCall`,
  `ToolResult`, `SendImage`, `CommandOutput`, `Error`, `History` returned
  from a switch_character, `ProviderFallbackWarning`, `UsageWarning`).
- Pure push frames (`Hello`, `Shutdown`, `Ping`, `NewMessage`,
  unsolicited `History`, `CacheWarning`) have no `rid`.
- A single `Message` or `Regen` may produce multiple `StreamEnd` frames if
  the daemon ran a tool loop; only the frame with `is_final = true`
  closes the request. Aggregating clients must keep reading until they
  see it.

## 6. Client → Server frames

All variants share `"type": "<snake_case>"`.

### 6.1 `hello`

```json
{
  "type": "hello",
  "client_type": "tui",
  "client_name": "shore-tui",
  "capabilities": ["streaming"],
  "character": "Alice"
}
```

| Field | Type | Notes |
|---|---|---|
| `client_type` | string | Free-form identifier, e.g. `"cli"`, `"tui"`, `"gui"`, `"matrix"`, `"mcp"`. Used only for logging. |
| `client_name` | string | Human-readable instance name. |
| `capabilities` | string[] | Optional. Currently informational — the daemon does not gate features on this list. `"streaming"` is conventional. |
| `character` | string \| null | Optional. Connect-time character selection. If omitted and only one character exists, the daemon selects it automatically; otherwise the session is "characterless" until a `switch_character` command runs. Unknown names are ignored with a warning. |

### 6.2 `message`

```json
{
  "type": "message",
  "rid": "rid_…",
  "text": "Hello, Alice.",
  "stream": true,
  "image_data": [
    { "filename": "snap.png", "data": "<base64>" }
  ],
  "images": [],
  "absence_seconds": 86400,
  "overrides": { "temperature": 0.7, "thinking_budget": 2048 }
}
```

| Field | Type | Notes |
|---|---|---|
| `rid` | string \| null | Optional. Recommended for matching responses. |
| `text` | string | User message body. |
| `stream` | bool | When `true`, the daemon emits `StreamStart`/`StreamChunk`/`StreamEnd`. When `false` (or omitted), the daemon still emits a `NewMessage` push when the assistant reply lands; see §7. |
| `images` | string[] | Legacy: filesystem paths visible to the daemon. Avoid unless daemon and client share a filesystem. |
| `image_data` | `ImageUpload[]` | Preferred. Base64 image bytes, each `{filename, data}`. |
| `absence_seconds` | u64 \| null | Optional. Time since last interaction; the daemon may inject framing. |
| `overrides` | `MessageOverrides` \| null | One-shot sampler overrides: `temperature` (f64), `top_p` (f64), `thinking_budget` (u32). Any omitted field uses the model default. |

### 6.3 `regen`

```json
{ "type": "regen", "rid": "rid_…", "stream": true, "guidance": "shorter" }
```

| Field | Type | Notes |
|---|---|---|
| `rid` | string \| null | Optional. |
| `stream` | bool | Same semantics as `message.stream`. |
| `guidance` | string \| null | Optional regen hint passed through to the engine. |

Regenerating the last assistant reply is non-destructive — the previous
response is preserved as an alternative on the same `msg_id` (see the
`alt` and `list_alternatives` commands).

### 6.4 `command`

```json
{
  "type": "command",
  "rid": "cmd_…",
  "name": "status",
  "args": { }
}
```

| Field | Type | Notes |
|---|---|---|
| `rid` | string \| null | Optional. |
| `name` | string | Command name. See §8 for the full list. |
| `args` | object | Free-form JSON; per-command schema in §8. May be `{}` or omitted. |

The response is either `CommandOutput` (success) or `Error` (failure), both
echoing `rid`.

### 6.5 `cancel`

```json
{ "type": "cancel" }
```

Cancels any in-flight generation owned by *this* session. Carries no
fields. The daemon also auto-cancels when every connected client
disconnects.

## 7. Server → Client frames

### 7.1 `hello` (handshake step 1)

```json
{
  "type": "hello",
  "v": 1,
  "server_name": "shore-daemon",
  "characters": [
    { "name": "Alice", "avatar": { "mime_type": "image/png", "data": "<b64>" } },
    { "name": "Bob"  }
  ]
}
```

- `v` is `1`.
- `characters[].avatar` is `null` when the character has no
  `avatar.{png,jpg,jpeg,webp}` file. The daemon embeds these so notification
  clients on remote machines still get icons.

### 7.2 `history`

Sent unsolicited after handshake, after a successful `switch_character`,
and as a broadcast whenever the active conversation mutates from outside
the session (edit/delete, compaction, autonomy messages, etc.).

```json
{
  "type": "history",
  "rid": null,
  "messages": [ /* Message[] */ ],
  "active_start": 0,
  "config": { "active_model": "claude-sonnet" },
  "selected_character": "Alice",
  "revision": 42
}
```

| Field | Type | Notes |
|---|---|---|
| `rid` | string \| null | Set only when the snapshot is the response to a `switch_character` command; otherwise omitted. |
| `messages` | `Message[]` | See §9. |
| `active_start` | usize | Index of the first message still in the model's active context. Zero on push snapshots (active-only); paged history responses may include archive scrollback below this index. |
| `config` | object | At minimum `active_model` (string \| null). Treat as opaque additive metadata. (A legacy `private` bool, always `false`, has been removed; tolerate its absence.) |
| `selected_character` | string \| null | Character the snapshot belongs to. |
| `revision` | u64 | Monotonic per-character revision counter. Use to detect stale snapshots. |

### 7.3 `new_message`

Push: an assistant or autonomous message has been appended.

```json
{
  "type": "new_message",
  "revision": 43,
  "character": "Alice",
  "origin": "autonomous",
  "msg_id": "m_…",
  "role": "assistant",
  "content": "…",
  "images": [],
  "content_blocks": [ … ],
  "timestamp": "2026-05-20T12:00:00Z"
}
```

`origin` is one of `"user_input"`, `"assistant_reply"`, `"autonomous"` (or
omitted on legacy daemons). The `Message` body is flattened into the
envelope; `origin` is carried by the flattened `Message` (§9.1), which puts
it at the same top-level position the envelope-level field historically
occupied — the wire shape is unchanged.

### 7.4 Stream frames

A non-empty generation emits the sequence:

```
stream_start
[ phase ]*
( stream_chunk | tool_call | tool_result | phase )*
stream_end (is_final = false)        ← if the daemon ran a tool loop
( stream_chunk | tool_call | tool_result )*
stream_end (is_final = true)
```

When the model calls a sub-agent (`ask_<name>`; see §7.4.5), the sub-agent's
own nested tool-loop frames are interleaved between the `tool_call` that
invokes it and the matching `tool_result`, each carrying a `subagent` tag:

```
tool_call (tool_name = "ask_research")              ← no subagent tag
  stream_start | stream_chunk | tool_call |
  tool_result | stream_end(is_final=false)          ← all tagged subagent="research"
tool_result (tool_name = "ask_research")            ← no subagent tag; output is the summary
```

#### 7.4.1 `stream_start`

```json
{ "type": "stream_start", "rid": "rid_…", "regen": false }
```

#### 7.4.2 `stream_chunk`

```json
{ "type": "stream_chunk", "rid": "rid_…", "text": "partial ", "content_type": "text" }
```

`content_type` is one of `"text"` or `"thinking"`. Older daemons may emit
no `content_type` field — treat missing as `"text"`.

#### 7.4.3 `phase`

```json
{ "type": "phase", "rid": "rid_…", "phase": "tool_use", "model": "claude-sonnet-4-6" }
```

`phase` is one of `"thinking"`, `"text_generation"`, `"tool_use"`. Useful
for UI affordances. `model` may be `null`.

#### 7.4.4 `stream_end`

```json
{
  "type": "stream_end",
  "rid": "rid_…",
  "msg_id": "m_…",
  "revision": 43,
  "content": "Full assembled assistant content for this turn",
  "metadata": {
    "tokens": { "input": 1234, "output": 567, "cache_read": 890, "cache_write": 0 },
    "timing": { "total_ms": 2340, "ttft_ms": 450 },
    "model": "claude-sonnet-4-6"
  },
  "finish_reason": "end_turn",
  "is_final": true
}
```

- `msg_id` and `revision` are present only on the **terminal** stream end
  (after persistence). Intermediate tool-loop ends omit them.
- `finish_reason` mirrors the upstream model's stop reason
  (`"end_turn"`, `"tool_use"`, `"max_tokens"`, …).
- `is_final` is the boundary marker. Missing field defaults to `true`
  (compatibility with pre-tool-loop daemons).

#### 7.4.5 Sub-agent frames (`subagent`)

`stream_start`, `stream_chunk`, `stream_end`, `tool_call`, `tool_result`, and
`send_image` carry an optional `subagent` field. When present, the frame
belongs to the nested tool loop of the named sub-agent (`[subagents.<name>]`,
invoked as `ask_<name>`), not to the primary model:

```json
{ "type": "stream_chunk", "rid": "rid_…", "text": "searching…", "content_type": "thinking", "subagent": "research" }
```

- The field is **omitted** for primary-model frames (so primary traffic and the
  cache prefix are unchanged, and pre-field clients parse as before).
- A sub-agent never emits a terminal `stream_end` (`is_final = true`), so a
  tagged `stream_end` never ends the primary generation.
- Because the primary loop is blocked awaiting the sub-agent and nesting is
  capped at one level, tagged frames never interleave with primary frames or
  with another sub-agent. Clients can bracket a nested section by watching the
  tag turn on and back off.
- Rendering is advisory: a client may ignore the field and show a flat stream,
  attribute it (e.g. nested/indented), or suppress sub-agent frames entirely.

### 7.5 `tool_call`

```json
{
  "type": "tool_call",
  "rid": "rid_…",
  "tool_id": "toolu_01",
  "tool_name": "memory_search",
  "input": { "query": "sister" }
}
```

Emitted between stream ends in a tool loop. `input` is always a JSON
object, not a stringified one.

### 7.6 `tool_result`

```json
{
  "type": "tool_result",
  "rid": "rid_…",
  "tool_id": "toolu_01",
  "tool_name": "memory_search",
  "output": "Your sister's name is Maya.",
  "is_error": false
}
```

### 7.7 `send_image`

```json
{
  "type": "send_image",
  "rid": "rid_…",
  "path": "/tmp/img.png",
  "caption": "generated chart",
  "data": "<base64>"
}
```

Sent when the character emits an image (image-generation tools, etc.).
`data` is the base64 payload for clients that can't read the daemon's
filesystem; legacy clients can still rely on `path`.

### 7.8 `command_output`

```json
{ "type": "command_output", "rid": "cmd_…", "name": "status", "data": { … } }
```

The successful response to a `command` request. Per-command `data` shape
is described in §8.

### 7.9 `error`

```json
{ "type": "error", "rid": "rid_…", "code": "busy", "message": "engine busy" }
```

`code` is one of the `ErrorCode` variants:

| `code` | Meaning |
|---|---|
| `protocol_error` | Frame ordering or shape violated the protocol. |
| `invalid_request` | The frame parsed but its arguments are wrong. |
| `not_found` | Named resource (message, character, model, provider…) does not exist. |
| `busy` | Engine is already running an operation that excludes this one (e.g. compaction in progress). |
| `provider_error` | Upstream LLM/provider failure surfaced to the caller. |
| `timeout` | Operation timed out. |
| `internal_error` | Catch-all server failure. |

Errors associated with a specific request carry the originating `rid`;
synchronous send-time errors (deserialization, oversized frame) may have
`rid = null`.

### 7.10 `cache_warning`

```json
{ "type": "cache_warning", "expected_tokens": 5000, "message": "cache miss" }
```

Broadcast when the Anthropic prompt cache invalidates unexpectedly.
Treat as a soft warning.

### 7.11 `provider_fallback_warning`

```json
{
  "type": "provider_fallback_warning",
  "rid": "rid_…",
  "provider": "openrouter",
  "from_key": "primary",
  "to_key": "secondary",
  "kind": "exhausted_quota",
  "status": 429,
  "message": "Primary key exhausted quota; rotated to secondary."
}
```

Emitted mid-request when the daemon rotates from one configured provider
key to another. The frame intentionally never contains the env-var name
or the key value. Only keys with `warn_on_fallback = true` raise this.

### 7.12 `usage_warning`

```json
{
  "type": "usage_warning",
  "rid": "rid_…",
  "budget": "daily total",
  "message": "Usage budget \"daily total\" reached 80% ($8.00/$10.00).",
  "current_cost": 8.0,
  "cost_limit": 10.0,
  "percent_used": 0.8,
  "crossed_warn_at": [0.8],
  "period": "day",
  "period_start": "2026-05-20T00:00:00Z",
  "reset_at": "2026-05-21T00:00:00Z"
}
```

Emitted when a configured usage budget crosses any new warn threshold.
Re-fires once per generation while still over budget so dismissed
warnings come back.

### 7.13 `ping`

```json
{ "type": "ping" }
```

Keepalive, every 30 s. Discard.

### 7.14 `shutdown`

```json
{ "type": "shutdown" }
```

Sent once when the daemon begins shutting down. The TCP connection
closes immediately afterward.

## 8. Commands

Send via the `command` client frame. The success response is a
`command_output` whose `data` field is described per command below;
failures land as `error` with the matching `rid`.

Some commands require a character context (`switch_character` and
character-scoped ops); a handful are "characterless" and can run before a
character is bound (`list_characters`, `list_models`, `list_providers`,
`list_provider_models`). The daemon enforces this distinction.

### 8.1 Navigation

#### `list_characters` *(characterless)*
- **args:** none
- **data:** `{ "characters": [ { "name": "Alice", "avatar": {…} | null }, … ] }`

#### `switch_character`
- **args:** `{ "name": "Alice" }`
- **data:** `{ "character": "Alice", "changed": true }`. `changed = false`
  when the requested name is already active. Following a successful
  switch, the daemon pushes a fresh `History` snapshot whose `rid` equals
  the command's `rid`.
- **errors:** `not_found` when the character directory is missing.

#### `character_info`
- **args:** `{ "name"?: "Alice" }` — defaults to the active character.
- **data:**
  ```json
  {
    "name": "Alice",
    "active": true,
    "config_dir": "…",
    "workspace_dir": "…",
    "has_definition": true,
    "definition_preview": "first 500 chars of SOUL.md",
    "bootstrap_files": ["SOUL.md", "USER.md", "AGENTS.md", "TOOLS.md", "HEARTBEAT.md"],
    "has_config_override": false,
    "pending_deferred_edits": [],
    "data_dir": "…",
    "has_data": true
  }
  ```

### 8.2 Conversation

Most conversation commands accept a `ref` (or `refs`) argument. A ref
resolves against the client-visible (tool-loop-merged) message list and
may be:

- `"last"` / `"latest"` — newest message
- A 1-based positive index (`"1"`, `"2"`, …) or negative index (`"-1"`, …)
- A literal `msg_id`

When `get` includes a `role` filter, its `ref` resolves against only messages
with that role.

#### `log`
- **args:** `{ "turns"?: u64, "count"?: u64, "role"?: "user" | "assistant" | "system" }`
  — default `turns = 64`. `turns` counts user turns; `count` counts raw
  messages. `role` filters the bounded page returned by those limits.
- **data:** see `history_page_payload` shape below.

#### `history_page`
- **args:** `{ "before"?: "active" | u64, "turns"?: u64, "count"?: u64, "role"?: "user" | "assistant" | "system" }`
  — `"active"` clamps `before` to the current `active_start`; numeric values
  are cursor positions returned from prior pages. `role` filters the bounded
  page returned by those limits.
- **data:**
  ```json
  {
    "messages": [ /* Message[] */ ],
    "active_start": 0,
    "cursor": 0,
    "next_before": 0,
    "has_more_before": false,
    "global_active_start": 12,
    "total_messages": 64,
    "total_turns": 64
  }
  ```

#### `get`
- **args:** `{ "ref": "<ref>", "role"?: "user" | "assistant" | "system" }`
- **data:** a single `Message` (see §9).

#### `edit`
- **args:** `{ "ref": "<ref>", "content": "…" }`
- **data:** `{ "ref": "<resolved msg_id>", "edited": true }`
- Side effect: daemon broadcasts a `history` push.

#### `delete`
- **args:** `{ "refs": "<ref>" | ["<ref>", …] }`
- **data:** `{ "deleted": ["m_…", …] }`
- Side effect: daemon broadcasts a `history` push.

#### `list_alternatives`
- **args:** `{ "ref"?: "<ref>" }` — defaults to the latest assistant message.
- **data:**
  ```json
  {
    "ref": "m_…",
    "alt_index": 1,
    "position": 2,
    "alt_count": 2,
    "alternatives": [
      { "index": 0, "position": 1, "active": false, "content": "…", "images": [], "timestamp": "…" },
      { "index": 1, "position": 2, "active": true,  "content": "…", "images": [], "timestamp": "…" }
    ]
  }
  ```

#### `alt`
- **args:** one of
  - `{ "ref"?: "<ref>", "direction": "prev" | "next" | "first" | "last" }` (default direction `"next"`)
  - `{ "ref"?: "<ref>", "position": u64 }` (1-based)
  - `{ "ref"?: "<ref>", "index": u64 }` (0-based)
- **data:** `{ "ref": "m_…", "alt_index": 0, "position": 1, "alt_count": 2, "content": "…" }`

#### `inject_system`
- **args:** `{ "text": "…" }`
- **data:** `{ "injected": true }`. Appends a `role: "system"` message to
  the conversation — useful for mid-conversation behaviour correction.

### 8.3 State

#### `status`
- **args:** none
- **data:**
  ```json
  {
    "character": "Alice",
    "message_count": 84,
    "turn_count": 84,
    "active_model": "claude-sonnet",
    "config_dir": "…", "data_dir": "…", "cache_dir": "…",
    "memory_mode": "markdown",
    "pending_deferred_edit_count": 0,
    "pending_deferred_edits": [],
    "tokens": { "input": 0, "output": 0, "cache_read": 0, "cache_write": 0 },
    "autonomy": { … },
    "activity": null
  }
  ```

#### `list_models` *(characterless OK)*
- **args:** `{ "include_hidden"?: bool }` — default `false`.
- **data:**
  ```json
  {
    "models": [
      { "name": "claude-sonnet", "qualified_name": "anthropic/claude-sonnet",
        "sdk": "anthropic", "provider": "anthropic", "model_id": "claude-sonnet-4-6",
        "source": "static" | "discovered", "hidden": false }
    ],
    "active": "anthropic/claude-sonnet",
    "include_hidden": false,
    "hidden_count": 0
  }
  ```

#### `model_info`
- **args:** `{ "name"?: "<model>" }` — defaults to the active model.
- **data:** the `ResolvedModel` JSON, augmented with `effective_sampler`
  and `scopes` (showing which preference layer set each sampler field —
  `static_default`, `global_default`, `character_default`,
  `global_model`, `character_model`).

#### `switch_model`
- **args:** `{ "name"?: "<model>", "include_hidden"?: bool }` — without
  `name`, returns the current active. With `name`, persists the choice to
  the character's preferences file.
- **data:**
  ```json
  {
    "active": "<input name>",
    "qualified_name": "anthropic/claude-sonnet",
    "provider": "anthropic",
    "model_id": "claude-sonnet-4-6",
    "changed": true
  }
  ```

#### `reset_model`
- **args:** none
- **data:** `{ "previous": "…", "previous_provider": "…", "previous_model_id": "…", "active": null, "reset_to": "config default" }`

#### `background_models` *(characterless OK)*
- **args:** none.
- **data:**
  ```json
  {
    "background": [
      { "task": "heartbeat", "model": "anthropic/claude-sonnet",
        "source": "inherited: active chat model" },
      { "task": "compaction", "model": "deepseek/deepseek-v4-pro",
        "source": "config: background.compaction" },
      { "task": "dreaming", "model": "deepseek/deepseek-v4-pro",
        "source": "config: background.model" }
    ]
  }
  ```
- Read-only view of which model each background task resolves to (per the
  `[defaults.background]` config section) and where the selection comes from:
  a per-task pin (`config: background.<task>`), the blanket pin
  (`config: background.model`), or `inherited: active chat model` when no
  background model is configured.

#### `set_model_setting`
- **args:** `{ "key": "<key>", "value": <number|string|bool|null>, "scope"?: "character" | "global", "background_task"?: "all" | "heartbeat" | "compaction" | "dreaming" }`
- **valid keys:** `temperature`, `top_p`, `reasoning_effort`,
  `budget_tokens`, `max_output_tokens`, `cache_ttl`, `sdk`,
  `replay_prior_thinking` (plus the vendor knobs `openrouter_provider`,
  `vertex_project`, `vertex_location`, `gemini_generation`,
  `gemini_web_search`, `zai_clear_thinking`, `zai_subscription`).
- **data:** `{ "changed": true, "scope": "character", "model": "…", "provider": "…", "model_id": "…", "key": "…", "value": … }`
- `value: null` clears the setting.
- `background_task` retargets the write at the model backing that background
  task instead of the active chat model (settings are keyed by
  `provider:model_id`, so no model switch is needed). `"all"` targets every
  background task at once and is rejected if they resolve to different models.

#### `model_settings`
- **args:** `{ "name"?: "<model>", "background_task"?: "all" | "heartbeat" | "compaction" | "dreaming" }` — defaults to the active model.
- **data:** `{ model, provider, model_id, effective_sampler, saved_global, saved_character, scopes }`.
- `background_task` reads the settings of the model backing that background
  task (same selector semantics as `set_model_setting`).

#### `memory`
- **args:** `{ "query"?: "string" }`
  - Without `query`: returns markdown memory file counts
    `{ character, entries, curated_files, daily_files, image_files }`.
  - With `query`: returns `{ character, query, result }` where `result`
    is the formatted hit listing.

#### `memory_changelog`
- **args:** `{ "limit"?: i64 }` — default `20`.
- **data:** `{ "character": "…", "changelog": [ { "timestamp": "…", "operation": "…", "description": "…" } ] }`.

#### `memory_dreams`
- **args:** `{ "limit"?: u64 }` — default `10`.
- **data:** `{ character, entries, path, exists }` where `entries` is the
  newest sections of the dreams audit log.

#### `memory_dream`
- **args:** `{ "status"?: bool, "dry_run"?: bool, "force"?: bool }`.
  - `status = true` returns `{ character, due, next_due_at, … }` and
    does *not* run a sweep.
  - Otherwise runs the librarian dreaming sweep. Returns the sweep result
    object on success, or `{ character, status: "not_due", enabled, frequency }`
    when a sweep was skipped.

#### `compact`
- **args:** `{ "dry_run"?: bool, "keep_turns"?: u64 }`.
- **data (success):**
  ```json
  {
    "status": "compacted",
    "character": "Alice",
    "memory_files_written": ["memory/…", …],
    "message_count": 64, "turn_count": 64, "compacted_turns": 64,
    "retained_count": 8, "retained_turns": 8,
    "new_conversation_id": "…"
  }
  ```
- **data (dry run):**
  ```json
  {
    "status": "dry_run",
    "character": "Alice",
    "would_write_files": ["…"],
    "file_ops_preview": [ { "path": "…", "content_preview": "…" } ],
    "message_count": 64, "turn_count": 64, "compacted_turns": 64,
    "retained_count": 8, "retained_turns": 8
  }
  ```
- **errors:** `busy` when a compaction is already running for this
  character, `invalid_request` for insufficient messages.

#### `config`
- **read:**
  - `{}` → `{ "config": <whole app config JSON> }`
  - `{ "key": "<section>" }` → `{ "key": "<section>", "config": <subtree> }` or `not_found`.
- **set:** `{ "key": "<key>", "value": "<string>" }`. Only a focused
  set of keys is settable at runtime:
  - `defaults.model` / `model` (must resolve to a known model)
  - `defaults.stream` / `stream` (`"true"` / `"false"`)
  - `autonomy.enabled` / `behavior.autonomy.enabled` (`"true"` / `"false"`)
- **data:** `{ "set": "<key>", "value": <coerced value> }`.

#### `config_check`
- **args:** none
- **data:** validation result with `valid`, `warnings`, `info`, plus
  config-dir / data-dir / cache-dir paths, `chat_models`,
  `tool_models`, `memory_mode`.

#### `config_reset`
- **args:** none
- **data:** `{ "reset": true, "message": "…", "config_path": "…", "invalidated": { "runtime_overrides": true } }`.

#### `diagnostics`
- **args:** `{ "count"?: u64 }` — default `10`. Recent ring-buffer entries.
- **data:** opaque diagnostics JSON (mirrors `Diagnostics::to_json`).

#### `heartbeat_log`
- **args:** `{ "count"?: u64 }` — default `20`.
- **data:** `{ "events": [ { "timestamp": "…", "kind": "…", "detail": "…" } ] }`.

#### `heartbeat_tick_now`
- **args:** none. Schedules a heartbeat tick immediately for the active
  character.
- **data:** `{ "status": "scheduled", "character": "…", "warning"?: "…" }`.

#### `heartbeat_set_dormant` / `heartbeat_set_active`
- **args:** none. Toggle the dormancy guard.
- **data:** `{ "status": "dormant" | "active", "character": "…" }`.

#### `usage`
A multi-mode usage / billing reporter. Argument switches pick a mode;
unspecified mode runs the default `summary`.

- **shared filter args:** `last` (`"today"`, `"week"`/`"this_week"`,
  `"month"`/`"this_month"`, `"all"`, or `"<N>h"` / `"<N>d"` / `"<N>w"`;
  default `"today"`), `character`, `provider`, `api_key`, `model`,
  `call_type`.
- **modes** (set the flag to `true`):
  - `budget = true` → `{ mode: "budget", timezone, allow_compaction_over_budget, budgets, spike_warnings }`.
  - `export_tsv = true` → `{ mode: "tsv", data: "…" }`.
  - `export_csv = true` → `{ mode: "csv", data: "…" }`.
  - `by_kind = true` → `{ mode: "summary_by_usage_kind", period, summary: [ {usage_kind, call_count, total_input, total_output, total_cache_read, total_cache_write, total_cost} ] }`.
  - `by_api_key = true` → `{ mode: "summary_by_api_key", period, summary: [ {provider, api_key_name, call_count, …, total_cost} ] }`.
  - `by_call_type = true` → `{ mode: "summary_by_call_type", period, summary: [ {call_type, …} ] }`.
  - `anomalies = true` → `{ mode: "anomalies", anomalies: [ {ts, character, model, call_type, anomaly, cache_read_tokens, cache_write_tokens} ] }`. Forces a 7-day window when `last = "today"`.
  - `refresh_pricing = true` → `{ mode: "refresh_pricing" }`. Clears the pricing cache.
  - `recalculate = true` (optionally `force: true`) →
    `{ mode: "recalculate", updated, total, failures: [ {model, reason} ] }`.
- **default (summary):**
  ```json
  {
    "mode": "summary",
    "period": "today",
    "timezone": "utc",
    "summary": [ { "provider": "anthropic", "model": "claude-sonnet", "call_count": 12, "total_input": …, "total_output": …, "total_cache_read": …, "total_cache_write": …, "total_cost": 0.42 } ],
    "cache_health": [ { "character": "Alice", "state": "warm" | "cold", "streak": 3 } ],
    "anomaly_count_7d": 0,
    "budgets": [ … ],
    "spike_warnings": [ … ]
  }
  ```

### 8.4 Provider discovery

#### `list_providers` *(characterless OK)*
- **args:** none
- **data:**
  ```json
  {
    "providers": [
      {
        "name": "openrouter",
        "enabled": true,
        "sdk": "openai" | null,
        "base_url": "https://…",
        "discovery_enabled": true,
        "keys": [ { "name": "primary", "enabled": true, "warn_on_fallback": true, "env_set": true } ],
        "cache": { "present": true, "models": 312, "visible": 280, "hidden": 32, "fetched_at": "…" }
      }
    ]
  }
  ```
- Output never contains env-var names or key values.

#### `refresh_provider_models`
- **args:** `{ "provider": "<name>" }`
- **data:** `{ "provider": "…", "model_count": 312, "fetched_at": "…", "cache_path": "…" }`
- **errors:** `not_found` (unknown provider), `invalid_request` (disabled,
  discovery disabled, or missing `base_url`), `provider_error` (no
  enabled key has a non-empty env value), `internal_error` (upstream
  request failed; the previous cache is preserved).

#### `refresh_all_provider_models`
- **args:** none
- **data:**
  ```json
  {
    "results": [
      { "provider": "openrouter", "ok": true,  "model_count": 312, "fetched_at": "…", "cache_path": "…" },
      { "provider": "openai",     "ok": false, "error": "no API key configured" }
    ],
    "skipped": [ { "provider": "anthropic", "reason": "discovery disabled" } ]
  }
  ```

#### `list_provider_models` *(characterless OK)*
- **args:** `{ "provider": "<name>", "include_hidden"?: bool }`
- **data:**
  ```json
  {
    "provider": "openrouter",
    "discovered": [ { "source": "discovered", "model_id": "…", "display_name": "…", "sdk": "…", "owned_by": "…", "context_length": …, "max_output_tokens": …, "supports_tools": …, "supports_images": …, "supports_reasoning": …, "supports_prompt_cache": …, "discovered_at": "…" } ],
    "hidden":     [ … same shape, filtered by `discovery.ignore` … ],
    "static":     [ { "source": "static", "name": "…", "qualified_name": "…", "model_id": "…", "sdk": "…", "max_output_tokens": … } ],
    "include_hidden": false,
    "cache": { "fetched_at": "…", "model_count": 312 }
  }
  ```
- Static models are never filtered. Setting `include_hidden = true` folds
  ignored discovered models back into `discovered` (and leaves `hidden`
  empty).

## 9. Shared types

### 9.1 `Message`

```json
{
  "msg_id": "m_…",
  "role": "user" | "assistant" | "system",
  "content": "human-readable text summary",
  "images": [ { "path": "…", "caption": "…", "data": "<base64>" } ],
  "content_blocks": [ ContentBlock, … ],
  "alt_index": 1,
  "alt_count": 2,
  "alternatives": [ MessageAlternative, … ],
  "timestamp": "2026-05-20T12:00:00Z",
  "origin": "autonomous"
}
```

- `content_blocks` is the canonical content representation. `content` is
  derived from the text/tool-result blocks for convenience. Clients
  reading legacy stored messages may see `content_blocks: []` and a
  populated `content`; treat them as equivalent (the daemon normalizes on
  load).
- `images[].data` is present on the wire but stripped from disk
  storage.
- `alt_index` / `alt_count` / `alternatives` are present only when the
  message has more than one stored response. `alt_index` is 0-based;
  `alternatives[alt_index]` matches the top-level `content`.
- `origin` is optional: `"user_input"`, `"assistant_reply"`, or
  `"autonomous"`. In `new_message` frames it is always stamped (§7.3). In
  `history` snapshots it appears only on autonomous (heartbeat-initiated)
  assistant messages — the daemon persists it only there — and is omitted on
  ordinary turns and on messages stored before origin tracking. The
  deep-idle archive uses it to keep unanswered autonomous messages in the
  active conversation across an archive boundary.

### 9.2 `ContentBlock`

Externally tagged with `type`. Variants:

```json
{ "type": "text", "text": "…" }
{ "type": "thinking", "thinking": "…", "signature": "…" | null }
{ "type": "redacted_thinking", "data": "<opaque>" }
{ "type": "tool_use", "id": "tu_…", "name": "…", "input": { … } }
{ "type": "tool_result", "tool_use_id": "tu_…", "content": "…", "is_error": false }
```

`thinking.signature` and `tool_result.is_error` default to `null`/`false`
on read when omitted.

### 9.3 `MessageAlternative`

```json
{
  "content": "…",
  "images": [ ImageRef, … ],
  "content_blocks": [ ContentBlock, … ],
  "timestamp": "2026-05-20T12:00:00Z"
}
```

### 9.4 `StreamMetadata`

```json
{
  "tokens":  { "input": u32, "output": u32, "cache_read": u32, "cache_write": u32 },
  "timing":  { "total_ms": u32, "ttft_ms": u32 },
  "model":   "claude-sonnet-4-6"
}
```

### 9.5 `CharacterInfo`

```json
{ "name": "Alice", "avatar": { "mime_type": "image/png", "data": "<base64>" } | null }
```

## 10. Client implementation checklist

A minimal SWP client needs to:

1. Resolve a server address (user override → `instances.json` →
   `127.0.0.1:7320`).
2. Open a TCP stream; read frames as newline-delimited JSON with a
   128 MiB cap.
3. Receive `ServerHello`, refuse `v != 1`.
4. Send `ClientHello` with `client_type`, `client_name`, optional
   `capabilities`, and optional `character`.
5. Receive `History` to seed local state (`messages`, `active_start`,
   `selected_character`, `revision`).
6. Loop:
   - Read frames; ignore `Ping`; treat `Shutdown` as EOF; silently skip any
     unrecognized `type` and keep reading (forward-compat, see §3).
   - For request-shaped sends, generate an `rid`, match incoming
     `StreamStart`/`StreamChunk`/`StreamEnd`/`ToolCall`/`ToolResult`/
     `Phase`/`CommandOutput`/`Error` frames by `rid`.
   - On `stream_end`, keep reading until `is_final = true` before
     considering the generation finished.
   - Apply `NewMessage` pushes and unsolicited `History` snapshots
     (compare `revision`).
7. On disconnect or `Shutdown`, close cleanly; the daemon already
   cancels any in-flight generation when the last session drops.

The bundled `shore-swp-client` crate is a useful reference
implementation: it owns the discovery, handshake, framing limit,
streaming aggregator (`collect_stream`), and per-tool-loop boundary
handling described above.
