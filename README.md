# Shore V2

Shore is a modular AI character engine built entirely in Rust. It provides persistent memory, autonomous behaviour, and multi-platform connectivity through a clean wire protocol.

## Architecture

| Binary | Language | Role |
|--------|----------|------|
| `shore-daemon` | Rust | Persistent daemon — engine, memory, autonomy, tool loop, LLM providers |
| `shore` | Rust | CLI — stateless commands |
| `shore-tui` | Rust | TUI — persistent connection, full terminal UI |
| `shore-matrix` | Rust | Matrix bridge (includes embedded homeserver management) |

All Rust services communicate via the Shore Wire Protocol (SWP) over Unix sockets or TCP. See [ARCHITECTURE.md](ARCHITECTURE.md) for the full design.

## Prerequisites

- **Rust** 1.75+ (stable)
- **SQLite** development headers (bundled via `rusqlite`)

## Build

### Rust workspace

```sh
cargo build --workspace --release
```

Produces four binaries in `target/release/`: `shore-daemon`, `shore`, `shore-tui`, `shore-matrix`.

## Configuration

All configuration lives in `$XDG_CONFIG_HOME/shore/` (default: `~/.config/shore/`).

### Directory layout

```
~/.config/shore/
├── config.toml                      # Main configuration (required)
├── user.md                          # Who you are — global fallback
├── prompts/
│   └── system.md                    # System prompt template — global fallback
└── characters/
    └── <CharacterName>/
        ├── character.md             # Character definition (required — enables discovery)
        ├── user.md                  # Who you are, from this character's perspective (optional)
        └── prompts/
            └── system.md            # System prompt template override for this character (optional)
```

Characters are discovered automatically by scanning `characters/` for subdirectories that contain `character.md`. No config entry is needed to register a character.

### Minimum viable setup

1. Set your API key (Anthropic example):
   ```sh
   export ANTHROPIC_API_KEY=sk-ant-...
   ```

2. Create `~/.config/shore/config.toml`:
   ```toml
   [defaults]
   model = "claude-sonnet"

   [chat.anthropic.claude-sonnet]
   model_id = "claude-sonnet-4-6"
   ```

3. Create a character:
   ```sh
   mkdir -p ~/.config/shore/characters/Alice
   ```
   Write `~/.config/shore/characters/Alice/character.md`:
   ```markdown
   Alice is a warm, curious companion who loves literature and long conversations.
   She has a dry sense of humour and remembers everything you've told her.
   ```

4. Start the daemon and talk:
   ```sh
   shore-daemon &
   shore send "Hello!"
   ```

Or use `shore character --new` to scaffold a character directory interactively.

### Character files

#### `character.md`

Describes the character's personality, background, and behaviour. This is injected into the system prompt as a dedicated block. Required for the character to be discovered.

#### `user.md`

Describes who *you* are — used to give the character context about the person it's talking to. Resolution order:

1. `characters/<name>/user.md` — character-specific (how this character knows you)
2. `user.md` — global fallback

Optional. If neither exists, no user block is injected.

#### `prompts/system.md`

The framing template that wraps the conversation. Controls tone, format, roleplay rules, etc. Resolution order:

1. `characters/<name>/prompts/system.md` — character-specific
2. `prompts/system.md` — global fallback
3. Built-in default: `You are {{char}}, in conversation with {{user}}.`

Supports template variables:

| Variable | Value |
|----------|-------|
| `{{char}}` / `{{character_name}}` | Character name |
| `{{user}}` | Your display name (`display_name` in config, or `$USER`) |
| `{{date}}` | Current date (e.g. `Friday, 2026-03-28`) |
| `{{time}}` | Current time (HH:MM) |

### config.toml

See `examples/config.toml` for a fully annotated reference. Key sections:

