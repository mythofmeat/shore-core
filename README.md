# Shore

Shore is a persistent AI character engine built in Rust. It is not a stateless chat wrapper: a long-running daemon owns character state, conversation history, memory, autonomy, tools, cache accounting, and client connections.

The project goal is personal and specific: make an AI character chat program that improves on the parts of SillyTavern that hurt, while leaning into long-lived character continuity, inspectable memory, Anthropic cache discipline, and a character that can do useful things with its own time.

For the current branch notes, read [OpenClawify Patch Notes](docs/PATCH_NOTES_OPENCLAWIFY.md).

## What Matters

- **One daemon, many clients.** CLI, TUI, GUI, MCP, and Matrix all talk to the same daemon state.
- **Markdown memory.** Long-term memory lives under each character's `workspace/memory/` as ordinary files.
- **Cache-safe prompt edits.** Character self-edits to protected prompt files are staged and only activate at compaction/reload boundaries.
- **Heartbeat autonomy.** Characters can use private heartbeat ticks to reflect, maintain memory, use tools, schedule the next wake, and optionally send a message.
- **Tool-rich conversations.** Characters can search memory, read/write workspace files, use web search, generate/send images, inspect activity, roll dice, check time, and use a scratchpad.
- **Budget awareness.** Usage and cost are recorded in a SQLite ledger; Anthropic prompt-cache behavior is tracked closely.

## Prerequisites

- Rust stable toolchain
- Linux in practice
- SQLite support, via bundled `rusqlite`
- Provider API keys for the models you configure

## Build

```sh
cargo build --workspace --release
```

Main binaries:

| Binary | Purpose |
| --- | --- |
| `shore-daemon` | persistent daemon |
| `shore` | CLI client |
| `shore-tui` | terminal UI |
| `shore-matrix` | Matrix bridge |
| `shore-mcp` | debug/development MCP bridge |
| `shore-gui` | Tauri desktop GUI, if built |

## Quick Start

Create `~/.config/shore/config.toml`:

```toml
[defaults]
model = "claude-sonnet"

[chat.anthropic.claude-sonnet]
model_id = "claude-sonnet-4-6"
api_key_env = "ANTHROPIC_API_KEY"
cache_ttl = "1h"
```

Create a character workspace:

```text
~/.config/shore/characters/Alice/workspace/
  SOUL.md
  USER.md
  AGENTS.md
  TOOLS.md
  HEARTBEAT.md
  memory/
```

Minimal `SOUL.md`:

```markdown
Alice is a warm, curious companion who loves literature and long conversations.
She remembers the user across time and keeps her own notes carefully.
```

Start the daemon and send a message:

```sh
target/release/shore-daemon &
target/release/shore send "Hello!"
```

Legacy `character.md`, `user.md`, and `prompts/system.md` character layouts are migrated into the workspace on first load.

## Current Docs

- [Goals](GOALS.md) — source of truth for project intent
- [Features](FEATURES.md) — user-facing behavior
- [Configuration](CONFIGURATION.md) — config reference
- [Architecture](ARCHITECTURE.md) — implementation map
- [Invariants](docs/INVARIANTS.md) — correctness constraints
- [Quirks](docs/QUIRKS.md) — sharp edges and external weirdness
- [OpenClawify Patch Notes](docs/PATCH_NOTES_OPENCLAWIFY.md) — what changed on this branch

## Tests

```sh
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace --release
```

Live API verification is intentionally separate because it uses real provider credentials and costs money.

## License

Private — all rights reserved.
