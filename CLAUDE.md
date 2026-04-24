# Agent Guidelines

`GOALS.md` is the source of truth for product intent. When docs and code disagree, inspect code for behavior and `GOALS.md` for purpose.

## Current Architecture

Shore is a daemon-centered Rust workspace. The daemon owns character state; clients are interchangeable surfaces.

Current memory model:

- markdown files under `characters/<Character>/workspace/memory/`
- optional rebuildable hybrid retrieval index
- no runtime SQLite/vector/RAG memory source of truth

Current prompt model:

- editable workspace prompt files
- active snapshots under `active_prompt/`
- protected self-edits activate at compaction/reload

## Build And Test

```sh
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace --release
```

Focused examples:

```sh
cargo test -p shore-daemon memory::deferred_edits
cargo test -p shore-daemon tools::workspace
cargo test -p shore-daemon --test suite
```

## Live Verification

Live/provider tests use real credentials and cost money. Run them before release when provider behavior is in scope.

`shore-mcp` is the preferred agent-driven end-to-end path. It defaults to an isolated test profile and only writes to the main profile with `--attach-main --allow-main-writes`.

## Testing Policy

- `shore-llm-client` provider parsing/streaming/cache behavior should use live or recorded real provider responses.
- Upstream daemon/client code may use deterministic test doubles or `shore-test-harness`.
- Do not claim provider compatibility from hand-written fake wire responses alone.

## Documentation Policy

Update docs with architectural changes:

- `docs/FEATURES.md` for user behavior
- `docs/CONFIGURATION.md` for config changes
- `docs/ARCHITECTURE.md` for structure and data flow
- `docs/INVARIANTS.md` for correctness constraints
- `docs/DECISIONS.md` for tradeoffs
- `docs/QUIRKS.md` for surprising behavior

Patch-note worthy user changes should also go in `docs/PATCH_NOTES_OPENCLAWIFY.md` until this branch lands.

## Code Style

- Rust stable
- prefer compiler-enforced correctness
- keep modules focused
- validate at external boundaries
- avoid panic in daemon runtime paths
- do not mutate prompt/cache boundaries accidentally
- keep tool and memory gates exact
