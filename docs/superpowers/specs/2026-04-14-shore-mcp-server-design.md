# Shore MCP Server — Design

**Status:** Draft
**Date:** 2026-04-14
**Author:** eshen (with Claude)

## Purpose

Expose Shore's functionality to MCP-speaking AI clients (Claude Code, and any other MCP host) for two use cases:

1. **Debugging.** Let an AI assistant drive Shore to reproduce bugs, inspect daemon state, tail logs, check ledger entries, and otherwise poke at a running Shore install without the human having to paste CLI output back and forth.
2. **Programmatic use.** Let an AI assistant hold actual conversations with a Shore character — `send`, `regen`, `log` — as part of a larger automated workflow.

By default, MCP operations target an **isolated test profile**, keeping the user's personal Shore state untouched. An explicit flag opts into attaching to the main profile for cases where real history is needed.

## Non-goals

- Replacing `shore-cli` as a human interface.
- Running in release builds or shipping in the default workspace binary set.
- Adding any transport other than stdio in v1.
- Exposing TUI, completions helpers, or other CLI commands that only make sense for humans.
- Supporting streaming tool output (MCP tools are request/response; `log --follow` becomes a bounded read).

## Architecture

```
┌─────────────────┐   stdio JSON-RPC   ┌────────────┐   shore-client TCP   ┌──────────────┐
│  MCP Client     │ ─────────────────> │ shore-mcp  │ ───────────────────> │ shore-daemon │
│ (Claude Code)   │ <───────────────── │  (stdio)   │ <─────────────────── │ (test OR main)│
└─────────────────┘                    └────────────┘                      └──────────────┘
```

`shore-mcp` is a new workspace crate that acts as a **thin client** of `shore-daemon`, structurally parallel to `shore-cli`. It depends on:

- `shore-client` — daemon RPC, instance discovery
- `shore-config` — profile path resolution (`SHORE_CONFIG_DIR`, `SHORE_DATA_DIR`, `SHORE_RUNTIME_DIR`)
- `shore-protocol` — wire message types
- `rmcp` — official Rust MCP SDK, used for the stdio server, tool routing, and JSON schema generation via `schemars`

No LLM logic lives in `shore-mcp`. No conversation state lives in `shore-mcp`. Every MCP tool call becomes one or more `shore-client` RPCs to a `shore-daemon` — either a dedicated test-profile daemon that `shore-mcp` spawns, or (with `--attach-main`) the user's normal daemon.

## Crate Layout

```
shore-mcp/
├── Cargo.toml
├── src/
│   ├── main.rs           # binary entry (debug-only, see below)
│   ├── server.rs         # rmcp server wiring, stdio transport
│   ├── profile.rs        # test profile discovery, daemon spawning, --attach-main
│   ├── gating.rs         # write-op gating based on profile_is_test
│   ├── handler.rs        # the #[tool_router] impl that holds shore-client handles
│   └── tools/
│       ├── mod.rs
│       ├── send.rs
│       ├── regen.rs
│       ├── log.rs
│       ├── status.rs
│       ├── character.rs
│       ├── model.rs
│       ├── memory.rs
│       ├── config.rs
│       ├── usage.rs
│       ├── debug.rs
│       └── matrix.rs
└── tests/
    ├── daemon_resolution.rs  # unit tests for profile.rs
    └── mcp_integration.rs    # spawn real daemon via shore-test-harness, drive via stdio
```

Each file in `tools/` defines one or more `#[tool]`-decorated methods on the handler struct, mirroring the corresponding `shore-cli` subcommand.

## Daemon Resolution (hybrid model)

On startup, `shore-mcp` runs this decision tree:

```
START
  │
  ├── --attach-main passed?
  │     YES → use shore-client discovery as-is (instances.json, client.toml default).
  │           Set profile_is_test = false.
  │           Do NOT spawn anything. Do NOT touch env vars.
  │
  └── NO (default path)
        │
        ├── --ephemeral passed?
        │     YES → create a fresh tempdir for SHORE_CONFIG_DIR/DATA_DIR/RUNTIME_DIR.
        │           Register a tempdir cleanup at shutdown.
        │     NO  → resolve persistent test profile paths:
        │             $XDG_DATA_HOME/shore-mcp-test/{config,data,runtime}
        │             (or $HOME/.local/share/shore-mcp-test/... on Linux default)
        │
        ├── Set SHORE_CONFIG_DIR/SHORE_DATA_DIR/SHORE_RUNTIME_DIR to those paths
        │   for the current process (and inherited by any spawned daemon).
        │
        ├── Look up instance id "shore-mcp-test" in that profile's instances.json.
        │
        ├── Alive daemon found? YES → attach.
        │
        └── NO → spawn shore-daemon as a child process with the test profile env vars
                 set and --instance-id=shore-mcp-test (or whatever flag the daemon uses
                 to pin its registered id). Wait for registration, then attach.
                 Set profile_is_test = true.
```

