# Shore V2

Shore is a persistent AI character engine built in Rust. Not a chat wrapper — a daemon that hosts one or more AI characters, remembers everything you've said to them, and lets them speak on their own between your messages when configured.

## What makes Shore different

Most AI tools start fresh each session. Shore's characters don't — a character you met yesterday remembers yesterday. Memory compacts, condenses, and stays searchable; conversations pick up where they left off; the character has durable continuity.

Shore runs as a persistent daemon. You talk to it from three clients: a **CLI** (`shore`) for quick commands, a **TUI** (`shore-tui`) for sitting in a conversation, and a **Matrix bridge** (`shore matrix`) for reaching characters from any Matrix client. All three share the same character, memory, and conversation state because they all connect to the same daemon.

Shore supports six LLM providers out of the box: Anthropic, OpenRouter, DeepSeek, Gemini, xAI, and ZhipuAI. You can run different operations — main conversation, memory work, summarization, tool-use — on different models, so you pay for quality where it matters and speed where it doesn't.

With autonomy enabled, characters speak on their own: checking in after a silence, reflecting on past conversations, surfacing old memories at the right moment. Disabled by default; opt in when you're ready.

## A day of use

You say hi to your character in the morning. It replies with a thread it's been thinking about since yesterday (a heartbeat tick overnight; it wrote a recap before going dormant). Later you ask it about a Doom WAD you mentioned last week — it pulls the right memory, cached from a conversation three days ago. You go heads-down for the afternoon. Around dinner, a scheduled tick fires and the character checks in on its own: *"hey, how'd that thing go?"* — because autonomy is on, and the character remembered you were working on something.

## Prerequisites

- **Rust** 1.75+ (stable toolchain)
- **SQLite** development headers (bundled via `rusqlite`)
- **Linux** — Shore is Linux-only in practice.

## Install

```sh
cargo build --workspace --release
```

Produces five binaries in `target/release/`:

| Binary | Purpose |
| ------ | ------- |
| `shore-daemon` | Persistent daemon (engine, memory, autonomy, LLM providers) |
| `shore` | CLI — stateless commands |
| `shore-tui` | Full terminal UI with a persistent connection |
| `shore-matrix` | Matrix bridge with embedded homeserver management |
| `shore-gui` *(if built)* | Desktop GUI client (Tauri + React) |
| `shore-gui-godot` *(if built)* | Experimental Godot-based GUI (RSI exercise) |

## Quick start

1. **Set an API key** (Anthropic shown; see [`docs/CONFIGURATION.md`](docs/CONFIGURATION.md#environment-variables) for others):

```sh
export ANTHROPIC_API_KEY=sk-ant-...
```

2. **Create a minimal config** at `~/.config/shore/config.toml`:

```toml
[defaults]
model = "claude-sonnet"

[chat.anthropic.claude-sonnet]
model_id = "claude-sonnet-4-6"
```

3. **Create a character**. Easiest way is the scaffolder:

```sh
./target/release/shore character --new
```

Or do it by hand — create `~/.config/shore/characters/Alice/character.md`:

```markdown
Alice is a warm, curious companion who loves literature and long conversations.
She has a dry sense of humour and remembers everything you've told her.
```

4. **Start the daemon and say hello**:

```sh
./target/release/shore-daemon &
./target/release/shore send "Hello!"
```

## What's next

- **[Features](docs/FEATURES.md)** — every feature explained: characters, memory, autonomy, heartbeat, tool use, clients (CLI / TUI / Matrix), prompt caching, diagnostics, remote access.
- **[Configuration](docs/CONFIGURATION.md)** — every config section with purpose, tradeoffs, and worked examples. See also [`examples/config.toml`](examples/config.toml) for the canonical option list.
- **[Architecture](docs/ARCHITECTURE.md)** — internals, for contributors.

## Tests

```sh
cargo test --workspace
```

## Linting

```sh
cargo clippy --workspace
```

## License

Private — all rights reserved.
