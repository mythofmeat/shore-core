# Integration Test Harness Design

## Problem

Shore has ~96 test functions, but none have ever caught a regression. Every production bug found in git history (phantom pings, infinite compaction loops, lagged client disconnects, message hangs after restart, tool ID collisions, compaction corruption, debug code shipping API keys) was discovered manually. Tests verify constructors, serialization, and shallow `.is_ok()` assertions — not the plumbing where bugs actually live.

## Goal

A test harness that boots a real daemon, connects a real SWP client, routes through real handlers, and exercises real persistence — mocking only at the HTTP boundary where requests leave the process. Tests must be fast, free (no API calls), deterministic, and able to catch the classes of bugs found in git history.

## Architecture

```
SWPConnection ──unix socket──▶ Server ──▶ MessageHandler ──▶ LlmClient ──reqwest──▶ MockServer
     ▲                                         │                                       │
     │                                    real persistence                        canned SSE
     │                                    real tool dispatch                      responses
     └─────────────────────────────────────────┘
```

### What's Real

- Daemon boots in-process (same pattern as existing e2e.rs)
- SWP connection over Unix socket in a temp directory
- MessageHandler dispatch, prompt assembly, tool execution
- JSONL message persistence, SQLite ledger, diagnostics
- Autonomy manager, interiority ticks, cache keepalive timers
- CharacterRegistry, character loading from disk

### What's Mocked

- HTTP responses from LLM providers — a `wiremock::MockServer` running on localhost
- All provider `base_url` fields point at the mock server (every provider already supports `base_url` override via config, no code changes needed)
- Time (via `tokio::time::pause()`) for autonomy/keepalive tests

## Components

### 1. `MockLlmServer`

Wraps `wiremock::MockServer`. Provides helpers to enqueue canned responses.

```rust
struct MockLlmServer {
    server: wiremock::MockServer,
}

impl MockLlmServer {
    async fn start() -> Self;

    /// Base URL to inject into model config
    fn base_url(&self) -> String;

    /// Enqueue a simple text response as an Anthropic SSE stream
    fn enqueue_text_response(&self, text: &str);

    /// Enqueue a response that includes tool use
    fn enqueue_tool_use_response(&self, tool_name: &str, tool_input: serde_json::Value);

    /// Enqueue a streaming response with multiple content blocks
    fn enqueue_streaming_response(&self, events: Vec<SseEvent>);

    /// Enqueue an error response (rate limit, server error, etc.)
    fn enqueue_error(&self, status: u16, body: &str);

    /// Enqueue a response that hangs (never sends EOF) for timeout testing
    fn enqueue_hanging_response(&self);

    /// Assert exactly N requests were made
    fn assert_request_count(&self, n: usize);

    /// Get the Nth request body for inspection
    fn get_request(&self, n: usize) -> RecordedRequest;
}
```

**SSE response format**: Canned Anthropic-format SSE streams. The mock returns `message_start`, `content_block_start`, `content_block_delta` (text or tool_use), `content_block_stop`, `message_delta`, `message_stop` events with realistic structure. A builder makes this easy:

```rust
MockLlmServer::anthropic_stream()
    .text("Hello from the test!")
    .usage(input: 50, output: 12)
    .build()  // → Vec<SseEvent>
```

### 2. `TestHarness`

Boots the full daemon stack, wires in the mock server, provides a client.

