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
model = "claude-sonnet"
heartbeat = "haiku"
dreaming = "claude-sonnet"
embedding = "text-large"
image_generation = "image"
display_name = "Ren"
stream = true
```

Selectors are aliases declared under `[chat.*]`, `[tools.*]`, `[embedding.*]`, or `[image_generation.*]`.

Important slots:

- `model` — normal conversation and conversation-to-memory compaction
- `heartbeat` — autonomous heartbeat ticks
- `dreaming` — private AI memory librarian passes; falls back to `model`
- `embedding` — optional hybrid retrieval profile
- `image_generation` — image generation profile

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

`MEMORY.md` is the index/map and replaces the old recap/digest concept. Normal chat reads `active_prompt/MEMORY.md`; edits to `workspace/memory/MEMORY.md` only become prompt-active after compaction/reload. It should not duplicate `USER.md` or `AGENTS.md`, which remain pinned prompt files. `DREAMS.md` is review output, not long-term memory. Machine-readable staging/debug state is written under `.dreams/`. Dreaming excludes generated artifacts from ordinary memory-source ingestion, including `.dreams/**`, `DREAMS.md`, `dreams.md`, `MEMORY.md`, and `dreaming/**`.

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
preserve_prior_turns = false
```

Default `false` strips prior-turn thinking/redacted-thinking blocks from future outgoing requests while preserving in-progress tool-loop thinking where providers require it.

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
