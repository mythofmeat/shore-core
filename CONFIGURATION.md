# Configuration

Shore loads `config.toml` from `$XDG_CONFIG_HOME/shore/` unless `--config` is provided. The config loader also supports includes and `conf.d/` overlays; see `examples/config.toml` for a complete commented file.

## Environment

Common variables:

| Variable | Purpose |
| --- | --- |
| `SHORE_CONFIG_DIR` | override config directory |
| `SHORE_DATA_DIR` | override data directory |
| `SHORE_RUNTIME_DIR` | override runtime directory |
| `SHORE_CACHE_DIR` | override cache directory |
| `SHORE_ADDR` | daemon address override |
| `SHORE_CHARACTER` | default CLI character |
| `ANTHROPIC_API_KEY` | Anthropic provider key |
| `OPENROUTER_API_KEY` | OpenRouter provider key |
| `TAVILY_API_KEY` | web search key |

A `.env` file in the config directory is loaded on startup.

## Client Connection

The `shore` CLI resolves the SWP daemon address in this order:

1. `--addr` or `SHORE_ADDR`
2. `$XDG_CONFIG_HOME/shore/client.toml`
3. the local `$XDG_RUNTIME_DIR/shore/instances.json` daemon registry
4. the default `127.0.0.1:7320`

Use `client.toml` when clients run on a different machine from the daemon:

```toml
default_address = "100.64.0.10:7320"
```

The packaged `shore-notify.service` also reads the optional
`$XDG_CONFIG_HOME/shore/notify.env` environment file. For a remote notifier,
set `SHORE_ADDR=100.64.0.10:7320` there or use `client.toml`.

## Hot Reload

The daemon watches supported config inputs and reloads runtime config after
changes settle briefly. This is always enabled.

Reloaded without restart:

- `config.toml`, explicit `include = [...]` TOML files, and `conf.d/*.toml`
- `.env`
- `characters/<Character>/config.toml`
- character discovery when character directories or legacy `character.md` files change

Hot reload updates model catalogs, defaults, behavior/tool settings, memory
settings, autonomy config, and merged per-character config for future work. It
keeps the previous runtime config if the changed files fail to parse or
validate.

Startup-owned settings still require a daemon restart, including `[daemon]`
listener settings, `[connections.matrix]`, `[tts]` connection setup,
`[notifications]`, `[services]`, and startup-only `[advanced]` diagnostics
toggles. Shore logs these as restart-required when it sees them change.

The watcher deliberately ignores `characters/<Character>/workspace/**`,
including prompt files and `workspace/memory/**`. Those files keep the normal
compaction/reload activation boundary described below.

## Minimal Config

```toml
[defaults]
model = "claude-sonnet"
display_name = "Ren"

[chat.anthropic.claude-sonnet]
model_id = "claude-sonnet-4-6"
api_key_env = "ANTHROPIC_API_KEY"
cache_ttl = "1h"
```

## `[daemon]`

```toml
[daemon]
addr = "127.0.0.1:7320"
unsafe_allow_remote_access = false
allowed_hosts = []

[daemon.http]
enabled = false
bind_addr = "127.0.0.1:0"
```

Non-loopback binds require `unsafe_allow_remote_access = true`. `allowed_hosts` is only a source-IP filter, not auth or TLS.

`[daemon.http]` starts the daemon's local HTTP listener. It is off by default
for API providers and is auto-enabled at daemon startup when any
`sdk = "claude_code"` chat model is configured, so the local `claude` CLI can
call back into Shore's MCP tool host. The default `127.0.0.1:0` binds an
ephemeral loopback port and is the recommended setting.

The HTTP listener is not authenticated and does not provide TLS. Keep
`bind_addr` on loopback unless you are on a trusted private or overlay network
and have set `[daemon].unsafe_allow_remote_access = true` intentionally. The
`allowed_hosts` filter applies to the SWP listener, not this HTTP MCP listener;
the `/mcp/<session-id>` URL should be treated as a bearer secret while a Claude
Code turn is active.

## `[defaults]`

```toml
[defaults]
model = "claude-sonnet"           # chat default; also fallback for background tasks
embedding = "text-large"
image_generation = "image"
display_name = "Ren"
stream = true

# Optional: split background tasks (heartbeat/compaction/dreaming) from chat.
# Most users only need `model` (everything one model) or `model` + `background.model`
# (split chat from all background work).
[defaults.background]
model = "haiku"                   # blanket model for every background task
# heartbeat = "haiku-fast"        # per-task overrides (optional)
# compaction = "claude-sonnet"
# dreaming = "claude-sonnet"
```