```rust
struct TestHarness {
    conn: SWPConnection,
    mock_llm: MockLlmServer,
    tmp_dir: TempDir,
    shutdown_tx: watch::Sender<()>,
    // handles for joining
    server_handle: JoinHandle<()>,
    handler_handle: JoinHandle<()>,
}

impl TestHarness {
    /// Boot daemon with default test character, connect client
    async fn boot() -> Self;

    /// Boot with custom config builder
    async fn boot_with(config: TestConfigBuilder) -> Self;

    /// Send a user message and collect the full streamed response
    async fn send_and_collect(&mut self, text: &str) -> CollectedResponse;

    /// Send a user message, don't wait for response
    async fn send(&mut self, text: &str);

    /// Receive the next SWP message (with timeout)
    async fn recv(&mut self) -> Option<ServerMsg>;

    /// Receive all messages until StreamEnd (with timeout)
    async fn collect_stream(&mut self) -> CollectedResponse;

    /// Send a slash command
    async fn send_command(&mut self, cmd: &str) -> Vec<ServerMsg>;

    /// Read the persisted JSONL for the active conversation
    fn read_persisted_messages(&self) -> Vec<serde_json::Value>;

    /// Read the SQLite ledger
    fn read_ledger(&self) -> Vec<LedgerEntry>;

    /// Access the mock server for enqueuing responses or assertions
    fn mock(&self) -> &MockLlmServer;

    /// Path to data directory
    fn data_dir(&self) -> &Path;

    /// Graceful shutdown
    async fn shutdown(self);
}
```

### 3. `CollectedResponse`

Aggregated result from a streamed LLM response.

```rust
struct CollectedResponse {
    /// Full assembled text
    text: String,
    /// Tool calls made during this response
    tool_calls: Vec<ToolCall>,
    /// Token usage from StreamEnd
    usage: Option<TokenUsage>,
    /// All raw SWP messages received
    raw_messages: Vec<ServerMsg>,
    /// Message ID assigned by daemon
    msg_id: Option<String>,
}
```

### 4. `TestConfigBuilder`

Customize the test setup when defaults aren't enough.

```rust
struct TestConfigBuilder {
    character_name: String,
    character_definition: String,
    tools_enabled: Vec<String>,
    autonomy_enabled: bool,
    cache_ttl: Option<Duration>,
    interiority_interval: Option<Duration>,
}
```

### 5. Time Control for Autonomy Tests

For testing keepalive pings, interiority ticks, and compaction triggers:

```rust
// At test start:
tokio::time::pause();

// Advance time to trigger a keepalive ping:
tokio::time::advance(Duration::from_mins(59)).await;

// Advance time to trigger interiority:
tokio::time::advance(Duration::from_hours(2)).await;
```

This requires that all autonomy code uses `tokio::time::Instant` (not `std::time::Instant`). If the codebase currently uses `std::time::Instant`, this needs a targeted migration in the autonomy module.

### 6. Chaos/Fault Injection Helpers

For resilience tests:

```rust
impl TestHarness {
    /// Kill the daemon abruptly (drop without shutdown)
    async fn crash(self) -> CrashedHarness;

    /// Reboot from a crash (reuse same tmp_dir)
    async fn reboot(crashed: CrashedHarness) -> Self;

    /// Corrupt a specific file in data_dir
    fn corrupt_file(&self, relative_path: &str);

    /// Truncate the JSONL mid-write
    fn truncate_jsonl(&self, bytes_to_keep: usize);
}
```

## Test Categories

### Category 1: Message Pipeline (catches: lagged clients, message hangs, stream assembly)

```
test_basic_message_roundtrip
    → send message, collect response, verify text matches mock
test_streaming_chunks_arrive_in_order
    → mock multi-chunk response, verify chunk ordering
test_message_persistence_after_response
    → send message, verify JSONL contains both user and assistant messages
test_message_ids_are_assigned
    → send message, verify msg_id in response and JSONL
test_concurrent_messages_from_multiple_clients
    → connect two clients, send from both, verify no interleaving corruption
test_slow_client_gets_disconnected
    → connect client, pause recv, send many messages, verify disconnect
```

### Category 2: Tool Execution (catches: tool ID collisions, tool dispatch failures)

```
test_tool_use_roundtrip
    → mock tool_use response, verify tool executes and result returns
test_duplicate_tool_ids_in_single_response
    → mock response with same tool called twice, verify distinct IDs
test_tool_timeout_handling
    → mock tool_use for a slow tool, verify timeout behavior
test_tool_result_persisted
    → verify tool_use and tool_result blocks in JSONL
```

### Category 3: Autonomy & Keepalive (catches: phantom pings, dead timers, lost requests)

