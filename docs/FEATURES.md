# Features

Every user-visible feature in Shore: what it does, why it exists, and how to use it. For the exhaustive config reference, see [`CONFIGURATION.md`](CONFIGURATION.md) and [`examples/config.toml`](../examples/config.toml).

## Characters

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
```

See [`CONFIGURATION.md`](CONFIGURATION.md#orientation) for the directory layout.

## Models and providers

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
```

You then reference aliases (`claude-sonnet`, `haiku-fast`) from `[defaults]`:

```toml
[defaults]
model = "claude-sonnet"        # main conversation
tool_model = "haiku-fast"      # tool-use calls
compaction = "haiku-fast"      # summarization
interiority = "claude-sonnet"  # private ticks
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

A character without durable memory is a parrot. Shore characters accumulate context deliberately: important turns from your conversations get compacted into searchable memory entries, and those entries get folded together over time so related facts coalesce instead of accumulating as duplicates.

### How it's stored

Shore keeps memory in two parallel indexes (both SQLite-backed):

- **Vector store** — semantic search. "That thing Ren said about the doom launcher" finds the right memory even if you don't remember the exact words.
- **Full-text search (FTS)** — keyword search. Exact phrases, names, filenames.

Every query runs against both; results merge.

### Compaction

**Compaction** is the process of turning old conversation turns into durable memory entries. After the session has been idle for `[memory.compaction].idle_trigger` (default `"30m"`), Shore summarizes older turns into entries and drops them from the hot conversation log.

Run it manually:

```sh
shore memory compact
```

### Collation

**Collation** reorganizes existing memory entries: merging duplicates, splitting overloaded entries, normalizing wording. It runs periodically in the background when `[memory.collation].enabled = true`, and can be triggered manually:

```sh
shore memory compact   # runs compaction, then collation
```

Without collation, memory grows into a slurry of near-duplicates. With it, related facts settle into coherent entries.

### The memory agent

Some operations (saving new memories, answering structured queries about memory) run through a small **memory agent** — a cheap model whose only job is to decide whether to save, and what to save. Configure which model it uses via `[defaults] memory_agent`.

### Queries and changelog

```sh
shore memory "doom launcher"         # free-text query
shore memory changelog               # recent memory writes
shore memory reindex                 # rebuild FTS and vector indexes
shore memory purge                   # delete memory entries (prompts for confirmation)
```

### Memory shell

For exploring or debugging memory, drop into the interactive shell:

```sh
shore memory shell
```

Inside the shell you can query, save, and edit memory directly using the memory agent.

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

### Interiority ticks

The core autonomy primitive is an **interiority tick** — one private moment where the character thinks, may use tools (search memory, look things up on the web, read its scratchpad, schedule its own next tick), and may or may not produce a message to send you.

At the end of every tick the character writes a **recap** — a short note about what it thought about and what it plans to follow up on. Recaps carry state forward from tick to tick, giving the character narrative continuity across its private life.

### Scheduling

The character self-schedules the next tick when it finishes one. If it doesn't pick a time, Shore falls back to `fallback_interiority_interval` (default `1h`).

A floor (`minimum_interiority_latency`, default `1h`) prevents ticks from piling up right after you send a message — the character needs breathing room.

### Wrap-up

If a tick goes long (many tool-use rounds), Shore caps it at `max_tool_rounds` (default `12`) and forces a wrap-up recap. This is a safety limit — the character can't spin forever inside a single tick.

### Dormancy

Two paths lead to the dormant phase:

- `dormant_after_interiority_turns` — this many ticks in a row with no user reply → sleep (default `3`)
- `dormant_after_idle_time` — this much total idle time → sleep until the user returns (default `48h`)

### How to enable

```toml
[behavior.autonomy]
enabled = true

[behavior.autonomy.interiority]
enabled = true
```

Both switches must be on. `[behavior.autonomy]` is the master gate; the `interiority` sub-table controls the tick behavior.

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
- `list_images` — list previously sent or generated images.
- `recall_image` — re-send a previously generated image by reference.
- `generate_image` — create a new image. Uses the model in `[defaults] image_generation`.
- `remember_image` — save a user-shared image to memory with context the character can recall later.

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
| `shore log --heartbeat` | Show the interiority / autonomy event log (wakeups, ticks, dormancy transitions) |
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

<!-- written in Task 15 -->

## Diagnostics

<!-- written in Task 15 -->

## Remote access

<!-- written in Task 15 -->

## Shell completions

<!-- written in Task 15 -->
