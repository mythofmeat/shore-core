# Shore

Shore is a persistent AI character engine built in Rust. A long-running daemon
owns character state, conversation history, memory, autonomy, tools, cache
accounting, and client connections. Clients are views and command senders; they
do not fork authoritative state.

The intent is personal and specific: make an AI character chat program that
keeps the SillyTavern-style repairable conversation workflow, but improves the
parts that hurt with long-lived character continuity, inspectable markdown
memory, Anthropic cache discipline, useful tools, and private autonomous time.

For release history, read [CHANGELOG.md](CHANGELOG.md).

## What Matters

- **One daemon, many clients.** CLI, TUI, GUI, MCP, and Matrix all talk to the
  same daemon state.
- **Repairable replies.** Regenerating an assistant response is non-destructive:
  old and new responses are kept as selectable alternates.
- **Archive-visible history.** Compacted conversation segments remain visible in
  bounded CLI/TUI scrollback pages, with a boundary showing what is outside
  active context.
- **Markdown memory.** Long-term memory lives under each character's
  `workspace/memory/` as ordinary git-diffable files.
- **Cache-safe prompt edits.** Character self-edits to prompt-visible files are
  staged and only activate at compaction/reload boundaries.
- **Heartbeat autonomy.** Characters can use private heartbeat ticks to reflect,
  maintain memory, use tools, schedule the next wake, and optionally message the
  user.
- **Tool-rich conversations.** Characters can inspect and edit workspace files,
  search workspace/history, use web/image/time/activity tools, and run narrow
  workspace commands.
- **Budget awareness.** Usage and cost are recorded in SQLite with model,
  call-kind, and configured API-key breakdowns; configurable hourly, daily,
  weekly, and monthly budgets can warn, block, or pause background work.

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
~/.config/shore/characters/Alice/
  avatar.png    # optional Matrix profile / desktop notification avatar
  workspace/
    SOUL.md       # character identity
    USER.md       # what this character knows about the user
    AGENTS.md     # standing operating guidance
    TOOLS.md      # tool-use guidance
    HEARTBEAT.md  # heartbeat-only guidance
    MEMORY.md     # optional/generated prompt-visible memory index
    memory/       # markdown long-term memory
```

Minimal `SOUL.md`:

```markdown
Alice is a warm, curious companion who loves literature and long conversations.
She remembers the user across time and keeps her own notes carefully.
```

Start the daemon and send a message:

```sh
cargo build --release -p shore-daemon -p shore-cli
target/release/shore-daemon &
target/release/shore send "Hello!"
```

To receive desktop notifications for autonomous character messages, run the
listener in your user session:

```sh
target/release/shore notify
```

Packaged installs can enable the user service:

```sh
systemctl --user enable --now shore-notify.service
```

The service does not start or require a local daemon. For a remote daemon, put
`SHORE_ADDR=host:7320` in `~/.config/shore/notify.env` or set
`default_address = "host:7320"` in `~/.config/shore/client.toml`.
The daemon includes avatar image data in character metadata, so notification
icons still work when the daemon's config directory is on another machine.

Use `shore notify --all-messages` to notify for normal assistant replies too.

Legacy `character.md`, `user.md`, and `prompts/system.md` character layouts are
migrated into the workspace on first load.

## Repo Layout

| Path | Contents |
| --- | --- |
| `core/` | shared protocol, config, and SWP client crates |
| `backend/` | daemon runtime plus backend support crates |
| `clients/` | CLI client (other clients live in their own repos) |
| `dev/` | deterministic test harness |

Main binaries built here:

| Binary | Purpose |
| --- | --- |
| `shore-daemon` | persistent daemon |
| `shore` | CLI client |

Out-of-tree clients and bridges (separate repos, consuming the core libraries
from crates.io):

- `shore-tui` — [mythofmeat/shore-tui](https://github.com/mythofmeat/shore-tui)
- `shore-gui` (Tauri desktop) — [mythofmeat/shore-gui](https://github.com/mythofmeat/shore-gui)
- `shore-matrix` (Matrix bridge) — [mythofmeat/shore-matrix](https://github.com/mythofmeat/shore-matrix)
- `shore-mcp` (debug/development MCP) — [mythofmeat/shore-mcp](https://github.com/mythofmeat/shore-mcp)

## Docs

- [ARCHITECTURE.md](ARCHITECTURE.md) — runtime model, invariants, security,
  observability, and validation guidance.
- [CONFIGURATION.md](CONFIGURATION.md) — config reference and examples.
- [docs/DAEMON_TS_CUTOVER.md](docs/DAEMON_TS_CUTOVER.md) — TypeScript daemon
  preview, soak, and default-switch runbook.
- [CLAUDE.md](CLAUDE.md) — short entry map for coding agents.
- [CHANGELOG.md](CHANGELOG.md) — release history.
- [OpenRouter Anthropic cache incident](docs/incidents/2026-05-22-openrouter-anthropic-tool-loop-cache.md)
  — post-mortem and user settings guide for the tool-loop cache fix.

Markdown under `backend/daemon/prompts/**` is runtime prompt text, not ordinary
documentation. Treat prompt changes like code changes.

## Build And Test

```sh
python3 scripts/harness-check.py
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p shore-daemon -p shore-cli
```

TypeScript daemon preview checks:

```sh
cd backend/daemon-ts
bun install --frozen-lockfile
bun run typecheck
bun test
bun run build
bun run smoketest:compiled
```

Focused checks:

```sh
cargo test -p shore-daemon memory::deferred_edits
cargo test -p shore-daemon tools::workspace
cargo test -p shore-daemon engine::prompt
cargo test -p shore-daemon --test suite
```

Live provider checks use real credentials and may cost money. Use them only when
provider behavior, streaming, tool use, or cache economics are in scope.

## License

Private — all rights reserved.
