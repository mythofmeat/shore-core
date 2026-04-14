# Shore MCP Server Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a debug-only MCP server (`shore-mcp`) that exposes Shore's full CLI surface as MCP tools, defaulting to an isolated test profile and opting into the main profile only with explicit flags.

**Architecture:** `shore-mcp` is a thin client of `shore-daemon` (parallel to `shore-cli`). It speaks MCP over stdio using `rmcp`, resolves its daemon via a hybrid discover-or-spawn model, and translates each tool call into an SWP command against `shore-client`. Build is gated behind the `enabled` feature + `debug_assertions`, so release builds produce no binary.

**Tech Stack:** Rust (stable 1.75+), tokio, `rmcp` (Rust MCP SDK), `schemars` (JSON schema derivation), existing `shore-client`, `shore-config`, `shore-protocol` crates, `shore-test-harness` for integration tests.

**Spec:** [`docs/superpowers/specs/2026-04-14-shore-mcp-server-design.md`](../specs/2026-04-14-shore-mcp-server-design.md)

---

## File Structure

### New files

- `shore-mcp/Cargo.toml` — crate manifest, feature-gated deps, `required-features` on bin
- `shore-mcp/src/main.rs` — binary entry, `debug_assertions`-gated
- `shore-mcp/src/lib.rs` — module re-exports (only used if feature + debug both on)
- `shore-mcp/src/server.rs` — rmcp stdio wiring, startup
- `shore-mcp/src/profile.rs` — daemon resolution (discover or spawn), env var setup
- `shore-mcp/src/gating.rs` — write-op gate logic for main profile
- `shore-mcp/src/handler.rs` — `ShoreMcpHandler` struct + `#[tool_router]` impl glue
- `shore-mcp/src/cli.rs` — clap struct for flags (`--attach-main`, `--ephemeral`, `--allow-main-writes`, `--daemon-addr`)
- `shore-mcp/src/tools/mod.rs` — tool module index
- `shore-mcp/src/tools/send.rs` — `send`, `regen` (streaming → collected response)
- `shore-mcp/src/tools/log.rs` — `log_tail`, `log_show`, `log_delete`, `log_edit`, `log_follow`, `log_heartbeat`
- `shore-mcp/src/tools/status.rs` — `status`, `status_diagnostics`
- `shore-mcp/src/tools/character.rs` — `character_list`, `character_switch`, `character_info`
- `shore-mcp/src/tools/model.rs` — `model_list`, `model_switch`, `model_info`, `model_reset`
- `shore-mcp/src/tools/memory.rs` — `memory_query`, `memory_compact`, `memory_collate`, `memory_purge`, `memory_changelog`, `memory_reindex`
- `shore-mcp/src/tools/config.rs` — `config_get`, `config_set`, `config_check`, `config_reset`
- `shore-mcp/src/tools/usage.rs` — `usage`
- `shore-mcp/src/tools/debug.rs` — one tool per `DebugCommand` variant
- `shore-mcp/tests/profile_resolution.rs` — unit tests for `profile.rs`
- `shore-mcp/tests/gating_rules.rs` — unit tests for `gating.rs`
- `shore-mcp/tests/mcp_integration.rs` — end-to-end test via `shore-test-harness` + stdio client
- `shore-mcp/README.md` — example `.mcp.json` fragment, usage notes
- `.cargo/config.toml` — aliases (`cargo mcp`, `cargo mcp-test`, `cargo mcp-run`)

### Modified files

- `Cargo.toml` (workspace root) — add `shore-mcp` to members
- `shore-daemon/src/main.rs` — add `--instance-id` flag
- `shore-client/src/stream.rs` — add `collect_stream()` helper + `StreamedResponse` type
- `shore-client/src/lib.rs` — re-export `collect_stream`, `StreamedResponse`
- `/home/eshen/.claude/CLAUDE.md` — testing policy revision (global)
- `CLAUDE.md` (project) — testing policy revision
- `docs/DECISIONS.md` — record MCP decisions + testing policy revision
- `docs/ARCHITECTURE.md` — add shore-mcp to crate graph
- `docs/QUIRKS.md` — populated during execution as needed

---

## Phase 1 — Daemon prerequisite

### Task 1: Add `--instance-id` flag to `shore-daemon`

**Files:**
- Modify: `shore-daemon/src/main.rs:22-30` (Cli struct)
- Modify: `shore-daemon/src/main.rs:141` (instance_id generation)
- Modify: `shore-daemon/src/main.rs:558-570` (existing `cli_parses_startup_flags` test)

- [ ] **Step 1: Write the failing test**

Add this test right after the existing `cli_parses_startup_flags` test in `shore-daemon/src/main.rs` (around line 571):

```rust
#[test]
fn cli_parses_instance_id_flag() {
    let cli = Cli::try_parse_from([
        "shore-daemon",
        "--instance-id",
        "shore-mcp-test",
    ])
    .unwrap();
    assert_eq!(cli.instance_id.as_deref(), Some("shore-mcp-test"));
}

#[test]
fn cli_instance_id_defaults_to_none() {
    let cli = Cli::try_parse_from(["shore-daemon"]).unwrap();
    assert!(cli.instance_id.is_none());
}
```

- [ ] **Step 2: Run the test to verify it fails**

```sh
cargo test -p shore-daemon --bin shore-daemon cli_parses_instance_id_flag
```

Expected: compile error — `Cli` has no field `instance_id`.

- [ ] **Step 3: Add the flag to `Cli`**

In `shore-daemon/src/main.rs`, extend the `Cli` struct (currently lines 22-30) to:

```rust
#[derive(Debug, Parser)]
#[command(name = "shore-daemon", about = "Shore daemon")]
struct Cli {
    /// Config file to load instead of $XDG_CONFIG_HOME/shore/config.toml.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// TCP listen address for this process (overrides SHORE_ADDR and config).
    #[arg(long, value_name = "ADDR")]
    addr: Option<String>,

    /// Pin the registered instance ID in `instances.json`.
    ///
    /// When unset, a fresh UUID is generated on every startup. Set this
    /// to give the daemon a stable, discoverable ID — used by `shore-mcp`
    /// to rediscover a previously-spawned test daemon.
    #[arg(long, value_name = "ID")]
    instance_id: Option<String>,
}
```

- [ ] **Step 4: Wire the flag into `main()`**

In `shore-daemon/src/main.rs`, locate line 141:

```rust
let instance_id = uuid::Uuid::new_v4().to_string();
```

Parse and use the CLI value. You need the parsed `Cli` here — currently `resolve_startup` consumes it. Simplest: parse once at the top of `main()`, store the `instance_id` option, then pass the rest into `resolve_startup`.

Replace the `main()` body around line 112-118 (current `resolve_startup(Cli::parse(), ...)`) with:

```rust
let cli_parsed = Cli::parse();
let instance_id_override = cli_parsed.instance_id.clone();
let StartupConfig {
    loaded,
    config_path,
    bind_addr: addr,
    bind_addr_source,
    remote_access_warnings,
} = resolve_startup(cli_parsed, startup_env_addr())?;
```

Then replace line 141:

```rust
let instance_id = uuid::Uuid::new_v4().to_string();
```

with:

```rust
let instance_id = instance_id_override
    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
```

- [ ] **Step 5: Run the tests**

```sh
cargo test -p shore-daemon --bin shore-daemon cli_parses_instance_id_flag cli_instance_id_defaults_to_none
```

Expected: PASS.

- [ ] **Step 6: Run the full daemon test suite**

```sh
cargo test -p shore-daemon
```