```
test_keepalive_ping_fires_after_59_minutes
    → send message (warms cache), advance 59min, verify mock received ping
test_keepalive_ping_sends_real_request
    → verify ping request body matches original (not stripped)
test_no_phantom_ping_when_no_prior_request
    → boot daemon, advance 59min, verify zero requests to mock
test_keepalive_primed_on_boot_from_persistence
    → send message, crash, reboot, advance 59min, verify ping fires
test_rebuilt_request_saved_after_restart
    → crash, reboot, trigger interiority tick, verify last_request populated
test_cache_invalidated_stops_pings
    → trigger compaction, advance 59min, verify no ping
test_failed_ping_retries_next_tick
    → enqueue error, advance 59min, verify retry on next 30s tick
```

### Category 4: Persistence & Recovery (catches: compaction corruption, restart hangs)

```
test_history_survives_restart
    → send messages, crash, reboot, connect, verify history in handshake
test_truncated_jsonl_recovery
    → send messages, truncate JSONL, reboot, verify graceful recovery
test_compaction_is_atomic
    → trigger compaction, verify all-or-nothing in SQLite
test_corrupt_fts_index_rebuilds
    → corrupt FTS table, trigger search, verify rebuild succeeds
test_socket_cleanup_on_restart
    → crash (stale socket), reboot, verify new client connects
```

### Category 5: Provider Edge Cases (catches: socket hangs, error handling)

```
test_provider_timeout_propagates_error
    → enqueue hanging response, verify client gets error within timeout
test_rate_limit_triggers_retry
    → enqueue 429 then success, verify retry with backoff
test_server_error_triggers_retry
    → enqueue 500 then success, verify retry
test_malformed_sse_handled_gracefully
    → enqueue garbage SSE, verify error propagates cleanly
```

### Category 6: Commands & Status

```
test_status_command_returns_token_counts
    → send message, send /status, verify token counts
test_list_characters_command
    → verify character list matches config
```

## Crate Structure

New test crate: `shore-test-harness` (workspace member, dev-only).

```
shore-test-harness/
├── Cargo.toml
├── src/
│   ├── lib.rs              # re-exports
│   ├── harness.rs          # TestHarness
│   ├── mock_llm.rs         # MockLlmServer + SSE builders
│   ├── config.rs           # TestConfigBuilder
│   ├── collected.rs        # CollectedResponse
│   └── chaos.rs            # Crash/reboot/corrupt helpers
```

Integration tests live in each crate's `tests/` directory and depend on `shore-test-harness`:

```
shore-daemon/tests/
├── e2e.rs                  # existing (keep as-is for now)
├── integration_pipeline.rs # Category 1 + 2
├── integration_autonomy.rs # Category 3
├── integration_recovery.rs # Category 4
├── integration_providers.rs # Category 5
```

## Dependencies

- `wiremock` — mock HTTP server (mature, async, supports request matching and response queuing)
- `tokio::time::pause()` — deterministic time control (already using tokio)
- `tempfile` — already used in existing tests

## Migration: `std::time::Instant` → `tokio::time::Instant`

For `tokio::time::pause()` to work, autonomy code must use `tokio::time::Instant`. Audit needed:
- `shore-daemon/src/autonomy/cache_keepalive.rs`
- `shore-daemon/src/autonomy/interiority.rs`
- `shore-daemon/src/autonomy/manager.rs`

If these use `std::time::Instant`, migrate them. This is a targeted change — only the autonomy module needs it.

## What This Does NOT Cover

- Testing the actual LLM response quality (out of scope — that's a prompt engineering problem)
- Testing the TUI rendering (needs a separate approach — terminal snapshot testing)
- Testing the Matrix bridge against a real homeserver (keep the existing `#[ignore]` tests for that)
- Load/performance testing (different concern)

## Success Criteria

A test suite where:
1. Every bug in the git history would have been caught by at least one test
2. Tests run in CI without API keys or network access
3. Tests complete in under 60 seconds total
4. Adding a new feature requires adding a test that exercises the real pipeline
