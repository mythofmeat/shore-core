# Unify Cache Anomaly Detection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the duplicate stream-level cache invalidation detector, unify on `CacheTracker`, remove the false-positive `UnexpectedRead` anomaly, and downgrade desktop notification urgency.

**Architecture:** The `CacheTracker` state machine in `shore-ledger` becomes the sole cache anomaly detector. The stream-level `check_cache_invalidation` + `CacheContext` in `shore-llm-client` are deleted. The `is_first_after_restart` and `has_seen_cache_read` atomic flags in the daemon are removed (they existed only for `CacheContext`). Desktop notifications downgraded from critical to normal urgency.

**Tech Stack:** Rust, shore workspace crates

---

## File Map

| File | Action | Responsibility |
|---|---|---|
| `shore-ledger/src/cache_tracker.rs` | Modify | Remove `UnexpectedRead`, update tests |
| `shore-llm-client/src/cache_forensics.rs` | Modify | Downgrade `--urgency=critical` → `--urgency=normal` |
| `shore-llm-client/src/stream.rs` | Modify | Remove `CacheContext`, `check_cache_invalidation`, update `consume()` signature, update tests |
| `shore-daemon/src/engine/tools.rs` | Modify | Remove `CacheContext` from `run_tool_loop` signature and call sites |
| `shore-daemon/src/handler/generation.rs` | Modify | Remove `CacheContext` construction and `has_seen_cache_read` update |
| `shore-daemon/src/handler/mod.rs` | Modify | Remove `has_seen_cache_read`, `is_first_after_restart` fields, `CacheContext` import, cache context construction |
| `shore-daemon/src/main.rs` | Modify | Remove `has_seen_cache_read` and `is_first_after_restart` Arc creation |
| `shore-config/src/app.rs` | Modify | Remove `cache_invalidation_warnings` field from `AdvancedConfig` |
| `shore-config/src/lib.rs` | Modify | Remove `cache_invalidation_warnings` from test fixtures |
| `shore-daemon/tests/e2e.rs` | Modify | Remove `has_seen_cache_read` from test setup |
| `shore-test-harness/src/harness.rs` | Modify | Remove `has_seen_cache_read` from harness setup |
| `examples/config.toml` | Modify | Remove `cache_invalidation_warnings` line |
| `shore-ledger/src/client.rs` | Modify | Remove `UnexpectedRead` match arm |

---

### Task 1: Remove `UnexpectedRead` from CacheTracker

**Files:**
- Modify: `shore-ledger/src/cache_tracker.rs`
- Modify: `shore-ledger/src/client.rs:86-89`

- [ ] **Step 1: Update `Anomaly` enum — remove `UnexpectedRead`**

