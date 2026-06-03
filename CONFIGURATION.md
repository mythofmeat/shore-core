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
settings, usage budgets, autonomy config, and merged per-character config for
future work. It keeps the previous runtime config if the changed files fail to
parse or validate.

Startup-owned settings still require a daemon restart, including `[daemon]`
listener settings, `[connections.matrix]`, `[notifications]`, `[services]`,
and startup-only `[advanced]` diagnostics toggles. Shore logs these as
restart-required when it sees them change.

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
```

Non-loopback binds require `unsafe_allow_remote_access = true`. `allowed_hosts` is only a source-IP filter, not auth or TLS.

## `[defaults]`

```toml
[defaults]
model = "claude-sonnet"           # initial chat model when a character has none selected
embedding = "openai:text-embedding-3-large"
image_generation = "openai:dall-e-3"
display_name = "Ren"
stream = true

# Optional: pin background tasks (heartbeat/compaction/dreaming) to a
# specific model. When this section is omitted, background tasks follow
# whichever model the character is currently using for chat.
[defaults.background]
model = "haiku"                   # blanket model for every background task
# heartbeat = "haiku-fast"        # per-task overrides (optional)
# compaction = "claude-sonnet"
# dreaming = "claude-sonnet"
```

Chat/tool selectors are aliases declared under `[chat.*]` or `[tools.*]`.
`embedding` and `image_generation` are bare `provider:model_id` identities
(see [Embedding](#embedding) and [Image generation](#image-generation)).

Important slots:

- `model` — chat default. Optional: if unset, chat starts on the first chat model declared in the catalog. Also acts as a late-stage fallback for background tasks (see below).
- `[defaults.background]` — heartbeat, compaction, and dreaming selectors. Each task chains `background.<task> → background.model → active chat model → defaults.model → first chat model`. When no background-specific model is configured, background work tracks the character's current chat selection, so `shore model <name>` moves heartbeat/compaction/dreaming alongside chat. Set `background.model` (or a per-task key) to pin background to a different model regardless of chat selection.
- `embedding` — optional hybrid retrieval model, as `provider:model_id` (e.g. `openai:text-embedding-3-small`)
- `image_generation` — image generation model, as `provider:model_id`

> **Deprecated:** the older top-level `defaults.heartbeat` and `defaults.dreaming` keys still parse but emit a deprecation warning and are forwarded into `[defaults.background]` at load time. Move them under `[defaults.background]` to silence the warning.

## Model Sections

Chat/tool models:

```toml
[chat.anthropic.claude-sonnet]
model_id = "claude-sonnet-4-6"
api_key_env = "ANTHROPIC_API_KEY"
cache_ttl = "1h"
max_output_tokens = 4096
max_context_tokens = 200000

[chat.openrouter.haiku]
model_id = "anthropic/claude-haiku-4-5"
api_key_env = "OPENROUTER_API_KEY"
base_url = "https://openrouter.ai/api/v1"
```

### Embedding

Embedding and image generation share the chat model shape: identity is a
bare `provider:model_id`, transport (`sdk`/`base_url`/`api_key_env`/`keys`)
comes from `[providers.<provider>]`, and an **optional** settings table
keyed by the same `provider:model_id` carries category knobs only. There is
no `[embedding.<alias>]` profile table — select the model with
`defaults.embedding`.

Shore only ships an OpenAI-compatible embedder; any endpoint that speaks
`/v1/embeddings` works (OpenAI, Together, Voyage's compat endpoint,
OpenRouter, or a self-hosted server like text-embedding-inference or
llama.cpp's HTTP server).

```toml
[defaults]
embedding = "openai:text-embedding-3-large"

# Transport + credentials (multi-key fallback supported via [[keys]]).
[providers.openai]
api_key_env = "OPENAI_API_KEY"