Notes:

- In persistent mode, the spawned daemon **outlives** the MCP server by default so state persists across MCP sessions. A `shore-mcp reset` subcommand stops the test daemon (via daemon RPC or PID file) and deletes the test profile directory.
- In ephemeral mode, the spawned daemon is killed on `shore-mcp` exit and the tempdir is removed.
- `--attach-main` is the **only** way to touch the real profile. There is no env var override and no config-file toggle for this — it must appear on the command line on every invocation.

## Write-Op Gating

MCP exposes **full CLI parity** as tools. Each tool handler checks `self.profile_is_test` before executing a mutating operation. When `profile_is_test == false` (i.e., `--attach-main` is set) and `--allow-main-writes` has **not** also been set, mutating tools return an MCP error:

```
refused: mutation tools are disabled when attached to the main profile.
re-launch without --attach-main, or pass --allow-main-writes.
```

### Gated tools (refused on main without `--allow-main-writes`)

- `send`, `regen` — mutate conversation history
- `config_set`, `config_reset`
- `character_switch`, `character_new`
- `model_switch`, `model_reset`
- `log_delete`, `log_edit`
- `memory_*` mutation variants
- `matrix_*` mutation variants
- `usage` with `--refresh-pricing` or `--recalculate --force`
- Any `debug_*` tool that mutates daemon state (TBD during plan phase — each DebugCommand variant must be audited).

### Always-allowed tools (read-only)

- `status`, `status_section`, `status_diagnostics`
- `log_tail`, `log_show`, `log_follow` (bounded read)
- `usage` (filter-only, no mutation flags)
- `config_get`, `config_check`, `config_path`
- `character_list`, `character_info`
- `model_list`, `model_info`
- `memory_query`
- Read-only `debug_*` variants (inspection, state dumps, etc.)

### Escape hatch

`--allow-main-writes` is a secondary flag that is a no-op unless `--attach-main` is also set. When both are present, all gates are removed and `shore-mcp` behaves exactly like a normal CLI client with full privileges against the main profile. This exists for the rare case where the user explicitly wants Claude Code to mutate their real Shore install — it must be a deliberate two-flag opt-in, not a single keystroke.

## Tool Surface (full CLI parity)

Each `CliCommand` variant in `shore-cli/src/cli.rs` becomes one or more MCP tools, with nested `Subcommand` enums flattened using underscore-separated names (`log_tail`, `character_switch`, etc.).

Tool parameter structs are defined in each `tools/*.rs` file using `#[derive(Deserialize, schemars::JsonSchema)]`. Parameter schemas are generated automatically by `rmcp`'s `#[tool]` macro.

### Explicit exclusions

The following CLI commands are **not** exposed as MCP tools:

- `completions` — shell-integration helper, meaningless over MCP.
- `complete` (hidden `__complete`) — internal completion helper.
- `log_follow` is exposed but implemented as a bounded read (reads for N seconds or until M messages arrive, whichever comes first, then returns). This is a deliberate reinterpretation of the CLI semantics for request/response transport.

### The `shore-cli` refactor implication

Today, `shore-cli/src/run.rs` contains the per-subcommand handler logic: taking parsed CLI args, calling `shore-client`, and formatting output. MCP tool handlers need to call the same underlying logic but return structured data rather than human-formatted output.

**Anticipated preparatory refactor:** lift the per-command "core logic" (the part between arg parsing and output formatting) out of `shore-cli/src/run.rs` and into either:

1. A new module in `shore-client` (if the logic is generic enough to belong there), or
2. A new crate `shore-commands` that both `shore-cli` and `shore-mcp` depend on.

