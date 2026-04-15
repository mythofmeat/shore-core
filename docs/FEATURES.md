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

<!-- written in Task 11 -->

## Autonomy

<!-- written in Task 12 — covers both ambient presence and interiority ticks -->

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