Selectors are aliases declared under `[chat.*]`, `[tools.*]`, `[embedding.*]`, or `[image_generation.*]`.

Important slots:

- `model` — chat default. Acts as the final fallback for background tasks too.
- `[defaults.background]` — heartbeat, compaction, and dreaming selectors. Each task chains `background.<task> → background.model → defaults.model → first chat model`. None of these consult the per-character active chat model, so a runtime `shore model <name>` (which only updates chat) does **not** move background tasks.
- `embedding` — optional hybrid retrieval profile
- `image_generation` — image generation profile

> **Deprecated:** the older top-level `defaults.heartbeat` and `defaults.dreaming` keys still parse but emit a deprecation warning and are forwarded into `[defaults.background]` at load time. Move them under `[defaults.background]` to silence the warning.

## Model Sections

Chat/tool models:

```toml
[chat.anthropic.claude-sonnet]
model_id = "claude-sonnet-4-6"
api_key_env = "ANTHROPIC_API_KEY"
cache_ttl = "1h"
max_tokens = 4096
max_context_tokens = 200000

[chat.openrouter.haiku]
model_id = "anthropic/claude-haiku-4-5"
api_key_env = "OPENROUTER_API_KEY"
base_url = "https://openrouter.ai/api/v1"
```

### Claude Code / Max Subscription

`sdk = "claude_code"` routes a chat model through the local `claude` CLI instead
of a provider HTTP API. The CLI uses the user's local OAuth login, so there is
no `api_key_env` for this model.

```toml
[chat.claude_code.sonnet-max]
model_id = "claude-sonnet-4-5"
max_tokens = 4096
```

Before using it, install Claude Code, run `claude auth login`, and verify with
`shore config --check`. The provider supports Shore tools through the daemon's
MCP listener, which the daemon auto-enables on loopback when `claude_code`
models are present. Client-visible streaming uses Claude Code partial-message
events for progressive text/thinking deltas. `max_tokens`, `temperature`,
`top_p`, and Anthropic prompt-cache knobs are not currently forwarded because
the `claude` CLI does not expose matching flags for this OAuth-backed path; see
`docs/claude-code-parity.md`. `shore usage` records Claude Code's reported
`total_cost_usd` as would-be API cost; actual subscription spend remains the
fixed Claude plan price.

By default, cold Claude Code starts with prior Shore history rewrite a native
Claude Code JSONL session file and launch with `--resume <session_id>`. This
preserves structured conversation context across compaction, daemon restart, or
subprocess death more faithfully than a flattened transcript. Set
`provider_options.native_session_replay = false` only for diagnostics or if a
future Claude Code release changes the private JSONL format.

Current-turn image input is supported through Shore's Claude Code MCP session:
the daemon exposes a private attachment tool for the request and the provider
points Claude at that tool instead of sending raw stream-json image blocks.
Direct stream-json image blocks are still a Claude Code CLI parity gap, and
older image-only history is not replayed as visual context.

Shore passes the system prompt through Claude Code's `--system-prompt-file`
flag to keep large prompts out of process arguments. That flag is an
undocumented Claude Code surface, so provider live tests are the compatibility
guard when upgrading the local `claude` CLI.

