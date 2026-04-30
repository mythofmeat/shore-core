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
```

Non-loopback binds require `unsafe_allow_remote_access = true`. `allowed_hosts` is only a source-IP filter, not auth or TLS.

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

Embedding profile:

```toml
[embedding.text-large]
provider = "openai"
model_id = "text-embedding-3-large"
api_key_env = "OPENAI_API_KEY"
```

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

Keys are tried in configured order on every request. When the daemon
falls back away from a key marked `warn_on_fallback = true` (e.g. an
exhausted budget cap), a one-line client warning surfaces. The
fallback is non-sticky: the next request retries from the top of the
list.

### Discovery and visibility

```toml
[providers.openrouter.discovery]
enabled = true
# gitignore-style; last match wins. `*` opt-out, `!pattern` opt-in.
visibility = [
  "*",
  "!anthropic/*",
  "!openai/*",
  "!google/gemini-*",
]
```

Discovered models populate the cache at
`$XDG_DATA_HOME/shore/providers/<name>/models.json`. Run
`shore provider refresh <name>` to update it. Hidden models stay in the
cache but are filtered out of `shore model` and `shore provider models
<name>` until `--all` (CLI) or `:model all` (TUI) is used. Manual
`[chat.<provider>.<alias>]` entries are never filtered — they are
intentional.

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
```

Autonomy requires the master switch. Heartbeat controls private autonomous ticks. All duration fields accept strings like `"30s"`, `"15m"`, `"2h"`, and `"48h"`.

## `[behavior.tool_use]`

```toml
[behavior.tool_use]
enabled = true
max_iterations = 10

[behavior.tool_use.tools]
memory = true
memory_read = true
memory_write = true
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
search_history = true
exec = true
```

All tools default to enabled. Set `enabled = false` to disable tool use entirely.

Memory gates:

- `memory = false` blocks `memory/...` workspace paths and disables conversation history search.
- `memory_read = false` blocks `read`, `list_files`, and `search` access to `memory/...` paths and disables `search_history`.
- `memory_write = false` blocks `write` and `edit` access to `memory/...` paths.
- `exec` is hidden when memory read/write access is not both enabled.

Legacy config keys such as `memory_search` and `memory_list` may still parse as
tool toggles, but they are compatibility keys and are not registered LLM tools.
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

Compaction writes markdown memory notes, archives old turns, and activates staged prompt-visible edits. It does not write `MEMORY.md`; dreaming maintains the canonical index, and compaction activates its prompt snapshot.

## `[memory.dreaming]`

```toml
[memory.dreaming]
enabled = false
frequency = "0 3 * * *"
max_tool_rounds = 12
```

Dreaming is opt-in and requires `[behavior.autonomy].enabled = true`. It runs independently of heartbeat as a private AI librarian pass. The character uses memory tools to inspect the existing flexible markdown layout, consolidate and dedupe durable notes, mark stale/superseded material, update the canonical `MEMORY.md`, and write an audit entry to `DREAMS.md`. When a cached chat request is available, the private librarian instruction is appended after that request prefix so the existing provider-side prompt cache can be reused.

`MEMORY.md` is the index/map and replaces the old recap/digest concept. Normal chat reads `active_prompt/MEMORY.md`; edits to `workspace/MEMORY.md` only become prompt-active after compaction/reload. It should not duplicate `USER.md` or `AGENTS.md`, which remain pinned prompt files. `DREAMS.md` is review output, not long-term memory. Machine-readable staging/debug state is written under `$XDG_DATA_HOME/shore/<Character>/dreams/`. Dreaming excludes generated artifacts from ordinary memory-source ingestion, including legacy `.dreams/**`, `DREAMS.md`, `dreams.md`, `MEMORY.md`, and `dreaming/**`.

## `[memory.retrieval]`

```toml
[memory.retrieval]
mode = "auto" # auto, lexical, hybrid
```

- `lexical` never calls embeddings.
- `auto` uses hybrid retrieval when an embedding profile is configured and usable.
- `hybrid` requests semantic+keyword ranking but falls back to lexical if embeddings fail.

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
voice = "alloy"
```

Used by `shore speak` and live-speak mode.

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
