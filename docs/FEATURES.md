# Features

Every user-visible feature in Shore: what it does, why it exists, and how to use it. For the exhaustive config reference, see [`CONFIGURATION.md`](CONFIGURATION.md) and [`examples/config.toml`](../examples/config.toml).

## Characters

A **character** in Shore is an AI persona with its own personality, memory, and conversation history. You can have multiple characters on the same install and switch between them.

### Why they exist

The core Shore mental model: you aren't chatting with a generic LLM, you're talking with a specific character that remembers you. Every character has its own memory store, its own conversation log, and its own system prompt.

### How to use

Characters live in `~/.config/shore/characters/<Name>/workspace/`. The presence of a `SOUL.md` file makes a character discoverable — no config entry needed.

**Required file:**

- `SOUL.md` — describes personality, background, behavior. Injected into the system prompt as a dedicated block.

**Optional files:**

- `USER.md` — describes who *you* are, from this character's perspective.
- `AGENTS.md` — overrides the main system prompt template. Falls back to the built-in default if absent.
- `TOOLS.md` — extra tool-use guidance injected as its own system block.
- `HEARTBEAT.md` — heartbeat-only guidance injected during heartbeat ticks, not normal chat turns.
- `memory/**/*.md` — durable markdown memory files curated by compaction and file tools.

Shore migrates old `character.md`, `user.md`, and `prompts/system.md` layouts into the workspace automatically on first load. The old global `~/.config/shore/user.md` is used only as a one-time seed during migration.

### Template variables

Anywhere in `SOUL.md`, `USER.md`, `AGENTS.md`, or `TOOLS.md`:

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
```

See [`CONFIGURATION.md`](CONFIGURATION.md#orientation) for the directory layout.

## Models and providers

Shore runs against real LLM APIs. You can use different models for different operations — for example, a big model for conversation and a cheap fast model for background memory work.

### Why it exists

A serious AI character does a lot of background work: summarizing conversations into memory, running tool-use loops, periodically reflecting via heartbeat ticks, and looking things up. If every one of those jobs used the same big model, cost and latency would be miserable. Per-operation model slots let you pay for quality where it matters and speed where it doesn't.

### Supported providers

Shore ships with six providers built in: `anthropic`, `openrouter`, `deepseek`, `gemini`, `xai`, `zhipuai`. Each expects its own API key as an env var — see [`CONFIGURATION.md` — Environment variables](CONFIGURATION.md#environment-variables).

### Declaring a model

Each model is an alias under `[chat.<provider>.<alias>]`:

```toml
[chat.anthropic.claude-sonnet]
model_id = "claude-sonnet-4-6"

