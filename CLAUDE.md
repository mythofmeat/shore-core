# Shore V2 — Claude Code Guidelines

## Project Overview

Shore is a modular AI character engine in Rust. Workspace crates: `shore-protocol`, `shore-config`, `shore-diagnostics`, `shore-client`, `shore-llm-client`, `shore-daemon`, `shore-cli`, `shore-tui`, `shore-matrix`. Binaries: `shore-daemon`, `shore` (CLI), `shore-tui`, `shore-matrix`.

## Build & Test

```sh
cargo build --workspace --release    # full build
cargo test --workspace               # unit tests
cargo test --test e2e -- --ignored   # e2e (requires OPENROUTER_API_KEY)
./scripts/live-test.sh               # live integration tests
```

## Priority (highest first)

1. **Verify with real binaries.** The highest priority is confirming that something works by compiling and running the actual binary. Unit tests are not sufficient — live tests with real API calls are mandatory for ensuring functionality.
2. **Ease of debugging and testing.** Code must be straightforward to debug and test in isolation.
3. **Small, discrete modules.** Keep each crate and module small with hard boundaries. ~2-5K LOC per crate, ~500 LOC per module.

## Live Testing Policy

**Live tests with real API calls are MANDATORY.** Never mock `shore-llm` or provider integrations. Use the test character in the config (`test_char` binary at project root) to perform real end-to-end tests. Claude is permitted and expected to use this test character to verify changes.

When verifying a change:
1. Build the affected crate(s)
2. Run the compiled binary against real APIs
3. Confirm the expected behavior in the actual output

## Mandatory Documentation

### decisions.md
All decisions, additions, and compromises must be recorded in [DECISIONS.md](DECISIONS.md). This includes:
- Features added, removed, or deferred
- Design trade-offs and why one approach was chosen over another
- Compromises made (and what was sacrificed)

### architecture.md
All architectural changes must be recorded in [ARCHITECTURE.md](ARCHITECTURE.md). This includes:
- New crates or modules
- Changes to the wire protocol (SWP)
- Changes to data flow between components
- New binary targets or services

### Quirks & Gotchas (QUIRKS.md)
Any idiosyncrasies, kludges, or unexpected behavior patterns must be recorded in [QUIRKS.md](QUIRKS.md). If you assume the program would behave a certain way and it does not, document it. Examples:
- API providers that deviate from their documented behavior
- Bun/runtime bugs that required workarounds
- Ordering or timing issues that aren't obvious from the code
- Anything where "this shouldn't be necessary but it is"

## Code Style

- Rust, stable toolchain (1.75+)
- Prefer compiler-enforced correctness over runtime checks
- No unnecessary abstractions — three similar lines beat a premature helper
- Only validate at system boundaries (user input, external APIs, wire protocol)
- Don't add comments, docstrings, or type annotations to unchanged code
