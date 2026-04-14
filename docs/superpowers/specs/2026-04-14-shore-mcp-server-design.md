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

### Relationship to `shore-cli` (minimal prep needed)

The CLI is already structured in a way that makes MCP a straightforward addition. `shore-cli/src/cli.rs:410` defines:

```rust
pub fn to_swp_command(cmd: &CliCommand) -> Option<(&'static str, serde_json::Value)>;
```

…which maps nearly every `CliCommand` variant to a `(swp_command_name, json_args)` tuple. The daemon already speaks JSON-in / JSON-out for these commands. `shore-cli/src/run.rs`'s catch-all branch is effectively `send_command(name, args)` → `recv_command_data` → format, meaning the "logic" of most CLI subcommands is just the command name plus its arguments. No extraction is required.

**`shore-mcp` does not depend on `shore-cli`.** Each MCP tool owns its own parameter struct (with `schemars::JsonSchema` so `rmcp` can generate the schema) and constructs its own SWP command inline. This keeps the two clients independent: both are peers of `shore-daemon`, and the set of SWP command names is the shared contract. If a new daemon command is added in the future, both clients update independently.

The only shore-cli-adjacent logic that needs to be shared lives in `shore-client`, not `shore-cli`:

**New helper: `shore-client::collect_stream()`**. Currently `shore-cli/src/run.rs` has a `recv_streaming_response` function that consumes a `send`/`regen` stream and renders chunks to stdout as they arrive. MCP can't render incrementally — tool responses are request/response. `shore-client` will get a new async helper that consumes the same stream and returns a structured aggregate:

```rust
pub struct StreamedResponse {
    pub text: String,              // accumulated StreamChunk text
    pub tool_calls: Vec<ToolCall>, // collected ToolCall frames
    pub tool_results: Vec<ToolResult>,
    pub end: StreamEndInfo,        // from StreamEnd (usage, finish_reason, etc.)
}

pub async fn collect_stream(conn: &mut SWPConnection) -> Result<StreamedResponse, ClientError>;
```

Both `shore-mcp` (for `send`/`regen` tool handlers) and potentially `shore-cli` (if refactored later to share the stream-handling logic, though this is explicitly out of scope for this spec) can use it. This is a ~50-line addition to `shore-client`, with unit tests using a mock SWP stream.

**Explicitly out of scope:** restructuring `shore-cli/src/run.rs`, `shore-cli/src/output/`, or `shore-cli/src/state.rs`. The CLI keeps its current shape unchanged. The only file `shore-mcp` work touches in `shore-cli` is the existing `to_swp_command` in `cli.rs` — and even that only if we decide to make it `pub` beyond its current crate. Current plan: we don't; `shore-mcp` duplicates the command-name strings deliberately so the two clients can evolve independently.

### Prerequisite: `shore-daemon --instance-id` flag

Currently `shore-daemon/src/main.rs:141` generates `instance_id = uuid::Uuid::new_v4().to_string()` unconditionally. The `shore-daemon` binary has no flag to override this, so there is no way for `shore-mcp` to spawn a daemon with a predictable, stable ID that it can rediscover after an MCP server restart. The registry (`shore-daemon-server/src/registry.rs`) already accepts arbitrary string IDs; only the daemon's entry point needs updating.

**Required daemon change (small):**

1. Add `--instance-id <ID>` to `shore-daemon`'s `Cli` struct (`main.rs:22-30`).
2. In `main()`, use the CLI value if set, otherwise fall back to `Uuid::new_v4().to_string()`. Default behavior is unchanged.
3. Add a test alongside `cli_parses_startup_flags` verifying the flag parses.

Estimated size: ~5 lines of code + 1 test. This is a **prerequisite** of the MCP implementation plan and should be the first commit in that plan, landing on its own before any `shore-mcp` crate work so it can be verified independently via `cargo test --workspace`.

No changes are needed to `shore-daemon-server/src/registry.rs` — it already supports arbitrary string IDs.

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

1. **Per-variant audit of `DebugCommand`.** Each variant needs to be classified as read-only or mutating so `gating.rs` knows which to refuse on main. A quick read of `shore-cli/src/cli.rs` DebugCommand and the corresponding daemon handlers during plan writing will resolve this.

2. **`log_follow` bounded-read parameters.** Default timeout and message cap? Suggested: 5 seconds, 50 messages, both overridable via tool parameters.

3. **Cleanup of orphaned test daemons.** If `shore-mcp` is killed uncleanly in persistent mode, the spawned daemon stays alive. Is that fine (next invocation re-attaches) or should there be a heartbeat/auto-shutdown mechanism? Leaning "fine" — `shore-mcp reset` handles intentional cleanup, and an orphaned test daemon is harmless because it's bound to loopback in a separate profile.

4. **`shore-client::collect_stream()` edge cases.** What happens if the stream ends with no `StreamEnd` frame (protocol bug, connection drop)? The helper should return a partial `StreamedResponse` with an explicit `end: None` or return a `ClientError::StreamTruncated`. Plan will decide.

5. **rmcp version pinning.** The spec uses `version = "*"` as a placeholder in the Cargo.toml snippet. The plan's first shore-mcp commit will pin whatever rmcp version is current on crates.io at that point and record it in `Cargo.lock`.

## Deliverables

The implementation plan will sequence these into ordered phases.

### Phase 1 — daemon prerequisite (lands independently, verifiable on its own)

1. `shore-daemon --instance-id <ID>` CLI flag with default-preserving fallback to UUID. Includes tests. Can merge to `main` as a self-contained change; `cargo test --workspace` verifies.

### Phase 2 — shared helper

2. `shore-client::collect_stream()` + supporting types (`StreamedResponse`, `StreamEndInfo`, etc.). Unit tests using a mock `SWPConnection`-like stream. Self-contained, merges independently.

### Phase 3 — policy + docs

3. `CLAUDE.md` update reflecting the testing-policy revision from the "Testing Strategy" section above.
4. `docs/DECISIONS.md` entry recording the testing policy revision and the rationale for the hybrid daemon model.

### Phase 4 — shore-mcp crate (the main work)

5. New `shore-mcp` crate with the file layout from the "Crate Layout" section.
6. `.cargo/config.toml` alias entries (`cargo mcp`, `cargo mcp-test`, `cargo mcp-run`).
7. Unit tests for `profile.rs` and `gating.rs` (no daemon, no LLM).
8. Integration test `tests/mcp_integration.rs` using `shore-test-harness` + a fixture-replaying LLM double to exercise every tool in the surface over real MCP JSON-RPC on stdio.
9. `docs/ARCHITECTURE.md` update describing `shore-mcp` and its place in the crate graph.
10. Documented example `.mcp.json` fragment (in `shore-mcp/README.md` or in-tree docs) showing how to add `shore-mcp` to a Claude Code or other MCP host config, with both default and `--attach-main` variants.

### Phase 5 — manual live verification (gating merge)

11. Manual live-verification pass, documented in the PR: launch `shore-mcp` from Claude Code against the default test profile, run `send` / `log_tail` / `status`, confirm end-to-end behavior with real LLM calls. Per `CLAUDE.md`'s "verify with real binaries" priority, this is the load-bearing gate, not `cargo test`.
12. `docs/QUIRKS.md` entry for any rmcp-specific or MCP-protocol surprises encountered during implementation (to be filled in during execution, not now).