[chat.openrouter.haiku-fast]
model_id = "anthropic/claude-haiku-4-5"
```

You then reference aliases (`claude-sonnet`, `haiku-fast`) from `[defaults]`:

```toml
[defaults]
model = "claude-sonnet"        # main conversation
tool_model = "haiku-fast"      # tool-use calls
compaction = "haiku-fast"      # summarization
heartbeat = "claude-sonnet"  # private ticks
```

### Runtime overrides

```sh
shore model                    # list available aliases
shore model haiku-fast         # switch active model (runtime override, per daemon)
shore model --reset            # clear the override and return to [defaults] model
```

For the full set of per-model options and the provider table, see [`CONFIGURATION.md` — `[chat]`](CONFIGURATION.md#chat).

## Conversations

The core loop. Send messages, regenerate responses, edit history.

### Why it exists

You need more than "send and receive." Conversations drift, responses miss, you realize a previous message was wrong. Shore's CLI gives you the full edit surface — edit past messages, delete them, regenerate with guidance — without jumping into a DB.

### Sending

```sh
shore send "Hello!"
shore send -i ~/Pictures/photo.png "What is this?"    # attach an image
shore send --thinking "Work through this carefully"   # extended thinking mode
```

### Regenerating

```sh
shore regen                                           # regen the last assistant response
shore regen --guidance "be more concise this time"    # regen with a nudge
```

The guidance is a one-shot hint injected on top of the existing context — it doesn't permanently change the character.

### The conversation log

```sh
shore log                                             # last 20 messages
shore log -n 50                                       # last 50
shore log -f                                          # follow mode — stream new messages
shore log last                                        # or: shore log -1 — one message
shore log edit <ref> "new text"                       # edit a past message
shore log delete <ref>                                # delete a message
```

`<ref>` accepts either a message ID or a negative index (`-1` = most recent, `-2` = previous, …).

## Memory

The character remembers things. Not just recent messages — things you told it weeks ago, facts about you, preferences, ongoing threads. Memory persists across sessions and daemon restarts.

### Why it exists

A character without durable memory is a parrot. Shore characters accumulate context deliberately: important turns from your conversations get compacted into markdown files the character can read, edit, and reorganize over time.

### How it's stored

Shore keeps each character's long-term memory as markdown under:

```text
$XDG_CONFIG_HOME/shore/characters/<Character>/workspace/memory/
```

The assistant can work with those files through memory tools (`memory_read`, `memory_write`, `memory_search`, `memory_list`) and through workspace file tools using the `memory/...` prefix when memory access is enabled. The only SQLite migration path is `scripts/migrate-memory.py`.

### Compaction

**Compaction** is the process of turning old conversation turns into durable markdown memory. After the session has been idle for `[memory.compaction].idle_trigger` (default `"30m"`), Shore summarizes older turns, gives the compaction model a bounded snapshot of existing memory files, writes updated markdown files, and drops compacted turns from the hot conversation log.

Run it manually:

```sh
shore memory compact
```

### Queries and files

```sh
shore memory "doom launcher"          # LLM-assisted markdown memory query
shore memory --direct "doom"          # direct text-search result formatting
shore memory compact                  # compact old conversation into markdown memory
```

For direct inspection, open the character's `workspace/memory/` directory or ask the character to use `memory_list`, `memory_read`, `memory_write`, and `memory_search`. The old interactive memory shell, collation, purge, and reindex commands are removed.

See [`CONFIGURATION.md` — `[memory]`](CONFIGURATION.md#memory) for tunables.

## Autonomy

**Autonomy** is the character acting on its own, without you prompting — thinking things through, using tools, and optionally sending an unprompted message. Disabled by default. You turn it on in config; the character then decides for itself when to do something.

### Why it exists

A character that only speaks when addressed feels like a vending machine. With autonomy on, the character can reflect on something you said hours ago, do its own research between your messages, consolidate memory, and decide on its own whether to reach out.

### Active vs dormant

The character has two phases:

- **Active** — may think on its own and send unprompted messages
- **Dormant** — silent; wakes up when you send a message

The character drifts from active to dormant after stretches of no engagement, and back to active when you speak up again.

### Heartbeat ticks

The core autonomy primitive is a **heartbeat tick** — one private moment where the character thinks, may use tools (search memory, look things up on the web, read its scratchpad, schedule its own next tick), and may or may not produce a message to send you.

At the end of every tick the character writes a **recap** — a short note about what it thought about and what it plans to follow up on. Recaps carry state forward from tick to tick, giving the character narrative continuity across its private life.

### Scheduling

The character self-schedules the next tick when it finishes one. If it doesn't pick a time, Shore falls back to `fallback_heartbeat_interval` (default `1h`).

A floor (`minimum_heartbeat_latency`, default `1h`) prevents ticks from piling up right after you send a message — the character needs breathing room.

### Wrap-up

If a tick goes long (many tool-use rounds), Shore caps it at `max_tool_rounds` (default `12`) and forces a wrap-up recap. This is a safety limit — the character can't spin forever inside a single tick.

### Dormancy

Two paths lead to the dormant phase:

- `dormant_after_heartbeat_turns` — this many ticks in a row with no user reply → sleep (default `3`)
- `dormant_after_idle_time` — this much total idle time → sleep until the user returns (default `48h`)

### How to enable

```toml
[behavior.autonomy]
enabled = true