Expected: all pass (you haven't changed behavior when the flag is absent).

- [ ] **Step 7: Run workspace verification**

```sh
cargo check --workspace
```

Expected: clean compile.

- [ ] **Step 8: Commit**

```sh
git add shore-daemon/src/main.rs
git commit -m "feat(daemon): add --instance-id flag for stable registry IDs

Default behavior (fresh UUID per launch) is preserved when the flag
is absent. Setting --instance-id pins the value written to
instances.json so callers like shore-mcp can rediscover a previously
spawned daemon across restarts."
```

---

## Phase 2 — `shore-client::collect_stream` helper

### Task 2: Define `StreamedResponse` type and write failing test

**Files:**
- Modify: `shore-client/src/stream.rs` (add new type + helper)
- Modify: `shore-client/src/lib.rs` (re-export)
- Create: inline tests inside `shore-client/src/stream.rs`

- [ ] **Step 1: Add the `StreamedResponse` type**

At the top of `shore-client/src/stream.rs` (after the `use` statements, before `pub trait StreamCallbacks`), add:

```rust
use shore_protocol::server_msg::{ToolCall, ToolResult};
use shore_protocol::types::StreamMetadata;

/// Aggregate result of consuming a full stream from `send`/`regen`.
///
/// Unlike `StreamHandler`, which is a stateful frame-by-frame accumulator,
/// this is the flattened end-state: everything a caller would want after
/// the stream has ended, in one struct.
#[derive(Debug, Clone)]
pub struct StreamedResponse {
    /// Final text content (from `StreamEnd.content`, which is the canonical
    /// full text — not just concatenated chunks).
    pub text: String,
    /// Tool calls collected during the stream, in order of arrival.
    pub tool_calls: Vec<ToolCall>,
    /// Tool results collected during the stream, in order of arrival.
    pub tool_results: Vec<ToolResult>,
    /// Metadata from `StreamEnd` — tokens, timing, model.
    pub metadata: StreamMetadata,
    /// Finish reason from `StreamEnd`.
    pub finish_reason: String,
}
```

Verify `ToolCall` and `ToolResult` exist in `shore-protocol::server_msg`:

```sh
rg -n 'pub struct ToolCall|pub struct ToolResult' shore-protocol/src/server_msg.rs
```

Expected: both found. If either has a different name, adjust the import accordingly before proceeding.

- [ ] **Step 2: Write the failing test for `collect_stream`**

Append this test to the existing `#[cfg(test)] mod tests` block inside `shore-client/src/lib.rs` (right before the closing `}` of the module):

```rust
#[tokio::test]
async fn collect_stream_aggregates_full_response() {
    use crate::stream::{collect_stream, StreamedResponse};
    use shore_protocol::server_msg::*;
    use shore_protocol::types::*;

    let (client_stream, server_stream) = duplex(8192);

    let server_handle = tokio::spawn(async move {
        let (_r, mut w) = tokio::io::split(server_stream);
        // Send a complete stream sequence.
        write_json_line(
            &mut w,
            &ServerMessage::StreamStart(StreamStart {
                rid: None,
                regen: false,
            }),
        )
        .await;
        write_json_line(
            &mut w,
            &ServerMessage::StreamChunk(StreamChunk {
                rid: None,
                text: "partial ".into(),
                content_type: "text".into(),
            }),
        )
        .await;
        write_json_line(
            &mut w,
            &ServerMessage::StreamChunk(StreamChunk {
                rid: None,
                text: "text".into(),
                content_type: "text".into(),
            }),
        )
        .await;
        write_json_line(
            &mut w,
            &ServerMessage::StreamEnd(StreamEnd {
                rid: None,
                content: "partial text".into(),
                metadata: StreamMetadata {
                    tokens: TokenCounts {
                        input: 10,
                        output: 5,
                        cache_read: 0,
                        cache_write: 0,
                    },
                    timing: TimingInfo {
                        total_ms: 100,
                        ttft_ms: 20,
                    },
                    model: "test-model".into(),
                },
                finish_reason: "end_turn".into(),
            }),
        )
        .await;
    });

    let mut conn = SWPConnection::from_raw_stream(client_stream);
    let response: StreamedResponse = collect_stream(&mut conn).await.unwrap();

    assert_eq!(response.text, "partial text");
    assert!(response.tool_calls.is_empty());
    assert!(response.tool_results.is_empty());
    assert_eq!(response.metadata.model, "test-model");
    assert_eq!(response.finish_reason, "end_turn");

    drop(conn);
    server_handle.await.unwrap();
}

#[tokio::test]
async fn collect_stream_propagates_server_error() {
    use crate::stream::collect_stream;
    use shore_protocol::error::ErrorCode;
    use shore_protocol::server_msg::*;

    let (client_stream, server_stream) = duplex(8192);

    let server_handle = tokio::spawn(async move {
        let (_r, mut w) = tokio::io::split(server_stream);
        write_json_line(
            &mut w,
            &ServerMessage::Error(Error {
                rid: None,
                code: ErrorCode::InternalError,
                message: "llm blew up".into(),
            }),
        )
        .await;
    });

    let mut conn = SWPConnection::from_raw_stream(client_stream);
    let result = collect_stream(&mut conn).await;
    assert!(result.is_err());
    assert!(format!("{}", result.unwrap_err()).contains("llm blew up"));

    drop(conn);
    server_handle.await.unwrap();
}

#[tokio::test]
async fn collect_stream_propagates_eof_as_disconnected() {
    use crate::stream::collect_stream;

    let (client_stream, server_stream) = duplex(8192);
    drop(server_stream);

    let mut conn = SWPConnection::from_raw_stream(client_stream);
    let result = collect_stream(&mut conn).await;
    assert!(result.is_err());
    assert!(format!("{}", result.unwrap_err()).contains("disconnected"));
}
```

- [ ] **Step 3: Run the tests to verify they fail**

```sh
cargo test -p shore-client collect_stream
```

Expected: compile error — `collect_stream` and/or `StreamedResponse` not found in `crate::stream`.

- [ ] **Step 4: Commit the failing tests** (staging them separately is fine for review purposes but combining with the implementation commit is also acceptable; prefer one commit for the full TDD cycle)

Skip this — we'll commit type + impl + tests together in Task 3's commit step.

### Task 3: Implement `collect_stream`

**Files:**
- Modify: `shore-client/src/stream.rs` (add function)
- Modify: `shore-client/src/lib.rs` (re-export)

- [ ] **Step 1: Implement `collect_stream`**

At the bottom of `shore-client/src/stream.rs` (after `impl Default for StreamHandler`), add:

```rust
/// Consume a full streaming response from a connection and return the
/// aggregated result.
///
/// This loops on `conn.recv()` until a `StreamEnd` arrives (or an error),
/// collecting tool calls / tool results along the way. It is the
/// request/response-shaped counterpart to `StreamHandler`'s frame-by-frame
/// API, intended for callers (like `shore-mcp`) that cannot render chunks
/// as they arrive and just want the final result.
///
/// Returns an error if:
/// - The server sends an `Error` frame.
/// - The connection closes before `StreamEnd` arrives.
/// - Any protocol-level stream assembly error occurs.
pub async fn collect_stream(
    conn: &mut crate::connection::SWPConnection,
) -> Result<StreamedResponse> {
    use shore_protocol::server_msg::ServerMessage;

    let mut handler = StreamHandler::new();
    let mut tool_calls = Vec::new();
    let mut tool_results = Vec::new();
    let mut final_end: Option<shore_protocol::server_msg::StreamEnd> = None;

    loop {
        let msg = conn.recv().await?;

        // Try feeding stream frames first.
        let consumed = handler.feed(&msg, None)?;

        if consumed {
            // If the stream just ended, capture the end frame and break.
            if !handler.is_active() && handler.final_content().is_some() {
                // Re-match the message to get the StreamEnd out. `feed`
                // consumed the reference but we still own `msg`.
                if let ServerMessage::StreamEnd(end) = msg {
                    final_end = Some(end);
                    break;
                }
            }
            continue;
        }

        // Not a stream frame — route to the collectors or error out.
        match msg {
            ServerMessage::ToolCall(tc) => tool_calls.push(tc),
            ServerMessage::ToolResult(tr) => tool_results.push(tr),
            ServerMessage::Error(err) => {
                return Err(ClientError::Protocol(err.message));
            }
            // Benign frames we ignore mid-stream.
            ServerMessage::Ping(_)
            | ServerMessage::Phase(_)
            | ServerMessage::NewMessage(_)
            | ServerMessage::History(_)
            | ServerMessage::SendImage(_) => {}
            // Anything else is a protocol surprise — log and continue.
            other => {
                tracing::debug!(?other, "collect_stream: ignoring unexpected frame");
            }
        }
    }

    let end = final_end.ok_or_else(|| {
        ClientError::Protocol("collect_stream: stream ended without StreamEnd frame".into())
    })?;

    Ok(StreamedResponse {
        text: end.content,
        tool_calls,
        tool_results,
        metadata: end.metadata,
        finish_reason: end.finish_reason,
    })
}
```

Note: if the compiler complains that `ServerMessage::ToolCall`, `ServerMessage::ToolResult`, `ServerMessage::Phase`, `ServerMessage::SendImage`, or `ServerMessage::NewMessage` don't exist, open `shore-protocol/src/server_msg.rs` and adjust the match arms to the actual variant names. The `run.rs` usage at `shore-cli/src/run.rs:252-282` confirms all five variants exist in the current tree.

- [ ] **Step 2: Re-export from `shore-client/src/lib.rs`**

Locate line 16 (`pub use stream::{StreamCallbacks, StreamHandler};`) and extend it to:

```rust
pub use stream::{collect_stream, StreamCallbacks, StreamHandler, StreamedResponse};
```

- [ ] **Step 3: Run the tests to verify they pass**

```sh
cargo test -p shore-client collect_stream
```

Expected: all three `collect_stream` tests pass.

- [ ] **Step 4: Run full shore-client tests**

```sh
cargo test -p shore-client
```

Expected: all pass.

- [ ] **Step 5: Run workspace check**

```sh
cargo check --workspace
```

Expected: clean.

- [ ] **Step 6: Commit**

```sh
git add shore-client/src/stream.rs shore-client/src/lib.rs
git commit -m "feat(client): add collect_stream helper for request/response consumers

Adds StreamedResponse and collect_stream() alongside the existing
frame-by-frame StreamHandler. Callers that can't render chunks
incrementally (like the upcoming shore-mcp server) get the final
aggregate — text, tool calls, tool results, metadata — in one call."
```

---

## Phase 3 — Docs & policy

### Task 4: Update testing policy in both CLAUDE.md files

**Files:**
- Modify: `/home/eshen/.claude/CLAUDE.md` (global)
- Modify: `CLAUDE.md` (project, at repo root)

Both files currently have identical "Agent Directives: Mechanical Overrides" content. The project file also has a "Live Testing Policy" section (currently absolute). We're softening it with a structured revision.

- [ ] **Step 1: Read the current project CLAUDE.md live-testing section**

```sh
rg -n 'Live Testing Policy' CLAUDE.md
```

Identify the exact line range of the "Live Testing Policy" section — it starts with `## Live Testing Policy` and ends before the next `## ` heading. Read those lines.

- [ ] **Step 2: Replace the section in project `CLAUDE.md`**

Replace the entire existing `## Live Testing Policy` block in `CLAUDE.md` with:

```markdown
## Testing Policy (revised 2026-04-14)

The policy "never mock `shore-llm`" exists to prevent one specific failure mode: hand-written mock LLM responses that pass unit tests while the real integration is broken. It is load-bearing for that narrow concern and actively harmful for everything else. This revision distinguishes the two cases.

### Rule 1 — `shore-llm-client` internals: no hand-written mocks

Response parsing, streaming, cache headers, error mapping, prompt cache behavior, and anything else inside `shore-llm-client` must be tested against real API responses — either via live tests gated behind `--ignored` (`cargo test --test e2e -- --ignored`, `./scripts/live-tests/live-test.sh`) or via **recorded fixtures** captured from real API responses. Hand-writing a fake HTTP response body for a unit test is forbidden in this crate, because that is exactly the failure mode the original policy exists to prevent.

### Rule 2 — upstream code may use trait-level test doubles

Code upstream of `shore-llm-client` — `shore-daemon` command routing, `shore-ledger` accounting, `shore-mcp` tool output shaping, `shore-cli` rendering, conversation state management, memory writes — is allowed to stand in a deterministic `LlmClient` implementation that returns pre-made `Message` values, or to use the existing wiremock-backed `MockLlmServer` in `shore-test-harness` (which mocks Anthropic's HTTP wire protocol with real-format SSE frames). These are not "mocking the LLM" in the sense the policy prohibits — they are not claiming to replicate API wire behavior. They are skipping past it to test the caller's own logic.

### Rule 3 — live tests remain mandatory for release verification

`cargo test --test e2e -- --ignored` and `./scripts/live-tests/live-test.sh` still exist and still hit real APIs with real credentials. Nothing in this revision weakens that gate. Recorded fixtures and trait doubles are for fast, deterministic CI-friendly tests — not a substitute for live verification before shipping.

### Rule 4 — recorded fixtures over hand-written stand-ins

When you do need to stand in for an LLM response in a test outside `shore-llm-client`, prefer recording the output of a real cheap model once and replaying it. Fixtures should be checked into the repo and re-recorded periodically (quarterly or whenever a provider behavior change is suspected).
```

- [ ] **Step 3: Mirror the same change into global `~/.claude/CLAUDE.md`**

The global file does not currently contain a "Live Testing Policy" section (it is project-specific), so no change is required there unless the user explicitly wants the revision to apply globally. Confirm by grepping:

```sh
rg -n 'Live Testing Policy|Testing Policy' /home/eshen/.claude/CLAUDE.md
```

If the grep finds nothing, skip editing the global file. If it does find a matching section, replace it with the same block written in Step 2.

- [ ] **Step 4: Run workspace verification (sanity — no code changed, but confirm nothing broke)**

```sh
cargo check --workspace
```

Expected: clean (nothing changed, this is just a baseline).

- [ ] **Step 5: Commit**

```sh
git add CLAUDE.md
git commit -m "docs(claude): revise live testing policy with structured rules

Splits the blanket 'never mock shore-llm' rule into four specific
cases: hand-written mocks forbidden inside shore-llm-client, trait
doubles and HTTP-level mocks permitted upstream, live tests still
mandatory for release verification, recorded fixtures over hand-
written stand-ins. Preserves the original intent (no fantasy-output
tests) while unblocking fast deterministic tests for callers."
```

### Task 5: Record MCP decisions in `docs/DECISIONS.md`

**Files:**
- Modify: `docs/DECISIONS.md`

- [ ] **Step 1: Read the existing DECISIONS.md to match its format**

```sh
cat docs/DECISIONS.md | head -60
```

Note the heading style, entry format, and ordering (newest first vs oldest first). Match it.

- [ ] **Step 2: Append the MCP decision entries**

Add new entries following the existing format. The entries to record are:

**Entry A — shore-mcp crate introduced as debug-only**

```markdown
### 2026-04-14 — shore-mcp crate added as a debug-only MCP server

Added a new `shore-mcp` crate exposing Shore's CLI surface as MCP tools for AI clients (primarily Claude Code). The crate is gated behind a `feature = "enabled"` + `required-features` on the `[[bin]]`, plus a `cfg(debug_assertions)` stub in `main.rs`, so it is never built by `cargo build --workspace --release` in the default configuration. A custom release profile that deliberately enables both the feature and debug_assertions will produce the real binary — supported but not "default."

**Why:** We wanted Claude Code (and other MCP clients) to drive Shore programmatically for debugging and automated workflows, without (a) bloating the shipped release binary set or (b) giving an AI client default access to the user's real Shore profile.

**Hybrid daemon model:** By default, `shore-mcp` targets an isolated test profile (`$XDG_DATA_HOME/shore-mcp-test/...`) and spawns a dedicated `shore-daemon` child process with `--instance-id=shore-mcp-test` if one is not already running. `--attach-main` opts into the user's real profile via normal discovery. Mutation tools (send/regen/config-set/character-switch/etc.) refuse to execute on the main profile unless `--allow-main-writes` is also passed.

**Sacrificed:** Zero-touch single-binary distribution. You can't `cargo install shore-mcp` from a release checkout without custom profile flags — that's intentional.
```

**Entry B — testing policy revision**

```markdown
### 2026-04-14 — Live-testing policy revised to a four-rule structure

The original blanket rule "never mock `shore-llm`" was causing tests to be skipped entirely rather than rewritten to use real API calls. Revised policy (see `CLAUDE.md` for the authoritative version):

1. Inside `shore-llm-client`: real API calls or recorded fixtures only, never hand-written HTTP responses.
2. Upstream of `shore-llm-client`: trait-level doubles and HTTP-level wiremock (via existing `MockLlmServer` in `shore-test-harness`) are allowed.
3. Live tests (`cargo test --test e2e -- --ignored`) remain mandatory for release verification.
4. When standing in for an LLM response outside `shore-llm-client`, prefer recorded fixtures over hand-written stand-ins.

**Why:** The policy was written to prevent fantasy-output tests — mocks that described the LLM as the author wished it behaved rather than how it actually did. That failure mode is specifically in the parsing/wire-protocol layer, which is confined to `shore-llm-client`. Code upstream of it doesn't benefit from real API calls at all; it benefits from deterministic inputs. The revision preserves the original intent for the layer where it matters and unblocks fast tests for everything else.

**Sacrificed:** Nothing, in theory. In practice, it raises the discipline bar: upstream tests are now allowed to use doubles, but reviewers have to check that those doubles don't creep into `shore-llm-client` itself.
```

- [ ] **Step 3: Run workspace verification (baseline sanity)**

```sh
cargo check --workspace
```

Expected: clean.

- [ ] **Step 4: Commit**

```sh
git add docs/DECISIONS.md
git commit -m "docs(decisions): record shore-mcp introduction and testing policy revision"
```

---

## Phase 4 — shore-mcp crate scaffold

### Task 6: Create crate skeleton with debug-only build gate

**Files:**
- Create: `shore-mcp/Cargo.toml`
- Create: `shore-mcp/src/main.rs`
- Create: `shore-mcp/src/lib.rs`
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Add `shore-mcp` to workspace members**

Open `Cargo.toml` at the workspace root. Locate the `members = [...]` block (lines 3-16). Add `"shore-mcp",` alphabetically. After edit, the relevant section should read:

```toml
[workspace]
resolver = "2"
members = [
    "shore-protocol",
    "shore-client",
    "shore-diagnostics",
    "shore-config",
    "shore-llm-client",
    "shore-daemon",
    "shore-daemon-server",
    "shore-cli",
    "shore-mcp",
    "shore-tui",
    "shore-ledger",
    "shore-test-harness",
    # "shore-matrix",  # disabled: matrix-sdk 0.16.0 hits recursion_limit on rustc 1.94+
]
```

- [ ] **Step 2: Create `shore-mcp/Cargo.toml`**

Write the following to `shore-mcp/Cargo.toml`:

```toml
[package]
name = "shore-mcp"
version = "0.1.0"
edition = "2021"
publish = false
description = "MCP server exposing Shore's CLI surface for debugging and programmatic use (debug-only)."

[features]
default = []
# Turning on `enabled` pulls in MCP deps and makes the bin buildable.
# Combined with `cfg(debug_assertions)` in main.rs, release builds
# produce no binary unless explicitly opted into via a custom profile.
enabled = ["dep:rmcp", "dep:schemars"]

[[bin]]
name = "shore-mcp"
required-features = ["enabled"]

[dependencies]
shore-client = { path = "../shore-client" }
shore-config = { path = "../shore-config" }
shore-protocol = { path = "../shore-protocol" }
tokio = { workspace = true }
anyhow = "1"
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
clap = { version = "4", features = ["derive"] }

# MCP deps — optional, gated behind `enabled`.
rmcp = { version = "0.1", optional = true, features = ["transport-io"] }
schemars = { version = "0.8", optional = true }

[dev-dependencies]
tempfile = { workspace = true }
shore-test-harness = { path = "../shore-test-harness" }
```

Note: the `rmcp` version `"0.1"` and feature name `"transport-io"` are placeholders based on the current crates.io state at plan-writing time. Before running `cargo build`, verify the actual current version and feature set:

```sh
cargo search rmcp --limit 5
```

If the version or feature names differ, update `Cargo.toml` before proceeding. The `rmcp` docs-at-the-time (context7 snapshot, see spec) referenced `tool_router`, `tool`, `handler::server::wrapper::Parameters`, and `handler::server::router::tool::ToolRouter` — if those paths have moved in a newer version, adjust module imports in Task 12+ accordingly.

- [ ] **Step 3: Create `shore-mcp/src/main.rs` with the debug-only gate**

Write:

```rust
// shore-mcp is a debug/testing-only binary. In release builds — even with
// the `enabled` feature on — `debug_assertions` is off by default, so the
// binary becomes a stub that refuses to run. Set a custom profile with
// `debug-assertions = true` if you really want a release build.

#[cfg(not(debug_assertions))]
fn main() {
    eprintln!(
        "shore-mcp is only available in debug builds. \
         Rebuild with `cargo build -p shore-mcp --features enabled` \
         (default dev profile) or a custom profile with \
         `debug-assertions = true`."
    );
    std::process::exit(1);
}

#[cfg(debug_assertions)]
mod cli;
#[cfg(debug_assertions)]
mod gating;
#[cfg(debug_assertions)]
mod handler;
#[cfg(debug_assertions)]
mod profile;
#[cfg(debug_assertions)]
mod server;
#[cfg(debug_assertions)]
mod tools;

#[cfg(debug_assertions)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("shore_mcp=info")),
        )
        .with_writer(std::io::stderr) // stdout is reserved for JSON-RPC
        .init();

    server::run().await
}
```

- [ ] **Step 4: Create `shore-mcp/src/lib.rs`**

Write:

```rust
// Module structure. Everything real is debug-gated in main.rs; this lib.rs
// exists so `cargo test -p shore-mcp --features enabled` can find tests in
// each module file.

#[cfg(debug_assertions)]
pub mod cli;
#[cfg(debug_assertions)]
pub mod gating;
#[cfg(debug_assertions)]
pub mod handler;
#[cfg(debug_assertions)]
pub mod profile;
#[cfg(debug_assertions)]
pub mod server;
#[cfg(debug_assertions)]
pub mod tools;
```

- [ ] **Step 5: Create minimum stubs for each declared module so the tree compiles**

Write a one-line stub for each of the modules the main.rs and lib.rs reference. We'll fill them in later — but they must exist for the crate to compile at all.

`shore-mcp/src/cli.rs`:

```rust
// Populated in Task 7.
```

`shore-mcp/src/profile.rs`:

```rust
// Populated in Task 8 and 9.
```

`shore-mcp/src/gating.rs`:

```rust
// Populated in Task 10.
```

`shore-mcp/src/handler.rs`:

```rust
// Populated in Task 11.
```

`shore-mcp/src/server.rs`:

```rust
// Populated in Task 11.

pub async fn run() -> anyhow::Result<()> {
    anyhow::bail!("shore-mcp server not yet implemented")
}
```

`shore-mcp/src/tools/mod.rs`:

```rust
// Populated in Tasks 12-18.
```

- [ ] **Step 6: Verify `cargo build --workspace` ignores shore-mcp (feature off)**

```sh
cargo build --workspace
```

Expected: completes successfully. Shore-mcp itself compiles as a library (since it's in the workspace), but **no `shore-mcp` binary is produced** because `required-features = ["enabled"]` is not satisfied.

Verify no binary:

```sh
ls target/debug/shore-mcp 2>&1 || echo "no binary — correct"
```

Expected: `no binary — correct`.

- [ ] **Step 7: Verify `cargo build --workspace --release` also ignores shore-mcp**

```sh
cargo build --workspace --release
```

Expected: completes successfully, and:

```sh
ls target/release/shore-mcp 2>&1 || echo "no release binary — correct"
```

Expected: `no release binary — correct`.

- [ ] **Step 8: Verify feature-enabled build produces a debug binary**

```sh
cargo build -p shore-mcp --features enabled
ls target/debug/shore-mcp
```

Expected: binary exists. Running it will fail (server not implemented yet), which is fine for now:

```sh
target/debug/shore-mcp 2>&1 || echo "expected fail (server stub)"
```

Expected: error from the stub `run()`, exit code non-zero.

- [ ] **Step 9: Verify feature-enabled release build produces a stub binary**

```sh
cargo build -p shore-mcp --features enabled --release
target/release/shore-mcp 2>&1
```

Expected: binary prints "shore-mcp is only available in debug builds" and exits with code 1.

- [ ] **Step 10: Run workspace tests to ensure nothing regressed**

```sh
cargo test --workspace
```

Expected: all pass.

- [ ] **Step 11: Commit**

```sh
git add Cargo.toml shore-mcp/
git commit -m "feat(mcp): scaffold shore-mcp crate with debug-only build gate

Adds the empty shore-mcp crate to the workspace with:
- Feature-gated rmcp and schemars deps
- required-features on the [[bin]] so default builds skip it
- cfg(debug_assertions) stub main so release builds (even with the
  feature explicitly enabled) produce a refuse-to-run binary
- Module stubs for cli/profile/gating/handler/server/tools

Release workspace builds produce no shore-mcp binary. Feature-enabled
debug builds produce a real (but not-yet-functional) binary."
```

### Task 7: Implement `cli.rs` with flag parsing

**Files:**
- Modify: `shore-mcp/src/cli.rs`
- Test: inline in `shore-mcp/src/cli.rs`

- [ ] **Step 1: Write the failing tests**

Replace the stub contents of `shore-mcp/src/cli.rs` with:

```rust
use clap::Parser;

/// Shore MCP server — exposes the Shore CLI surface as MCP tools.
#[derive(Debug, Parser, Clone)]
#[command(name = "shore-mcp", version, about)]
pub struct Cli {
    /// Attach to the user's main Shore daemon profile instead of the
    /// isolated test profile. Mutation tools are refused in this mode
    /// unless `--allow-main-writes` is also set.
    #[arg(long)]
    pub attach_main: bool,

    /// Use a fresh tempdir profile instead of the persistent test profile
    /// at `$XDG_DATA_HOME/shore-mcp-test/`. Ignored if `--attach-main` is
    /// set. The tempdir and its spawned daemon are torn down on exit.
    #[arg(long, conflicts_with = "attach_main")]
    pub ephemeral: bool,

    /// Permit mutation tools to execute against the main profile. Requires
    /// `--attach-main`; a no-op otherwise. This is a deliberate two-flag
    /// opt-in, not a default.
    #[arg(long, requires = "attach_main")]
    pub allow_main_writes: bool,

    /// Override the daemon TCP address instead of discovering it. Useful
    /// for integration tests where the daemon is already running and its
    /// address is known. Mutually exclusive with the default spawn path.
    #[arg(long, value_name = "ADDR")]
    pub daemon_addr: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        let mut argv = vec!["shore-mcp"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv)
    }

    #[test]
    fn defaults_are_all_false() {
        let cli = parse(&[]).unwrap();
        assert!(!cli.attach_main);
        assert!(!cli.ephemeral);
        assert!(!cli.allow_main_writes);
        assert!(cli.daemon_addr.is_none());
    }

    #[test]
    fn attach_main_flag() {
        let cli = parse(&["--attach-main"]).unwrap();
        assert!(cli.attach_main);
    }

    #[test]
    fn ephemeral_and_attach_main_are_mutually_exclusive() {
        let err = parse(&["--ephemeral", "--attach-main"]).unwrap_err();
        assert!(err.to_string().contains("cannot be used with"));
    }

    #[test]
    fn allow_main_writes_requires_attach_main() {
        let err = parse(&["--allow-main-writes"]).unwrap_err();
        assert!(err.to_string().contains("requires"));
    }

    #[test]
    fn allow_main_writes_accepted_with_attach_main() {
        let cli = parse(&["--attach-main", "--allow-main-writes"]).unwrap();
        assert!(cli.attach_main);
        assert!(cli.allow_main_writes);
    }

    #[test]
    fn daemon_addr_override() {
        let cli = parse(&["--daemon-addr", "127.0.0.1:7999"]).unwrap();
        assert_eq!(cli.daemon_addr.as_deref(), Some("127.0.0.1:7999"));
    }
}
```

- [ ] **Step 2: Run the tests**

```sh
cargo test -p shore-mcp --features enabled --lib cli::
```

Expected: all six tests pass.

- [ ] **Step 3: Commit**

```sh
git add shore-mcp/src/cli.rs
git commit -m "feat(mcp): add Cli struct with attach-main / ephemeral / allow-main-writes / daemon-addr"
```

### Task 8: Implement `profile.rs` path resolution

**Files:**
- Modify: `shore-mcp/src/profile.rs`
- Create: `shore-mcp/tests/profile_resolution.rs`

- [ ] **Step 1: Write the failing tests**

Create `shore-mcp/tests/profile_resolution.rs`:

```rust
use std::path::PathBuf;

use shore_mcp::profile::{resolve_profile, ProfileKind, ResolvedProfile};

#[test]
fn attach_main_uses_main_profile_with_no_overrides() {
    let profile = resolve_profile(shore_mcp::cli::Cli {
        attach_main: true,
        ephemeral: false,
        allow_main_writes: false,
        daemon_addr: None,
    })
    .unwrap();

    assert_eq!(profile.kind, ProfileKind::Main);
    assert!(!profile.is_test());
    assert!(profile.env_overrides.is_empty());
}

#[test]
fn default_mode_uses_persistent_test_paths() {
    let profile = resolve_profile(shore_mcp::cli::Cli {
        attach_main: false,
        ephemeral: false,
        allow_main_writes: false,
        daemon_addr: None,
    })
    .unwrap();

    assert_eq!(profile.kind, ProfileKind::PersistentTest);
    assert!(profile.is_test());
    // Must export all three env vars.
    let keys: Vec<_> = profile.env_overrides.iter().map(|(k, _)| k.clone()).collect();
    assert!(keys.contains(&"SHORE_CONFIG_DIR".to_string()));
    assert!(keys.contains(&"SHORE_DATA_DIR".to_string()));
    assert!(keys.contains(&"SHORE_RUNTIME_DIR".to_string()));

    // All three paths should share a common ancestor named "shore-mcp-test".
    for (_, path) in &profile.env_overrides {
        assert!(
            PathBuf::from(path)
                .components()
                .any(|c| c.as_os_str() == "shore-mcp-test"),
            "expected shore-mcp-test in path: {path}"
        );
    }
}

#[test]
fn ephemeral_mode_uses_tempdir() {
    let profile = resolve_profile(shore_mcp::cli::Cli {
        attach_main: false,
        ephemeral: true,
        allow_main_writes: false,
        daemon_addr: None,
    })
    .unwrap();

    assert_eq!(profile.kind, ProfileKind::Ephemeral);
    assert!(profile.is_test());
    assert!(profile.tempdir.is_some(), "ephemeral profile must own a tempdir");
}
```

- [ ] **Step 2: Write the failing unit tests inline in `profile.rs`**

Replace the stub `shore-mcp/src/profile.rs` with:

```rust
use std::path::PathBuf;

use crate::cli::Cli;

/// Kind of profile we resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileKind {
    /// `--attach-main`: user's real daemon.
    Main,
    /// Default: persistent test profile at $XDG_DATA_HOME/shore-mcp-test.
    PersistentTest,
    /// `--ephemeral`: tempdir, torn down on exit.
    Ephemeral,
}

/// Resolved profile info. Consumers set env vars before spawning the daemon.
#[derive(Debug)]
pub struct ResolvedProfile {
    pub kind: ProfileKind,
    /// (env_var_name, value) pairs to export before starting shore-client
    /// discovery or spawning a daemon. Empty for `Main`.
    pub env_overrides: Vec<(String, String)>,
    /// Tempdir handle, only set for `Ephemeral`. Drop-on-exit keeps the
    /// profile directory alive for the lifetime of the MCP server.
    pub tempdir: Option<tempfile::TempDir>,
}

impl ResolvedProfile {
    /// Whether mutation tools are gated (i.e., this is NOT the main profile).
    pub fn is_test(&self) -> bool {
        !matches!(self.kind, ProfileKind::Main)
    }
}

/// Resolve which profile to use from parsed CLI args.
pub fn resolve_profile(cli: Cli) -> anyhow::Result<ResolvedProfile> {
    if cli.attach_main {
        return Ok(ResolvedProfile {
            kind: ProfileKind::Main,
            env_overrides: Vec::new(),
            tempdir: None,
        });
    }

    if cli.ephemeral {
        let td = tempfile::tempdir()?;
        let base = td.path().to_path_buf();
        let overrides = build_env_overrides(&base);
        return Ok(ResolvedProfile {
            kind: ProfileKind::Ephemeral,
            env_overrides: overrides,
            tempdir: Some(td),
        });
    }

    // Persistent test profile.
    let base = persistent_test_base();
    let overrides = build_env_overrides(&base);
    Ok(ResolvedProfile {
        kind: ProfileKind::PersistentTest,
        env_overrides: overrides,
        tempdir: None,
    })
}

/// Default location for the persistent test profile.
///
/// Uses `$XDG_DATA_HOME/shore-mcp-test/` or `$HOME/.local/share/shore-mcp-test/`
/// as a fallback. Never returns a path inside the user's real Shore profile.
fn persistent_test_base() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("shore-mcp-test");
        }
    }
    if let Some(home) = dirs::home_dir() {
        return home.join(".local").join("share").join("shore-mcp-test");
    }
    // Last-resort fallback. If HOME is unset the user has bigger problems.
    PathBuf::from("/tmp/shore-mcp-test")
}

fn build_env_overrides(base: &std::path::Path) -> Vec<(String, String)> {
    let config = base.join("config");
    let data = base.join("data");
    let runtime = base.join("runtime");
    vec![
        (
            "SHORE_CONFIG_DIR".into(),
            config.to_string_lossy().into_owned(),
        ),
        (
            "SHORE_DATA_DIR".into(),
            data.to_string_lossy().into_owned(),
        ),
        (
            "SHORE_RUNTIME_DIR".into(),
            runtime.to_string_lossy().into_owned(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank_cli() -> Cli {
        Cli {
            attach_main: false,
            ephemeral: false,
            allow_main_writes: false,
            daemon_addr: None,
        }
    }

    #[test]
    fn main_profile_has_no_env_overrides() {
        let cli = Cli {
            attach_main: true,
            ..blank_cli()
        };
        let resolved = resolve_profile(cli).unwrap();
        assert_eq!(resolved.kind, ProfileKind::Main);
        assert!(resolved.env_overrides.is_empty());
        assert!(!resolved.is_test());
    }

    #[test]
    fn persistent_profile_under_xdg_data_home() {
        std::env::set_var("XDG_DATA_HOME", "/tmp/test-shore-mcp-xdg");
        let resolved = resolve_profile(blank_cli()).unwrap();
        assert_eq!(resolved.kind, ProfileKind::PersistentTest);
        for (_, path) in &resolved.env_overrides {
            assert!(path.starts_with("/tmp/test-shore-mcp-xdg/shore-mcp-test"));
        }
        std::env::remove_var("XDG_DATA_HOME");
    }

    #[test]
    fn ephemeral_profile_keeps_tempdir_alive() {
        let cli = Cli {
            ephemeral: true,
            ..blank_cli()
        };
        let resolved = resolve_profile(cli).unwrap();
        assert_eq!(resolved.kind, ProfileKind::Ephemeral);
        let tempdir_path = resolved.tempdir.as_ref().unwrap().path().to_path_buf();
        assert!(tempdir_path.exists());
        // Env overrides must live under the tempdir.
        for (_, path) in &resolved.env_overrides {
            assert!(path.starts_with(tempdir_path.to_str().unwrap()));
        }
    }
}
```

- [ ] **Step 3: Add `dirs` to `shore-mcp/Cargo.toml` dependencies**

Under `[dependencies]`, add:

```toml
dirs = { workspace = true }
```

- [ ] **Step 4: Run the inline tests**

```sh
cargo test -p shore-mcp --features enabled --lib profile::
```

Expected: three tests pass.

- [ ] **Step 5: Run the external integration test**

```sh
cargo test -p shore-mcp --features enabled --test profile_resolution
```

Expected: three tests pass.

- [ ] **Step 6: Run workspace check**

```sh
cargo check --workspace
```

Expected: clean.

- [ ] **Step 7: Commit**

```sh
git add shore-mcp/src/profile.rs shore-mcp/tests/profile_resolution.rs shore-mcp/Cargo.toml
git commit -m "feat(mcp): add profile resolution (main/persistent/ephemeral)"
```

### Task 9: Implement `profile.rs` daemon discover-or-spawn

**Files:**
- Modify: `shore-mcp/src/profile.rs`
- Test: inline

This task extends `profile.rs` with the actual daemon attach logic: given a `ResolvedProfile`, return a live `SWPConnection`.

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `shore-mcp/src/profile.rs`:

```rust
#[tokio::test]
async fn attach_uses_daemon_addr_override_when_set() {
    use std::io::Write;

    // Stand up a bogus TCP listener so the connect attempt fails with
    // a protocol error rather than a connect error — enough to prove
    // we went to the overridden address and skipped discovery.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());

    // Accept one connection and write garbage so the SWP handshake fails.
    let handle = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let _ = s.write_all(b"not-a-valid-hello\n");
    });

    let cli = Cli {
        attach_main: true,
        ephemeral: false,
        allow_main_writes: false,
        daemon_addr: Some(addr.clone()),
    };
    let resolved = resolve_profile(cli.clone()).unwrap();

    let result = attach(&resolved, &cli).await;
    assert!(result.is_err(), "handshake should fail on bogus daemon");
    // The error message should mention protocol, not discovery.
    let err_str = format!("{}", result.unwrap_err());
    assert!(
        err_str.contains("protocol") || err_str.contains("hello") || err_str.contains("version"),
        "expected protocol-level error, got: {err_str}"
    );

    handle.join().unwrap();
}
```

- [ ] **Step 2: Add the `attach()` function**

Append to `shore-mcp/src/profile.rs` (before the `#[cfg(test)]` block):

```rust
use shore_client::{discover, ClientError, SWPConnection, ServerAddr};

/// The stable instance ID that `shore-mcp` uses when spawning a test daemon.
pub const MCP_INSTANCE_ID: &str = "shore-mcp-test";

/// Resolve a live daemon connection for the given profile.
///
/// Decision tree (matches the spec):
/// - `--daemon-addr` set: connect directly, skip discovery and spawning.
/// - `Main`: normal shore-client discovery.
/// - `PersistentTest` / `Ephemeral`:
///     - Export env overrides so discovery resolves to the test profile.
///     - Look up `MCP_INSTANCE_ID` in that profile's instances.json.
///     - If found, attach.
///     - Otherwise, spawn a shore-daemon child process with
///       `--instance-id=MCP_INSTANCE_ID`, wait for registration, then attach.
pub async fn attach(
    profile: &ResolvedProfile,
    cli: &Cli,
) -> anyhow::Result<SWPConnection> {
    // 1. Export env overrides BEFORE any discovery or spawn.
    for (k, v) in &profile.env_overrides {
        std::env::set_var(k, v);
    }

    // 2. Explicit --daemon-addr wins.
    if let Some(addr) = &cli.daemon_addr {
        let (conn, _hello, _history) = SWPConnection::connect(
            &ServerAddr(addr.clone()),
            "mcp",
            "shore-mcp",
            None,
        )
        .await?;
        return Ok(conn);
    }

    // 3. Main profile: normal discovery, no spawning.
    if matches!(profile.kind, ProfileKind::Main) {
        let addr = discover(None)?;
        let (conn, _hello, _history) =
            SWPConnection::connect(&addr, "mcp", "shore-mcp", None).await?;
        return Ok(conn);
    }

    // 4. Test profile: look up MCP_INSTANCE_ID, spawn on miss.
    match discover(Some(MCP_INSTANCE_ID)) {
        Ok(addr) => {
            let (conn, _hello, _history) =
                SWPConnection::connect(&addr, "mcp", "shore-mcp", None).await?;
            Ok(conn)
        }
        Err(ClientError::Discovery(_)) => {
            // No live test daemon — spawn one.
            spawn_and_attach_test_daemon().await
        }
        Err(e) => Err(e.into()),
    }
}

async fn spawn_and_attach_test_daemon() -> anyhow::Result<SWPConnection> {
    use std::process::Stdio;
    use tokio::process::Command;
    use tokio::time::{sleep, Duration};

    // Find the shore-daemon binary in the current target dir.
    // Precedence: explicit SHORE_DAEMON_BIN env var, then $CARGO_TARGET_DIR,
    // then ./target/debug/shore-daemon as a fallback.
    let binary = shore_daemon_path()?;

    // Bind port 0 trick: let the daemon pick a free port via --addr=127.0.0.1:0.
    // The daemon will register the resolved addr in instances.json for us to discover.
    let child = Command::new(&binary)
        .arg("--instance-id")
        .arg(MCP_INSTANCE_ID)
        .arg("--addr")
        .arg("127.0.0.1:0")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn {}: {e}", binary.display()))?;

    // We intentionally DO NOT wait() on the child — in persistent mode the
    // daemon outlives the MCP server. Dropping the Child handle detaches it.
    drop(child);

    // Poll instances.json for up to 5 seconds waiting for registration.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(addr) = discover(Some(MCP_INSTANCE_ID)) {
            let (conn, _hello, _history) =
                SWPConnection::connect(&addr, "mcp", "shore-mcp", None).await?;
            return Ok(conn);
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!(
                "spawned shore-daemon did not register instance '{MCP_INSTANCE_ID}' within 5s"
            );
        }
        sleep(Duration::from_millis(100)).await;
    }
}

fn shore_daemon_path() -> anyhow::Result<PathBuf> {
    if let Ok(explicit) = std::env::var("SHORE_DAEMON_BIN") {
        let p = PathBuf::from(explicit);
        if p.exists() {
            return Ok(p);
        }
    }
    // Fall back to PATH lookup.
    if let Ok(p) = which::which("shore-daemon") {
        return Ok(p);
    }
    anyhow::bail!(
        "could not find shore-daemon binary. Set SHORE_DAEMON_BIN to an explicit path, \
         or put shore-daemon on PATH (e.g. after `cargo build -p shore-daemon`)."
    )
}
```

- [ ] **Step 3: Add `which` and verify `tempfile` is already in deps**

Add to `shore-mcp/Cargo.toml` under `[dependencies]`:

```toml
tempfile = { workspace = true }
which = "6"
```

- [ ] **Step 4: Run the new test**

```sh
cargo test -p shore-mcp --features enabled --lib profile::tests::attach_uses_daemon_addr
```

Expected: passes (the test only exercises the `--daemon-addr` path, which requires no spawned daemon and no env manipulation).

- [ ] **Step 5: Run all profile tests to check for regressions**

```sh
cargo test -p shore-mcp --features enabled
```

Expected: all pass.

- [ ] **Step 6: Commit**

```sh
git add shore-mcp/src/profile.rs shore-mcp/Cargo.toml
git commit -m "feat(mcp): add daemon discover-or-spawn with --daemon-addr override"
```

### Task 10: Implement `gating.rs` for write-op refusal

**Files:**
- Modify: `shore-mcp/src/gating.rs`
- Create: `shore-mcp/tests/gating_rules.rs`

- [ ] **Step 1: Write the failing tests**

Create `shore-mcp/tests/gating_rules.rs`:

```rust
use shore_mcp::gating::{check, GateContext, GateDecision};

fn test_ctx(is_test: bool, allow_main_writes: bool) -> GateContext {
    GateContext {
        profile_is_test: is_test,
        allow_main_writes,
    }
}

#[test]
fn read_only_tools_always_allowed() {
    let ctx = test_ctx(false, false);
    for tool in &[
        "status",
        "status_diagnostics",
        "log_tail",
        "log_show",
        "log_heartbeat",
        "usage",
        "config_get",
        "config_check",
        "character_list",
        "character_info",
        "model_list",
        "model_info",
        "memory_query",
    ] {
        assert_eq!(
            check(tool, &ctx),
            GateDecision::Allow,
            "read-only tool {tool} should always be allowed"
        );
    }
}

#[test]
fn mutating_tools_allowed_on_test_profile() {
    let ctx = test_ctx(true, false);
    for tool in &[
        "send",
        "regen",
        "config_set",
        "character_switch",
        "model_switch",
        "log_delete",
    ] {
        assert_eq!(
            check(tool, &ctx),
            GateDecision::Allow,
            "mutating tool {tool} should be allowed on test profile"
        );
    }
}

#[test]
fn mutating_tools_refused_on_main_profile_without_allow_writes() {
    let ctx = test_ctx(false, false);
    for tool in &[
        "send",
        "regen",
        "config_set",
        "character_switch",
        "model_switch",
        "log_delete",
    ] {
        match check(tool, &ctx) {
            GateDecision::Refuse(_) => {}
            other => panic!("expected Refuse for {tool}, got {other:?}"),
        }
    }
}

#[test]
fn mutating_tools_allowed_on_main_with_explicit_opt_in() {
    let ctx = test_ctx(false, true);
    assert_eq!(check("send", &ctx), GateDecision::Allow);
    assert_eq!(check("config_set", &ctx), GateDecision::Allow);
}

#[test]
fn unknown_tools_are_refused() {
    let ctx = test_ctx(true, false);
    match check("nonexistent_tool", &ctx) {
        GateDecision::Refuse(msg) => assert!(msg.contains("unknown")),
        other => panic!("expected Refuse for unknown tool, got {other:?}"),
    }
}
```

- [ ] **Step 2: Implement `gating.rs`**

Replace the stub `shore-mcp/src/gating.rs` with:

```rust
/// Context for gate decisions.
#[derive(Debug, Clone, Copy)]
pub struct GateContext {
    /// `true` if we are on an isolated test profile, `false` on main.
    pub profile_is_test: bool,
    /// `true` if `--allow-main-writes` was passed (only meaningful when
    /// `profile_is_test == false`).
    pub allow_main_writes: bool,
}

/// Result of a gate check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    Allow,
    Refuse(String),
}

/// Classification of a tool as read-only, mutating, or unknown.
enum ToolClass {
    ReadOnly,
    Mutating,
    Unknown,
}

fn classify(tool: &str) -> ToolClass {
    match tool {
        // ── read-only ──────────────────────────────────────────────
        "status"
        | "status_diagnostics"
        | "log_tail"
        | "log_show"
        | "log_heartbeat"
        | "log_follow"
        | "usage"
        | "config_get"
        | "config_check"
        | "config_path"
        | "character_list"
        | "character_info"
        | "model_list"
        | "model_info"
        | "memory_query"
        | "memory_changelog" => ToolClass::ReadOnly,

        // ── mutating ───────────────────────────────────────────────
        "send"
        | "regen"
        | "log_delete"
        | "log_edit"
        | "config_set"
        | "config_reset"
        | "character_switch"
        | "character_new"
        | "model_switch"
        | "model_reset"
        | "memory_compact"
        | "memory_collate"
        | "memory_purge"
        | "memory_reindex"
        | "usage_refresh_pricing"
        | "usage_recalculate"
        | "debug_tick_now"
        | "debug_status_dormant"
        | "debug_status_active" => ToolClass::Mutating,

        _ => ToolClass::Unknown,
    }
}

pub fn check(tool: &str, ctx: &GateContext) -> GateDecision {
    match classify(tool) {
        ToolClass::ReadOnly => GateDecision::Allow,
        ToolClass::Mutating => {
            if ctx.profile_is_test || ctx.allow_main_writes {
                GateDecision::Allow
            } else {
                GateDecision::Refuse(format!(
                    "refused: tool `{tool}` mutates state and cannot run \
                     against the main profile. Re-launch without \
                     --attach-main, or pass --allow-main-writes to opt in."
                ))
            }
        }
        ToolClass::Unknown => GateDecision::Refuse(format!("refused: unknown tool `{tool}`")),
    }
}
```

- [ ] **Step 3: Run the tests**

```sh
cargo test -p shore-mcp --features enabled --test gating_rules
```

Expected: all five tests pass.

- [ ] **Step 4: Commit**

```sh
git add shore-mcp/src/gating.rs shore-mcp/tests/gating_rules.rs
git commit -m "feat(mcp): add gating rules for write-op refusal on main profile"
```

### Task 11: Scaffold `handler.rs` + `server.rs` with rmcp stdio wiring

**Files:**
- Modify: `shore-mcp/src/handler.rs`
- Modify: `shore-mcp/src/server.rs`
- Modify: `shore-mcp/src/tools/mod.rs`

- [ ] **Step 1: Implement the bare handler**

Replace the stub `shore-mcp/src/handler.rs` with:

```rust
use std::sync::Arc;

use tokio::sync::Mutex;

use rmcp::handler::server::tool::ToolRouter;
use rmcp::tool_router;
use shore_client::SWPConnection;

use crate::cli::Cli;
use crate::gating::GateContext;

/// Handler struct passed to rmcp as the server state.
///
/// Holds the single `SWPConnection` to shore-daemon (wrapped in a Mutex
/// because MCP tool calls may be concurrent and we need serial SWP access),
/// plus the gate context for mutation-tool refusal.
pub struct ShoreMcpHandler {
    pub conn: Arc<Mutex<SWPConnection>>,
    pub gate: GateContext,
    pub(crate) tool_router: ToolRouter<Self>,
}

#[tool_router]
impl ShoreMcpHandler {
    pub fn new(conn: SWPConnection, cli: &Cli, profile_is_test: bool) -> Self {
        let gate = GateContext {
            profile_is_test,
            allow_main_writes: cli.allow_main_writes,
        };
        Self {
            conn: Arc::new(Mutex::new(conn)),
            gate,
            tool_router: Self::tool_router(),
        }
    }
}
```

Note: the `#[tool_router]` macro and the `tool_router()` method it generates are from `rmcp`. Individual `#[tool]` methods will be added in subsequent tasks inside other `impl ShoreMcpHandler` blocks. If rmcp requires all `#[tool]` methods to live in the same `impl` block as the `#[tool_router]` macro, we will consolidate them here — **check rmcp's macro docs before writing Task 12**.

- [ ] **Step 2: Implement `server.rs` with the stdio loop**

Replace the stub `shore-mcp/src/server.rs` with:

```rust
use clap::Parser;

use crate::cli::Cli;
use crate::handler::ShoreMcpHandler;
use crate::profile;

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let resolved = profile::resolve_profile(cli.clone())?;
    let profile_is_test = resolved.is_test();

    tracing::info!(
        kind = ?resolved.kind,
        profile_is_test,
        allow_main_writes = cli.allow_main_writes,
        "resolved shore-mcp profile"
    );

    let conn = profile::attach(&resolved, &cli).await?;
    let handler = ShoreMcpHandler::new(conn, &cli, profile_is_test);

    // Wire handler to rmcp stdio transport. The exact API may vary by
    // rmcp version — see rmcp's `handler::server::router::tool` docs. The
    // call below matches rmcp 0.1's expected stdio entry point; update
    // if the version pinned in Cargo.toml differs.
    rmcp::ServiceExt::serve(handler, rmcp::transport::stdio())
        .await
        .map_err(|e| anyhow::anyhow!("rmcp stdio service failed: {e}"))?;

    // Keep the ephemeral tempdir alive for the lifetime of the server.
    // If this were dropped earlier, the profile directory would vanish
    // while the daemon is still running.
    drop(resolved);

    Ok(())
}
```

**Important:** the `rmcp::ServiceExt::serve` / `rmcp::transport::stdio` API references are based on the spec's context7 snapshot of rmcp. If the version pinned in Cargo.toml uses a different entry point, **check `rmcp --example` or docs.rs/rmcp/latest** and update this call. Expect possibly:
- A different module path for `stdio()` (e.g. `rmcp::transport::io::stdio` or `rmcp::stdio()`)
- A different trait/method name than `ServiceExt::serve`
- A required wrapper around `handler` (e.g. `.into_service()`)

If the API has shifted, update the call and any needed imports — do not modify `ShoreMcpHandler`'s shape.

- [ ] **Step 3: Leave `tools/mod.rs` empty for now**

It will be filled by Tasks 12-18.

- [ ] **Step 4: Try to build**

```sh
cargo build -p shore-mcp --features enabled
```

Expected outcomes:
- If rmcp's API matches the spec's references: clean build.
- If it doesn't: compile errors pointing at `rmcp::ServiceExt`, `rmcp::transport::stdio`, or `#[tool_router]`. Follow the errors, consult `cargo doc -p rmcp --open` or docs.rs, and adjust imports and call sites until clean. **Do not** invent wrapping types — stick to rmcp's documented pattern.

- [ ] **Step 5: Run the binary with `--daemon-addr` pointing at a dead address to confirm the stdio loop starts**

```sh
target/debug/shore-mcp --attach-main --daemon-addr 127.0.0.1:1 < /dev/null 2>&1 | head -20
```

Expected: an error about the daemon connection failing, logged to stderr. This proves the binary parses CLI, resolves the profile, and reaches the `attach()` call. It does NOT prove the stdio protocol loop runs correctly — that's what the integration test in Task 19 is for.

- [ ] **Step 6: Commit**

```sh
git add shore-mcp/src/handler.rs shore-mcp/src/server.rs
git commit -m "feat(mcp): scaffold ShoreMcpHandler + rmcp stdio server"
```

### Task 12: Add read-only tools — status, log, usage

**Files:**
- Modify: `shore-mcp/src/handler.rs` (add tool methods)
- Create: `shore-mcp/src/tools/mod.rs` (declare submodules)
- Create: `shore-mcp/src/tools/status.rs`
- Create: `shore-mcp/src/tools/log.rs`
- Create: `shore-mcp/src/tools/usage.rs`

The rmcp macro pattern from the spec's context7 snapshot is:

```rust
#[tool_router]
impl Server {
    #[tool(name = "adder", description = "Modular add two integers")]
    fn add(
        &self,
        Parameters(AddParameter { left, right }): Parameters<AddParameter>
    ) -> Json<AddOutput> { ... }
}
```

If rmcp's current version allows multiple `impl` blocks to contribute tools via `#[tool_router]`, we will put each tool group in its own file. If not, all tools must live in `handler.rs` in one `impl` block. **Verify before proceeding** — read rmcp's `tool_router` macro documentation once and commit to the correct layout for this and all subsequent tool tasks.

For the rest of this plan, the example code assumes multiple-impl-blocks is allowed. If it isn't, merge every `tools/*.rs` file's contents into the single `impl ShoreMcpHandler` block in `handler.rs` instead, and delete the `tools/` subdirectory. The parameter types and tool bodies remain identical either way.

- [ ] **Step 1: Declare tool submodules**

Replace stub `shore-mcp/src/tools/mod.rs`:

```rust
pub mod log;
pub mod status;
pub mod usage;
```

- [ ] **Step 2: Add a shared helper for "send a command, drain, return data"**

Append to `shore-mcp/src/handler.rs`:

```rust
use rmcp::{model::CallToolResult, ErrorData};
use serde_json::Value;
use shore_protocol::server_msg::ServerMessage;

impl ShoreMcpHandler {
    /// Check gates, send an SWP command, drain to CommandOutput, return JSON.
    pub(crate) async fn run_cmd(
        &self,
        tool_name: &str,
        swp_name: &str,
        args: Value,
    ) -> Result<Value, ErrorData> {
        // Gate.
        match crate::gating::check(tool_name, &self.gate) {
            crate::gating::GateDecision::Allow => {}
            crate::gating::GateDecision::Refuse(msg) => {
                return Err(ErrorData::invalid_params(msg, None));
            }
        }

        let mut conn = self.conn.lock().await;
        conn.send_command(swp_name, args)
            .await
            .map_err(|e| ErrorData::internal_error(format!("send_command: {e}"), None))?;

        loop {
            let msg = conn
                .recv()
                .await
                .map_err(|e| ErrorData::internal_error(format!("recv: {e}"), None))?;
            match msg {
                ServerMessage::CommandOutput(co) => return Ok(co.data),
                ServerMessage::Error(err) => {
                    return Err(ErrorData::internal_error(err.message, None));
                }
                ServerMessage::Ping(_)
                | ServerMessage::History(_)
                | ServerMessage::NewMessage(_)
                | ServerMessage::SendImage(_)
                | ServerMessage::Phase(_) => {}
                other => {
                    tracing::debug!(?other, "run_cmd: ignoring unexpected frame");
                }
            }
        }
    }

    /// Wrap a JSON Value as a successful `CallToolResult`.
    pub(crate) fn json_result(data: Value) -> Result<CallToolResult, ErrorData> {
        use rmcp::model::{Content, CallToolResult};
        let content = Content::text(
            serde_json::to_string_pretty(&data)
                .unwrap_or_else(|_| "<non-serializable>".to_string()),
        );
        Ok(CallToolResult::success(vec![content]))
    }
}
```

Notes:
- `ErrorData`, `CallToolResult`, and `Content` are rmcp types. Import paths may differ by version — check rmcp docs if these don't resolve.
- `CommandOutput` and `Error` variant names come from `shore-protocol/src/server_msg.rs` and are confirmed to exist by `shore-cli/src/run.rs:750-759`.

- [ ] **Step 3: Implement `status` tools**

Create `shore-mcp/src/tools/status.rs`:

```rust
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::handler::ShoreMcpHandler;

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct StatusParams {}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct DiagnosticsParams {
    #[serde(default = "default_count")]
    pub count: u32,
}

fn default_count() -> u32 {
    10
}

#[tool_router]
impl ShoreMcpHandler {
    #[tool(
        name = "status",
        description = "Show daemon and session status. Returns the full status JSON."
    )]
    pub async fn tool_status(
        &self,
        Parameters(_p): Parameters<StatusParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self.run_cmd("status", "status", json!({})).await?;
        Self::json_result(data)
    }

    #[tool(
        name = "status_diagnostics",
        description = "Show recent API calls, tool invocations, and errors from the daemon."
    )]
    pub async fn tool_status_diagnostics(
        &self,
        Parameters(p): Parameters<DiagnosticsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("status_diagnostics", "diagnostics", json!({ "count": p.count }))
            .await?;
        Self::json_result(data)
    }
}
```

**Caveat:** if rmcp 0.1 only allows one `#[tool_router]` per struct, this won't compile (you'll get "tool_router already defined"). In that case, move the `#[tool]` methods into `handler.rs`'s existing `impl` block and delete the second `#[tool_router]` attribute. The param struct definitions stay in `tools/status.rs`.

- [ ] **Step 4: Implement `log` tools**

Create `shore-mcp/src/tools/log.rs`:

```rust
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::handler::ShoreMcpHandler;

#[derive(Deserialize, JsonSchema, Debug)]
pub struct LogTailParams {
    /// Number of recent messages to return.
    #[serde(default = "default_tail_count")]
    pub count: u32,
}

fn default_tail_count() -> u32 {
    20
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct LogShowParams {
    /// Message reference (e.g. "last", "-1", "3").
    pub msg_ref: String,
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct LogDeleteParams {
    /// Message refs to delete.
    pub msg_refs: Vec<String>,
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct LogEditParams {
    pub msg_ref: String,
    pub content: String,
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct LogHeartbeatParams {
    #[serde(default = "default_tail_count")]
    pub count: u32,
}

#[tool_router]
impl ShoreMcpHandler {
    #[tool(
        name = "log_tail",
        description = "Return the last N messages from the conversation log."
    )]
    pub async fn tool_log_tail(
        &self,
        Parameters(p): Parameters<LogTailParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("log_tail", "log", json!({ "count": p.count }))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "log_show",
        description = "Fetch a single message by reference (last, -1, or a numeric index)."
    )]
    pub async fn tool_log_show(
        &self,
        Parameters(p): Parameters<LogShowParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("log_show", "get", json!({ "ref": p.msg_ref }))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "log_heartbeat",
        description = "Show heartbeat probe decisions and timing history for the last N messages."
    )]
    pub async fn tool_log_heartbeat(
        &self,
        Parameters(p): Parameters<LogHeartbeatParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("log_heartbeat", "heartbeat_log", json!({ "count": p.count }))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "log_delete",
        description = "Delete one or more messages from the conversation log. Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_log_delete(
        &self,
        Parameters(p): Parameters<LogDeleteParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("log_delete", "delete", json!({ "refs": p.msg_refs }))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "log_edit",
        description = "Edit the content of a single message in the conversation log. Mutating."
    )]
    pub async fn tool_log_edit(
        &self,
        Parameters(p): Parameters<LogEditParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "log_edit",
                "edit",
                json!({ "ref": p.msg_ref, "content": p.content }),
            )
            .await?;
        Self::json_result(data)
    }
}
```

- [ ] **Step 5: Implement `usage` tool**

Create `shore-mcp/src/tools/usage.rs`:

```rust
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::handler::ShoreMcpHandler;

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct UsageParams {
    /// Time period: "today", "7d", "30d", "all". Default: "today".
    #[serde(default = "default_last")]
    pub last: String,
    pub character: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub call_type: Option<String>,
    /// Group results by call type instead of filtering.
    #[serde(default)]
    pub by_call_type: bool,
    #[serde(default)]
    pub anomalies: bool,
}

fn default_last() -> String {
    "today".to_string()
}

#[tool_router]
impl ShoreMcpHandler {
    #[tool(
        name = "usage",
        description = "Token usage statistics and costs. Read-only — excludes refresh_pricing / recalculate / export_csv which are CLI-only."
    )]
    pub async fn tool_usage(
        &self,
        Parameters(p): Parameters<UsageParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "usage",
                "usage",
                json!({
                    "last": p.last,
                    "character": p.character,
                    "provider": p.provider,
                    "model": p.model,
                    "call_type": p.call_type,
                    "by_call_type": p.by_call_type,
                    "anomalies": p.anomalies,
                    "export_csv": false,
                    "export_tsv": false,
                    "refresh_pricing": false,
                    "recalculate": false,
                    "force": false,
                }),
            )
            .await?;
        Self::json_result(data)
    }
}
```

- [ ] **Step 6: Build to verify multi-`tool_router` layout**

```sh
cargo build -p shore-mcp --features enabled
```

Expected outcomes:
- Success: the layout works, move on to Step 7.
- Error about duplicate `tool_router` definitions: rmcp 0.1 requires a single `#[tool_router]`. In that case:
    1. Remove the `#[tool_router]` attribute from every file in `tools/`.
    2. Move every `#[tool]` method into the single `impl ShoreMcpHandler` block in `handler.rs` that already has `#[tool_router]`.
    3. Keep the `Parameters<...>` structs where they are (in `tools/*.rs`) and import them into `handler.rs`.
    4. Rebuild. Apply the same pattern to all future tool tasks.

- [ ] **Step 7: Commit**

```sh
git add shore-mcp/src/tools/ shore-mcp/src/handler.rs
git commit -m "feat(mcp): add status, log, usage tools"
```

### Task 13: Add character and model tools

**Files:**
- Create: `shore-mcp/src/tools/character.rs`
- Create: `shore-mcp/src/tools/model.rs`
- Modify: `shore-mcp/src/tools/mod.rs`

- [ ] **Step 1: Create `tools/character.rs`**

```rust
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::handler::ShoreMcpHandler;

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct CharacterListParams {}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct CharacterInfoParams {
    /// Character name to inspect. Empty string = currently selected.
    #[serde(default)]
    pub name: String,
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct CharacterSwitchParams {
    pub name: String,
}

#[tool_router]
impl ShoreMcpHandler {
    #[tool(
        name = "character_list",
        description = "List all available characters configured on the daemon."
    )]
    pub async fn tool_character_list(
        &self,
        Parameters(_p): Parameters<CharacterListParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("character_list", "list_characters", json!({}))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "character_info",
        description = "Detailed info about a character (prompt, model, settings)."
    )]
    pub async fn tool_character_info(
        &self,
        Parameters(p): Parameters<CharacterInfoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("character_info", "character_info", json!({ "name": p.name }))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "character_switch",
        description = "Switch the active character. Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_character_switch(
        &self,
        Parameters(p): Parameters<CharacterSwitchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "character_switch",
                "switch_character",
                json!({ "name": p.name }),
            )
            .await?;
        Self::json_result(data)
    }
}
```

- [ ] **Step 2: Create `tools/model.rs`**

```rust
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::handler::ShoreMcpHandler;

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct ModelListParams {}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct ModelInfoParams {
    pub name: Option<String>,
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct ModelSwitchParams {
    pub name: String,
}

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct ModelResetParams {}

#[tool_router]
impl ShoreMcpHandler {
    #[tool(
        name = "model_list",
        description = "List chat models in the daemon's model catalog."
    )]
    pub async fn tool_model_list(
        &self,
        Parameters(_p): Parameters<ModelListParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self.run_cmd("model_list", "list_models", json!({})).await?;
        Self::json_result(data)
    }

    #[tool(
        name = "model_info",
        description = "Detailed info about a specific model (or the currently active one if no name is given)."
    )]
    pub async fn tool_model_info(
        &self,
        Parameters(p): Parameters<ModelInfoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let args = match p.name {
            Some(n) => json!({ "name": n }),
            None => json!({}),
        };
        let data = self.run_cmd("model_info", "model_info", args).await?;
        Self::json_result(data)
    }

    #[tool(
        name = "model_switch",
        description = "Switch the active chat model. Mutating."
    )]
    pub async fn tool_model_switch(
        &self,
        Parameters(p): Parameters<ModelSwitchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("model_switch", "switch_model", json!({ "name": p.name }))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "model_reset",
        description = "Reset the active model to the config default. Mutating."
    )]
    pub async fn tool_model_reset(
        &self,
        Parameters(_p): Parameters<ModelResetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("model_reset", "reset_model", json!({}))
            .await?;
        Self::json_result(data)
    }
}
```

- [ ] **Step 3: Update `tools/mod.rs`**

```rust
pub mod character;
pub mod log;
pub mod model;
pub mod status;
pub mod usage;
```

- [ ] **Step 4: Build**

```sh
cargo build -p shore-mcp --features enabled
```

Expected: clean.

- [ ] **Step 5: Commit**

```sh
git add shore-mcp/src/tools/character.rs shore-mcp/src/tools/model.rs shore-mcp/src/tools/mod.rs
git commit -m "feat(mcp): add character and model tools"
```

### Task 14: Add memory and config tools

**Files:**
- Create: `shore-mcp/src/tools/memory.rs`
- Create: `shore-mcp/src/tools/config.rs`
- Modify: `shore-mcp/src/tools/mod.rs`

- [ ] **Step 1: Create `tools/memory.rs`**

```rust
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::handler::ShoreMcpHandler;

#[derive(Deserialize, JsonSchema, Debug)]
pub struct MemoryQueryParams {
    pub query: String,
    /// Skip the researcher and query the memory agent directly.
    #[serde(default)]
    pub direct: bool,
}

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct MemoryCompactParams {}

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct MemoryCollateParams {
    #[serde(default)]
    pub full: bool,
    pub limit: Option<u32>,
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct MemoryPurgeParams {
    pub older_than: String,
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct MemoryChangelogParams {
    #[serde(default = "default_changelog_limit")]
    pub limit: u32,
}

fn default_changelog_limit() -> u32 {
    20
}

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct MemoryReindexParams {}

#[tool_router]
impl ShoreMcpHandler {
    #[tool(
        name = "memory_query",
        description = "Query the memory system via the researcher (or directly with direct=true)."
    )]
    pub async fn tool_memory_query(
        &self,
        Parameters(p): Parameters<MemoryQueryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "memory_query",
                "memory",
                json!({ "query": p.query, "direct": p.direct }),
            )
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "memory_compact",
        description = "Trigger a memory compaction pass. Mutating."
    )]
    pub async fn tool_memory_compact(
        &self,
        Parameters(_p): Parameters<MemoryCompactParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("memory_compact", "compact", json!({ "collate": true }))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "memory_collate",
        description = "Run a memory collation pass. Mutating."
    )]
    pub async fn tool_memory_collate(
        &self,
        Parameters(p): Parameters<MemoryCollateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut args = json!({ "full": p.full });
        if let Some(l) = p.limit {
            args["limit"] = json!(l);
        }
        let data = self.run_cmd("memory_collate", "collate", args).await?;
        Self::json_result(data)
    }

    #[tool(
        name = "memory_purge",
        description = "Purge memory entries older than the given cutoff. Mutating."
    )]
    pub async fn tool_memory_purge(
        &self,
        Parameters(p): Parameters<MemoryPurgeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "memory_purge",
                "memory_purge",
                json!({ "older_than": p.older_than }),
            )
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "memory_changelog",
        description = "Recent memory changes log. Read-only."
    )]
    pub async fn tool_memory_changelog(
        &self,
        Parameters(p): Parameters<MemoryChangelogParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "memory_changelog",
                "memory_changelog",
                json!({ "limit": p.limit }),
            )
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "memory_reindex",
        description = "Rebuild memory indices. Mutating."
    )]
    pub async fn tool_memory_reindex(
        &self,
        Parameters(_p): Parameters<MemoryReindexParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("memory_reindex", "memory_reindex", json!({}))
            .await?;
        Self::json_result(data)
    }
}
```

- [ ] **Step 2: Create `tools/config.rs`**

```rust
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::handler::ShoreMcpHandler;

#[derive(Deserialize, JsonSchema, Debug)]
pub struct ConfigGetParams {
    /// Config key to get. Empty string returns the full config.
    #[serde(default)]
    pub key: String,
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct ConfigSetParams {
    pub key: String,
    pub value: String,
}

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct ConfigCheckParams {}

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct ConfigResetParams {}

#[tool_router]
impl ShoreMcpHandler {
    #[tool(
        name = "config_get",
        description = "Get a config value by key, or the full config if key is empty."
    )]
    pub async fn tool_config_get(
        &self,
        Parameters(p): Parameters<ConfigGetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "config_get",
                "config",
                json!({ "key": p.key, "value": null }),
            )
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "config_set",
        description = "Set a config value. Mutating."
    )]
    pub async fn tool_config_set(
        &self,
        Parameters(p): Parameters<ConfigSetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "config_set",
                "config",
                json!({ "key": p.key, "value": p.value }),
            )
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "config_check",
        description = "Validate configuration and return any warnings. Read-only."
    )]
    pub async fn tool_config_check(
        &self,
        Parameters(_p): Parameters<ConfigCheckParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("config_check", "config_check", json!({}))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "config_reset",
        description = "Reset runtime overrides and reload config from disk. Mutating."
    )]
    pub async fn tool_config_reset(
        &self,
        Parameters(_p): Parameters<ConfigResetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("config_reset", "config_reset", json!({}))
            .await?;
        Self::json_result(data)
    }
}
```

- [ ] **Step 3: Update `tools/mod.rs`**

```rust
pub mod character;
pub mod config;
pub mod log;
pub mod memory;
pub mod model;
pub mod status;
pub mod usage;
```

- [ ] **Step 4: Build**

```sh
cargo build -p shore-mcp --features enabled
```

Expected: clean.

- [ ] **Step 5: Commit**

```sh
git add shore-mcp/src/tools/memory.rs shore-mcp/src/tools/config.rs shore-mcp/src/tools/mod.rs
git commit -m "feat(mcp): add memory and config tools"
```

### Task 15: Add debug tools (interiority)

**Files:**
- Create: `shore-mcp/src/tools/debug.rs`
- Modify: `shore-mcp/src/tools/mod.rs`

The existing `DebugCommand` enum in `shore-cli/src/cli.rs:365-378` has three variants — all three are mutating per `shore-cli/src/cli.rs:473-477`.

- [ ] **Step 1: Create `tools/debug.rs`**

```rust
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::handler::ShoreMcpHandler;

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct DebugEmptyParams {}

#[tool_router]
impl ShoreMcpHandler {
    #[tool(
        name = "debug_tick_now",
        description = "Force an interiority tick right now. Mutating."
    )]
    pub async fn tool_debug_tick_now(
        &self,
        Parameters(_p): Parameters<DebugEmptyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("debug_tick_now", "interiority_tick_now", json!({}))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "debug_status_dormant",
        description = "Set interiority status to dormant. Mutating."
    )]
    pub async fn tool_debug_status_dormant(
        &self,
        Parameters(_p): Parameters<DebugEmptyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "debug_status_dormant",
                "interiority_set_dormant",
                json!({}),
            )
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "debug_status_active",
        description = "Set interiority status to active. Mutating."
    )]
    pub async fn tool_debug_status_active(
        &self,
        Parameters(_p): Parameters<DebugEmptyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "debug_status_active",
                "interiority_set_active",
                json!({}),
            )
            .await?;
        Self::json_result(data)
    }
}
```

- [ ] **Step 2: Update `tools/mod.rs`**

```rust
pub mod character;
pub mod config;
pub mod debug;
pub mod log;
pub mod memory;
pub mod model;
pub mod status;
pub mod usage;
```

- [ ] **Step 3: Build**

```sh
cargo build -p shore-mcp --features enabled
```

Expected: clean.

- [ ] **Step 4: Commit**

```sh
git add shore-mcp/src/tools/debug.rs shore-mcp/src/tools/mod.rs
git commit -m "feat(mcp): add debug (interiority) tools"
```

### Task 16: Add `send` and `regen` tools (streaming → collected)

**Files:**
- Create: `shore-mcp/src/tools/send.rs`
- Modify: `shore-mcp/src/tools/mod.rs`

`send` and `regen` are gated as mutating (they alter conversation state). They use `shore-client::collect_stream` (Phase 2) to return a structured aggregate.

- [ ] **Step 1: Create `tools/send.rs`**

```rust
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use shore_client::collect_stream;
use shore_protocol::client_msg::MessageOverrides;

use crate::handler::ShoreMcpHandler;

#[derive(Deserialize, JsonSchema, Debug)]
pub struct SendParams {
    /// Message text.
    pub text: String,
    /// Optional sampling temperature override for this message.
    pub temperature: Option<f64>,
    /// Optional top-p override.
    pub top_p: Option<f64>,
    /// Optional extended-thinking budget in tokens. Pass 0 to disable.
    pub thinking: Option<u32>,
    /// If true, inject as a system instruction instead of a user message.
    #[serde(default)]
    pub system: bool,
}

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct RegenParams {
    /// Optional guidance for the regeneration.
    pub guidance: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct SendOutput {
    pub text: String,
    pub finish_reason: String,
    pub model: String,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub total_ms: u64,
    pub tool_calls: usize,
    pub tool_results: usize,
}

#[tool_router]
impl ShoreMcpHandler {
    #[tool(
        name = "send",
        description = "Send a message to the active character and return the full assembled response. Mutating (alters conversation history)."
    )]
    pub async fn tool_send(
        &self,
        Parameters(p): Parameters<SendParams>,
    ) -> Result<CallToolResult, ErrorData> {
        // Gate first — send is classified as mutating.
        match crate::gating::check("send", &self.gate) {
            crate::gating::GateDecision::Allow => {}
            crate::gating::GateDecision::Refuse(msg) => {
                return Err(ErrorData::invalid_params(msg, None));
            }
        }

        let mut conn = self.conn.lock().await;

        if p.system {
            // System injection — same as shore-cli's `inject_system` command.
            conn.send_command("inject_system", json!({ "text": p.text }))
                .await
                .map_err(|e| ErrorData::internal_error(format!("send_command: {e}"), None))?;
            // Drain to CommandOutput.
            let data = drain_to_command_output(&mut conn).await?;
            return Self::json_result(data);
        }

        let overrides = if p.temperature.is_some() || p.top_p.is_some() || p.thinking.is_some() {
            Some(MessageOverrides {
                temperature: p.temperature,
                top_p: p.top_p,
                thinking_budget: p.thinking,
            })
        } else {
            None
        };

        conn.send_message_full(&p.text, true, vec![], overrides)
            .await
            .map_err(|e| ErrorData::internal_error(format!("send_message_full: {e}"), None))?;

        let resp = collect_stream(&mut conn)
            .await
            .map_err(|e| ErrorData::internal_error(format!("collect_stream: {e}"), None))?;

        let output = SendOutput {
            text: resp.text.clone(),
            finish_reason: resp.finish_reason.clone(),
            model: resp.metadata.model.clone(),
            tokens_in: resp.metadata.tokens.input,
            tokens_out: resp.metadata.tokens.output,
            total_ms: resp.metadata.timing.total_ms,
            tool_calls: resp.tool_calls.len(),
            tool_results: resp.tool_results.len(),
        };
        Self::json_result(serde_json::to_value(output).unwrap())
    }

    #[tool(
        name = "regen",
        description = "Regenerate the last assistant response, optionally with guidance. Mutating."
    )]
    pub async fn tool_regen(
        &self,
        Parameters(p): Parameters<RegenParams>,
    ) -> Result<CallToolResult, ErrorData> {
        match crate::gating::check("regen", &self.gate) {
            crate::gating::GateDecision::Allow => {}
            crate::gating::GateDecision::Refuse(msg) => {
                return Err(ErrorData::invalid_params(msg, None));
            }
        }

        let mut conn = self.conn.lock().await;
        conn.send_regen(true, p.guidance.clone())
            .await
            .map_err(|e| ErrorData::internal_error(format!("send_regen: {e}"), None))?;

        let resp = collect_stream(&mut conn)
            .await
            .map_err(|e| ErrorData::internal_error(format!("collect_stream: {e}"), None))?;

        let output = SendOutput {
            text: resp.text,
            finish_reason: resp.finish_reason,
            model: resp.metadata.model,
            tokens_in: resp.metadata.tokens.input,
            tokens_out: resp.metadata.tokens.output,
            total_ms: resp.metadata.timing.total_ms,
            tool_calls: resp.tool_calls.len(),
            tool_results: resp.tool_results.len(),
        };
        Self::json_result(serde_json::to_value(output).unwrap())
    }
}

async fn drain_to_command_output(
    conn: &mut shore_client::SWPConnection,
) -> Result<serde_json::Value, ErrorData> {
    use shore_protocol::server_msg::ServerMessage;
    loop {
        let msg = conn
            .recv()
            .await
            .map_err(|e| ErrorData::internal_error(format!("recv: {e}"), None))?;
        match msg {
            ServerMessage::CommandOutput(co) => return Ok(co.data),
            ServerMessage::Error(err) => {
                return Err(ErrorData::internal_error(err.message, None));
            }
            _ => {}
        }
    }
}
```

- [ ] **Step 2: Update `tools/mod.rs`**

```rust
pub mod character;
pub mod config;
pub mod debug;
pub mod log;
pub mod memory;
pub mod model;
pub mod send;
pub mod status;
pub mod usage;
```

- [ ] **Step 3: Build**

```sh
cargo build -p shore-mcp --features enabled
```

Expected: clean. If `MessageOverrides` import path is wrong, grep the actual path:

```sh
rg -n 'pub struct MessageOverrides' shore-protocol/src
```

And adjust the `use` statement.

- [ ] **Step 4: Run all existing tests (regression check)**

```sh
cargo test -p shore-mcp --features enabled
```

Expected: all pass.

- [ ] **Step 5: Commit**

```sh
git add shore-mcp/src/tools/send.rs shore-mcp/src/tools/mod.rs
git commit -m "feat(mcp): add send and regen tools using collect_stream"
```

### Task 17: Add `log_follow` as a bounded read

**Files:**
- Modify: `shore-mcp/src/tools/log.rs`

The spec specifies a bounded read: wait up to N seconds or until M messages arrive, then return whatever we have. Defaults: 5 seconds, 50 messages.

- [ ] **Step 1: Add params and the tool**

Append to `shore-mcp/src/tools/log.rs` (inside the file, but add the method to the `impl` block):

```rust
#[derive(Deserialize, JsonSchema, Debug)]
pub struct LogFollowParams {
    #[serde(default = "default_follow_seconds")]
    pub seconds: u64,
    #[serde(default = "default_follow_cap")]
    pub cap: u32,
}

fn default_follow_seconds() -> u64 {
    5
}

fn default_follow_cap() -> u32 {
    50
}
```

And extend the existing `#[tool_router] impl ShoreMcpHandler` block in `log.rs` with:

```rust
    #[tool(
        name = "log_follow",
        description = "Tail the log for new messages for a bounded duration. Returns whatever arrives before the timeout or message cap. Read-only."
    )]
    pub async fn tool_log_follow(
        &self,
        Parameters(p): Parameters<LogFollowParams>,
    ) -> Result<CallToolResult, ErrorData> {
        use shore_protocol::server_msg::ServerMessage;
        use std::time::{Duration, Instant};

        // Gate (read-only, but gate to keep consistency).
        match crate::gating::check("log_follow", &self.gate) {
            crate::gating::GateDecision::Allow => {}
            crate::gating::GateDecision::Refuse(msg) => {
                return Err(ErrorData::invalid_params(msg, None));
            }
        }

        let mut conn = self.conn.lock().await;
        let deadline = Instant::now() + Duration::from_secs(p.seconds);
        let mut collected: Vec<serde_json::Value> = Vec::new();

        while Instant::now() < deadline && (collected.len() as u32) < p.cap {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let recv_fut = conn.recv();
            let msg = match tokio::time::timeout(remaining, recv_fut).await {
                Ok(Ok(m)) => m,
                Ok(Err(e)) => {
                    return Err(ErrorData::internal_error(format!("recv: {e}"), None));
                }
                Err(_elapsed) => break,
            };
            match msg {
                ServerMessage::NewMessage(nm) => {
                    collected.push(serde_json::to_value(nm).unwrap_or(serde_json::Value::Null));
                }
                ServerMessage::Ping(_) | ServerMessage::History(_) | ServerMessage::Phase(_) => {}
                ServerMessage::Shutdown(_) => break,
                _ => {}
            }
        }

        Self::json_result(json!({ "messages": collected }))
    }
```

- [ ] **Step 2: Build**

```sh
cargo build -p shore-mcp --features enabled
```

Expected: clean.

- [ ] **Step 3: Commit**

```sh
git add shore-mcp/src/tools/log.rs
git commit -m "feat(mcp): add log_follow as a bounded-read tool"
```

---

## Phase 5 — Integration test + live verification

### Task 18: Write an integration test against `shore-test-harness`

**Files:**
- Create: `shore-mcp/tests/mcp_integration.rs`

This test boots a real daemon via `TestHarness`, writes the harness's daemon info into a temp `instances.json`, launches `shore-mcp` as a subprocess with `--daemon-addr` pointing at the harness, and exchanges MCP JSON-RPC frames to exercise at least three representative tools (`status`, `character_list`, `send`).

- [ ] **Step 1: Write the test**

Create `shore-mcp/tests/mcp_integration.rs`:

```rust
// Integration test: launch shore-mcp as a subprocess against a real daemon
// booted by TestHarness, speak MCP JSON-RPC over stdin/stdout, exercise a
// small representative tool set.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use shore_test_harness::{AnthropicStreamBuilder, TestHarness};

async fn send_jsonrpc(
    stdin: &mut tokio::process::ChildStdin,
    method: &str,
    id: u32,
    params: serde_json::Value,
) -> std::io::Result<()> {
    let frame = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    let line = serde_json::to_string(&frame).unwrap();
    stdin.write_all(line.as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await
}

async fn recv_jsonrpc_response(
    reader: &mut BufReader<tokio::process::ChildStdout>,
) -> std::io::Result<serde_json::Value> {
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "mcp stdout closed",
            ));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("json: {e}"))
        })?;
        // Skip notifications (no id).
        if value.get("id").is_some() {
            return Ok(value);
        }
    }
}

#[tokio::test]
async fn shore_mcp_initializes_and_calls_tools_against_real_daemon() {
    // Boot a daemon via the harness. This gives us a running SWP server
    // at `harness.addr` with a wiremock-backed mock LLM.
    let harness = TestHarness::boot().await;
    let daemon_addr = harness.addr.clone();

    // Preload a response for the `send` tool.
    harness
        .mock_llm
        .prepare_response(
            AnthropicStreamBuilder::new()
                .text("hello from mock llm")
                .usage(5, 7),
        )
        .await;

    // Locate the shore-mcp binary built by the current workspace.
    // `cargo test -p shore-mcp --features enabled` places it at
    // target/debug/shore-mcp.
    let bin = std::env::var("CARGO_BIN_EXE_shore-mcp").unwrap_or_else(|_| {
        // Fallback for when running `cargo test` without the env var.
        format!(
            "{}/target/debug/shore-mcp",
            std::env::var("CARGO_WORKSPACE_DIR").unwrap_or_else(|_| ".".into())
        )
    });

    // Launch shore-mcp with --attach-main + --daemon-addr.
    // --attach-main skips the test-profile env-var manipulation; the
    // gate will be CLOSED for writes on this profile, but we pass
    // --allow-main-writes so the `send` tool can fire.
    let mut child = Command::new(bin)
        .args([
            "--attach-main",
            "--allow-main-writes",
            "--daemon-addr",
            &daemon_addr,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn shore-mcp");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    // 1. `initialize` handshake.
    send_jsonrpc(
        &mut stdin,
        "initialize",
        1,
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "integration-test", "version": "0" }
        }),
    )
    .await
    .unwrap();
    let init_resp = tokio::time::timeout(Duration::from_secs(5), recv_jsonrpc_response(&mut reader))
        .await
        .expect("initialize timed out")
        .expect("initialize read failed");
    assert_eq!(init_resp["id"], 1);
    assert!(init_resp.get("result").is_some(), "no result in initialize response");

    // 2. `tools/list`.
    send_jsonrpc(&mut stdin, "tools/list", 2, serde_json::json!({}))
        .await
        .unwrap();
    let list_resp = tokio::time::timeout(Duration::from_secs(5), recv_jsonrpc_response(&mut reader))
        .await
        .expect("tools/list timed out")
        .expect("tools/list read failed");
    let tools = list_resp["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for required in &["status", "character_list", "send", "log_tail"] {
        assert!(
            names.contains(required),
            "missing tool `{required}` in advertised list: {names:?}"
        );
    }

    // 3. Call `status`.
    send_jsonrpc(
        &mut stdin,
        "tools/call",
        3,
        serde_json::json!({ "name": "status", "arguments": {} }),
    )
    .await
    .unwrap();
    let status_resp = tokio::time::timeout(Duration::from_secs(10), recv_jsonrpc_response(&mut reader))
        .await
        .expect("status timed out")
        .expect("status read failed");
    assert_eq!(status_resp["id"], 3);
    assert!(
        status_resp.get("error").is_none(),
        "status errored: {status_resp}"
    );

    // 4. Call `send`.
    send_jsonrpc(
        &mut stdin,
        "tools/call",
        4,
        serde_json::json!({
            "name": "send",
            "arguments": { "text": "hi there" }
        }),
    )
    .await
    .unwrap();
    let send_resp = tokio::time::timeout(Duration::from_secs(30), recv_jsonrpc_response(&mut reader))
        .await
        .expect("send timed out")
        .expect("send read failed");
    assert_eq!(send_resp["id"], 4);
    assert!(
        send_resp.get("error").is_none(),
        "send errored: {send_resp}"
    );
    // The tool result content is a serialized SendOutput; verify it contains the mock text.
    let content_text = send_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text content");
    assert!(
        content_text.contains("hello from mock llm"),
        "send output did not contain mock text: {content_text}"
    );

    // Shut down shore-mcp cleanly.
    drop(stdin);
    let _ = child.kill().await;
}
```

Notes on rmcp JSON-RPC shape:
- `initialize`, `tools/list`, and `tools/call` are MCP standard methods. If rmcp diverges from the spec at `2024-11-05`, adjust the protocol version.
- `send_jsonrpc` uses newline-delimited JSON because the rmcp stdio transport frames by newlines. If rmcp uses a different framing (Content-Length headers), the test must match — check rmcp examples before assuming.

- [ ] **Step 2: Run the test**

```sh
cargo test -p shore-mcp --features enabled --test mcp_integration -- --nocapture
```

Expected: passes. If it fails, the failure mode will tell you where to look:
- Subprocess never produces output → rmcp stdio wiring in `server.rs` is wrong; revisit Task 11.
- Subprocess produces output but no response to `initialize` → rmcp framing mismatch; inspect the raw bytes.
- `tools/list` missing tools → confirm every `#[tool]` attribute made it into the final router.
- `send` tool call errors with "refused" → the gate is closed; we passed `--allow-main-writes` so this would mean the gate logic is wrong. Check `gating::check("send", ...)` behavior under `profile_is_test=false, allow_main_writes=true`.

If you need to iterate, `--nocapture` shows the shore-mcp stderr (tracing logs) in the test output.

- [ ] **Step 3: Run the full workspace test suite**

```sh
cargo test --workspace
```

Expected: all pass, including the new integration test.

- [ ] **Step 4: Commit**

```sh
git add shore-mcp/tests/mcp_integration.rs
git commit -m "test(mcp): integration test over stdio against real test-harness daemon"
```

### Task 19: Add `.cargo/config.toml` aliases

**Files:**
- Create or modify: `.cargo/config.toml`

- [ ] **Step 1: Check for existing `.cargo/config.toml`**

```sh
ls .cargo/config.toml 2>&1 || echo "missing"
```

If missing, create it. If present, read it and merge the new aliases into the existing `[alias]` table (creating the table if it's absent).

- [ ] **Step 2: Write or merge aliases**

The final file (if created new) should contain:

```toml
[alias]
mcp = "build -p shore-mcp --features enabled"
mcp-test = "test -p shore-mcp --features enabled"
mcp-run = "run -p shore-mcp --features enabled --"
```

If the file already existed with other content, insert these three aliases into the existing `[alias]` section and leave everything else alone.

- [ ] **Step 3: Verify the aliases work**

```sh
cargo mcp
```

Expected: rebuilds shore-mcp with the feature enabled, produces a `target/debug/shore-mcp` binary.

```sh
cargo mcp-test --test gating_rules
```

Expected: runs the gating integration test.

- [ ] **Step 4: Commit**

```sh
git add .cargo/config.toml
git commit -m "chore(cargo): add shore-mcp build/test/run aliases"
```

### Task 20: Write `shore-mcp/README.md` with `.mcp.json` example

**Files:**
- Create: `shore-mcp/README.md`

- [ ] **Step 1: Write the README**

```markdown
# shore-mcp

MCP server exposing Shore's CLI surface as tools for AI clients (Claude Code, etc.). Debug-only: not included in default release builds.

## Building

```sh
cargo mcp          # or: cargo build -p shore-mcp --features enabled
cargo mcp-test     # run tests
cargo mcp-run      # run the server (will fail without an MCP client wired to stdin/stdout)
```

## Default behavior — isolated test profile

By default, `shore-mcp` runs against an isolated test profile at `$XDG_DATA_HOME/shore-mcp-test/` (or `~/.local/share/shore-mcp-test/`). If no test daemon is already running, it spawns one as a child process and attaches. The test daemon persists across `shore-mcp` restarts.

```sh
shore-mcp                  # attach to (or spawn) the test daemon
shore-mcp --ephemeral      # tempdir-backed profile, torn down on exit
shore-mcp reset            # (planned) stop the test daemon and delete the profile
```

## Attaching to the main profile

Explicit two-flag opt-in to drive your real Shore install:

```sh
shore-mcp --attach-main                      # read-only tools only
shore-mcp --attach-main --allow-main-writes  # everything, including send/regen/config-set
```

## Example `.mcp.json` fragment (Claude Code)

```json
{
  "mcpServers": {
    "shore": {
      "command": "/absolute/path/to/target/debug/shore-mcp",
      "args": [],
      "env": {}
    },
    "shore-main-readonly": {
      "command": "/absolute/path/to/target/debug/shore-mcp",
      "args": ["--attach-main"],
      "env": {}
    }
  }
}
```

Paste the first entry into your Claude Code MCP config to give Claude Code access to the isolated test profile. Paste the second (optionally renamed) for occasional read-only inspection of your real Shore install.

## Tool surface

Read-only: `status`, `status_diagnostics`, `log_tail`, `log_show`, `log_heartbeat`, `log_follow`, `usage`, `config_get`, `config_check`, `character_list`, `character_info`, `model_list`, `model_info`, `memory_query`, `memory_changelog`.

Mutating (gated on main profile): `send`, `regen`, `log_delete`, `log_edit`, `config_set`, `config_reset`, `character_switch`, `model_switch`, `model_reset`, `memory_compact`, `memory_collate`, `memory_purge`, `memory_reindex`, `debug_tick_now`, `debug_status_dormant`, `debug_status_active`.

## Debug-only build enforcement

`shore-mcp` is excluded from the default `cargo build --workspace --release` output. It is produced only when both:

1. The `enabled` feature is passed (`--features enabled`), which pulls in the `rmcp` and `schemars` dependencies.
2. `debug_assertions` is on (default for `dev` and `test` profiles; off for `release` unless a custom profile re-enables them).

If both conditions are met but the binary is invoked in a release build, it prints a "debug-only" message and exits with code 1. This is intentional belt-and-suspenders: even a custom-profile release user has to know what they're asking for.
```

- [ ] **Step 2: Commit**

```sh
git add shore-mcp/README.md
git commit -m "docs(mcp): add README with usage, .mcp.json example, and tool surface"
```

### Task 21: Update `docs/ARCHITECTURE.md` with shore-mcp

**Files:**
- Modify: `docs/ARCHITECTURE.md`

- [ ] **Step 1: Read the existing file structure**

```sh
rg -n '^## |^# ' docs/ARCHITECTURE.md
```

Note the section headings — specifically where crates are listed. There is likely a section called "Crates" or "Workspace Layout" or similar.

- [ ] **Step 2: Add a `shore-mcp` paragraph to the crate listing**

Insert (alphabetically or at the logical position in the existing crate list):

```markdown
### `shore-mcp`

Debug-only MCP (Model Context Protocol) server exposing Shore's CLI surface as MCP tools for AI clients. Structurally parallel to `shore-cli`: both are thin clients of `shore-daemon`, both depend on `shore-client` for the SWP wire, and both translate their own input model (clap vs MCP tool calls) into the same daemon command vocabulary. Not included in default release builds — gated behind a `feature = "enabled"` + `cfg(debug_assertions)` + `required-features` on the binary. See `shore-mcp/README.md` for build and usage details, and `docs/superpowers/specs/2026-04-14-shore-mcp-server-design.md` for the design.
```

- [ ] **Step 3: If the document has a crate-dependency graph, add `shore-mcp` as a peer of `shore-cli`**

If there's an ASCII or Mermaid diagram showing crate relationships, insert `shore-mcp` at the same level as `shore-cli` with arrows to `shore-client`, `shore-config`, `shore-protocol`.

- [ ] **Step 4: Commit**

```sh
git add docs/ARCHITECTURE.md
git commit -m "docs(arch): add shore-mcp crate to the architecture overview"
```

### Task 22: Manual live verification (the mandatory gate)

**No file changes.** This task is the human gate from the spec: the plan is not complete until this passes.

- [ ] **Step 1: Build the binary**

```sh
cargo mcp
```

Expected: `target/debug/shore-mcp` exists.

- [ ] **Step 2: Build shore-daemon so it can be spawned by shore-mcp**

```sh
cargo build -p shore-daemon
export SHORE_DAEMON_BIN="$PWD/target/debug/shore-daemon"
```

Export the explicit path so `shore-mcp`'s `spawn_and_attach_test_daemon` can find the binary regardless of PATH.

- [ ] **Step 3: Add `shore-mcp` to Claude Code's MCP config**

Edit your Claude Code MCP config (typically `~/.config/claude-code/mcp.json` or equivalent — check Claude Code's docs) and add the test-profile entry from `shore-mcp/README.md`. Set `SHORE_DAEMON_BIN` in the `env` field so the spawned daemon can be located.

- [ ] **Step 4: Restart Claude Code and verify the tool list**

In a new Claude Code session, type something like "what MCP tools do you have for Shore?" The listing should include every tool in `shore-mcp/src/tools/` (~23 tools). If any are missing, verify the `#[tool]` macros all made it through the `#[tool_router]` aggregator.

- [ ] **Step 5: Exercise representative tools**

In Claude Code, call each of:

- `status` — should return daemon status JSON.
- `character_list` — should list configured characters.
- `model_list` — should list available chat models.
- `send` with a short message — should hit the real LLM via the spawned test daemon, return the response text. **This is the load-bearing live verification.** If this works end-to-end, the stack is functioning.
- `log_tail` — should show the message from the previous `send` call in the history.

- [ ] **Step 6: Verify gate behavior**

Kill Claude Code. Restart with the `shore-main-readonly` entry (or launch shore-mcp manually with `--attach-main` and no `--allow-main-writes`). Try calling `config_set` — it should return a gate-refusal error. Try `status` — it should work fine.

- [ ] **Step 7: Clean up the test daemon**

```sh
pgrep -af shore-daemon
```

Verify the spawned test daemon is running. Kill it manually for now (`shore-mcp reset` is planned but out of scope):

```sh
pkill -f 'shore-daemon.*shore-mcp-test'
```

- [ ] **Step 8: Document anything surprising in `docs/QUIRKS.md`**

If anything during the live test was surprising or non-obvious (rmcp framing quirk, daemon spawn timing issue, env var propagation gotcha, tool result shape mismatch, etc.), record it in `docs/QUIRKS.md` following the existing entry format. Commit separately.

```sh
git add docs/QUIRKS.md
git commit -m "docs(quirks): record $TOPIC encountered during shore-mcp live verification"
```

- [ ] **Step 9: Merge readiness**

With the live verification pass documented (in the PR description, or in a committed note), the feature is ready for merge. Before finishing:

```sh
cargo check --workspace
cargo test --workspace
cargo build --workspace --release
```

Expected: all three commands clean. The release build should NOT produce a `shore-mcp` binary — verify:

```sh
ls target/release/shore-mcp 2>&1 || echo "no release binary — correct"
```

---

## Summary

- **Phase 1 (1 task)**: Add `shore-daemon --instance-id` flag.
- **Phase 2 (2 tasks)**: Add `shore-client::collect_stream` + types.
- **Phase 3 (2 tasks)**: Update CLAUDE.md testing policy, record DECISIONS.md entries.
- **Phase 4 (12 tasks)**: Scaffold `shore-mcp` crate, implement profile resolution, implement gating, scaffold rmcp server, add tool files grouped by CLI category.
- **Phase 5 (5 tasks)**: Integration test, cargo aliases, README, ARCHITECTURE.md, manual live verification.

Each commit represents one reviewable unit. Each phase has an independent verification step before the next phase begins.