The exact form depends on what `run.rs` actually looks like. This refactor is a **prerequisite** of the MCP server and must be completed (and verified with `cargo test --workspace`) before any MCP tool handlers are written. If the refactor turns out to be larger than roughly one day of work, it will be split into its own spec and the MCP spec will depend on it.

`shore-cli` retains its formatting/presentation layer unchanged; only the core "what does this command actually do" logic moves.

## Debug-Only Build Enforcement

`shore-mcp` is **not** part of the default release binary set. The enforcement is twofold.

### `Cargo.toml`

```toml
[package]
name = "shore-mcp"
version = "0.1.0"
edition = "2021"
publish = false

[features]
default = []
# Turning on `enabled` pulls in the MCP deps and makes the bin buildable.
enabled = ["dep:rmcp", "dep:schemars"]

[[bin]]
name = "shore-mcp"
required-features = ["enabled"]

[dependencies]
# shore deps (always required — crate itself compiles even without the feature)
shore-client = { path = "../shore-client" }
shore-config = { path = "../shore-config" }
shore-protocol = { path = "../shore-protocol" }
# MCP deps — optional, gated behind the `enabled` feature.
# Exact versions resolved at implementation time via `cargo add`; the
# plan will pin whatever rmcp version is current on crates.io at that
# point and record it in Cargo.lock.
rmcp = { version = "*", optional = true }
schemars = { version = "*", optional = true }
# runtime
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "process"] }
anyhow = { workspace = true }
tracing = { workspace = true }
```

Making `rmcp` and `schemars` optional + feature-gated means `cargo build --workspace --release` doesn't pull them in at all unless someone explicitly passes `--features shore-mcp/enabled`, keeping release builds lean.

### `src/main.rs`

```rust
#[cfg(not(debug_assertions))]
fn main() {
    eprintln!("shore-mcp is only available in debug builds");
    std::process::exit(1);
}

#[cfg(debug_assertions)]
mod server;
#[cfg(debug_assertions)]
mod profile;
#[cfg(debug_assertions)]
mod gating;
#[cfg(debug_assertions)]
mod handler;
#[cfg(debug_assertions)]
mod tools;

#[cfg(debug_assertions)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    server::run().await
}
```

### Resulting behavior

| Command | Produces `shore-mcp` binary? |
|---|---|
| `cargo build --workspace` | No (feature not enabled) |
| `cargo build --workspace --release` | No (feature not enabled) |
| `cargo build -p shore-mcp --features enabled` | Yes, with real MCP code |
| `cargo build -p shore-mcp --features enabled --release` | Yes, but as a stub (prints the "debug-only" message and exits 1) |
| `cargo test -p shore-mcp --features enabled` | Yes, tests run |

The release-mode stub is intentional belt-and-suspenders: if someone explicitly enables the feature in release (e.g., a custom profile), the binary still refuses to do anything. This matches the user's preference: the feature is available for custom workflows but is not "included in the default suite of binaries."

### Cargo alias

A `.cargo/config.toml` alias will be added:

```toml
[alias]
mcp = "build -p shore-mcp --features enabled"
mcp-test = "test -p shore-mcp --features enabled"
mcp-run = "run -p shore-mcp --features enabled --"
```

So the dev workflow is `cargo mcp` to build, `cargo mcp-run -- --attach-main` to run, etc.

## Testing Strategy

### Testing policy revision (scope: this spec, but generalizable)

The current project policy ("never mock `shore-llm`") was written to stop a specific failure mode: tests that used hand-written mock LLM responses passed with flying colors while the real integration was broken. The policy is load-bearing for one narrow concern and actively harmful for everything else.

**This spec adopts — and recommends that `CLAUDE.md` and `docs/DECISIONS.md` be updated to reflect — a refined policy:**

1. **`shore-llm-client` internals must never use hand-written mocks.** Response parsing, streaming, cache headers, error mapping, prompt cache behavior — these must be tested against real API responses, either live (`--ignored` gated tests against `OPENROUTER_API_KEY`) or via **recorded fixtures** captured from real API responses. Hand-writing a fake HTTP response body for a unit test is forbidden in this crate because that's exactly the failure mode the policy exists to prevent.

2. **Code upstream of `shore-llm-client` may use trait-level test doubles.** Anything that consumes an LLM response (`shore-daemon` command routing, `shore-ledger` accounting, `shore-mcp` tool output formatting, `shore-cli` output rendering, conversation state management) is allowed to stand in a deterministic `LlmClient` implementation that returns pre-made `Message` values. These doubles are not "mocking the LLM" in the sense the policy prohibits — they are not claiming to replicate API wire behavior. They are skipping past it to test the caller's own logic.

