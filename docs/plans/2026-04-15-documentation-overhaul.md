# Documentation Overhaul Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite `README.md` from scratch and add two user-facing docs (`docs/FEATURES.md`, `docs/CONFIGURATION.md`) so an end user can install Shore, understand what it does, and find every config key they might want to change.

**Architecture:** Three markdown files written in order of dependency (CONFIGURATION first, FEATURES second, README last). `examples/config.toml` remains canonical for the full option list; prose docs never duplicate it. Internal docs (`ARCHITECTURE.md`, `DECISIONS.md`, `QUIRKS.md`, `docs/specs/`) untouched.

**Tech Stack:** Markdown. Shell for verification (grep, ripgrep). `cargo` to confirm commands are valid.

**Spec:** `docs/specs/2026-04-15-documentation-overhaul.md` (commit `3607b83`).

**Note on TDD for docs:** Each task uses an "acceptance criteria → write → verify → commit" rhythm. "Failing test" equals the criteria list; "verify passes" equals grep/anchor checks confirming every required string appears.

---

## Task 1: Pre-flight — audit `examples/config.toml` staleness

**Why:** `CONFIGURATION.md` points users to `examples/config.toml` as the canonical option reference. If the example file is missing current keys or has stale ones, the docs will link to a broken reference.

**Files:**
- Inspect: `examples/config.toml`
- Compare against: `shore-config/src/`, `shore-daemon/src/config.rs` (wherever config keys are read)

- [ ] **Step 1: List every config key currently read by the code**

Run:
```sh
rg -n '\.get\(|\.as_|\.field\(|deserialize|serde\(rename' shore-config/src/ shore-daemon/src/ | rg -v 'test' | sort -u > /tmp/code_keys.txt
```

Expected: a list of every key read by Shore code. Scan it by eye; extract TOML key paths (e.g., `behavior.autonomy.interiority.max_tool_rounds`).

- [ ] **Step 2: List every key documented in `examples/config.toml`**

Run:
```sh
rg -n '^\s*#?\s*[a-z_]+\s*=' examples/config.toml | sort -u > /tmp/example_keys.txt
```

Expected: every key (commented or not) in the example file.

- [ ] **Step 3: Diff the two lists**

Review `/tmp/code_keys.txt` vs `/tmp/example_keys.txt` by eye. Flag:
- Keys read by code but absent from `examples/config.toml` → the example is stale
- Keys in `examples/config.toml` but not read anywhere → the example has dead entries

- [ ] **Step 4: Fix any discrepancies in `examples/config.toml`**

Add missing keys (commented out, with a short `# what this does` line). Remove keys that reference deleted functionality. Do NOT add keys that *might* exist — only ones currently read by code.

- [ ] **Step 5: Commit pre-flight fixes (if any)**

```sh
git add examples/config.toml
git commit -m "docs(config): sync examples/config.toml with current config keys"
```

If no discrepancies were found, skip the commit and note "no drift found" in the task summary.

---

## Task 2: Scaffold `docs/CONFIGURATION.md`

**Files:**
- Create: `docs/CONFIGURATION.md`

- [ ] **Step 1: Acceptance criteria**

File exists with:
- Top-level title `# Configuration`
- All ten section anchors present as empty headings (content written in later tasks):
  - `## Orientation`
  - `## Environment variables`
  - `## [daemon]`
  - `## [defaults]`
  - `## [behavior.autonomy]`
  - `## [behavior.tool_use]`
  - `## [memory]`
  - `## [chat]`
  - `## [advanced]`
  - `## client.toml`

- [ ] **Step 2: Create file with skeleton**

```markdown
# Configuration

Where every Shore setting lives, what it does, and when to change it. For the exhaustive option list see [`examples/config.toml`](../examples/config.toml).

## Orientation

<!-- written in Task 3 -->

## Environment variables

<!-- written in Task 3 -->

## `[daemon]`

<!-- written in Task 4 -->

## `[defaults]`

<!-- written in Task 4 -->

## `[behavior.autonomy]`

<!-- written in Task 5 -->

## `[behavior.tool_use]`

<!-- written in Task 6 -->

## `[memory]`

<!-- written in Task 6 -->

## `[chat]`

<!-- written in Task 7 -->

## `[advanced]`

<!-- written in Task 7 -->

## `client.toml`

<!-- written in Task 7 -->
```

- [ ] **Step 3: Verify scaffold**

Run:
```sh
rg -c '^##' docs/CONFIGURATION.md
```

Expected: `10` (ten second-level headings).

- [ ] **Step 4: Commit**

```sh
git add docs/CONFIGURATION.md
git commit -m "docs: scaffold CONFIGURATION.md with section headings"
```

---

## Task 3: Write CONFIGURATION.md §1 Orientation + §2 Environment variables

**Files:**
- Modify: `docs/CONFIGURATION.md`

- [ ] **Step 1: Acceptance criteria**

**§1 Orientation** must cover:
- Config directory: `$XDG_CONFIG_HOME/shore/` (default `~/.config/shore/`)
- Directory layout (tree diagram of `config.toml`, `user.md`, `prompts/`, `characters/<Name>/`)
- `include = [...]` explicit includes
- `conf.d/*.toml` auto-loading
- Precedence: CLI flag → env var → config file (applies to `--addr`/`SHORE_ADDR`/`[daemon].addr`, `--config`, `--character`/`SHORE_CHARACTER`)
- Daemon startup specifics: explicit `--config <path>` must exist (no silent default creation); remote-access safety enforced against the final resolved address

**§2 Environment variables** must be a single grep-friendly table listing at minimum:
- `ANTHROPIC_API_KEY`, `OPENROUTER_API_KEY`, `DEEPSEEK_API_KEY`, `GEMINI_API_KEY`, `XAI_API_KEY`, `ZAI_API_KEY`
- `TAVILY_API_KEY` (web search)
- `SHORE_ADDR`, `SHORE_CHARACTER`
- `XDG_CONFIG_HOME`, `XDG_DATA_HOME` (briefly, just where Shore looks)

Each row: env var name, what it does.

- [ ] **Step 2: Write §1**

Replace the `<!-- written in Task 3 -->` comment under `## Orientation` with:

```markdown
Shore reads all configuration from `$XDG_CONFIG_HOME/shore/` (defaults to `~/.config/shore/`). A minimal install needs one file (`config.toml`) and one character directory (`characters/<Name>/character.md`).

### Directory layout

` ``
~/.config/shore/
├── config.toml                  # main configuration — required
├── user.md                      # who you are (global fallback) — optional
├── prompts/
│   └── system.md                # system prompt template (global fallback) — optional
└── characters/
    └── <CharacterName>/
        ├── character.md         # required (presence enables discovery)
        ├── user.md              # character-specific override — optional
        └── prompts/
            └── system.md        # character-specific system prompt — optional
` ``
(replace backtick groups above with triple-backticks when writing)

Characters are discovered by scanning `characters/` for subdirectories containing `character.md`. No config entry is needed to register a character.

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
```

**Important:** in the actual file, replace the fenced-block escape notation `` ` `` pairs with triple backticks. The plan shows escaped forms to avoid nested-fence confusion.

- [ ] **Step 3: Write §2**

Replace the `<!-- written in Task 3 -->` comment under `## Environment variables`:

```markdown
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
```

- [ ] **Step 4: Verify**

Run:
```sh
rg -c 'ANTHROPIC_API_KEY|OPENROUTER_API_KEY|SHORE_ADDR|SHORE_CHARACTER|XDG_CONFIG_HOME|TAVILY_API_KEY' docs/CONFIGURATION.md
```

Expected: non-zero count covering every env var in the criteria.

Also run:
```sh
rg -c 'include =|conf.d|precedence|unsafe_allow_remote_access' docs/CONFIGURATION.md
```

Expected: each phrase appears at least once.

- [ ] **Step 5: Commit**

```sh
git add docs/CONFIGURATION.md
git commit -m "docs(config): write orientation + environment variables"
```

---

## Task 4: Write CONFIGURATION.md §3 `[daemon]` + §4 `[defaults]`

**Files:**
- Modify: `docs/CONFIGURATION.md`

- [ ] **Step 1: Acceptance criteria**

**§3 `[daemon]`** must cover:
- `addr` (default `127.0.0.1:7320`) and when you change it
- `unsafe_allow_remote_access` (required for non-loopback binds)
- `allowed_hosts` (source-IP allowlist; not authentication)
- Explicit warning: no TLS, use only on trusted private overlays

