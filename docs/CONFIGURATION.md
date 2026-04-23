# Configuration

Where every Shore setting lives, what it does, and when to change it. For the exhaustive option list see [`examples/config.toml`](../examples/config.toml).

## Orientation

Shore reads all configuration from `$XDG_CONFIG_HOME/shore/` (defaults to `~/.config/shore/`). A minimal install needs one file (`config.toml`) and one character workspace (`characters/<Name>/workspace/SOUL.md`).

### Directory layout

```
~/.config/shore/
├── config.toml                  # main configuration — required
└── characters/
    └── <CharacterName>/
        ├── config.toml          # character-specific config override — optional
        └── workspace/
            ├── SOUL.md          # required (presence enables discovery)
            ├── USER.md          # character-specific user context — optional
            ├── AGENTS.md        # system prompt template override — optional
            ├── TOOLS.md         # extra tool-use guidance — optional
            ├── HEARTBEAT.md     # heartbeat-only guidance — optional
            └── memory/
                └── ...          # durable markdown memory files
```

Characters are discovered by scanning `characters/` for subdirectories containing `workspace/SOUL.md`. No config entry is needed to register a character. Legacy `character.md` layouts are auto-migrated on first load.

### Splitting configuration across files

Two mechanisms let you split config out of the main `config.toml`:

- `include = ["extra.toml", "another.toml"]` at the top of `config.toml` — explicit, order-preserving includes.
- `conf.d/*.toml` — any `.toml` file dropped in `~/.config/shore/conf.d/` is auto-loaded.

Later files override earlier ones. `conf.d/` is loaded in filename order.

### Precedence

For the settings that accept multiple sources, the order (highest wins):

1. CLI flag — `shore-daemon --addr ...`, `shore --character ...`, `shore-daemon --config <path>`
2. Environment variable — `SHORE_ADDR`, `SHORE_CHARACTER`
3. Config file — `[daemon].addr`, etc.

If you pass `--config <path>` the file must already exist. Shore no longer silently creates a default config at an arbitrary operator-supplied path.

Remote-access safety is enforced against the *final* resolved address, so a CLI or env override binding to a non-loopback address still requires `[daemon].unsafe_allow_remote_access = true`.

## Environment variables

Shore reads these environment variables. API keys are read on demand by each provider.

| Variable | Used by | Purpose |
|---|---|---|
| `ANTHROPIC_API_KEY` | `[chat.anthropic.*]` | Anthropic API authentication |
| `OPENROUTER_API_KEY` | `[chat.openrouter.*]` | OpenRouter API authentication |
| `DEEPSEEK_API_KEY` | `[chat.deepseek.*]` | DeepSeek API authentication |
| `GEMINI_API_KEY` | `[chat.gemini.*]` | Google Gemini API authentication |
| `XAI_API_KEY` | `[chat.xai.*]` | xAI Grok API authentication |
| `ZAI_API_KEY` | `[chat.zhipuai.*]` | ZhipuAI API authentication |
| `TAVILY_API_KEY` | `behavior.tool_use.search` | Web search backend |
| `SHORE_ADDR` | daemon + clients | Bind / target address; overrides config file, overridden by `--addr` |
| `SHORE_CHARACTER` | CLI / TUI | Default character to talk to; overridden by `--character` |
| `XDG_CONFIG_HOME` | startup | Where Shore looks for `~/.config/shore/` (standard XDG) |
| `XDG_DATA_HOME` | startup | Where Shore stores persistent data (standard XDG) |

Individual providers may support additional env vars for per-model overrides — see [`examples/config.toml`](../examples/config.toml) for the full list.

## `[daemon]`

Controls how the daemon binds and who can reach it. By default the daemon is localhost-only; you have to opt in to remote binds explicitly.

**When to change:** only when you want to reach the daemon from another machine on a trusted private network (Tailscale, WireGuard, a VPN).

```toml
[daemon]
addr = "127.0.0.1:7320"
# unsafe_allow_remote_access = true
# allowed_hosts = ["100.64.0.2"]
```