# Optional: per-model category settings (only `dimensions`).
[embedding."openai:text-embedding-3-large"]
dimensions = 1024
```

```toml
# Self-hosted (e.g. text-embedding-inference) — register it as a provider.
# `api_key_env` still has to point at a set variable; if your server doesn't
# validate keys, set it to any non-empty value.
[defaults]
embedding = "local-tei:BAAI/bge-large-en-v1.5"

[providers.local-tei]
base_url = "http://127.0.0.1:8080/v1"
api_key_env = "TEI_API_KEY"

[embedding."local-tei:BAAI/bge-large-en-v1.5"]
dimensions = 1024
```

When no embedding model is configured (and `defaults.embedding` doesn't
reference one), the workspace `search` tool's `hybrid` and `vector` modes
degrade to lexical-only at the call site. Configure an embedding model to
enable semantic search.

### Image generation

Same shape — identity `provider:model_id`, transport on `[providers.*]`,
optional settings table with `size`, `quality` (OpenAI), `aspect_ratio`,
and `image_size` (OpenRouter).

```toml
[defaults]
image_generation = "openai:dall-e-3"

[providers.openai]
api_key_env = "OPENAI_API_KEY"

[image_generation."openai:dall-e-3"]
size = "1024x1024"
quality = "hd"
```

> **Migration:** the older flat `[embedding.<alias>]` /
> `[image_generation.<alias>]` tables (with inline `model_id`/`provider`/
> `api_key_env`/`base_url`) were removed. Move identity into the
> `provider:model_id` key, transport onto `[providers.<provider>]`, and keep
> only category settings in the table. A leftover flat block now fails config
> load with a migration error.

## Providers

Provider entries replace per-model `api_key_env` duplication and unlock
runtime model discovery. Static `[chat.<provider>.<alias>]` entries keep
working unchanged alongside the registry — they never require migration.

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
Friendly key names are usage metadata only; Shore never sends them to providers
or logs key values. `shore usage --by-api-key` groups ledger spend by these
names, and `shore usage --api-key overflow` filters to one key.

### Provider-wide defaults

Provider-level behavioral and vendor knobs live under `[providers.<name>.defaults]`:

```toml
[providers.or-anthropic]
sdk = "anthropic"
api_key_env = "OR_KEY"

[providers.or-anthropic.defaults]
max_output_tokens = 8192
openrouter_provider = { order = ["Anthropic"] }
```

This is the same field set as the per-model overlay (`[models."provider:model_id"]`
in `preferences/`), applied provider-wide as the lowest user-config tier. It carries
behavioral defaults (`max_output_tokens`, `cache_ttl`, sampler knobs) and vendor knobs
(`openrouter_provider`, `vertex_*`, `gemini_*`, `zai_*`) onto every model the provider
resolves — discovered, trusted (`provider:model_id`), or static. Transport
(`sdk`/`base_url`/`api_key_env`/`keys`) belongs on the provider entry itself and is
rejected inside `[.defaults]`.

> Provider-level scalars under `[chat.<provider>]` were retired — move behavioral
> defaults to `[providers.<provider>.defaults]` and transport to
> `[providers.<provider>]`. Per-model `[chat.<provider>.<alias>]` fields are
> unaffected.

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
Providers with `sdk = "anthropic"` use Anthropic's native `GET /v1/models`
API. Other providers use the OpenAI-compatible `GET <base_url>/models`
shape. Well-known provider keys with default base URLs, including
`anthropic`, `openai`, and `openrouter`, can omit `base_url`; custom
providers need it.
Hidden models stay in the cache but are filtered out of `shore model` and
`shore provider models <name>` until `--all` (CLI) or `:model all` (TUI)
is used. Manual `[chat.<provider>.<alias>]` entries are never filtered —
they are intentional.

### Effective catalog and merge order

At runtime the daemon resolves models against an effective catalog
that merges three sources:

1. Static `[chat.<provider>.<alias>]` entries (this file).
2. Discovered `[providers.<name>]` cache rows.
3. Trusted `provider:model_id` refs (routed via `[providers.<name>]` transport).
4. `[providers.<name>.defaults]` provider-wide behavioral/vendor defaults.
5. Hardcoded provider defaults for well-known providers.

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
3. `[providers.<provider>.defaults]` provider-wide defaults.
4. Static `[chat.<provider>.<alias>]` overrides.
5. Saved global preferences (`preferences/global.toml`).
6. Saved per-character preferences (`characters/<C>/preferences/models.toml`).

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
max_result_chars = 20000  # Truncate each tool result past this many characters; 0 disables

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

`max_result_chars` caps how many characters a single tool result may contribute
to the conversation. It defaults to `20000` (~5k tokens of code-like output);
set it to `0` to disable truncation and preserve full tool output. When a result
exceeds the limit it is cut at a character boundary and a one-line notice
(`[tool_result truncated: showing first N of M characters]`) is appended so the
model knows output was dropped. The truncation is persisted, so the shortened
result — not the original — is what gets replayed on later turns, capping its
context cost for the rest of the conversation.

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
max_tool_rounds = 12
```