Embedding profiles. Shore only ships an OpenAI-compatible embedder; any
endpoint that speaks `/v1/embeddings` works (OpenAI, Together, Voyage's
compat endpoint, OpenRouter, or a self-hosted server like
text-embedding-inference or llama.cpp's HTTP server).

```toml
# Hosted OpenAI:
[embedding.text-large]
model_id = "text-embedding-3-large"
api_key_env = "OPENAI_API_KEY"

# Self-hosted (e.g. text-embedding-inference). `api_key_env` still has
# to point at a set variable; if your server doesn't validate keys, set
# it to any non-empty value.
[embedding.local-tei]
model_id = "BAAI/bge-large-en-v1.5"
api_key_env = "TEI_API_KEY"
base_url = "http://127.0.0.1:8080/v1"
dimensions = 1024
```

When no `[embedding.*]` profile is configured (and `defaults.embedding`
doesn't reference one), the workspace `search` tool's `hybrid` and
`vector` modes degrade to lexical-only at the call site. Configure an
embedding profile to enable semantic search.

## Providers

Provider entries replace per-model `api_key_env` duplication and unlock
runtime `/v1/models` discovery. Static `[chat.<provider>.<alias>]`
entries keep working unchanged alongside the registry — they never
require migration.

### Single-key form (compact)

```toml
[providers.openai]
api_key_env = "OPENAI_API_KEY"
```

The compact `api_key_env` is folded into a synthetic key named
`default`. Combining it with explicit `[[keys]]` is rejected; pick one
form per provider.

### Multiple keys: budget/overflow rotation

```toml
[providers.openrouter]
sdk = "openai"
base_url = "https://openrouter.ai/api/v1"

[[providers.openrouter.keys]]
name = "budget"
env = "OPENROUTER_API_KEY_BUDGET"
warn_on_fallback = true

[[providers.openrouter.keys]]
name = "overflow"
env = "OPENROUTER_API_KEY_OVERFLOW"
```

Keys are tried in configured order on every request, including streaming chat
turns and non-streaming background work such as heartbeat, compaction,
dreaming, and cache keepalive. When an interactive chat request falls back away
from a key marked `warn_on_fallback = true` (e.g. an exhausted budget cap), a
one-line client warning surfaces; background rotations are recorded in logs, and
autonomy/keepalive rotations are also visible in heartbeat status. The fallback
is non-sticky: the next request retries from the top of the list.

### Discovery and filtering

```toml
[providers.openrouter.discovery]
enabled = true
# gitignore-style; last match wins. `*` opt-out, `!pattern` opt-in.
ignore = [
  "*",
  "!anthropic/*",
  "!openai/*",
  "!google/gemini-*",
]
```

Discovered models populate the cache at
`$XDG_CACHE_HOME/shore/providers/<name>/models.json`. The daemon
auto-refreshes any discovery-enabled provider whose cache is missing or
older than 24h, both at startup and on a 24h cadence while running. Run
`shore provider refresh <name>` (or `shore provider refresh` to fan out
across every discovery-enabled provider) to force a refetch out of band.
Hidden models stay in the cache but are filtered out of `shore model` and
`shore provider models <name>` until `--all` (CLI) or `:model all` (TUI)
is used. Manual `[chat.<provider>.<alias>]` entries are never filtered —
they are intentional.

### Effective catalog and merge order

At runtime the daemon resolves models against an effective catalog
that merges three sources:

1. Static `[chat.<provider>.<alias>]` entries (this file).
2. Discovered `[providers.<name>]` cache rows.
3. Hardcoded provider defaults for well-known providers.

Conflict rules:

- Static aliases always win when matched by short name (`sonnet`) or
  qualified name (`chat.openrouter.sonnet`).
- When a static entry shares `(provider, model_id)` with a discovered
  row, the static entry wins for explicit fields and the discovered
  row is hidden from listings (no duplicate row).
- Discovered models can be selected at runtime via the bare upstream
  id (`anthropic/claude-sonnet-4.5`) or the disambiguated form
  (`openrouter:anthropic/claude-sonnet-4.5`). `[defaults].model` must
  still reference a static alias — define one (see
  `examples/config.toml`) when you want a discovered model to be the
  startup default.

## Sampler Preferences

`shore model setting <key> <value>` and `:setting <key> <value>` write
saved sampler overrides keyed by `(provider, model_id)`. Storage:

```text
$XDG_DATA_HOME/shore/preferences/global.toml
$XDG_DATA_HOME/shore/characters/<Character>/preferences/models.toml
```

Merge order (lowest to highest precedence):

1. Hardcoded provider defaults.
2. Discovered model metadata.
3. Static `[chat.<provider>.<alias>]` overrides.
4. Saved global preferences (`preferences/global.toml`).
5. Saved per-character preferences (`characters/<C>/preferences/models.toml`).

`reasoning_effort` accepts `low`/`medium`/`high` or `off` (cleared).
The legacy `shore reasoning ...` command writes through the same
store. One-shot overrides — `shore model --all <name>`, `:model all
<name>`, `shore provider refresh <name>` — apply to a single call and
are never persisted.

## Character Workspaces

Characters live under:

```text
$XDG_CONFIG_HOME/shore/characters/<Character>/workspace/
```

Expected files:

```text
SOUL.md
USER.md
AGENTS.md
TOOLS.md
HEARTBEAT.md
MEMORY.md     # optional/generated prompt-visible memory index
memory/
```

Legacy `character.md`, `user.md`, and `prompts/system.md` are migrated into the workspace on first load.

## `[behavior.autonomy]`

```toml
[behavior.autonomy]
enabled = false

[behavior.autonomy.heartbeat]
enabled = true
fallback_heartbeat_interval = "1h"
dormant_after_heartbeat_turns = 3
dormant_after_idle_time = "48h"
minimum_heartbeat_latency = "1h"
max_tool_rounds = 12
wrap_up_grace_rounds = 3
```

Autonomy requires the master switch. Heartbeat controls private autonomous ticks. All duration fields accept strings like `"30s"`, `"15m"`, `"2h"`, and `"48h"`.

`max_tool_rounds` is the normal tool-use budget per heartbeat tick. When that budget (or the wall-clock loop deadline) is reached without natural termination, the daemon appends a wrap-up nudge that asks the character to record any unfinished work into `HEARTBEAT.md` and respond `HEARTBEAT_OK` (or send a final `<sendMessage>`). `wrap_up_grace_rounds` is the additional tool-use budget granted after that nudge so the model can finish the wrap-up turn. Total worst-case rounds per tick = `max_tool_rounds + wrap_up_grace_rounds`. Notes the model leaves in `HEARTBEAT.md` are read into the prompt at the start of every subsequent heartbeat.

## `[behavior.tool_use]`

```toml
[behavior.tool_use]
enabled = true
max_iterations = 10

[behavior.tool_use.tools]
web_search = true
fetch_url = true
generate_image = true
check_time = true
roll_dice = true
activity_heatmap = true
read = true
write = true
edit = true
list_files = true
search = true
delete = true
search_history = true
exec = true
```

All tools default to enabled. Set `enabled = false` to disable tool use entirely.

- In private conversations, `search_history` and `exec` are hidden.
- Workspace file tools (`read`, `write`, `edit`, `list_files`, `search`, `delete`) treat `memory/...` as an ordinary workspace subdirectory.

Legacy config keys such as `memory_search` and `memory_list` may still parse as
tool toggles, but they are compatibility keys and are not registered LLM tools.
Legacy keys like `memory`, `memory_read`, and `memory_write` are also inert; use
tool-name toggles directly (`search_history = false`, `read = false`, etc.).
There is no `send_image` toggle for uploaded attachments; generated-image
sending is controlled by `generate_image`.

`exec` is allowlisted and argument-sandboxed. Path-like arguments must stay inside the character workspace.

### `[behavior.tool_use.search]`

```toml
[behavior.tool_use.search]
api_key_env = "TAVILY_API_KEY"
max_results = 5
search_depth = "basic"
include_answer = true
```

## `[memory.compaction]`

```toml
[memory.compaction]
enabled = true
idle_trigger = "30m"
min_turns = 8
max_turns = 16
max_context_tokens = 200000
keep_recent_turns = 2
```

Compaction writes markdown memory notes, archives old turns, and activates staged prompt-visible edits. It also updates `MEMORY.md` with the conversational throughline so the next conversation can pick up where this one left off; dreaming reorganizes the index later. When the autonomy manager has a cached chat request, compaction reuses that prefix and appends only the carry-forward instruction (the trailing `role:"system"` message is wrapped to a `<system_instruction>` user turn by the Anthropic provider), preserving the live conversation's prompt cache. After compaction, cache keepalive keeps its existing deadline and rebuilds the request from disk if needed, so stable pinned system prompt sections can stay warm even though the old conversation tail was discarded.

## `[memory.dreaming]`

```toml
[memory.dreaming]
enabled = false
frequency = "0 3 * * *"
max_tool_rounds = 12
```

`frequency` is a five-field cron schedule: `minute hour day-of-month month day-of-week`.
It supports `*`, lists, ranges, steps, month/day names, and `0` or `7` for Sunday;
for example, `0 6 * * 1` runs Mondays at 06:00.

Dreaming is opt-in and requires `[behavior.autonomy].enabled = true`. It runs independently of heartbeat as a private AI librarian pass. The character uses memory tools to inspect the existing flexible markdown layout, consolidate and dedupe durable notes, mark stale/superseded material, and update the canonical `MEMORY.md`. The daemon writes a timestamped audit entry to the dreams log automatically once the pass finishes — the model itself does not write `DREAMS.md`. Dreaming may also edit the protected prompt files (`SOUL.md`, `USER.md`, `AGENTS.md`, `TOOLS.md`, `HEARTBEAT.md`); those edits are staged through the active-prompt snapshot and take effect at the next compaction/reload boundary. When a cached chat request is available, the private librarian instruction is appended after that request prefix so the existing provider-side prompt cache can be reused.

`MEMORY.md` is the index/map and replaces the old recap/digest concept. Normal chat reads `active_prompt/MEMORY.md`; edits to `workspace/MEMORY.md` only become prompt-active after compaction/reload. It should not duplicate `USER.md` or `AGENTS.md`, which remain pinned prompt files.

The dreams audit log lives at `$XDG_DATA_HOME/shore/<Character>/DREAMS.md` (data dir, not workspace) so it never bleeds into prompts or memory snapshots. Use `shore memory dreams [--limit N]` to inspect recent entries. Machine-readable dreaming staging/debug state is written under `$XDG_DATA_HOME/shore/<Character>/dreams/`.

## Advanced Diagnostics

`[advanced].api_payload_logging = true` writes per-call provider request and
response JSON under `$XDG_CACHE_HOME/shore/debug/api_logs/`. These files are
diagnostic payload dumps, not durable user state.

`[advanced].cache_forensics = true` writes Anthropic prompt-cache forensic
events under `$XDG_CACHE_HOME/shore/cache_forensics.jsonl`. The durable cache
state and anomaly summary remain in the usage ledger.

## `[memory.retrieval]`

```toml
[memory.retrieval]
mode = "auto" # auto, lexical, hybrid
max_file_bytes = 2097152
max_indexed_files = 50000
max_total_indexed_bytes = 1073741824
max_embed_chars_per_file = 4000
binary = "skip" # skip, metadata, try_embed
```

- `lexical` never calls embeddings.
- `auto` uses hybrid retrieval when an embedder is resolved and usable.
- `hybrid` requests semantic+keyword ranking but falls back to lexical if embeddings fail.
- Lexical and hybrid both scan the workspace tree (including `memory/`) for
  text files.
- `max_file_bytes` controls the per-file size cap for lexical/hybrid search.
- `max_indexed_files` and `max_total_indexed_bytes` bound workspace index walks.
- `max_embed_chars_per_file` limits how much text from each file is embedded.
- `binary` controls non-UTF8 handling for indexing:
  - `skip`: skip binary files.
  - `metadata`: track binary files as skipped metadata only.
  - `try_embed`: reserve space for future binary embedders; current OpenAI-compatible text embedders still record these as unsupported.

The hybrid index is rebuildable and non-authoritative.

## `[memory.thinking]`

```toml
[memory.thinking]
preserve_prior_turns = true
```

Default `true` keeps prior-turn thinking/redacted-thinking blocks in outgoing requests. Set `false` to strip them and save the tokens they consume on each subsequent turn — only safe with providers that don't depend on prior-turn thinking (e.g. Anthropic Claude 4.x). DeepSeek V3.1+ and Moonshot Kimi-thinking reject requests that omit prior `reasoning_content` while in thinking mode and ignore the user setting. In-progress tool-loop thinking is always preserved.

## `[notifications]`

```toml
[notifications]
enabled = false
backend = "notify_send"
generation_threshold = "0s"
```

Backends include `notify_send`, `ntfy`, and `command`.

## `[tts]`

```toml
[tts]
enabled = false
host = "127.0.0.1"
port = 8778
model = "tts-1"
voice = "alloy"
```

Used by `shore speak` and live-speak mode. Shore does not run a speech model
itself; the daemon proxies to an OpenAI-compatible TTS server at
`http://{host}:{port}/v1/audio/speech`.

Requests include `model`, `input`, `voice`, and `response_format = "wav"`.
The server must return WAV audio because Shore strips the WAV header and relays
PCM chunks to the CLI/TUI audio player. If `voice` is unset, Shore sends the
character name as the voice, which is convenient only when the TTS server has a
matching voice configured.

For a local or LAN TTS server, the usual shape is:

```toml
[tts]
enabled = true
host = "vegetable"
port = 8778
model = "tts-1"      # or the model name your server expects
voice = "alloy"      # or a voice installed on your server
```

A `400 Bad Request` from `/v1/audio/speech` usually means the TTS server
rejected the requested `model`, `voice`, or response format.

## `[connections.matrix]`

External mode connects to an existing homeserver:

```toml
[connections.matrix]
enabled = true
homeserver = "https://matrix.example.com"
user_id = "@shore:example.com"
room_id = "!room:example.com"
trusted_user = "@user:example.com"
```

Embedded mode manages a conduwuit-compatible homeserver:

```toml
[connections.matrix]
trusted_user = "@user:shore.local"

[connections.matrix.embedded]
server_name = "shore.local"
bind_address = "127.0.0.1"
port = 6167
admin_password = "change-me"
```

## Validation

```sh
shore config --check
shore config
shore config --path
```