- `addr` — listen address. `--addr` and `SHORE_ADDR` override this (see [Orientation](#orientation)).
- `unsafe_allow_remote_access` — **required** for any non-loopback bind. Without it Shore refuses to start.
- `allowed_hosts` — source-IP allowlist. An allowed host can connect without any further check.

*This is unauthenticated TCP.* `allowed_hosts` is not authentication; there is no TLS. Use only on private/overlay networks you already trust. See [`examples/config.toml`](../examples/config.toml) for every daemon option.

## `[defaults]`

Defaults that apply when a command doesn't override them. Most users set `model` and `display_name` and leave the rest alone.

```toml
[defaults]
model = "claude-sonnet"       # must match a key in [chat.*.*]
display_name = "Your Name"    # fills `{{user}}` in templates
# stream = true
# tool_model = "mistral-small"
# memory_query = "mistral-small"
# compaction = "mistral-small"
# heartbeat = "claude-sonnet"
# image_generation = "gemini-flash"
```

**Per-operation model slots** let you run heavy work (the main conversation) on one model and cheap background work on another. Each slot, if omitted, falls back to `model`:

- `tool_model` — used when the character is invoking tools (web search, memory, etc.)
- `memory_query` — the small model used for markdown memory question answering
- `compaction` — conversation summarization into markdown memory
- `heartbeat` — the "private moment" autonomous ticks
- `image_generation` — which model handles `generate_image`

Any value passed here must match an alias declared under `[chat.<provider>.<alias>]`.

See [`examples/config.toml`](../examples/config.toml) for every default.

## `[behavior.autonomy]`

Controls whether the character speaks on its own. Disabled by default. Autonomy in Shore is implemented via **heartbeat** — self-scheduled private ticks where the character can think, use tools, and optionally send you a message. Cache keepalive is not part of autonomy; it is an Anthropic prompt-cache cost-saving subsystem that runs separately from heartbeat behavior.

See [FEATURES.md — Autonomy](FEATURES.md#autonomy) for what this actually does. This section is the config reference.

### Active vs dormant

The character has two phases: **active** (responsive, may schedule heartbeat ticks) and **dormant** (silent; wakes on a user message). The character goes dormant after `dormant_after_heartbeat_turns` ticks with no user reply, or after `dormant_after_idle_time` of total silence.

### `[behavior.autonomy]` — the umbrella

```toml
[behavior.autonomy]
enabled = false   # master switch for autonomous speech
```

Only one top-level field. Everything else lives under `heartbeat`.

### `[behavior.autonomy.heartbeat]` — self-scheduled private ticks

```toml
[behavior.autonomy.heartbeat]
enabled = true
fallback_heartbeat_interval = "1h"      # base cadence when the character doesn't self-schedule
dormant_after_heartbeat_turns = 3       # consecutive ticks with no user reply before sleeping
dormant_after_idle_time = "48h"           # hard idle ceiling before sleeping until user returns
minimum_heartbeat_latency = "1h"        # floor between a user message and the next tick
max_tool_rounds = 12                      # tool-use rounds per tick before forcing a wrap-up recap
```

The character schedules its own next tick when it finishes one; `fallback_heartbeat_interval` only applies when it doesn't.

`minimum_heartbeat_latency` prevents ticks from firing the second you stop typing — the character needs breathing room.

`max_tool_rounds` is a safety limit — if a tick wanders, Shore forces a wrap-up recap at this many tool rounds.

All time fields accept human durations (`"30s"`, `"15m"`, `"2h"`, `"48h"`).

See [`examples/config.toml`](../examples/config.toml) for every option.

## `[behavior.tool_use]`

Controls which tools the character can call mid-response. Every tool is enabled by default; disable selectively if a tool is expensive or inappropriate for your character.

See [FEATURES.md — Tool use](FEATURES.md#tool-use) for what each tool actually does.

```toml
[behavior.tool_use]
enabled = true
max_iterations = 10   # max tool-call rounds per turn before forcing a final response

[behavior.tool_use.tools]
memory = true
memory_read = true
memory_write = true
memory_search = true
memory_list = true
send_image = true
generate_image = true
web_search = true
fetch_url = true
check_time = true
roll_dice = true
activity_heatmap = true
scratchpad_list = true     # browse the character's persistent scratchpad
scratchpad_read = true
scratchpad_write = true
scratchpad_delete = true
read = true                # workspace file read
write = true               # workspace file write
edit = true                # workspace file edit
list_files = true          # workspace file listing
exec = true                # allowlisted workspace commands
```

**When to change:**
- Set `enabled = false` to disable tool use entirely.
- Set `memory = false` to disable all memory tools and block `memory/...` access through workspace tools.
- Granular `memory_read` / `memory_write` also gate workspace `memory/...` reads and writes; `exec` is hidden unless both are enabled.
- Drop individual tool toggles to `false` when you want the character to not have access (e.g. `generate_image = false` if you don't have image-gen credits).
- Lower `max_iterations` if the character is going in circles; raise it if complex tasks need more rounds.

### `[behavior.tool_use.search]` — web search backend

```toml
[behavior.tool_use.search]
api_key_env = "TAVILY_API_KEY"
max_results = 5
search_depth = "basic"       # "basic" or "advanced"
include_answer = true
```

Shore uses [Tavily](https://tavily.com/) for web search. `api_key_env` names the environment variable holding the key (default `TAVILY_API_KEY`).

See [`examples/config.toml`](../examples/config.toml) for every tool-use option.

## `[memory]`

Controls the memory subsystem's background work. Runtime memory is markdown-only and controlled by the `memory*` tool toggles above. This table tunes when old conversation turns are compacted into memory files.

See [FEATURES.md — Memory](FEATURES.md#memory) for the runtime memory model.

### `[memory.compaction]`

```toml
[memory.compaction]
enabled = true
idle_trigger = "30m"       # trigger after this much inactivity
min_turns = 8              # don't compact below this many user turns
max_turns = 16             # force compaction at this many user turns
max_context_tokens = 200000 # force compaction when last turn's prompt
                           # context (input + cache_read + cache_creation)
                           # reaches this many tokens; 0 disables
keep_recent_turns = 2      # user turns retained verbatim after compaction
```

Compaction condenses old conversation turns into durable markdown memory files. Before writing, the compaction model sees a bounded snapshot of existing memory files so it can merge and update instead of blindly creating duplicates. `idle_trigger` is how long the session must be idle before compaction kicks in; `min_turns` / `max_turns` bracket when it's allowed to run; `keep_recent_turns` controls how much recent conversation stays verbatim.

`max_context_tokens` is a cost-driven trigger complementary to `max_turns`: per-turn content varies wildly (heavy-thinking turns are several times larger than light chat), so turn count is a poor proxy for context cost. The trigger fires when the just-completed turn's prompt tokens cross the threshold (still floored by `min_turns` to prevent thrash). The default **200000** acts as a context-window safety net (matching Claude 4.x's 200K ceiling) rather than a cost optimiser. For actual cost savings on Opus 4.7, lower it to around **30000** — the per-call cost curve has an elbow near 30K where median cost roughly doubles. Tune for your model and conversation shape; recorded call sizes are in the ledger CSV (`shore usage --export-csv`). Set to `0` to disable entirely.

### `[memory.thinking]`

```toml
[memory.thinking]
preserve_prior_turns = false   # re-send prior-turn extended-thinking
                               # blocks in every request (pre-2026-04-18
                               # behavior); default false = strip them
```

Anthropic's Claude 4.x models emit signed `thinking` blocks when extended
thinking / `reasoning_effort` is on. Those blocks must be included within
an in-progress tool-use loop (same turn), but the model does not attend
to thinking from prior completed turns — re-sending them on every
subsequent request just burns input/cache tokens. Default `false` strips
them from history when building the outgoing request; set `true` only if
a future provider or model you're testing needs the old behavior.

See [`examples/config.toml`](../examples/config.toml) for every memory option.

## `[chat]`

Where models are declared. An alias under `[chat.<provider>.<alias>]` is what you pass to `--model` or set in `[defaults] model`.

### Providers

| Provider key | SDK             | API key env var         |
| ------------ | --------------- | ----------------------- |
| `anthropic`  | anthropic       | `ANTHROPIC_API_KEY`     |
| `openrouter` | openai-compat   | `OPENROUTER_API_KEY`    |
| `deepseek`   | deepseek        | `DEEPSEEK_API_KEY`      |
| `gemini`     | gemini          | `GEMINI_API_KEY`        |
| `xai`        | openai-compat   | `XAI_API_KEY`           |
| `zhipuai`    | zhipuai         | `ZAI_API_KEY`           |

### Per-model options

```toml
[chat.anthropic.claude-sonnet]
model_id = "claude-sonnet-4-6"
# temperature = 0.7
# max_tokens = 4096
# max_context_tokens = 200000
# cache_ttl = "5m"       # Anthropic prompt-cache TTL
# reasoning_effort = "medium"   # provider-specific
# budget_tokens = 16000         # extended thinking budget (Anthropic)
```

- `model_id` — the provider's canonical model ID. Required.
- `temperature`, `max_tokens`, `max_context_tokens` — standard LLM knobs.
- `cache_ttl` — how long prompt-cache entries live. Provider-specific (Anthropic only currently).
- `reasoning_effort`, `budget_tokens` — extended thinking controls (Anthropic reasoning models).

See [`examples/config.toml`](../examples/config.toml) for every per-model option and for embedding/image profiles.

## `[advanced]`

Opt-in diagnostic knobs you probably don't want on by default.

```toml
[advanced]
cache_forensics = false   # opt-in per-request cache diagnostics
```

When `cache_forensics = true`, Shore writes a line per LLM request to `{data_dir}/cache_forensics.jsonl` with cache-hit / cache-miss / cache-create counts. Useful when debugging a suspected caching regression; noisy otherwise.

See [`examples/config.toml`](../examples/config.toml) for every advanced option.

## `client.toml`

A separate file, `$XDG_CONFIG_HOME/shore/client.toml`, tells clients (CLI, TUI, bridges) where to reach a daemon. Useful when the daemon runs on another machine (e.g. over Tailscale).

```toml
default_address = "100.64.0.1:7320"
```

**Address resolution order** (highest wins):

1. `--addr` CLI flag
2. `default_address` in `client.toml`
3. Instance discovery (local daemon registry)
4. `127.0.0.1:7320` as a final fallback

`client.toml` alone does **not** enable remote access. To accept non-loopback connections the daemon side must also set `[daemon].unsafe_allow_remote_access = true` and (optionally) `allowed_hosts` — see [`[daemon]`](#daemon).

See [`examples/client.toml`](../examples/client.toml) for a full example.
