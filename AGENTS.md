# Agent Entry Map

## Start Here

- [README.md](README.md): product intent, quick start, repo layout.
- [ARCHITECTURE.md](ARCHITECTURE.md): runtime model, invariants, security,
  observability, and validation.
- [CONFIGURATION.md](CONFIGURATION.md): config reference.
- [CHANGELOG.md](CHANGELOG.md): release history.

When docs and code disagree, inspect the code for behavior and `README.md` for
purpose. Then update the relevant kept doc in the same change.

## Repo Shape

- `core/`: protocol, config, and shared SWP client crates.
- `backend/`: daemon, SWP server, LLM, ledger, and diagnostics crates.
- `clients/`: CLI, TUI, Tauri GUI, and experimental Godot GUI surfaces.
- `bridges/`: external service bridges such as Matrix.
- `dev/`: MCP tooling and deterministic test harnesses.

The daemon owns character state. Clients observe and send commands; they do not
fork authoritative state.

## Load-Bearing Rules

- Markdown memory under `characters/<Character>/workspace/memory/**/*.md` is the
  runtime long-term memory source of truth.
- Prompt-visible workspace files activate from `active_prompt/`, not directly
  from the editable workspace.
- Edits to `SOUL.md`, `USER.md`, `AGENTS.md`, `TOOLS.md`, `HEARTBEAT.md`, and
  `MEMORY.md` stay staged until compaction/reload.
- Compaction may update workspace-root `MEMORY.md` with carry-forward
  throughlines; dreaming may reorganize it later.
- Unexpected Anthropic cache invalidation is a serious regression.
- Workspace tools must prevent path traversal and symlink escape.
- `exec` must not invoke a shell and must keep path-like arguments inside the
  character workspace.
- Non-loopback daemon access must be explicit and must not be described as auth
  or TLS.

## Build And Test

```sh
python3 scripts/harness-check.py
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p shore-daemon -p shore-cli -p shore-tui -p shore-matrix
```

Focused checks:

```sh
cargo test -p shore-daemon memory::deferred_edits
cargo test -p shore-daemon tools::workspace
cargo test -p shore-daemon engine::prompt
cargo test -p shore-daemon --test suite
```

Live/provider checks use real credentials and may cost money. Use them only when
provider behavior is in scope.

## Documentation Policy

- Current behavior and product intent: update [README.md](README.md).
- Config changes: update [CONFIGURATION.md](CONFIGURATION.md).
- Runtime, architecture, invariants, security, observability, or validation
  changes: update [ARCHITECTURE.md](ARCHITECTURE.md).
- Patch-note worthy user changes: update [CHANGELOG.md](CHANGELOG.md).
- Runtime prompt changes under `backend/daemon/prompts/**` are code changes.

Run `python3 scripts/harness-check.py` before handing off changes that touch
docs, architecture, tool surfaces, memory, prompt assembly, or agent guidance.