[behavior.autonomy.heartbeat]
enabled = true
```

Both switches must be on. `[behavior.autonomy]` is the master gate; the `heartbeat` sub-table controls the tick behavior.

See [`CONFIGURATION.md` — `[behavior.autonomy]`](CONFIGURATION.md#behaviorautonomy) for every tunable.

## Tool use

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
- `generate_image` — create a new image. Uses the model in `[defaults] image_generation`.

#### Scratchpad

A persistent filesystem the character can read and write for notes that outlive any single conversation — think of it as the character's private notebook.

- `scratchpad_list` — browse the scratchpad tree.
- `scratchpad_read` — read a scratchpad file.
- `scratchpad_write` — create or overwrite a scratchpad file.
- `scratchpad_delete` — remove a scratchpad file.

#### Activity

- `activity_heatmap` — generate a heatmap of recent usage activity.

### Loop budget

The character can invoke tools iteratively — use one, see the result, decide whether to use another. `[behavior.tool_use] max_iterations` (default 10) is the cap on how many rounds per turn. Hit the cap and Shore forces a final response.

See [`CONFIGURATION.md` — `[behavior.tool_use]`](CONFIGURATION.md#behaviortool_use) for toggles and search tuning.

## Clients

Three clients ship with Shore: the CLI (`shore`), the TUI (`shore-tui`), and the Matrix bridge (`shore matrix`).

### CLI

```sh
shore [--character <name>] <command>
```

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
| `shore log --heartbeat` | Show the heartbeat / autonomy event log (wakeups, ticks, dormancy transitions) |
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
| `shore memory compact` | Compact conversation into markdown memory |
| `shore memory changelog` | Recent memory writes |

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

### TUI

```sh
shore-tui
```

`shore-tui` is a full-screen terminal client. It holds a persistent connection to the daemon, streams messages as they arrive, and gives you a richer editing surface than the CLI. Use the TUI when you want to *live in* a Shore conversation rather than send one-off commands.

Everything the CLI does is reachable from the TUI. The CLI is useful for scripting; the TUI is useful for actually talking.

### Matrix bridge

The `shore matrix` subcommand bridges a Shore character into a Matrix homeserver. Shore includes an embedded Synapse homeserver manager, so you don't have to set Matrix up separately.

```sh
shore matrix setup                        # initialize the embedded homeserver and provision characters
shore matrix register --username alice    # register a Matrix user account
```

After setup the character appears as a Matrix bot you can DM or invite into rooms. See [`examples/config.toml`](../examples/config.toml) for Matrix connection configuration.

## Prompt caching

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
```

### Cache forensics

If caching looks broken, opt in to per-request forensics:

```toml
[advanced]
cache_forensics = true
```

Shore then writes each request's cache accounting (hits, misses, creates) to `{data_dir}/cache_forensics.jsonl`. Noisy — leave off in normal operation.

See [`CONFIGURATION.md` — `[advanced]`](CONFIGURATION.md#advanced).

## Diagnostics

Shore keeps a rolling record of recent activity: LLM requests, tool invocations, errors, and token/cost accounting.

```sh
shore status                  # daemon + session summary
shore status --diagnostics    # full diagnostics: API calls, tools, errors, tokens
```

The diagnostics output includes:

- Recent API calls (model, tokens in/out, cached tokens, duration)
- Recent tool invocations (name, duration, outcome)
- Recent errors with context
- Running token and cost totals for the current session

Use this when something is slow, something failed silently, or you want to know how much the last hour cost you.

## Remote access

Shore is localhost-only by default. You can opt in to binding on a non-loopback address (for reaching your daemon from another machine over a trusted network), but the protocol is unauthenticated TCP.

*No TLS yet — authenticated remote access is deferred. Only bind remotely on private overlays you already trust (Tailscale, WireGuard, VPN).*

### Enabling

```toml
[daemon]
addr = "100.64.0.1:7320"
unsafe_allow_remote_access = true
allowed_hosts = ["100.64.0.2"]   # optional source-IP allowlist
```

`unsafe_allow_remote_access = true` is required for any non-loopback bind. Without it Shore refuses to start.

`allowed_hosts` is a source-IP allowlist *only* — it is not authentication, and it is not encryption. It stops unknown IPs from connecting; it doesn't stop anyone who can spoof an allowed IP or who's listening on the wire.

### Tailscale

The most ergonomic private overlay. Both machines join your tailnet, each gets a stable `100.x.x.x` address, and `allowed_hosts` can list the peer's tailnet IP.

### Client side

On the client machine, point `client.toml` at the remote daemon:

```toml
default_address = "100.64.0.1:7320"
```

See [`CONFIGURATION.md` — `client.toml`](CONFIGURATION.md#clienttoml) and [`CONFIGURATION.md` — `[daemon]`](CONFIGURATION.md#daemon).

## Shell completions

Generate shell completion scripts:

```sh
shore completions bash > ~/.local/share/bash-completion/completions/shore
shore completions zsh > ~/.zfunc/_shore
shore completions fish > ~/.config/fish/completions/shore.fish
```

Supports `bash`, `zsh`, `fish`, `elvish`, `powershell`.