**§4 `[defaults]`** must cover:
- `model` — primary conversation model; must match a `[chat.*.*]` alias
- Per-operation model slots, each named explicitly with what it controls:
  - `tool_model` — tool-use calls
  - `memory_agent` — memory agent queries
  - `collation` — memory collation (merge/split/normalize)
  - `compaction` — conversation → memory compaction
  - `interiority` — autonomous interiority ticks
  - `embedding` — embeddings profile
  - `image_generation` — image generation profile
- `display_name` — `{{user}}` template var; falls back to `$USER`
- `stream` — whether to stream responses by default

- [ ] **Step 2: Write §3**

Replace the placeholder under `## [daemon]`:

```markdown
Controls how the daemon binds and who can reach it. By default the daemon is localhost-only; you have to opt in to remote binds explicitly.

**When to change:** only when you want to reach the daemon from another machine on a trusted private network (Tailscale, WireGuard, a VPN).

```toml
[daemon]
addr = "127.0.0.1:7320"
# unsafe_allow_remote_access = true
# allowed_hosts = ["100.64.0.2"]
` ``

- `addr` — listen address. `--addr` and `SHORE_ADDR` override this (see [Orientation](#orientation)).
- `unsafe_allow_remote_access` — **required** for any non-loopback bind. Without it Shore refuses to start.
- `allowed_hosts` — source-IP allowlist. An allowed host can connect without any further check.

*This is unauthenticated TCP.* `allowed_hosts` is not authentication; there is no TLS. Use only on private/overlay networks you already trust. See [`examples/config.toml`](../examples/config.toml) for every daemon option.
```

