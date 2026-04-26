# Reliability

Reliability work should give agents direct feedback loops, not just prose.

## Local Checks

Use the narrowest useful check first:

```sh
python3 scripts/harness-check.py
cargo fmt --all --check
cargo test -p shore-daemon engine::prompt
cargo test -p shore-daemon tools::workspace
cargo test -p shore-daemon memory::deferred_edits
cargo test -p shore-daemon --test suite
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The release build gate is:

```sh
cargo build --release -p shore-daemon -p shore-cli -p shore-tui -p shore-matrix
```

## Agent-Legible Surfaces

- `dev/test-harness`: boots an in-process daemon with a mock LLM backend.
- `dev/mcp`: drives Shore through the same SWP path as clients, with isolated
  default profiles.
- `scripts/cache-tests`: checks cache prefix, heartbeat, compaction, and
  concurrency behavior.
- `scripts/live-tests`: runs real provider smoke/autonomy checks when keys and
  budget are available.
- `backend/llm/src/cache_forensics.rs`: writes cache-event JSONL diagnostics.
- `shore-ledger`: records usage, cost, cache reads/writes, and anomalies.

## Worktree Isolation

Prefer isolated config/data/runtime directories for manual or scripted checks:

```sh
export SHORE_CONFIG_DIR="$(mktemp -d)"
export SHORE_DATA_DIR="$(mktemp -d)"
export SHORE_RUNTIME_DIR="$(mktemp -d)"
```

`shore-mcp` defaults to an isolated persistent test profile and only touches the
main profile with `--attach-main --allow-main-writes`.

## Provider Verification

Live provider behavior must not be inferred from hand-written fake responses
alone. Use deterministic mocks for fast regression coverage, then use recorded
or live provider responses before release when provider request formatting,
streaming, tool use, or cache economics are in scope.

Live checks may cost money. Make that explicit in handoffs.

## Release Gates

Before a release, run:

- harness checker;
- formatting and clippy;
- full workspace tests;
- release build for shipped binaries;
- relevant cache tests;
- live provider smoke tests if provider behavior changed;
- Matrix live verification if Matrix behavior changed.