In `shore-ledger/src/cache_tracker.rs`, remove the `UnexpectedRead` variant:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anomaly {
    UnexpectedWrite,
    /// The cache was Warm, TTL expired (→ Cold), and the next call was NOT a
    /// keepalive — meaning the keepalive system failed to bridge the gap.
    KeepaliveMiss,
}
```

- [ ] **Step 2: Update Cold branch in `observe()` — no anomaly on cache read**

Replace the `CacheState::Cold` match arm (lines 185-197):

```rust
CacheState::Cold => {
    if obs.cache_read_tokens > 0 || obs.cache_write_tokens > 0 {
        self.state = CacheState::Warm;
    }
    None
}
```

- [ ] **Step 3: Remove `UnexpectedRead` match arm from `client.rs`**

In `shore-ledger/src/client.rs`, remove the `Anomaly::UnexpectedRead` arm from the anomaly string mapping (line 87):

```rust
let anomaly_str = result.anomaly.map(|a| match a {
    Anomaly::UnexpectedWrite => "unexpected_write",
    Anomaly::KeepaliveMiss => "keepalive_miss",
});
```

- [ ] **Step 4: Update tests — remove `UnexpectedRead` test, fix `unexpected_read_does_not_repeat`**

In `shore-ledger/src/cache_tracker.rs`, replace the `cold_anomaly_on_unexpected_cache_read` test:

```rust
#[test]
fn cold_to_warm_on_cache_read() {
    let mut tracker = CacheTracker::new();
    let result = tracker.observe(&Observation {
        ts: "2026-04-05T12:00:00Z".into(),
        model: "claude-opus-4-6".into(),
        thinking_enabled: true,
        cache_read_tokens: 500,
        cache_write_tokens: 0,
        call_type: "message".into(),
    });
    assert!(result.anomaly.is_none());
    assert_eq!(
        tracker.state(),
        CacheState::Warm,
        "Cold + cache_read > 0 must transition to Warm"
    );
}
```

Replace the `unexpected_read_does_not_repeat_on_subsequent_calls` test:

```rust
#[test]
fn cold_to_warm_no_anomaly_on_subsequent_calls() {
    let mut tracker = CacheTracker::new();
    // First call: Cold + cache hit → no anomaly, transitions to Warm.
    let r1 = tracker.observe(&Observation {
        ts: "2026-04-05T12:00:00Z".into(),
        model: "claude-opus-4-6".into(),
        thinking_enabled: true,
        cache_read_tokens: 500,
        cache_write_tokens: 100,
        call_type: "message".into(),
    });
    assert!(r1.anomaly.is_none());
    assert_eq!(tracker.state(), CacheState::Warm);

    // Second call: Warm + increasing cache_read → no anomaly.
    let r2 = tracker.observe(&Observation {
        ts: "2026-04-05T12:00:30Z".into(),
        model: "claude-opus-4-6".into(),
        thinking_enabled: true,
        cache_read_tokens: 600,
        cache_write_tokens: 50,
        call_type: "message".into(),
    });
    assert!(r2.anomaly.is_none());
    assert_eq!(tracker.state(), CacheState::Warm);
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p shore-ledger`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add shore-ledger/src/cache_tracker.rs shore-ledger/src/client.rs
git commit -m "refactor: remove Anomaly::UnexpectedRead from CacheTracker

A cache read when the tracker thinks state is Cold is not an anomaly —
it means the tracker's internal state was stale (daemon restart, TTL
reconstruct, OpenRouter routing). Only UnexpectedWrite and KeepaliveMiss
remain as real anomalies."
```

---

### Task 2: Downgrade notification urgency

**Files:**
- Modify: `shore-llm-client/src/cache_forensics.rs:149`

- [ ] **Step 1: Change `--urgency=critical` to `--urgency=normal`**

In `shore-llm-client/src/cache_forensics.rs`, line 149:

```rust
    let _ = std::process::Command::new("notify-send")
        .args(["--urgency=normal", "--app-name=shore", &summary, &body])
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p shore-llm-client`
Expected: All tests pass.

- [ ] **Step 3: Commit**

```bash
git add shore-llm-client/src/cache_forensics.rs
git commit -m "fix: downgrade cache anomaly notification urgency to normal"
```

---

### Task 3: Remove `CacheContext` and `check_cache_invalidation` from stream.rs

**Files:**
- Modify: `shore-llm-client/src/stream.rs`

- [ ] **Step 1: Remove `CacheContext` struct and `Default` impl (lines 14-44)**

Delete the entire `CacheContext` struct and its `Default` impl.

- [ ] **Step 2: Remove the `cache_ctx` parameter from `consume()`**

Change the signature from:

```rust
pub async fn consume(
    &self,
    reader: &mut BufReader<impl AsyncRead + Unpin>,
    regen: bool,
    cache_ctx: &CacheContext,
) -> Result<StreamResult, LlmError> {
```

To:

```rust
pub async fn consume(
    &self,
    reader: &mut BufReader<impl AsyncRead + Unpin>,
    regen: bool,
) -> Result<StreamResult, LlmError> {
```

- [ ] **Step 3: Remove the `check_cache_invalidation` call inside `consume()`**

At the end of the `consume` method (around line 243), delete:

```rust
                    // Check for cache invalidation (section 13.3).
                    check_cache_invalidation(&self.push_tx, cache_ctx, &usage);
```

- [ ] **Step 4: Remove `check_cache_invalidation` function (lines 260-310)**

Delete the entire function and its doc comment.

- [ ] **Step 5: Remove unused imports**

Update the import at line 1 — remove `CacheWarning`:

```rust
use shore_protocol::server_msg::{
    ServerMessage, StreamChunk, StreamEnd, StreamStart,
};
```

Remove `error` from the tracing import (line 7), if it's no longer used elsewhere in the file:

```rust
use tracing::{debug, info};
```

- [ ] **Step 6: Remove the `CacheContext` re-export from the `stream` module**

Check if `stream.rs` is a module file. The `CacheContext` was imported from `shore_llm_client::stream::CacheContext` by external crates — removing the struct removes the export automatically.

- [ ] **Step 7: Update all tests in stream.rs — remove `CacheContext` from `consume()` calls**

For every test that calls `consumer.consume(&mut reader, false, &ctx)` or `consumer.consume(&mut reader, true, &ctx)`:
- Remove the `let ctx = CacheContext { ... };` or `let ctx = CacheContext::default();` line
- Change `consumer.consume(&mut reader, false, &ctx)` to `consumer.consume(&mut reader, false)`
- Change `consumer.consume(&mut reader, true, &ctx)` to `consumer.consume(&mut reader, true)`

Tests affected (by function name):
- `consume_simple_stream` — remove `ctx` construction (lines 335-341), update call (line 359)
- `consume_regen_flag` — remove `ctx` (line 416), update call
- `consume_tool_use_stream` — remove `ctx` (line 476), update call
- `consume_thinking_stream` — remove `ctx` (line 519), update call
- `consume_thinking_with_redacted_flag` — remove `ctx` (line 568), update call
- `empty_stream_returns_error` — remove `ctx` (line 597), update call
- `stream_consumer_propagates_regen` — remove `ctx` (line 623), update call
- `consume_multi_text_blocks` — remove `ctx` (line 656), update call
- `consume_unknown_event_types_skipped` — remove `ctx` (line 692), update call
- `consume_no_model_in_start_returns_empty_string` — remove `ctx` (line 732), update call
- `consume_empty_content_blocks` — remove `ctx` (line 762), update call
- `malformed_json_mid_stream` — remove `ctx` (line 942), update call (line 953)
- `thinking_signature_without_thinking_text` — remove `ctx` (line 971), update call (line 988)
- `broadcast_channel_no_receivers` — remove `ctx` (line 1007), update call (line 1024)

- [ ] **Step 8: Delete the entire cache invalidation test section (lines 785-936)**

Delete these tests entirely:
- `cache_invalidation_triggers_warning`
- `cache_invalidation_skips_first_turn`
- `cache_invalidation_skips_after_restart`
- `cache_invalidation_skips_after_compaction`
- `cache_invalidation_respects_config_disabled`
- `cache_invalidation_no_warning_when_cache_hit`

- [ ] **Step 9: Run tests**

Run: `cargo test -p shore-llm-client`
Expected: All tests pass.

- [ ] **Step 10: Commit**

```bash
git add shore-llm-client/src/stream.rs
git commit -m "refactor: remove CacheContext and check_cache_invalidation from stream

The duplicate stream-level cache invalidation detector is removed.
CacheTracker in shore-ledger is now the sole anomaly detector."
```

---

### Task 4: Remove `CacheContext` from daemon engine/tools

**Files:**
- Modify: `shore-daemon/src/engine/tools.rs`

- [ ] **Step 1: Remove `CacheContext` from `run_tool_loop` signature**

Remove the `cache_ctx: &CacheContext` parameter (line 55) and the `CacheContext` import (line 10):

```rust
use shore_llm_client::stream::StreamConsumer;
```

```rust
pub async fn run_tool_loop(
    client: &LedgerClient,
    push_tx: &broadcast::Sender<ServerMessage>,
    request: &mut LlmRequest,
    mut result: StreamResult,
    ctx: &dyn ToolContext,
    max_iterations: u32,
    diag: &Arc<Mutex<Diagnostics>>,
    character: &str,
    thinking_enabled: bool,
) -> Result<ToolLoopResult, ToolLoopError> {
```

- [ ] **Step 2: Update `consume()` call in the tool loop (line 266)**

Change:

```rust
            .consume(ledger_stream.reader_mut(), false, cache_ctx)
```

To:

```rust
            .consume(ledger_stream.reader_mut(), false)
```

- [ ] **Step 3: Update all test call sites in tools.rs**

Search for `CacheContext` in `shore-daemon/src/engine/tools.rs`. Each test that constructs a `CacheContext::default()` and passes it to `run_tool_loop` needs:
- Remove the `let cache_ctx = CacheContext::default();` line
- Remove `&cache_ctx,` from the `run_tool_loop(...)` call

Affected test sites (approximate lines): 408, 451, 544, 591, 658, 702.

- [ ] **Step 4: Run tests**

Run: `cargo test -p shore-daemon`
Expected: All tests pass (or at least compile — some daemon tests may need the full e2e fixture).

- [ ] **Step 5: Commit**

```bash
git add shore-daemon/src/engine/tools.rs
git commit -m "refactor: remove CacheContext from run_tool_loop signature"
```

---

### Task 5: Remove `CacheContext` from daemon handler and generation

**Files:**
- Modify: `shore-daemon/src/handler/mod.rs`
- Modify: `shore-daemon/src/handler/generation.rs`
- Modify: `shore-daemon/src/main.rs`
- Modify: `shore-daemon/tests/e2e.rs`
- Modify: `shore-test-harness/src/harness.rs`

- [ ] **Step 1: Remove fields from `GenContext` (handler/mod.rs)**

Remove `is_first_after_restart` and `has_seen_cache_read` from the `GenContext` struct (lines 125, 127):

```rust
struct GenContext {
    registry: Arc<Mutex<CharacterRegistry>>,
    llm_client: LedgerClient,
    push_tx: broadcast::Sender<ServerMessage>,
    autonomy: AutonomyManager,
    /// Set by the compaction task after a successful compaction.
    compaction_occurred: Arc<std::sync::atomic::AtomicBool>,
    /// Accumulated token counts (shared with CommandContext for status display).
    session_tokens: Arc<std::sync::Mutex<SessionTokens>>,
    /// In-memory diagnostics ring buffers.
    diagnostics: Arc<std::sync::Mutex<shore_diagnostics::Diagnostics>>,
    /// Push notification service.
    notifier: NotificationService,
}
```

- [ ] **Step 2: Remove fields from `MessageHandler` (handler/mod.rs)**

Remove `is_first_after_restart` and `has_seen_cache_read` from `MessageHandler` (lines 160-161):

```rust
pub struct MessageHandler {
    pub registry: Arc<Mutex<CharacterRegistry>>,
    pub cmd_ctx: CommandContext,
    pub llm_client: LedgerClient,
    pub push_tx: broadcast::Sender<ServerMessage>,
    pub compaction_occurred: Arc<std::sync::atomic::AtomicBool>,
    pub autonomy: AutonomyManager,
    pub notifier: NotificationService,
    pub generation_handle: Option<tokio::task::JoinHandle<()>>,
}
```

- [ ] **Step 3: Remove from `gen_context()` builder (handler/mod.rs:307-319)**

Remove the two lines from `gen_context()`:

```rust
fn gen_context(&self) -> GenContext {
    GenContext {
        registry: self.registry.clone(),
        llm_client: self.llm_client.clone(),
        push_tx: self.push_tx.clone(),
        autonomy: self.autonomy.clone(),
        compaction_occurred: self.compaction_occurred.clone(),
        session_tokens: self.cmd_ctx.session_tokens.clone(),
        diagnostics: self.cmd_ctx.diagnostics.clone(),
        notifier: self.notifier.clone(),
    }
}
```

- [ ] **Step 4: Remove cache context construction block in handler/mod.rs (lines 694-705)**

Delete the entire block that builds `CacheContext` for the tool loop and the `is_first_after_restart.store(false)` call. Replace with just passing to `run_tool_phase` without `cache_ctx`:

Remove:
```rust
    // Build cache context for tool loop.
    let tool_cache_warnings = matches!(resolved.sdk, Sdk::Anthropic)
        && effective_config.app.advanced.cache_invalidation_warnings;
    let cache_ctx = CacheContext {
        conversation_turn_count: engine_arc.lock().await.messages().len(),
        is_first_after_restart: ctx.is_first_after_restart.load(Ordering::Acquire),
        is_first_after_compaction: false,
        cache_invalidation_warnings: tool_cache_warnings,
        has_seen_cache_read: ctx.has_seen_cache_read.load(Ordering::Acquire),
    };

    ctx.is_first_after_restart.store(false, Ordering::Release);
```

- [ ] **Step 5: Update `run_tool_phase` call (handler/mod.rs) — remove `&cache_ctx` argument**

In the `run_tool_phase(...)` call, remove the `&cache_ctx` argument.

- [ ] **Step 6: Update `run_tool_phase` signature (handler/generation.rs:137-149)**

Remove `cache_ctx: &CacheContext` parameter. Update the `#[instrument(skip(...))]` attribute to remove `cache_ctx`.

- [ ] **Step 7: Remove `CacheContext` construction from `stream_with_retry` (handler/generation.rs:62-71)**

Delete the `cache_warnings`/`cache_ctx` block. Update the `consumer.consume(...)` call to remove `&cache_ctx`.

- [ ] **Step 8: Remove `has_seen_cache_read` update (handler/generation.rs:91-92)**

Delete:
```rust
                if r.usage.cache_read_tokens > 0 {
                    ctx.has_seen_cache_read.store(true, Ordering::Release);
                }
```

- [ ] **Step 9: Remove unused imports from handler/mod.rs and generation.rs**

In `handler/mod.rs` (line 52), remove:
```rust
use shore_llm_client::stream::CacheContext;
```

Also remove `Sdk` import if no longer used (check: it's used for `matches!(resolved.sdk, Sdk::Anthropic)` in the tool cache warnings block which we deleted, but it may also be used in generation.rs).

In `handler/generation.rs` (line 19), change:
```rust
use shore_llm_client::stream::{CacheContext, StreamConsumer};
```
To:
```rust
use shore_llm_client::stream::StreamConsumer;
```

Remove unused `Ordering` imports if applicable (check that `compaction_occurred` still uses them).

- [ ] **Step 10: Update `run_tool_phase` call inside `generation.rs`**

The `run_tool_phase(...)` call passes `cache_ctx` — remove that argument.

- [ ] **Step 11: Remove from `main.rs` (lines 181-184, 216)**

Remove:
```rust
    let has_seen_cache_read = Arc::new(AtomicBool::new(false));
```

And remove `has_seen_cache_read,` from the `MessageHandler` construction.

Also remove:
```rust
    let is_first_after_restart = Arc::new(AtomicBool::new(true));
```

And remove `is_first_after_restart,` from the `MessageHandler` construction.

Clean up unused `AtomicBool` import if `compaction_occurred` is the only remaining user (it uses `Arc<std::sync::atomic::AtomicBool>` inline — check).

- [ ] **Step 12: Remove from e2e tests (shore-daemon/tests/e2e.rs)**

Remove `has_seen_cache_read` and `is_first_after_restart` from both `MessageHandler` constructions (around lines 238-239 and 721-722).

- [ ] **Step 13: Remove from test harness (shore-test-harness/src/harness.rs:179-180)**

Remove `has_seen_cache_read` and `is_first_after_restart` from the `MessageHandler` construction.

- [ ] **Step 14: Run tests**

Run: `cargo test -p shore-daemon`
Expected: All tests pass.

- [ ] **Step 15: Commit**

```bash
git add shore-daemon/src/handler/mod.rs shore-daemon/src/handler/generation.rs shore-daemon/src/main.rs shore-daemon/tests/e2e.rs shore-test-harness/src/harness.rs
git commit -m "refactor: remove CacheContext plumbing from daemon handler/generation

Remove is_first_after_restart and has_seen_cache_read atomic flags.
These existed solely for the now-removed stream-level cache invalidation
detector. CacheTracker in shore-ledger handles all anomaly detection."
```

---

### Task 6: Remove `cache_invalidation_warnings` config key

**Files:**
- Modify: `shore-config/src/app.rs`
- Modify: `shore-config/src/lib.rs`
- Modify: `examples/config.toml`

- [ ] **Step 1: Remove field from `AdvancedConfig` (app.rs:683-685)**

Remove:
```rust
    /// Warn when prompt cache is unexpectedly invalidated (§13.3).
    #[serde(default = "default_true")]
    pub cache_invalidation_warnings: bool,
```

- [ ] **Step 2: Remove from `Default` impl (app.rs:710)**

Remove:
```rust
            cache_invalidation_warnings: true,
```

- [ ] **Step 3: Remove from defaults test (app.rs:740)**

Remove:
```rust
        assert!(config.advanced.cache_invalidation_warnings);
```

- [ ] **Step 4: Remove from config parsing test (lib.rs:613, 637)**

In the test fixture string, remove:
```
cache_invalidation_warnings = false
```

Remove the assertion:
```rust
        assert!(!loaded.app.advanced.cache_invalidation_warnings);
```

- [ ] **Step 5: Remove from defaults test (lib.rs:658)**

Remove:
```rust
        assert!(loaded.app.advanced.cache_invalidation_warnings);
```

- [ ] **Step 6: Remove from example config (examples/config.toml:135)**

Remove:
```toml
# cache_invalidation_warnings = true
```

- [ ] **Step 7: Run tests**

Run: `cargo test -p shore-config`
Expected: All tests pass.

**Note:** Because `AdvancedConfig` uses `#[serde(deny_unknown_fields)]`, removing the field means existing user configs with `cache_invalidation_warnings = true` will fail to parse. This is intentional — it surfaces the removal clearly. If backward compat is preferred, we could add `#[serde(skip)]` instead, but a clean break is simpler.

- [ ] **Step 8: Commit**

```bash
git add shore-config/src/app.rs shore-config/src/lib.rs examples/config.toml
git commit -m "refactor: remove cache_invalidation_warnings config key

No longer needed — CacheTracker in shore-ledger handles all cache
anomaly detection unconditionally when forensics is enabled."
```

---

### Task 7: Full workspace build and test

- [ ] **Step 1: Build the entire workspace**

Run: `cargo build --workspace`
Expected: Clean build with no errors.

- [ ] **Step 2: Run all workspace tests**

Run: `cargo test --workspace`
Expected: All tests pass.

- [ ] **Step 3: Fix any remaining compilation errors**

If any crate has a lingering `CacheContext` reference, `cache_invalidation_warnings` access, or `UnexpectedRead` match arm, fix it.

- [ ] **Step 4: Commit any fixes**

```bash
git add -A
git commit -m "fix: resolve remaining compilation issues from cache unification"
```