(Note: replace the escaped `` ` `` backtick groups above with triple backticks in the actual file.)

- [ ] **Step 3: Write §4**

Replace the placeholder under `## [defaults]`:

```markdown
Defaults that apply when a command doesn't override them. Most users set `model` and `display_name` and leave the rest alone.

```toml
[defaults]
model = "claude-sonnet"       # must match a key in [chat.*.*]
display_name = "Your Name"    # fills `{{user}}` in templates
# stream = true
# tool_model = "mistral-small"
# memory_agent = "mistral-small"
# collation = "mistral-small"
# compaction = "mistral-small"
# interiority = "claude-sonnet"
# embedding = "text-large"
# image_generation = "gemini-flash"
` ``

**Per-operation model slots** let you run heavy work (the main conversation) on one model and cheap background work on another. Each slot, if omitted, falls back to `model`:

- `tool_model` — used when the character is invoking tools (web search, memory, etc.)
- `memory_agent` — the small model that drives the memory query/save loop
- `collation` — memory entry merge/split/normalize
- `compaction` — conversation summarization into memory
- `interiority` — the "private moment" autonomous ticks
- `embedding` — which embedding profile to use (see `[chat.<provider>.<alias>]` with an embedding model)
- `image_generation` — which model handles `generate_image`

Any value passed here must match an alias declared under `[chat.<provider>.<alias>]`.

See [`examples/config.toml`](../examples/config.toml) for every default.
```

- [ ] **Step 4: Verify**

Run:
```sh
rg -c 'tool_model|memory_agent|collation|compaction|interiority|embedding|image_generation' docs/CONFIGURATION.md
```

Expected: all seven keys named.

```sh
rg -c 'unsafe_allow_remote_access|allowed_hosts|no TLS|TLS' docs/CONFIGURATION.md
```

Expected: remote-access safety language present.

- [ ] **Step 5: Commit**

```sh
git add docs/CONFIGURATION.md
git commit -m "docs(config): write [daemon] and [defaults] sections"
```

---

## Task 5: Write CONFIGURATION.md §5 `[behavior.autonomy]` (+ heartbeat + interiority)

**Files:**
- Modify: `docs/CONFIGURATION.md`

- [ ] **Step 1: Acceptance criteria**

One grouped section (the three tables interact; explaining them separately hides that). Must cover:

- `[behavior.autonomy]` top-level: `enabled`, `personality`, `max_unanswered`, `max_deferral_hours`
- `[behavior.autonomy.heartbeat]`: `enabled`, `session_gap_secs`, `session_probe_floor_secs`, `dormant_threshold`
- `[behavior.autonomy.interiority]`: `enabled`, `fallback_interiority_interval`, `dormant_after_interiority_turns`, `dormant_after_idle_time`, `minimum_interiority_latency`, `max_tool_rounds`
- Plain-language explanation of **active** vs **dormant** phase
- What `personality` actually tunes (probe frequency)
- Explicit pointer to `FEATURES.md#autonomy` and `FEATURES.md#interiority` for the narrative

- [ ] **Step 2: Write the section**

Replace the placeholder under `## [behavior.autonomy]`:

```markdown
Controls whether the character speaks on its own and how often. Disabled by default. Three related tables: the umbrella `[behavior.autonomy]` table, the `heartbeat` sub-table (reactive probes after idle gaps), and the `interiority` sub-table (self-scheduled private ticks).

See [FEATURES.md — Autonomy](FEATURES.md#autonomy) and [FEATURES.md — Interiority](FEATURES.md#interiority) for the full story of *what these do*. This section is the config reference.

### Active vs dormant

The character has two phases: **active** (responsive, may probe or tick) and **dormant** (silent; wakes on a user message). Both heartbeat and interiority have their own thresholds for entering the dormant phase.

### `[behavior.autonomy]` — the umbrella

```toml
[behavior.autonomy]
enabled = false             # master switch for autonomous speech
personality = 0.5           # 0.0 (reserved) → 1.0 (proactive); shapes probe frequency
max_unanswered = 1          # back off after this many unanswered autonomous messages
max_deferral_hours = 24.0   # hard cap on how long the character will wait before sending
` ``

**When to change:** enable `enabled = true` once you're ready for unprompted messages. Raise `personality` for a more forward character; drop it for something restrained.

### `[behavior.autonomy.heartbeat]` — reactive probes

```toml
[behavior.autonomy.heartbeat]
enabled = true
session_gap_secs = 1800           # 30 min idle marks a session boundary
session_probe_floor_secs = 180    # minimum idle before a post-session probe fires
dormant_threshold = 1             # consecutive unanswered probes before going dormant
` ``

Heartbeat fires probes *after a gap in conversation*. A probe is a chance for the character to check in. `session_gap_secs` defines what counts as a gap; `session_probe_floor_secs` prevents probes firing the second you stop typing.

### `[behavior.autonomy.interiority]` — self-scheduled private ticks

```toml
[behavior.autonomy.interiority]
enabled = true
fallback_interiority_interval = "1h"      # base cadence when the character doesn't self-schedule
dormant_after_interiority_turns = 3       # consecutive ticks with no user reply before sleeping
dormant_after_idle_time = "48h"           # hard idle ceiling before sleeping until user returns
minimum_interiority_latency = "1h"        # floor between a user message and the next tick
max_tool_rounds = 12                      # tool-use rounds per tick before forcing a wrap-up recap
` ``

Interiority is different from heartbeat: it's the character thinking/acting on its own, not reacting to your silence. The character can schedule its own next tick; `fallback_interiority_interval` only applies when it doesn't.

`max_tool_rounds` is a safety limit — if a tick wanders, it gets wrapped up at this many tool rounds.

All time fields accept human durations (`"30s"`, `"15m"`, `"2h"`, `"48h"`).

See [`examples/config.toml`](../examples/config.toml) for every option.
```

- [ ] **Step 3: Verify**

Run:
```sh
rg -c 'enabled|personality|max_unanswered|max_deferral_hours|session_gap_secs|session_probe_floor_secs|dormant_threshold|fallback_interiority_interval|dormant_after_interiority_turns|dormant_after_idle_time|minimum_interiority_latency|max_tool_rounds' docs/CONFIGURATION.md
```

Expected: every key named.

```sh
rg -c 'active|dormant' docs/CONFIGURATION.md
```

Expected: both phases mentioned.

- [ ] **Step 4: Commit**

```sh
git add docs/CONFIGURATION.md
git commit -m "docs(config): write [behavior.autonomy] section with heartbeat + interiority"
```

---

## Task 6: Write CONFIGURATION.md §6 `[behavior.tool_use]` + §7 `[memory]`

**Files:**
- Modify: `docs/CONFIGURATION.md`

- [ ] **Step 1: Acceptance criteria**

**§6 `[behavior.tool_use]`** must cover:
- `enabled`, `max_iterations`
- `[behavior.tool_use.tools]` per-tool toggles: `memory`, `send_image`, `list_images`, `recall_image`, `generate_image`, `web_search`, `fetch_url`, `check_time`, `roll_dice`, `activity_heatmap`
- `[behavior.tool_use.search]`: `api_key_env`, `max_results`, `search_depth`, `include_answer`

**§7 `[memory]`** must cover:
- `[memory.compaction]`: `enabled`, `idle_trigger_minutes`
- `[memory.collation]` if present
- Pointer to `FEATURES.md#memory` for what these *mean*

- [ ] **Step 2: Write §6**

Replace the placeholder under `## [behavior.tool_use]`:

```markdown
Controls which tools the character can call mid-response. Every tool is enabled by default; disable selectively if a tool is expensive or inappropriate for your character.

See [FEATURES.md — Tool use](FEATURES.md#tool-use) for what each tool actually does.

```toml
[behavior.tool_use]
enabled = true
max_iterations = 10   # max tool-call rounds per turn before forcing a final response

[behavior.tool_use.tools]
memory = true
send_image = true
list_images = true
recall_image = true
generate_image = true
web_search = true
fetch_url = true
check_time = true
roll_dice = true
activity_heatmap = true
` ``

**When to change:**
- Set `enabled = false` to disable tool use entirely.
- Drop individual tool toggles to `false` when you want the character to not have access (e.g. `generate_image = false` if you don't have image-gen credits).
- Lower `max_iterations` if the character is going in circles; raise it if complex tasks need more rounds.

### `[behavior.tool_use.search]` — web search backend

```toml
[behavior.tool_use.search]
api_key_env = "TAVILY_API_KEY"
max_results = 5
search_depth = "basic"       # "basic" or "advanced"
include_answer = true
` ``

Shore uses [Tavily](https://tavily.com/) for web search. `api_key_env` names the environment variable holding the key (default `TAVILY_API_KEY`).

See [`examples/config.toml`](../examples/config.toml) for every tool-use option.
```

- [ ] **Step 3: Write §7**

Replace the placeholder under `## [memory]`:

```markdown
Controls the memory subsystem's background work. Memory itself is always on — these tables tune *when compaction and collation run*, not whether memory exists.

See [FEATURES.md — Memory](FEATURES.md#memory) for what compaction and collation are.

### `[memory.compaction]`

```toml
[memory.compaction]
enabled = true
idle_trigger_minutes = 30
` ``

Compaction condenses old conversation turns into durable memory entries. `idle_trigger_minutes` is how long the session must be idle before compaction kicks in.

### `[memory.collation]`

```toml
[memory.collation]
enabled = true
` ``

Collation periodically merges, splits, and normalizes memory entries so related facts coalesce and contradictions get reconciled.

See [`examples/config.toml`](../examples/config.toml) for every memory option.
```

- [ ] **Step 4: Verify**

Run:
```sh
rg -c 'memory|send_image|list_images|recall_image|generate_image|web_search|fetch_url|check_time|roll_dice|activity_heatmap' docs/CONFIGURATION.md
```

Expected: every tool named.

```sh
rg -c 'idle_trigger_minutes|Tavily|TAVILY_API_KEY' docs/CONFIGURATION.md
```

Expected: each phrase appears.

- [ ] **Step 5: Commit**

```sh
git add docs/CONFIGURATION.md
git commit -m "docs(config): write [behavior.tool_use] and [memory] sections"
```

---

## Task 7: Write CONFIGURATION.md §8 `[chat]` + §9 `[advanced]` + §10 `client.toml`

**Files:**
- Modify: `docs/CONFIGURATION.md`

- [ ] **Step 1: Acceptance criteria**

**§8 `[chat.<provider>.<alias>]`** must include:
- Provider table with columns: provider key, SDK, API key env var
  - All six: `anthropic`, `openrouter`, `deepseek`, `gemini`, `xai`, `zhipuai`
- Per-model options list: `model_id`, `temperature`, `max_tokens`, `max_context_tokens`, `reasoning_effort`, `budget_tokens`, `cache_ttl`
- Worked example (`[chat.anthropic.claude-sonnet]` with `model_id`, `cache_ttl`)

**§9 `[advanced]`** must cover `cache_forensics` opt-in.

**§10 `client.toml`** must cover:
- Location (`$XDG_CONFIG_HOME/shore/client.toml`)
- `default_address`
- Precedence (`--addr` → `client.toml` → instance discovery → `127.0.0.1:7320`)
- Explicit note that `client.toml` alone doesn't enable remote access; the daemon side needs `[daemon]` opt-in

- [ ] **Step 2: Write §8**

Replace the placeholder under `## [chat]`:

```markdown
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
` ``

- `model_id` — the provider's canonical model ID. Required.
- `temperature`, `max_tokens`, `max_context_tokens` — standard LLM knobs.
- `cache_ttl` — how long prompt-cache entries live. Provider-specific (Anthropic only currently).
- `reasoning_effort`, `budget_tokens` — extended thinking controls (Anthropic reasoning models).

See [`examples/config.toml`](../examples/config.toml) for every per-model option and for embedding/image profiles.
```

- [ ] **Step 3: Write §9**

Replace the placeholder under `## [advanced]`:

```markdown
Opt-in diagnostic knobs you probably don't want on by default.

```toml
[advanced]
cache_forensics = false   # opt-in per-request cache diagnostics
` ``

When `cache_forensics = true`, Shore writes a line per LLM request to `{data_dir}/cache_forensics.jsonl` with cache-hit / cache-miss / cache-create counts. Useful when debugging a suspected caching regression; noisy otherwise.

See [`examples/config.toml`](../examples/config.toml) for every advanced option.
```

- [ ] **Step 4: Write §10**

Replace the placeholder under `## client.toml`:

```markdown
A separate file, `$XDG_CONFIG_HOME/shore/client.toml`, tells clients (CLI, TUI, bridges) where to reach a daemon. Useful when the daemon runs on another machine (e.g. over Tailscale).

```toml
default_address = "100.64.0.1:7320"
` ``

**Address resolution order** (highest wins):

1. `--addr` CLI flag
2. `default_address` in `client.toml`
3. Instance discovery (local daemon registry)
4. `127.0.0.1:7320` as a final fallback

`client.toml` alone does **not** enable remote access. To accept non-loopback connections the daemon side must also set `[daemon].unsafe_allow_remote_access = true` and (optionally) `allowed_hosts` — see [`[daemon]`](#daemon).

See [`examples/client.toml`](../examples/client.toml) for a full example.
```

- [ ] **Step 5: Verify**

Run:
```sh
rg -c 'anthropic|openrouter|deepseek|gemini|xai|zhipuai' docs/CONFIGURATION.md
```

Expected: all six providers named.

```sh
rg -c 'cache_forensics|default_address|cache_ttl|reasoning_effort|budget_tokens' docs/CONFIGURATION.md
```

Expected: all keys present.

- [ ] **Step 6: Commit**

```sh
git add docs/CONFIGURATION.md
git commit -m "docs(config): write [chat], [advanced], and client.toml sections"
```

---

## Task 8: CONFIGURATION.md final verification

**Files:**
- Review: `docs/CONFIGURATION.md`

- [ ] **Step 1: Coverage check against `examples/config.toml`**

For each section heading in `examples/config.toml` (`[defaults]`, `[daemon]`, `[behavior.autonomy]`, `[behavior.autonomy.heartbeat]`, `[behavior.autonomy.interiority]`, `[behavior.tool_use]`, `[behavior.tool_use.tools]`, `[behavior.tool_use.search]`, `[memory.compaction]`, `[memory.collation]`, `[chat.*]`, `[advanced]`), verify at least one sentence in `CONFIGURATION.md` references that section path.

Run:
```sh
for section in daemon defaults behavior.autonomy behavior.tool_use memory.compaction memory.collation chat advanced; do
  echo -n "$section: "
  rg -c "$section" docs/CONFIGURATION.md || echo 0
done
```

Expected: every section returns a non-zero count.

- [ ] **Step 2: Anchor stability check**

Generate the anchors `FEATURES.md` will link to:
```sh
rg -n '^## ' docs/CONFIGURATION.md
```

Note the exact heading text — these are the anchors Tasks 10-14 will reference. If any heading needs renaming, do it now before `FEATURES.md` starts.

- [ ] **Step 3: Commit (empty commit if no changes)**

If any corrections were needed, commit them:
```sh
git add docs/CONFIGURATION.md
git commit -m "docs(config): coverage and anchor fixes"
```

Otherwise skip.

---

## Task 9: Scaffold `docs/FEATURES.md`

**Files:**
- Create: `docs/FEATURES.md`

- [ ] **Step 1: Acceptance criteria**

File exists with:
- Top-level title `# Features`
- Short intro paragraph explaining the doc's purpose
- All twelve section anchors present as empty headings:
  - `## Characters`
  - `## Models and providers`
  - `## Conversations`
  - `## Memory`
  - `## Autonomy`
  - `## Interiority`
  - `## Tool use`
  - `## Clients`
  - `## Prompt caching`
  - `## Diagnostics`
  - `## Remote access`
  - `## Shell completions`

- [ ] **Step 2: Create file**

```markdown
# Features

Every user-visible feature in Shore: what it does, why it exists, and how to use it. For the exhaustive config reference, see [`CONFIGURATION.md`](CONFIGURATION.md) and [`examples/config.toml`](../examples/config.toml).

## Characters

<!-- written in Task 10 -->

## Models and providers

<!-- written in Task 10 -->

## Conversations

<!-- written in Task 10 -->

## Memory

<!-- written in Task 11 -->

## Autonomy

<!-- written in Task 12 -->

## Interiority

<!-- written in Task 12 -->

## Tool use

<!-- written in Task 13 -->

## Clients

<!-- written in Task 14 -->

## Prompt caching

<!-- written in Task 15 -->

## Diagnostics

<!-- written in Task 15 -->

## Remote access

<!-- written in Task 15 -->

## Shell completions

<!-- written in Task 15 -->
```

- [ ] **Step 3: Verify**

Run:
```sh
rg -c '^## ' docs/FEATURES.md
```

Expected: `12`.

- [ ] **Step 4: Commit**

```sh
git add docs/FEATURES.md
git commit -m "docs: scaffold FEATURES.md with section headings"
```

---

## Task 10: FEATURES.md §1 Characters + §2 Models and providers + §3 Conversations

**Files:**
- Modify: `docs/FEATURES.md`

- [ ] **Step 1: Acceptance criteria**

**§1 Characters** covers: auto-discovery from `characters/`, required `character.md`, optional `user.md`, optional `prompts/system.md`, resolution order for `user.md` and `system.md`, template variables (`{{char}}`/`{{character_name}}`, `{{user}}`, `{{date}}`, `{{time}}`), `--character` flag and `SHORE_CHARACTER` env var, `shore character` subcommands (`list`, switch, `--info`, `--new`). Defines "character" in plain language in the first paragraph.

**§2 Models and providers** covers: multi-provider support (names all six providers), per-operation model slots explained in user terms (not restating CONFIGURATION.md's table — describes *why* you'd use different models for different operations), runtime model override via `shore model <alias>` / `--reset`, pointer to `CONFIGURATION.md#chat` for setup.

**§3 Conversations** covers: `shore send <msg>`, `send -i image.png`, `send --thinking`, `shore regen`, `regen --guidance "..."`, `shore log` family (default tail, `-n N`, `-f` follow, `last`/`-1`, `edit <ref> <text>`, `delete <ref>`).

- [ ] **Step 2: Write §1**

Replace the placeholder under `## Characters`:

```markdown
A **character** in Shore is an AI persona with its own personality, memory, and conversation history. You can have multiple characters on the same install and switch between them.

### Why they exist

The core Shore mental model: you aren't chatting with a generic LLM, you're talking with a specific character that remembers you. Every character has its own memory store, its own conversation log, and its own system prompt.

### How to use

Characters live in `~/.config/shore/characters/<Name>/`. The presence of a `character.md` file makes a character discoverable — no config entry needed.

**Required file:**

- `character.md` — describes personality, background, behavior. Injected into the system prompt as a dedicated block.

**Optional files:**

- `user.md` — describes who *you* are, from this character's perspective. Falls back to the global `~/.config/shore/user.md`.
- `prompts/system.md` — overrides the system prompt template. Falls back to global, then to the built-in default.

**Resolution order** for `user.md` and `system.md`:

1. Character-specific: `characters/<Name>/user.md` or `characters/<Name>/prompts/system.md`
2. Global fallback: `~/.config/shore/user.md` or `~/.config/shore/prompts/system.md`
3. (System prompt only) built-in default: `You are {{char}}, in conversation with {{user}}.`

### Template variables

Anywhere in `character.md`, `user.md`, or `system.md`:

| Variable                            | Value                                       |
| ----------------------------------- | ------------------------------------------- |
| `{{char}}` / `{{character_name}}`   | The character's name (directory name)       |
| `{{user}}`                          | Your display name (`[defaults] display_name`, or `$USER`) |
| `{{date}}`                          | Current date, e.g. `Friday, 2026-03-28`     |
| `{{time}}`                          | Current time, `HH:MM`                       |

### Choosing a character at runtime

- `shore --character Alice send "hi"` — one-off override
- `export SHORE_CHARACTER=Alice` — session default
- If only one character exists, it's selected automatically.

### CLI commands

```sh
shore character                    # list all discovered characters
shore character Alice              # switch the daemon's active character to Alice
shore character --info             # detailed info on the currently-active character
shore character --new              # scaffold a new character directory interactively
` ``

See [`CONFIGURATION.md`](CONFIGURATION.md#orientation) for the directory layout.
```

- [ ] **Step 3: Write §2**

Replace the placeholder under `## Models and providers`:

```markdown
Shore runs against real LLM APIs. You can use different models for different operations — for example, a big model for conversation and a cheap fast model for background memory work.

### Why it exists

A serious AI character does a lot of background work: summarizing conversations into memory, running tool-use loops, periodically reflecting via interiority ticks, looking things up, writing embeddings. If every one of those jobs used the same big model, cost and latency would be miserable. Per-operation model slots let you pay for quality where it matters and speed where it doesn't.

### Supported providers

Shore ships with six providers built in: `anthropic`, `openrouter`, `deepseek`, `gemini`, `xai`, `zhipuai`. Each expects its own API key as an env var — see [`CONFIGURATION.md` — Environment variables](CONFIGURATION.md#environment-variables).

### Declaring a model

Each model is an alias under `[chat.<provider>.<alias>]`:

```toml
[chat.anthropic.claude-sonnet]
model_id = "claude-sonnet-4-6"

[chat.openrouter.haiku-fast]
model_id = "anthropic/claude-haiku-4-5"
` ``

You then reference aliases (`claude-sonnet`, `haiku-fast`) from `[defaults]`:

```toml
[defaults]
model = "claude-sonnet"        # main conversation
tool_model = "haiku-fast"      # tool-use calls
compaction = "haiku-fast"      # summarization
interiority = "claude-sonnet"  # private ticks
` ``

### Runtime overrides

```sh
shore model                    # list available aliases
shore model haiku-fast         # switch active model (runtime override, per daemon)
shore model --reset            # clear the override and return to [defaults] model
` ``

For the full set of per-model options and the provider table, see [`CONFIGURATION.md` — `[chat]`](CONFIGURATION.md#chat).
```

- [ ] **Step 4: Write §3**

Replace the placeholder under `## Conversations`:

```markdown
The core loop. Send messages, regenerate responses, edit history.

### Why it exists

You need more than "send and receive." Conversations drift, responses miss, you realize a previous message was wrong. Shore's CLI gives you the full edit surface — edit past messages, delete them, regenerate with guidance — without jumping into a DB.

### Sending

```sh
shore send "Hello!"
shore send -i ~/Pictures/photo.png "What is this?"    # attach an image
shore send --thinking "Work through this carefully"   # extended thinking mode
` ``

### Regenerating

```sh
shore regen                                           # regen the last assistant response
shore regen --guidance "be more concise this time"    # regen with a nudge
` ``

The guidance is a one-shot hint injected on top of the existing context — it doesn't permanently change the character.

### The conversation log

```sh
shore log                                             # last 20 messages
shore log -n 50                                       # last 50
shore log -f                                          # follow mode — stream new messages
shore log last                                        # or: shore log -1 — one message
shore log edit <ref> "new text"                       # edit a past message
shore log delete <ref>                                # delete a message
` ``

`<ref>` accepts either a message ID or a negative index (`-1` = most recent, `-2` = previous, …).
```

- [ ] **Step 5: Verify**

Run:
```sh
rg -c 'character.md|user.md|prompts/system.md|\{\{char\}\}|\{\{user\}\}|\{\{date\}\}|\{\{time\}\}' docs/FEATURES.md
```

Expected: all character template assets named.

```sh
rg -c 'anthropic|openrouter|deepseek|gemini|xai|zhipuai' docs/FEATURES.md
```

Expected: all six providers named.

```sh
rg -c 'shore send|shore regen|shore log|--thinking|--guidance' docs/FEATURES.md
```

Expected: every conversation command named.

- [ ] **Step 6: Commit**

```sh
git add docs/FEATURES.md
git commit -m "docs(features): write characters, models, and conversations"
```

---

## Task 11: FEATURES.md §4 Memory

**Files:**
- Modify: `docs/FEATURES.md`

- [ ] **Step 1: Acceptance criteria**

Must cover:
- Plain-language description of what "memory" means in Shore (not a DB jargon dump)
- The two storage layers: vector search (semantic) + FTS (full-text)
- **Compaction** — defined on first use: conversation turns → memory entries
- **Collation** — defined: merge / split / normalize entries
- **Memory changelog** — what's it for
- **Memory agent** — the small model that searches and writes memory
- **Memory shell** — interactive debugging/inspection
- Every `shore memory` subcommand: query, `compact`, `changelog`, `reindex`, `purge`, `shell`
- Pointer to `CONFIGURATION.md#memory` for tunables

- [ ] **Step 2: Write the section**

Replace the placeholder under `## Memory`:

```markdown
The character remembers things. Not just recent messages — things you told it weeks ago, facts about you, preferences, ongoing threads. Memory persists across sessions and daemon restarts.

### Why it exists

A character without durable memory is a parrot. Shore characters accumulate context deliberately: important turns from your conversations get compacted into searchable memory entries, and those entries get folded together over time so related facts coalesce instead of accumulating as duplicates.

### How it's stored

Shore keeps memory in two parallel indexes (both SQLite-backed):

- **Vector store** — semantic search. "That thing Ren said about the doom launcher" finds the right memory even if you don't remember the exact words.
- **Full-text search (FTS)** — keyword search. Exact phrases, names, filenames.

Every query runs against both; results merge.

### Compaction

**Compaction** is the process of turning old conversation turns into durable memory entries. After the session has been idle for `[memory.compaction].idle_trigger_minutes` (default 30), Shore summarizes older turns into entries and drops them from the hot conversation log.

Run it manually:

```sh
shore memory compact
` ``

### Collation

**Collation** reorganizes existing memory entries: merging duplicates, splitting overloaded entries, normalizing wording. It runs periodically in the background when `[memory.collation].enabled = true`, and can be triggered manually:

```sh
shore memory compact   # runs compaction, then collation
` ``

Without collation, memory grows into a slurry of near-duplicates. With it, related facts settle into coherent entries.

### The memory agent

Some operations (saving new memories, answering structured queries about memory) run through a small **memory agent** — a cheap model whose only job is to decide whether to save, and what to save. Configure which model it uses via `[defaults] memory_agent`.

### Queries and changelog

```sh
shore memory "doom launcher"         # free-text query
shore memory changelog               # recent memory writes
shore memory reindex                 # rebuild FTS and vector indexes
shore memory purge                   # delete memory entries (prompts for confirmation)
` ``

### Memory shell

For exploring or debugging memory, drop into the interactive shell:

```sh
shore memory shell
` ``

Inside the shell you can query, save, and edit memory directly using the memory agent.

See [`CONFIGURATION.md` — `[memory]`](CONFIGURATION.md#memory) for tunables.
```

- [ ] **Step 3: Verify**

Run:
```sh
rg -c 'vector|FTS|full-text|compaction|collation|memory agent|changelog|reindex|purge|memory shell' docs/FEATURES.md
```

Expected: every memory concept named.

```sh
rg -c 'shore memory (compact|changelog|reindex|purge|shell)' docs/FEATURES.md
```

Expected: every subcommand shown.

- [ ] **Step 4: Commit**

```sh
git add docs/FEATURES.md
git commit -m "docs(features): write memory section"
```

---

## Task 12: FEATURES.md §5 Autonomy + §6 Interiority

**Files:**
- Modify: `docs/FEATURES.md`

- [ ] **Step 1: Acceptance criteria**

**§5 Autonomy** covers:
- Plain-language definition of "autonomy" in Shore
- **Heartbeat** defined — probes fired after idle gaps
- **Active** / **dormant** phases defined
- `personality` dial explained in user terms
- `max_unanswered`, `max_deferral_hours` mentioned
- How to enable (`[behavior.autonomy] enabled = true`)
- Pointer to `CONFIGURATION.md#behaviorautonomy`

**§6 Interiority** covers:
- Plain-language definition: "the character thinking/acting on its own"
- **Tick** defined — one private moment
- **Recap** defined — what the character writes at the end of a tick to carry forward state
- How interiority differs from heartbeat (not reactive, self-scheduled)
- `max_tool_rounds` wrap-up behavior
- Dormancy pathways (`dormant_after_interiority_turns`, `dormant_after_idle_time`)
- Pointer to `CONFIGURATION.md#behaviorautonomy`

- [ ] **Step 2: Write §5**

Replace the placeholder under `## Autonomy`:

```markdown
**Autonomy** is the character speaking on its own, without you prompting. Disabled by default. You turn it on in config; Shore then decides when the character should check in.

### Why it exists

A character that only speaks when addressed feels like a vending machine. With autonomy on, the character can say "hey, I was thinking about what you said yesterday," reach out after a long silence, or surface a memory at a relevant moment.

### Active vs dormant

The character has two phases:

- **Active** — responsive, may send unprompted messages
- **Dormant** — silent; wakes up when you send a message

The character drifts from active to dormant based on your engagement, and back to active when you speak up again.

### Heartbeat

The simplest autonomy mechanism: after a conversation gap, Shore may fire a **probe** — a chance for the character to check in. Heartbeat tunables:

- `session_gap_secs` — how long is "a gap" (default 30 min)
- `session_probe_floor_secs` — minimum idle before a probe can fire (default 3 min)
- `dormant_threshold` — consecutive unanswered probes before going dormant

### The `personality` dial

`[behavior.autonomy] personality` (0.0 to 1.0) shapes how often the character probes. At `0.0` the character is reserved — it probes rarely. At `1.0` it's forward — it probes often. `0.5` is balanced.

### Backoff

Two safety nets prevent runaway autonomous messaging:

- `max_unanswered` — if the character has this many unanswered messages in a row, it stops until you reply
- `max_deferral_hours` — hard cap on how long the character will wait before sending a queued autonomous message

### How to enable

```toml
[behavior.autonomy]
enabled = true
personality = 0.5
` ``

See [`CONFIGURATION.md` — `[behavior.autonomy]`](CONFIGURATION.md#behaviorautonomy) for every tunable.
```

- [ ] **Step 3: Write §6**

Replace the placeholder under `## Interiority`:

```markdown
**Interiority** is the character having a private moment: thinking, remembering, looking things up, maybe acting. Separate from autonomy's heartbeat — interiority is proactive (self-scheduled), heartbeat is reactive (triggered by idle gaps).

### Why it exists

Real presence means the character has an inner life even when you're not there. Interiority lets the character reflect, do its own research, consolidate memory, and decide on its own whether to reach out.

### Ticks and recaps

A **tick** is one unit of interiority — the character's private moment. During a tick the character can use tools (search memory, look things up on the web, schedule its own next tick), and may or may not produce a message to send you.

At the end of every tick the character writes a **recap** — a short note about what it thought about and what it plans to follow up on. Recaps carry state forward from tick to tick, giving the character narrative continuity across its private life.

### Scheduling

The character self-schedules the next tick when it finishes one. If it doesn't schedule, Shore falls back to `fallback_interiority_interval` (default 1h).

A floor (`minimum_interiority_latency`, default 1h) prevents ticks from piling up right after you send a message — the character needs breathing room.

### Wrap-up

If a tick goes long (many tool-use rounds), Shore caps it at `max_tool_rounds` (default 12) and forces a wrap-up recap. This is a safety limit — the character can't spin forever inside a single tick.

### Dormancy

Two paths lead interiority into the dormant phase:

- `dormant_after_interiority_turns` — this many ticks in a row with no user reply → sleep
- `dormant_after_idle_time` — this much total idle time (default 48h) → sleep until the user returns

### How to enable

```toml
[behavior.autonomy.interiority]
enabled = true
` ``

(Requires `[behavior.autonomy] enabled = true` as the master switch.)

See [`CONFIGURATION.md` — `[behavior.autonomy]`](CONFIGURATION.md#behaviorautonomy) for every tunable.
```

- [ ] **Step 4: Verify**

Run:
```sh
rg -c 'heartbeat|active|dormant|probe|personality|max_unanswered|max_deferral_hours' docs/FEATURES.md
```

Expected: every autonomy concept named.

```sh
rg -c 'tick|recap|fallback_interiority_interval|minimum_interiority_latency|max_tool_rounds|dormant_after_interiority_turns|dormant_after_idle_time' docs/FEATURES.md
```

Expected: every interiority concept named.

- [ ] **Step 5: Commit**

```sh
git add docs/FEATURES.md
git commit -m "docs(features): write autonomy and interiority"
```

---

## Task 13: FEATURES.md §7 Tool use

**Files:**
- Modify: `docs/FEATURES.md`

- [ ] **Step 1: Acceptance criteria**

Must cover:
- Plain-language description of tool use
- Every tool, one short paragraph each, with its exact toggle name:
  - `memory` — memory query/save mid-response
  - `web_search` — Tavily-backed search
  - `fetch_url` — fetch and read a URL
  - `check_time` — look up the current time / timezone
  - `roll_dice` — dice roller for rpg/game scenarios
  - `activity_heatmap` — produce a usage/activity visualization
  - `send_image`, `list_images`, `recall_image`, `generate_image` — image tool family
- `max_iterations` behavior
- Pointer to `CONFIGURATION.md#behaviortool_use`

- [ ] **Step 2: Write the section**

Replace the placeholder under `## Tool use`:

```markdown
Mid-response, the character can call **tools** — structured actions like searching memory, hitting the web, or generating an image. The character decides which tools to invoke; Shore runs them and feeds the result back.

### Why it exists

A character that only knows what's in its context window can't look things up, can't generate images, can't count dice for a tabletop session. Tools give the character the power to *do* things between "you asked" and "it answered."

### The tool surface

Every tool has an exact toggle under `[behavior.tool_use.tools]`. All are enabled by default.

#### Memory

- `memory` — search and save memory mid-response. The character can recall a past fact, or decide to save something you just told it.

#### Web

- `web_search` — Tavily-backed search. Requires `TAVILY_API_KEY` (see [`CONFIGURATION.md` — Environment variables](CONFIGURATION.md#environment-variables)).
- `fetch_url` — fetch a URL and read it. Used when a specific page is worth reading in full.

#### Time and chance

- `check_time` — current time / day of the week / timezone. Useful for "what day is it" and for the character to time-stamp its own reasoning.
- `roll_dice` — dice roller. Supports standard RPG notation (`3d6`, `d20+4`).

#### Images

- `send_image` — send an image back as part of the reply.
- `list_images` — list previously sent or generated images.
- `recall_image` — re-send a previously generated image by reference.
- `generate_image` — create a new image. Uses the model in `[defaults] image_generation`.

#### Activity

- `activity_heatmap` — generate a heatmap of recent usage activity.

### Loop budget

The character can invoke tools iteratively — use one, see the result, decide whether to use another. `[behavior.tool_use] max_iterations` (default 10) is the cap on how many rounds per turn. Hit the cap and Shore forces a final response.

See [`CONFIGURATION.md` — `[behavior.tool_use]`](CONFIGURATION.md#behaviortool_use) for toggles and search tuning.
```

- [ ] **Step 3: Verify**

Run:
```sh
rg -c 'memory|web_search|fetch_url|check_time|roll_dice|activity_heatmap|send_image|list_images|recall_image|generate_image' docs/FEATURES.md
```

Expected: every tool toggle named.

```sh
rg -c 'max_iterations|Tavily' docs/FEATURES.md
```

Expected: each present.

- [ ] **Step 4: Commit**

```sh
git add docs/FEATURES.md
git commit -m "docs(features): write tool use section"
```

---

## Task 14: FEATURES.md §8 Clients

**Files:**
- Modify: `docs/FEATURES.md`

- [ ] **Step 1: Acceptance criteria**

Must contain three subsections:

**§8.1 CLI** — full command reference (the table currently in README moves here). Must include every command from the current README's CLI table. Grouped logically (send/regen, log, character, model, memory, status, config, completions).

**§8.2 TUI** — `shore-tui`, what it adds over CLI (persistent connection, full terminal UI).

**§8.3 Matrix bridge** — `shore matrix setup`, `shore matrix register --username alice`, pointer to `examples/config.toml` for connection config.

- [ ] **Step 2: Write §8.1 CLI**

Under `## Clients`, append:

```markdown
Three clients ship with Shore: the CLI (`shore`), the TUI (`shore-tui`), and the Matrix bridge (`shore matrix`).

### CLI

```
shore [--character <name>] <command>
` ``

Full command reference:

#### Conversation

| Command | Description |
| ------- | ----------- |
| `shore send <message>` | Send a message |
| `shore send -i image.png <message>` | Attach an image |
| `shore send --thinking <message>` | Send with extended thinking |
| `shore regen` | Regenerate the last assistant response |
| `shore regen --guidance "..."` | Regenerate with guidance |

#### Log

| Command | Description |
| ------- | ----------- |
| `shore log` | Last 20 messages |
| `shore log -n 50` | Last N messages |
| `shore log -f` | Follow mode — stream new messages |
| `shore log last` / `shore log -1` | Single most recent message |
| `shore log edit <ref> <text>` | Edit a message |
| `shore log delete <ref>` | Delete a message |

#### Character

| Command | Description |
| ------- | ----------- |
| `shore character` | List available characters |
| `shore character <name>` | Switch to a character |
| `shore character --info` | Detail on the active character |
| `shore character --new` | Scaffold a new character directory |

#### Model

| Command | Description |
| ------- | ----------- |
| `shore model` | List available models |
| `shore model <alias>` | Runtime model override |
| `shore model --reset` | Clear the runtime override |

#### Memory

| Command | Description |
| ------- | ----------- |
| `shore memory <query>` | Free-text query |
| `shore memory compact` | Compact conversation → memory; then collate |
| `shore memory changelog` | Recent memory writes |
| `shore memory reindex` | Rebuild FTS and vector indexes |
| `shore memory purge` | Delete memory entries |
| `shore memory shell` | Interactive memory shell |

#### Status / config

| Command | Description |
| ------- | ----------- |
| `shore status` | Daemon and session status |
| `shore status --diagnostics` | Recent API calls, tool invocations, errors |
| `shore config` | Show current configuration |
| `shore config --path` | Print the config directory path |
| `shore config --check` | Validate configuration |
| `shore config --reset` | Reload config from disk (clear runtime overrides) |

#### Completions

| Command | Description |
| ------- | ----------- |
| `shore completions <shell>` | Generate shell completions for `bash`, `zsh`, `fish`, etc. |

The `--character` flag (or `SHORE_CHARACTER` env var) selects which character to talk to. If only one character exists it's selected automatically.
```

- [ ] **Step 3: Write §8.2 TUI**

Append:

```markdown
### TUI

```sh
shore-tui
` ``

`shore-tui` is a full-screen terminal client. It holds a persistent connection to the daemon, streams messages as they arrive, and gives you a richer editing surface than the CLI. Use the TUI when you want to *live in* a Shore conversation rather than send one-off commands.

Everything the CLI does is reachable from the TUI. The CLI is useful for scripting; the TUI is useful for actually talking.
```

- [ ] **Step 4: Write §8.3 Matrix bridge**

Append:

```markdown
### Matrix bridge

The `shore matrix` subcommand bridges a Shore character into a Matrix homeserver. Shore includes an embedded Synapse homeserver manager, so you don't have to set Matrix up separately.

```sh
shore matrix setup                        # initialize the embedded homeserver and provision characters
shore matrix register --username alice    # register a Matrix user account
` ``

After setup the character appears as a Matrix bot you can DM or invite into rooms. See [`examples/config.toml`](../examples/config.toml) for Matrix connection configuration.
```

- [ ] **Step 5: Verify**

Run:
```sh
rg -c '^#### (Conversation|Log|Character|Model|Memory|Status|Completions)' docs/FEATURES.md
```

Expected: `7`.

```sh
rg -c 'shore-tui|shore matrix setup|shore matrix register' docs/FEATURES.md
```

Expected: each present.

- [ ] **Step 6: Commit**

```sh
git add docs/FEATURES.md
git commit -m "docs(features): write clients section (CLI, TUI, Matrix)"
```

---

## Task 15: FEATURES.md §9-§12 (caching, diagnostics, remote access, completions)

**Files:**
- Modify: `docs/FEATURES.md`

- [ ] **Step 1: Acceptance criteria**

**§9 Prompt caching** — what prompt caching does (cost), provider pinning caveat (OpenRouter load balancing can break caching), `cache_ttl` tuning, cache forensics opt-in.

**§10 Diagnostics** — `shore status --diagnostics`, what it shows (API calls, tool invocations, errors, token accounting).

**§11 Remote access** — one italicized "no TLS yet" aide-memoire, localhost-only default, `[daemon].unsafe_allow_remote_access`, `allowed_hosts`, Tailscale recommendation.

**§12 Shell completions** — one paragraph + the command.

- [ ] **Step 2: Write §9 Prompt caching**

Replace placeholder under `## Prompt caching`:

```markdown
Prompt caching lets providers re-use the same long prompt prefix across requests at a fraction of the cost. Shore uses it aggressively — system prompts, character definitions, and a growing fraction of the conversation history all cache.

### Why it matters

Most of the tokens Shore sends on any given request are the same as the last request: the same system prompt, the same character definition, the same earlier conversation. Without caching you pay full input price for every one of those tokens on every request. With caching, identical prefixes cost ~10% of normal input price (Anthropic) or are free (some providers).

### Provider pinning caveat

OpenRouter, by default, load-balances across providers. Two consecutive requests can hit two different backends, which each have their own cache state — cache hits plummet. When using caching through OpenRouter, pin a single provider in your OpenRouter settings (e.g. `provider = { order = ["Anthropic"] }`).

### Tuning

Anthropic exposes a `cache_ttl` per model — how long cached prefixes stick around.

```toml
[chat.anthropic.claude-sonnet]
model_id = "claude-sonnet-4-6"
cache_ttl = "5m"    # short TTL for active conversations
# cache_ttl = "1h"  # longer for slow-moving characters
` ``

### Cache forensics

If caching looks broken, opt in to per-request forensics:

```toml
[advanced]
cache_forensics = true
` ``

Shore then writes each request's cache accounting (hits, misses, creates) to `{data_dir}/cache_forensics.jsonl`. Noisy — leave off in normal operation.

See [`CONFIGURATION.md` — `[advanced]`](CONFIGURATION.md#advanced).
```

- [ ] **Step 3: Write §10 Diagnostics**

Replace placeholder under `## Diagnostics`:

```markdown
Shore keeps a rolling record of recent activity: LLM requests, tool invocations, errors, and token/cost accounting.

```sh
shore status                  # daemon + session summary
shore status --diagnostics    # full diagnostics: API calls, tools, errors, tokens
` ``

The diagnostics output includes:

- Recent API calls (model, tokens in/out, cached tokens, duration)
- Recent tool invocations (name, duration, outcome)
- Recent errors with context
- Running token and cost totals for the current session

Use this when something is slow, something failed silently, or you want to know how much the last hour cost you.
```

- [ ] **Step 4: Write §11 Remote access**

Replace placeholder under `## Remote access`:

```markdown
Shore is localhost-only by default. You can opt in to binding on a non-loopback address (for reaching your daemon from another machine over a trusted network), but the protocol is unauthenticated TCP.

*No TLS yet — authenticated remote access is deferred. Only bind remotely on private overlays you already trust (Tailscale, WireGuard, VPN).*

### Enabling

```toml
[daemon]
addr = "100.64.0.1:7320"
unsafe_allow_remote_access = true
allowed_hosts = ["100.64.0.2"]   # optional source-IP allowlist
` ``

`unsafe_allow_remote_access = true` is required for any non-loopback bind. Without it Shore refuses to start.

`allowed_hosts` is a source-IP allowlist *only* — it is not authentication, and it is not encryption. It stops unknown IPs from connecting; it doesn't stop anyone who can spoof an allowed IP or who's listening on the wire.

### Tailscale

The most ergonomic private overlay. Both machines join your tailnet, each gets a stable `100.x.x.x` address, and `allowed_hosts` can list the peer's tailnet IP.

### Client side

On the client machine, point `client.toml` at the remote daemon:

```toml
default_address = "100.64.0.1:7320"
` ``

See [`CONFIGURATION.md` — `client.toml`](CONFIGURATION.md#clienttoml) and [`CONFIGURATION.md` — `[daemon]`](CONFIGURATION.md#daemon).
```

- [ ] **Step 5: Write §12 Shell completions**

Replace placeholder under `## Shell completions`:

```markdown
Generate shell completion scripts:

```sh
shore completions bash > ~/.local/share/bash-completion/completions/shore
shore completions zsh > ~/.zfunc/_shore
shore completions fish > ~/.config/fish/completions/shore.fish
` ``

Supports `bash`, `zsh`, `fish`, `elvish`, `powershell`.
```

- [ ] **Step 6: Verify**

Run:
```sh
rg -c 'cache_ttl|cache_forensics|OpenRouter' docs/FEATURES.md
```

Expected: caching concepts present.

```sh
rg -c 'shore status --diagnostics|no TLS|TLS|unsafe_allow_remote_access|Tailscale|shore completions' docs/FEATURES.md
```

Expected: all present.

- [ ] **Step 7: Commit**

```sh
git add docs/FEATURES.md
git commit -m "docs(features): write prompt caching, diagnostics, remote access, completions"
```

---

## Task 16: FEATURES.md cross-link verification

**Files:**
- Review: `docs/FEATURES.md`

- [ ] **Step 1: Every link to CONFIGURATION.md resolves**

List every intra-doc link in FEATURES.md:
```sh
rg -n '\(CONFIGURATION.md[^)]*\)' docs/FEATURES.md
```

For each `#anchor` fragment, verify the matching heading exists:
```sh
rg -n '^## ' docs/CONFIGURATION.md
```

Anchors are computed by lowercasing the heading and replacing non-alphanumeric runs with a single `-`. `[daemon]` becomes `daemon`. `[behavior.autonomy]` becomes `behaviorautonomy` (periods drop because they're in backticks; check the actual behavior with your markdown renderer — GitHub strips punctuation inside backticks from anchors).

If GitHub's anchoring for backticked headings doesn't match what `FEATURES.md` links to, rename headings in CONFIGURATION.md to non-backticked forms (e.g., `## Daemon` instead of `## [daemon]`) and update `FEATURES.md` links to match.

- [ ] **Step 2: Every term is defined on first use**

Scan `FEATURES.md` top-to-bottom. These terms must be defined on (or before) their first appearance:
- character
- compaction
- collation
- memory agent
- heartbeat
- probe
- active / dormant
- tick
- recap
- tool use

Run:
```sh
rg -n 'compaction|collation|interiority|tick|recap|dormant|heartbeat|probe' docs/FEATURES.md | head
```

Visually confirm the first occurrence of each is adjacent to a definition (bolded term, "**X** is …" pattern).

- [ ] **Step 3: Commit (if fixes)**

If heading renames or definition fixes were needed:
```sh
git add docs/FEATURES.md docs/CONFIGURATION.md
git commit -m "docs: fix cross-links and first-use definitions"
```

---

## Task 17: Rewrite README.md

**Files:**
- Rewrite: `README.md`

- [ ] **Step 1: Acceptance criteria**

Target: ~200 lines. New README contains exactly these sections in order:

1. `# Shore V2` title
2. One-paragraph "what Shore is" (persistent character engine, daemon + clients, multi-provider)
3. `## What makes Shore different` — 3-4 paragraphs expanding
4. `## A day of use` — one narrative paragraph
5. `## Prerequisites` — Rust 1.75+, SQLite headers, Linux
6. `## Install` — `cargo build --workspace --release`
7. `## Quick start` — set API key → minimal config.toml → character → daemon + send
8. `## What's next` — pointers to FEATURES.md, CONFIGURATION.md, ARCHITECTURE.md
9. `## Tests` — one line
10. `## Linting` — one line
11. `## License`

Must **not** contain:
- CLI reference table (moved to FEATURES.md)
- Provider table (moved to CONFIGURATION.md)
- Character-files deep-dive (moved to FEATURES.md)
- Template variables table (moved to FEATURES.md)
- Daemon startup precedence block (moved to CONFIGURATION.md)
- `client.toml` detail (moved to CONFIGURATION.md)
- "Platform notes" block

- [ ] **Step 2: Write the new README**

Replace the entire contents of `README.md` with:

```markdown
# Shore V2

Shore is a persistent AI character engine built in Rust. Not a chat wrapper — a daemon that hosts one or more AI characters, remembers everything you've said to them, and lets them speak on their own between your messages when configured.

## What makes Shore different

Most AI tools start fresh each session. Shore's characters don't — a character you met yesterday remembers yesterday. Memory compacts, condenses, and stays searchable; conversations pick up where they left off; the character has durable continuity.

Shore runs as a persistent daemon. You talk to it from three clients: a **CLI** (`shore`) for quick commands, a **TUI** (`shore-tui`) for sitting in a conversation, and a **Matrix bridge** (`shore matrix`) for reaching characters from any Matrix client. All three share the same character, memory, and conversation state because they all connect to the same daemon.

Shore supports six LLM providers out of the box: Anthropic, OpenRouter, DeepSeek, Gemini, xAI, and ZhipuAI. You can run different operations — main conversation, memory work, summarization, tool-use — on different models, so you pay for quality where it matters and speed where it doesn't.

With autonomy enabled, characters speak on their own: checking in after a silence, reflecting on past conversations, surfacing old memories at the right moment. Disabled by default; opt in when you're ready.

## A day of use

You say hi to your character in the morning. It replies with a thread it's been thinking about since yesterday (interiority tick overnight; it wrote a recap). Later you ask it about a Doom WAD you mentioned last week — it pulls the right memory, cached from a conversation three days ago. You go heads-down for the afternoon. Around dinner it probes you: *"hey, how'd that thing go?"* — because autonomy is on, heartbeat fired after the idle gap, and the character remembered you were working on something.

## Prerequisites

- **Rust** 1.75+ (stable toolchain)
- **SQLite** development headers (bundled via `rusqlite`)
- **Linux** — Shore is Linux-only in practice.

## Install

```sh
cargo build --workspace --release
` ``

Produces five binaries in `target/release/`:

| Binary | Purpose |
| ------ | ------- |
| `shore-daemon` | Persistent daemon (engine, memory, autonomy, LLM providers) |
| `shore` | CLI — stateless commands |
| `shore-tui` | Full terminal UI with a persistent connection |
| `shore-matrix` | Matrix bridge with embedded homeserver management |
| `shore-gui` *(if built)* | GUI client |

## Quick start

1. **Set an API key** (Anthropic shown; see [`docs/CONFIGURATION.md`](docs/CONFIGURATION.md#environment-variables) for others):

```sh
export ANTHROPIC_API_KEY=sk-ant-...
` ``

2. **Create a minimal config** at `~/.config/shore/config.toml`:

```toml
[defaults]
model = "claude-sonnet"

[chat.anthropic.claude-sonnet]
model_id = "claude-sonnet-4-6"
` ``

3. **Create a character**. Easiest way is the scaffolder:

```sh
./target/release/shore character --new
` ``

Or do it by hand — create `~/.config/shore/characters/Alice/character.md`:

```markdown
Alice is a warm, curious companion who loves literature and long conversations.
She has a dry sense of humour and remembers everything you've told her.
` ``

4. **Start the daemon and say hello**:

```sh
./target/release/shore-daemon &
./target/release/shore send "Hello!"
` ``

## What's next

- **[Features](docs/FEATURES.md)** — every feature explained: characters, memory, autonomy, interiority, tool use, clients (CLI / TUI / Matrix), prompt caching, diagnostics, remote access.
- **[Configuration](docs/CONFIGURATION.md)** — every config section with purpose, tradeoffs, and worked examples. See also [`examples/config.toml`](examples/config.toml) for the canonical option list.
- **[Architecture](docs/ARCHITECTURE.md)** — internals, for contributors.

## Tests

```sh
cargo test --workspace
` ``

## Linting

```sh
cargo clippy --workspace
` ``

## License

Private — all rights reserved.
```

(Note: all escaped `` ` `` triple-backticks in the plan render as real triple-backticks in the file.)

- [ ] **Step 3: Verify**

Line count:
```sh
wc -l README.md
```

Expected: roughly 100–130 lines (tighter than 200, fine).

Section count:
```sh
rg -c '^## ' README.md
```

Expected: 7–8 (What makes different / A day of use / Prerequisites / Install / Quick start / What's next / Tests / Linting / License — up to 9 but merging Tests+Linting is optional).

Forbidden-content check:
```sh
rg -c '^\| Command \| Description' README.md
```
Expected: `0` (CLI reference table is gone).

```sh
rg -c '^\| Provider key' README.md
```
Expected: `0` (provider table is gone).

```sh
rg -c 'unsafe_allow_remote_access|Tailscale|client\.toml' README.md
```
Expected: `0` (remote-access content moved).

Link resolution:
```sh
rg -n '\](docs/[A-Z]+\.md' README.md
```

Expected: links to `docs/FEATURES.md`, `docs/CONFIGURATION.md`, `docs/ARCHITECTURE.md` all resolve (files exist).

Quick-start command sanity:
```sh
grep -q 'cargo build --workspace --release' README.md && echo OK
grep -q 'shore character --new' README.md && echo OK
grep -q 'shore send' README.md && echo OK
```

Expected: three `OK` lines.

- [ ] **Step 4: Commit**

```sh
git add README.md
git commit -m "docs: rewrite README as quick-start + what-Shore-is entry point"
```

---

## Task 18: End-to-end verification

**Files:**
- Read-only review across `README.md`, `docs/FEATURES.md`, `docs/CONFIGURATION.md`

- [ ] **Step 1: Success criterion 1 — new user orientation**

Read `README.md` from top to bottom as if you've never seen Shore. Confirm:
- After the first paragraph, you know what Shore *is*
- After "What makes Shore different," you know whether you want to install it
- Quick start tells you exactly what to do with no gaps

If any beat breaks down, fix inline and commit separately.

- [ ] **Step 2: Success criterion 2 — quick start runs**

In a clean shell:

```sh
cd /home/eshen/Development/silvershore
export ANTHROPIC_API_KEY=... # (use a real key)
cargo build --workspace --release 2>&1 | tail -5
` ``

Expected: successful build; binaries in `target/release/`.

If you have a throwaway config dir, follow the README's Quick Start steps verbatim. Confirm `shore send "Hello!"` returns a response. (If you'd rather not run a real send, skip — but note it as a manual-verification gap in the task summary.)

- [ ] **Step 3: Success criterion 3 — every config key is reachable**

For every section in `examples/config.toml`, confirm at least one reference appears in the three markdown files combined:

```sh
for section in daemon defaults behavior.autonomy behavior.autonomy.heartbeat behavior.autonomy.interiority behavior.tool_use behavior.tool_use.tools behavior.tool_use.search memory.compaction memory.collation chat advanced; do
  hits=$(rg -c "$section" README.md docs/FEATURES.md docs/CONFIGURATION.md 2>/dev/null | wc -l)
  echo "$section -> $hits files"
done
` ``

Expected: every section shows up in at least one file.

- [ ] **Step 4: Success criterion 4 — every term is defined**

Read FEATURES.md for these terms in order of first appearance and confirm each is defined (bolded + `**X** is...` pattern) the first time it's used:

- character, model, provider, conversation
- memory, compaction, collation, memory agent
- autonomy, heartbeat, probe, active, dormant, personality
- interiority, tick, recap
- tool use, tool
- prompt caching

Eyeball scan; not a grep test. If you catch any undefined first-uses, fix inline.

- [ ] **Step 5: Success criterion 5 — grep reachability**

Sample spot-checks a user would do:

```sh
rg -l 'max_tool_rounds' README.md docs/*.md examples/config.toml
rg -l 'session_gap_secs' README.md docs/*.md examples/config.toml
rg -l 'cache_forensics' README.md docs/*.md examples/config.toml
rg -l 'allowed_hosts' README.md docs/*.md examples/config.toml
rg -l 'dormant_after_idle_time' README.md docs/*.md examples/config.toml
` ``

Expected: every key returns at least one file.

- [ ] **Step 6: Final commit (if fixes)**

If verification found gaps, fix them and commit:

```sh
git add README.md docs/FEATURES.md docs/CONFIGURATION.md
git commit -m "docs: verification pass fixes"
```

Otherwise:

```sh
echo "Verification clean — no fixes needed."
```

- [ ] **Step 7: Close out TODO entry**

Edit `TODO/TODO.md`: check off the "update *ALL* documentation" entry and its two sub-bullets. Commit:

```sh
git add TODO/TODO.md
git commit -m "todo: mark documentation overhaul complete"
```

---

## Summary

**Files produced:**
- `README.md` — rewritten
- `docs/FEATURES.md` — new (~500–700 lines)
- `docs/CONFIGURATION.md` — new (~400–500 lines)

**Files touched (minor):**
- `examples/config.toml` — only if Task 1 finds drift
- `TODO/TODO.md` — checks off the completed item

**Files untouched:**
- `docs/ARCHITECTURE.md`, `docs/DECISIONS.md`, `docs/QUIRKS.md`
- `docs/specs/` (except the spec + this plan)
- `CLAUDE.md`, `CHANGELOG.md`
- `shore-mcp/README.md`
- Every per-crate source directory

**Commit cadence:** one commit per task (17 commits for main work + optional verification commit).

**Rollback:** every task is a standalone commit; `git reset --hard <prior-commit>` on the branch reverses any single task cleanly.