Compaction writes markdown memory notes, archives old turns, and activates staged prompt-visible edits. It also updates `MEMORY.md` with the conversational throughline so the next conversation can pick up where this one left off; dreaming reorganizes the index later. When the autonomy manager has a cached chat request, compaction reuses that prefix and appends only the carry-forward instruction (the trailing `role:"system"` message is wrapped to a `<system_instruction>` user turn by the Anthropic provider), preserving the live conversation's prompt cache. After compaction, cache keepalive keeps its existing deadline and rebuilds the request from disk if needed, so stable pinned system prompt sections can stay warm even though the old conversation tail was discarded.

Compaction runs a tool loop: the model calls `write` / `edit` on files under `memory/` and on the workspace-root `MEMORY.md`. Writes to any other path (`SOUL.md`, `USER.md`, `DREAMS.md`, paths outside `memory/`, etc.) are rejected at the dispatch wrapper. `max_tool_rounds` caps how many tool-use rounds a single pass may run. If the pass finishes with **zero** allowed memory writes — because the model used only read-only tools, only attempted disallowed paths, or hit `max_tool_rounds` — the active conversation is **not** archived and the next trigger will retry. This is by design: silent "archive with no writes" was the failure mode of the pre-tool-loop XML path.

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
response JSON under `$XDG_CACHE_HOME/shore/debug/api_logs/` for chat traffic
and `$XDG_CACHE_HOME/shore/debug/api_logs_long/` for background tasks
(compaction, dreaming, heartbeat). These files are diagnostic payload dumps,
not durable user state. Rotation is operator-managed; the split lets you run
different retention timers on the two tiers — chat churns fast and is rarely
useful beyond a few days, while background payloads stay valuable for
weeks-long forensic analysis. Example cron:

```sh
find ~/.cache/shore/debug/api_logs/      -type f -mtime +3  -delete
find ~/.cache/shore/debug/api_logs_long/ -type f -mtime +30 -delete
```

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

This value is the **global fallback**. The quality effect is model-dependent — for example Claude Opus 4.8 is reproducibly better with it OFF, while minimax-m3 / glm-5.1 want it ON — so it can be overridden **per model** through the preference overlay (`shore model setting preserve_prior_turns <bool>` / `:setting preserve_prior_turns <bool>`, the same path as `reasoning_effort` etc.). An unset per-model value inherits this global default; there is no auto-promotion in either direction. The DeepSeek/Kimi reasoning-replay floor is enforced regardless of either setting.

## `[notifications]`

```toml
[notifications]
enabled = false
backend = "notify_send"
generation_threshold = "0s"
```

Backends include `notify_send`, `ntfy`, and `command`.

Per-event toggles live under `[notifications.events]`. Usage budget threshold
crossings use `usage_warning = true` by default.

## `[usage]`