3. **Where determinism is needed for a full end-to-end test, use recorded fixtures.** Run a one-off capture script against a real cheap model, save the request/response pair as a test fixture, replay it via a fixture-backed `LlmClient` implementation. Re-record periodically (quarterly or whenever a provider behavior change is suspected). This gives deterministic tests that are still grounded in real API output, which is the actual spirit of the original policy.

4. **Live tests remain mandatory for release verification.** `cargo test --test e2e -- --ignored` and `./scripts/live-tests/live-test.sh` still exist and still hit real APIs. Nothing in this revision weakens that gate — the recorded-fixtures path is for fast, deterministic CI-friendly tests, not as a substitute for live verification before shipping.

**This revision should be reflected in `CLAUDE.md` and `docs/DECISIONS.md` as part of the shore-mcp implementation plan.** If a future reviewer disagrees with this revision, the MCP spec's testing strategy should fall back to "real daemon + real LLM on every test run" — which is workable but slow.

### `shore-mcp` specific tests

Under the revised policy:

1. **Unit tests — `profile.rs` / `gating.rs`.** Pure-logic tests: does the daemon-resolution decision tree pick the right paths for each flag combination? Does `gating::check()` refuse the right tools in the right profiles? No daemon, no LLM, no network.

2. **Integration test — `tests/mcp_integration.rs`.** Uses `shore-test-harness` to spin up a real `shore-daemon` in a temp profile. Launches `shore-mcp` as a subprocess, speaks MCP JSON-RPC on its stdin/stdout, calls each tool, asserts the response shape. The daemon's underlying `shore-llm-client` is configured (via `shore-test-harness`) to use a fixture-replaying test double, so `send`/`regen` return deterministic responses without hitting real APIs. Every tool in the surface gets at least one round-trip test.

3. **Manual live-verification test — documented in spec, run by hand.** Launch `shore-mcp` in default mode from Claude Code's MCP config, run `send` / `log_tail` / `status` against the real (test-profile) daemon with real LLM calls, verify behavior end-to-end. This is the mandatory pre-merge verification and replaces "just run `cargo test`."

4. **Optional `--ignored` live integration test.** Same shape as (2) but with the real `shore-llm-client` talking to OpenRouter. Exists for the user to run manually when suspicious something is wrong; not run in normal development.

## Open Questions (to be resolved in implementation plan)

1. **Exact form of the `shore-cli` core-logic refactor.** New module in `shore-client`, new shared crate, or in-place restructure? Depends on reading `shore-cli/src/run.rs` carefully. The implementation plan must resolve this before writing any MCP tool handlers.

2. **Per-variant audit of `DebugCommand`.** Each variant needs to be classified as read-only or mutating so `gating.rs` knows which to refuse on main.

3. **`log_follow` bounded-read parameters.** Default timeout and message cap? Suggested: 5 seconds, 50 messages, both overridable via tool parameters.

4. **Test daemon spawn mechanism.** Does `shore-daemon` already accept a command-line flag to pin its registered instance ID? If not, a small daemon change is needed to support the hybrid model.

5. **Cleanup of orphaned test daemons.** If `shore-mcp` is killed uncleanly in persistent mode, the spawned daemon stays alive. Is that fine (next invocation re-attaches) or should there be a heartbeat/auto-shutdown mechanism? Leaning "fine" — `shore-mcp reset` handles intentional cleanup.

## Deliverables

1. `shore-cli` core-logic refactor (prerequisite, possibly its own spec).
2. `CLAUDE.md` + `docs/DECISIONS.md` update reflecting the testing policy revision.
3. New `shore-mcp` crate with the file layout above.
4. `.cargo/config.toml` alias entries.
5. Integration test using `shore-test-harness` + fixture-replaying LLM.
6. `docs/ARCHITECTURE.md` update describing `shore-mcp` and its place in the crate graph.
7. `docs/QUIRKS.md` entry for any rmcp-specific surprises encountered during implementation.
8. Documented example `.mcp.json` fragment showing how to add `shore-mcp` to a Claude Code or other MCP host config, with both default and `--attach-main` variants.