```toml
[defaults]
model = "claude-sonnet"          # Must match a key in [chat.*.*]
display_name = "Your Name"       # {{user}} in templates; falls back to $USER

[behavior.autonomy]
enabled = false                  # Allow unprompted messages from the character
personality = 0.5                # 0.0–1.0; shapes probe frequency

[behavior.tool_use]
enabled = true
max_iterations = 10

[memory]
rag_results = 5                  # Memory entries injected per prompt
rag_threshold = 0.3

[memory.compaction]
enabled = true
idle_trigger_minutes = 30

[chat.anthropic.claude-sonnet]
model_id = "claude-sonnet-4-6"

[chat.openai.gpt-4o]
model_id = "gpt-4o"
```

#### Model configuration

Models are declared under `[chat.<provider>.<alias>]`. The alias is what you pass to `--model` or set in `[defaults] model`. Known providers (with pre-configured defaults):

| Provider key | SDK | API key env var |
|---|---|---|
| `anthropic` | anthropic | `ANTHROPIC_API_KEY` |
| `openrouter` | openai | `OPENROUTER_API_KEY` |
| `deepseek` | deepseek | `DEEPSEEK_API_KEY` |
| `gemini` | gemini | `GEMINI_API_KEY` |
| `xai` | openai | `XAI_API_KEY` |
| `zhipuai` | zhipuai | `ZAI_API_KEY` |

Per-model options include `temperature`, `max_tokens`, `max_context_tokens`, `reasoning_effort`, `budget_tokens`, `cache_ttl`, and more. See `examples/config.toml` for the full list.

## Usage

Start the daemon:

```sh
shore-daemon
```

### CLI reference

```
shore [--character <name>] <command>
```

| Command | Description |
|---------|-------------|
| `shore send <message>` | Send a message |
| `shore send -i image.png <message>` | Send a message with an attached image |
| `shore send --thinking <message>` | Send with extended thinking enabled |
| `shore regen` | Regenerate the last assistant response |
| `shore regen --guidance "..."` | Regenerate with optional guidance |
| `shore log` | Show recent conversation (last 20 messages) |
| `shore log -n 50` | Show last 50 messages |
| `shore log -f` | Follow mode — stream new messages as they arrive |
| `shore log last` / `shore log -1` | Show a single message by reference |
| `shore log edit <ref> <text>` | Edit a message in the conversation |
| `shore log delete <ref>` | Delete a message |
| `shore character` | List available characters |
| `shore character <name>` | Switch to a character |
| `shore character --info` | Show detailed info about the current character |
| `shore character --new` | Scaffold a new character directory |
| `shore model` | List available models |
| `shore model <alias>` | Switch to a model (runtime override) |
| `shore model --reset` | Clear runtime model override |
| `shore memory <query>` | Search memory |
| `shore memory compact` | Compact conversation → memory entries, then collate |
| `shore memory changelog` | Show recent memory changelog |
| `shore memory reindex` | Rebuild FTS and vector indexes |
| `shore memory shell` | Interactive memory agent shell |
| `shore status` | Show daemon and session status |
| `shore status --diagnostics` | Show recent API calls, tool invocations, and errors |
| `shore config` | Show current configuration |
| `shore config --path` | Print the config directory path |
| `shore config --check` | Validate configuration |
| `shore config --reset` | Reload config from disk (clear runtime overrides) |
| `shore completions <shell>` | Generate shell completions |

The `--character` flag (or `SHORE_CHARACTER` env var) selects which character to talk to. If only one character exists, it is selected automatically.

### TUI

```sh
shore-tui
```

Launches a full terminal UI with a persistent connection to the daemon.

### Matrix bridge

```sh
shore matrix setup     # Initialize embedded homeserver and provision characters
shore matrix register --username alice  # Register a user account
```

See `examples/config.toml` for Matrix connection configuration.

## Running Tests

```sh
cargo test --workspace
```

## Linting

```sh
cargo clippy --workspace
```

## Migrating from V1

If upgrading from Shore V1 (Python), see [docs/V1-MIGRATION.md](docs/past_versions/V1-MIGRATION.md) for a complete mapping of config keys, data paths, and binary names. The daemon automatically detects V1-style configuration and prints migration guidance at startup.

## License

Private — all rights reserved.
