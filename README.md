# Shore V2

Shore is a modular AI character engine built on a Rust core with a TypeScript LLM provider proxy. It provides persistent memory, autonomous behaviour, and multi-platform connectivity through a clean wire protocol.

## Architecture

| Binary | Language | Role |
|--------|----------|------|
| `shore-daemon` | Rust | Persistent daemon — engine, memory, autonomy, tool loop |
| `shore-llm` | TypeScript | LLM provider proxy — wraps official SDKs, streams completions |
| `shore-cli` | Rust | CLI — stateless commands |
| `shore-tui` | Rust | TUI — persistent connection, full terminal UI |
| `shore-matrix` | Rust | Matrix bridge (includes Synapse management) |

All Rust services communicate via the Shore Wire Protocol (SWP) over Unix sockets or TCP. See [ARCHITECTURE.md](ARCHITECTURE.md) for the full design.

## Prerequisites

- **Rust** 1.75+ (stable)
- **Node.js** 20+ and **npm**
- **SQLite** development headers (bundled via `rusqlite`)

## Build

### Rust workspace (daemon, CLI, TUI, Matrix bridge)

```sh
cargo build --workspace --release
```

This produces four binaries in `target/release/`:
- `shore-daemon`
- `shore-cli`
- `shore-tui`
- `shore-matrix`

### LLM provider proxy

```sh
cd shore-llm
npm install
npm run build
```

## Configuration

Configuration files live in `$XDG_CONFIG_HOME/shore/` (default `~/.config/shore/`).

### config.toml

```toml
[daemon]
socket_path = "/tmp/shore.sock"

[character]
name = "Shore"
```

### models.toml

```toml
[[models]]
name = "claude-sonnet"
provider = "anthropic"
model_id = "claude-sonnet-4-20250514"

[[models]]
name = "gpt-4o"
provider = "openai"
model_id = "gpt-4o"
```

See `examples/` for annotated example files.

## Usage

Start the daemon:

```sh
shore-daemon
```

In a separate terminal, use the CLI:

```sh
shore send "Hello, Shore!"
shore status
shore memory "recent events"
shore compact
shore collate
```

Or launch the TUI for an interactive session:

```sh
shore-tui
```

Start the LLM proxy (required by the daemon for completions):

```sh
cd shore-llm
npm start
```

## Running Tests

```sh
# Rust
cargo test --workspace

# TypeScript
cd shore-llm && npm test
```

## Linting

```sh
cargo clippy --workspace
```

## Migrating from V1

If upgrading from Shore V1 (Python), see [docs/V1-MIGRATION.md](docs/V1-MIGRATION.md) for a complete mapping of config keys, data paths, and binary names. The daemon automatically detects V1-style configuration and prints migration guidance at startup.

## License

Private — all rights reserved.
