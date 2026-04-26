# Agent Entry Map

Shore is a daemon-centered AI character engine. Keep this file short: it is a
map into the repo, not the full manual.

## Start Here

- Product intent: [GOALS.md](GOALS.md)
- Knowledge base map: [docs/README.md](docs/README.md)
- Architecture map: [ARCHITECTURE.md](ARCHITECTURE.md)
- Current behavior: [FEATURES.md](FEATURES.md)
- Config surface: [CONFIGURATION.md](CONFIGURATION.md)
- Architectural decisions: [DECISIONS.md](DECISIONS.md)
- Correctness invariants: [docs/dev-info/INVARIANTS.md](docs/dev-info/INVARIANTS.md)
- Harness practices: [docs/HARNESS_ENGINEERING.md](docs/HARNESS_ENGINEERING.md)
- Observability: [docs/OBSERVABILITY.md](docs/OBSERVABILITY.md)

When docs and code disagree, inspect the code for behavior and `GOALS.md` for
purpose. Then update the docs in the same change.

## Current Shape

- `core/` holds protocol, config, and shared SWP client crates.
- `backend/` holds the daemon, SWP server, LLM, ledger, and diagnostics crates.
- `clients/` holds CLI, TUI, Tauri GUI, and experimental Godot GUI surfaces.
- `bridges/` holds external service bridges such as Matrix.
- `dev/` holds MCP tooling and deterministic test harnesses.

The daemon owns character state. Clients observe and send commands; they do not
fork authoritative state.

## Load-Bearing Rules

- Markdown memory under `characters/<Character>/workspace/memory/**/*.md` is the
  runtime long-term memory source of truth.
- Protected prompt files activate from `active_prompt/`, not directly from the
  editable workspace.
- Edits to `SOUL.md`, `USER.md`, `AGENTS.md`, `TOOLS.md`, and `HEARTBEAT.md`
  must stay staged until compaction or reload.
- Unexpected Anthropic cache invalidation is a serious regression.
- Workspace tools must prevent path traversal and symlink escape.
- `exec` must not invoke a shell and must keep path-like arguments inside the
  character workspace.
- Non-loopback daemon access must be explicit and must not be described as auth
  or TLS.

Deeper rule sources live in [docs/dev-info/INVARIANTS.md](docs/dev-info/INVARIANTS.md),
[docs/dev-info/PROMPT_CACHING.md](docs/dev-info/PROMPT_CACHING.md), and
[docs/SECURITY.md](docs/SECURITY.md).

## Build And Test

```sh
python3 scripts/harness-check.py
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p shore-daemon -p shore-cli -p shore-tui -p shore-matrix
```

Focused examples:

```sh
cargo test -p shore-daemon memory::deferred_edits
cargo test -p shore-daemon tools::workspace
cargo test -p shore-daemon engine::prompt
cargo test -p shore-daemon --test suite
```

Live/provider checks use real credentials and may cost money. Use them only
when provider behavior is in scope; see [docs/RELIABILITY.md](docs/RELIABILITY.md).

## Agent Workflow

- Start with the small map above, then open only the docs needed for the task.
- For non-trivial work, keep an execution plan in
  [docs/exec-plans/active](docs/exec-plans/active) using
  [docs/PLANS.md](docs/PLANS.md).
- Prefer deterministic harnesses in `dev/test-harness`, the MCP surface in
  `dev/mcp`, and the diagnostics in [docs/OBSERVABILITY.md](docs/OBSERVABILITY.md)
  for end-to-end checks.
- Encode repeated review feedback as docs, tests, lints, or harness checks.
- Update [docs/QUALITY_SCORE.md](docs/QUALITY_SCORE.md) when a change alters a
  quality grade, known gap, or validation expectation.

## Documentation Policy

- User-visible behavior changes: update [FEATURES.md](FEATURES.md).
- Config changes: update [CONFIGURATION.md](CONFIGURATION.md).
- Architecture/data-flow changes: update [ARCHITECTURE.md](ARCHITECTURE.md).
- Correctness constraints: update [docs/dev-info/INVARIANTS.md](docs/dev-info/INVARIANTS.md).
- Tradeoffs: update [DECISIONS.md](DECISIONS.md).
- Sharp edges: update [docs/dev-info/QUIRKS.md](docs/dev-info/QUIRKS.md).
- Patch-note worthy user changes: update [CHANGELOG.md](CHANGELOG.md).

Run `python3 scripts/harness-check.py` before handing off a change that touches
docs, architecture, tool surfaces, memory, prompt assembly, or agent guidance.
