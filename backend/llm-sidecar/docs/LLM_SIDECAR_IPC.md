# LLM Sidecar IPC Contract

Status: implementation in progress (2026-06-02). Goal: move the LLM **wire**
(provider SDK calls) out of the Rust `shore-llm` crate into a Bun **sidecar**
that uses official vendor SDKs, while the Rust daemon, prompt assembly, tool
loop, memory, and SWP server stay byte-for-byte unchanged. Zero user-visible
behavior change except "OpenAI-compatible providers stop mangling tool loops."

## The seam (why this is small)

The daemon talks to the wire through a narrow interface (`backend/daemon` →
`shore_llm`):

- `LlmClient::stream_raw(&LlmRequest) -> StreamReader` (`backend/llm/src/lib.rs:312`)
- `LlmClient::generate(&LlmRequest) -> GenerateResponse` (`lib.rs:332`)
- `LlmClient::image_generate(&ImageGenerateParams) -> ImageGenerateResponse` (`lib.rs:351`)

`stream_raw` calls `providers::stream` (`backend/llm/src/providers/mod.rs`), which
returns a `DuplexStream` carrying **newline-delimited JSON `StreamEvent` lines**.
The daemon's `StreamConsumer::consume` (`backend/llm/src/stream.rs:38`) reads those
lines and never sees raw provider SSE. **That NDJSON `StreamEvent` vocabulary is
the contract** — it already exists, is serde-stable, and is covered by the
`stream.rs` test suite.

The cut: replace the body of `providers::stream`/`generate`/`image_generate` so
that instead of dispatching to `anthropic.rs`/`openai.rs`/`gemini.rs`/`zai.rs`
(~8k LOC) it POSTs the request to the sidecar and returns a reader over the
sidecar's response. Everything above `providers::*` is unchanged.

## Transport

**Unix domain socket, HTTP/1.1, one request per call.** Recommended over stdio:

- The daemon already models "read NDJSON lines from an async reader"; an HTTP
  response body over a UDS drops straight into `stream_raw`'s existing
  `BufReader` return shape.