Usage budgets are evaluated from the SQLite ledger that powers `shore usage`.
`shore usage --budget` prints budget state and spike warnings; `shore usage
--json` and `shore usage --budget --json` return machine-readable JSON.

```toml
[usage]
timezone = "local"                  # "local" or "utc"; default: local
allow_compaction_over_budget = true # default: true

[[usage.budgets]]
name = "daily total"
period = "day"                      # hour, day, week, month
cost_usd = 10.00
warn_at = [0.5, 0.8, 1.0]
limit = "warn"                      # warn, block, pause_background

[[usage.budgets]]
name = "background"
period = "day"
usage_kind = ["heartbeat", "dreaming"]
cost_usd = 2.00
limit = "pause_background"

[[usage.budgets]]
name = "overflow key monthly"
period = "month"
provider = "openrouter"
api_key = "overflow"
cost_usd = 25.00
limit = "block"
allow_compaction_over_budget = false # optional per-budget override

[[usage.budgets]]
name = "billing cycle"
period = "month"
cost_usd = 200.00
reset_day_of_month = 15              # 1-31; clamps to last day on short months
reset_hour = 0                       # 0-23; default 0

[[usage.budgets]]
name = "work week"
period = "week"
cost_usd = 50.00
reset_day_of_week = "thursday"       # monday..sunday; default monday
reset_hour = 6

[usage.spike_warnings]
enabled = true
period = "hour"
multiplier = 3.0
min_cost_usd = 1.00
```

Budget filters are optional and combine with AND semantics. Supported filters
are `character`, `provider`, `api_key`, `model`, `call_type`, and
`usage_kind`. `usage_kind` accepts the grouped names from `shore usage
--by-kind`, such as `message_no_tools`, `message_with_tools`, `heartbeat`,
`compaction`, and `dreaming`.

`limit = "warn"` reports status only. `limit = "block"` rejects matching LLM
calls once committed ledger spend has reached the limit. `limit =
"pause_background"` rejects matching background calls after the limit while
leaving user chat available. Shore does not interrupt an in-flight generation;
limits are checked before starting the next LLM call. Compaction is allowed
over budget by default because reducing prompt context can lower the next chat
turn's cost; set `allow_compaction_over_budget = false` globally or on a
specific budget for a strict stop.

Custom reset anchors let a budget align to a billing cycle, work week, or
"my day starts at 6am" schedule instead of the default top-of-period
boundary. All anchor fields are optional; defaults preserve the historical
behavior (midnight, Monday, the 1st).

| Field | Valid on `period =` | Range | Default |
| --- | --- | --- | --- |
| `reset_hour` | `day`, `week`, `month` | 0–23 | 0 |
| `reset_day_of_week` | `week` | `monday`..`sunday` | `monday` |
| `reset_day_of_month` | `month` | 1–31, clamped to the last day on short months | 1 |

A month budget with `reset_day_of_month = 31` resets on the last calendar
day of months shorter than 31 (Feb 28/29, Apr 30, etc.), so every month
gets exactly one reset.

When committed spend crosses a `warn_at` threshold, the daemon emits one
`usage_warning` server frame to the active requester and fires the
`usage_warning` notification event. Threshold warnings are de-duped per budget,
period window, and threshold — with one exception: once committed spend is at
or above 100% of the budget, the warning re-fires on every subsequent check so
the operator keeps seeing the over-limit signal as spend continues to accrue.
The re-fired event carries `crossed_warn_at = [1.0]` regardless of which
`warn_at` thresholds the user configured.

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

When the daemon supervises `shore-matrix`, it sets the bridge process log filter
from `SHORE_MATRIX_RUST_LOG`. The default keeps Shore bridge lifecycle logs but
suppresses routine Matrix SDK sync and key-backup chatter:
`warn,shore_matrix=info,matrix_sdk_crypto::backups=error`.

## Validation

```sh
shore config --check
shore config
shore config --path
```