- UDS is loopback-only by construction (satisfies the "non-loopback access must
  be explicit" rule — there is no port, no network surface). Socket file lives
  under the daemon runtime dir, mode `0600`.
- Concurrency for free: compaction, dreaming, heartbeat, and chat run
  concurrent LLM calls; separate HTTP requests need no custom multiplexing.
- Cancellation for free: Rust `AbortSignal`/drop → close connection → Bun
  `request.signal` aborts → forward to the SDK's abort.
- Clean error mapping via HTTP status (below).

Socket path passed to the sidecar via `--socket <path>` (or `SHORE_LLM_SOCKET`).

The Rust daemon sidecar transport is gated by config and defaults off:

```toml
[advanced.llm_sidecar]
enabled = true
# optional; default is <runtime_dir>/llm.sock
socket_path = "/run/user/1000/shore/llm.sock"
```

### Endpoints

| Method | Path           | Request body        | Response                            |
|--------|----------------|---------------------|-------------------------------------|
| POST   | `/v1/stream`   | `SidecarRequest`    | `application/x-ndjson` StreamEvents |
| POST   | `/v1/generate` | `SidecarRequest`    | `application/json` GenerateResponse |
| POST   | `/v1/image`    | `ImageRequest`      | `application/json` ImageResponse    |
| GET    | `/healthz`     | —                   | `200 ok` (startup/supervision)      |

## Request frame — `SidecarRequest`

The serialized `LlmRequest` (`backend/llm/src/types.rs`), already serde-derived,
**minus** the `#[serde(skip)]` transient fields (`api_key_name`, `rid`,
`forensic_character`, `retain_long`) which stay Rust-side. Fields the sidecar
consumes:

```jsonc
{
  "sdk": "anthropic" | "openai" | "zai" | "gemini",   // dialect → which SDK
  "model": "anthropic/claude-opus-4.8",
  "api_key": "...",                 // bearer; sidecar applies x-api-key vs Authorization
  "base_url": "https://openrouter.ai/api/v1",  // optional override
  "messages": [ /* provider-native blocks, ALREADY ASSEMBLED by Rust */ ],
  "system":   [ /* system blocks */ ],          // optional
  "tools":    [ /* tool defs */ ],              // optional
  "max_tokens": 8192,
  "temperature": 1.0,               // optional
  "top_p": 0.95,                    // optional
  "provider_options": { "cache_ttl": "5m", "thinking": {...} },  // optional
  "provider_key": "deepseek"        // optional; provider-specific behavior
}
```

Notes:
- `messages`/`system`/`tools` arrive **fully assembled** (Anthropic-canonical
  block shape — the daemon's `engine/prompt.rs` already produced them). The
  sidecar's per-SDK adapter converts canonical → that SDK's wire shape.
- `api_key` crosses the boundary. UDS `0600` keeps it host-local; acceptable,
  documented. Never logged by the sidecar.
- **The sidecar does NOT replay prior thinking as `reasoning_content`/`reasoning`**
  (the deepseek/kimi tool-loop bug — see `project_deepseek_reasoning_replay_bug`).
  This divergence from the Rust `openai.rs` is intentional and is the fix.

## Stream response — `StreamEvent` NDJSON (the existing vocabulary)

One JSON object per line. Tag field is `type`, snake_case. Defined at
`backend/llm/src/types.rs:155`, consumed at `stream.rs:81`.

```jsonc
{"type":"start","model":"<model>"}                         // exactly once, first
{"type":"text","text":"<delta>"}                           // incremental
{"type":"thinking","text":"<delta>"}                       // incremental
{"type":"thinking_signature","signature":"<opaque>"}       // see ordering
{"type":"redacted_thinking","data":"<opaque>"}             // complete block
{"type":"tool_use","id":"<id>","name":"<name>","input":{}} // CONSOLIDATED (full input)
{"type":"done",
 "content":"<final text string>",
 "finish_reason":"end_turn|tool_use|max_tokens|stop_sequence|refusal",
 "usage":{"input_tokens":N,"output_tokens":N,
          "cache_read_tokens":N,"cache_creation_tokens":N,
          "total_cost_usd":0.0},                            // cost optional
 "timing":{"total_ms":N,"time_to_first_token_ms":N}}        // exactly once, last
```

### Ordering rules (load-bearing — `StreamConsumer` relies on them)

1. `start` first, once.
2. `text`/`thinking`/`tool_use` interleave in true arrival order. The consumer
   flushes a pending text/thinking buffer into a content block whenever the
   block type changes, so order = persisted block order.
3. `thinking_signature` MUST follow its `thinking` deltas and precede the next
   non-thinking event — the consumer attaches it to the thinking block on flush
   (`stream.rs:130`, `flush_thinking`). An orphan signature (no preceding
   thinking) is discarded.
4. `tool_use` is **one consolidated event with the full parsed `input`** — NOT
   start/delta/done. The consumer pushes a complete `ToolUse` block and a
   `ToolUseEvent` for the tool loop (`stream.rs:143`).
5. `done` last, once. `content` is the final assembled **text string** (not a
   blocks array); the structured blocks are accumulated by the consumer from the
   granular events above. EOF before `done` → `LlmError::IncompleteStream`.

## Non-streaming response — `GenerateResponse`

`/v1/generate` returns one JSON (`types.rs` `GenerateResponse`):

```jsonc
{ "content":"<text>", "content_blocks":[...], "finish_reason":"...",
  "usage":{...}, "timing":{...}, "model":"..." }
```

Same `usage`/`timing` shapes as streaming. `content_blocks` is the full
structured sequence (text/thinking/tool_use) — the sidecar builds it from the
single completion.

## Error model (matches current behavior — no daemon change)

- **Pre-stream failure** (bad request, upstream 4xx/5xx before any event): sidecar
  returns non-2xx with body → Rust maps to `LlmError::HttpStatus{status, body}`.
- **Connection failure**: → `LlmError::Request`.
- **Mid-stream upstream failure** (after `start`): sidecar closes the connection
  without `done` → Rust `StreamConsumer` returns `LlmError::IncompleteStream`.
  This matches today exactly: `StreamEvent` has **no** error variant, so we do
  NOT add one (that would touch the daemon). Granularity is unchanged from
  status quo.
- **Refusal**: NOT a wire concern. `LlmError::Refusal` is never produced by the
  providers — it's a *post-response* decision in the Rust retry layer:
  `is_refusal(content, finish_reason)` (`retry.rs:156`) checks
  `finish_reason == "content_filter" | "refusal"` or refusal phrases in
  <500-char content, and `should_retry_refusal` acts on the completed
  `StreamResult`. The sidecar just passes `finish_reason` through faithfully.
  Zero sidecar work, zero daemon change.

## SDK coverage (`Sdk` enum: Anthropic, Openai, Zai, Gemini)

| `sdk`     | Sidecar adapter            | Status        |
|-----------|----------------------------|---------------|
| anthropic | `@anthropic-ai/sdk`        | exists (`providers/anthropic.ts`) |
| openai    | `openai` (+ base_url swap) | exists (`providers/openai.ts`) — covers deepseek/kimi/xai/nanogpt |
| gemini    | `@google/genai`            | **new adapter** — distinct wire (below) |
| zai       | official Z.ai JS SDK, or `openai` + extra_body | **new adapter** — NOT a base_url swap (below) |

**Gemini** (`generativelanguage.googleapis.com/v1beta/models/{m}:streamGenerateContent?alt=sse`):
distinct wire — system prompt goes in `systemInstruction` (not a message), tools
are `functionDeclarations`, thinking is `thinkingConfig{thinkingBudget|thinkingLevel}`
(3.x maps `reasoning_effort` strings → `thinkingLevel`). Build on `@google/genai`,
which models all three first-class.

**Z.ai** — speaks OpenAI chat-completions for messages/tools, but is NOT a plain
base_url swap (`providers/zai.rs`):
- dual base URLs — `api.z.ai/api/paas/v4` vs `…/coding/paas/v4`, toggled by the
  `zai_subscription` provider option;
- custom thinking control in the request body:
  `"thinking":{"type":"enabled","clear_thinking":<bool>}` (the
  `zai_clear_thinking` option) — NOT OpenAI's `reasoning_effort`;
- a `reasoning_content` field.
Use the official Z.ai JS SDK if a maintained one exists; otherwise the `openai`
SDK pointed at the Z.ai base_url with the thinking/clear_thinking fields injected
via the SDK's extra-body passthrough. Either way it's a dedicated adapter. (Like
all sidecar adapters, it does NOT replay prior thinking as `reasoning_content` —
only honors Z.ai's documented `clear_thinking` input control.)

## TS adapter gap (what must change to emit the contract)

The current adapters emit a different in-house `ChatEvent` union
(`src/llm/types.ts`): `text_delta`, `thinking_delta`, `tool_use_start`,
`tool_use_input_delta`, `tool_use_done`, `done{content: ContentBlock[]}`. To emit
the `StreamEvent` NDJSON above:

1. Emit `start{model}` on stream open.
2. `text_delta` → `text`; `thinking_delta` → `thinking` (rename only).
3. **Add `thinking_signature`** from Anthropic `signature_delta` — REQUIRED for
   thinking replay + cache correctness. (Currently not surfaced.)
4. **Add `redacted_thinking`** from Anthropic redacted blocks.
5. Collapse `tool_use_start`+`input_delta`+`done` into one `tool_use{id,name,input}`
   (parse accumulated `argsJson`). The adapter already accumulates this.
6. `done`: emit `content` as the final **string**, `finish_reason` (map
   `stopReason`), `usage` in snake_case (+ `total_cost_usd` passthrough from
   OpenRouter `cost`), and `timing{total_ms, time_to_first_token_ms}` (sidecar
   measures: ttft = first post-`start` event; total = at `done`).

## Stays Rust-side (explicitly NOT moved)

`preprocess_request`, credential resolution (`credentials.rs`), cache-forensics +
`debug_log` payload logging (operate on `LlmRequest` pre-send), model
`discovery`/catalog, connection-level retry wrapper, and the **tool loop**
(`engine/tools.rs`). The sidecar is a stateless per-call function:
`SidecarRequest in → StreamEvent NDJSON out`.

## Supervision / packaging

- `bun build --compile` emits `backend/llm-sidecar/dist/shore-llm-sidecar`.
- When `[advanced.llm_sidecar].enabled = true`, `shore-daemon` configures
  `shore-llm` to use the configured socket path, defaulting to
  `<runtime_dir>/llm.sock`.
- The daemon starts a sidecar supervisor when `shore-llm-sidecar` is found on
  `PATH` or next to the running daemon. It spawns
  `shore-llm-sidecar --socket <path>`, creates the socket directory, polls
  `GET /healthz`, restarts with backoff on spawn/health/exit failures, and
  SIGTERMs the child on daemon shutdown.
- If the binary is absent, the daemon logs a warning and continues; this keeps
  manually managed development sidecars possible at the configured socket path.
- The Arch `shore-daemon` PKGBUILD installs both binaries.
- The Debian/Pi helper at `contrib/debian/build-shore-daemon-deb.sh` builds a
  `.deb` that installs both `shore-daemon` and `shore-llm-sidecar`.

## Retry (resolved — stays Rust-side, unchanged)

`RetryPolicy{max_retries:2}` with exponential backoff lives *above* the wire
(`retry.rs`): retries on `IncompleteStream`, HTTP 5xx, and 429; never on other
4xx; falls back to the configured fallback model on exhaustion. This is unchanged.
**The sidecar must DISABLE the official SDKs' built-in retry (`maxRetries: 0`)** so
we don't double-retry and the existing Rust semantics stay identical; the sidecar
surfaces upstream status as HTTP status so `should_retry_error` keys off the
mapped `LlmError::HttpStatus{status,…}`.

## Image generation (resolved)

`POST /v1/image`. `image_generate` dispatches to the OpenAI images API for ALL
providers (`providers/mod.rs:157` → `openai::image_generate`), so this is one
thin `openai` SDK `images.generate` call regardless of `provider_key`. The
existing `src/llm/images.ts` likely already covers it.

Request (`ImageGenerateParams`): `{provider_key, model, api_key, base_url,
prompt, size?, quality?, aspect_ratio?, image_size?}`.
Response (`ImageGenerateResponse`): `{url, revised_prompt, timing:{total_ms}}`.

## Open items before cutover

All five design open-items are resolved above. Remaining build tasks:
1. Confirm a maintained official **Z.ai JS SDK** exists; else use `openai` +
   extra_body.
2. Build the **Gemini** adapter on `@google/genai`.
3. Add **`thinking_signature`** + **`redacted_thinking`** emission to
   `anthropic.ts` (the one real correctness gap).
```
