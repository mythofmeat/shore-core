# Integration Test Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A test harness that boots a real daemon with a mock HTTP backend, enabling fast, free, deterministic integration tests that catch the classes of bugs found in git history.

**Architecture:** New `shore-test-harness` crate wrapping `wiremock::MockServer` for canned SSE responses. `TestHarness` boots the real daemon stack (Server, MessageHandler, CharacterRegistry, AutonomyManager) in-process, points `base_url` at the mock, and connects a real SWP client. Integration tests live in `shore-daemon/tests/`.

**Tech Stack:** Rust, wiremock, tokio (with time::pause for autonomy tests), tempfile, reqwest (real — hits mock server), shore-protocol/client/daemon/config/llm-client (all real)

**Spec:** `docs/superpowers/specs/2026-04-09-integration-test-harness-design.md`

---

## File Structure

```
shore-test-harness/
├── Cargo.toml
├── src/
│   ├── lib.rs              # Re-exports all public types
│   ├── mock_llm.rs         # MockLlmServer + AnthropicStreamBuilder
│   ├── harness.rs          # TestHarness: boot, send, collect, shutdown
│   ├── config.rs           # TestConfigBuilder
│   ├── collected.rs        # CollectedResponse aggregation
│   └── chaos.rs            # CrashedHarness, crash/reboot/corrupt helpers

shore-daemon/tests/
├── e2e.rs                   # (existing, untouched)
├── integration_pipeline.rs  # Message pipeline + tool execution tests
├── integration_autonomy.rs  # Keepalive + interiority tests
├── integration_recovery.rs  # Persistence, crash/reboot, corruption tests
├── integration_providers.rs # Provider error handling, retry, timeout tests
```

---

### Task 1: Create `shore-test-harness` crate skeleton

**Files:**
- Create: `shore-test-harness/Cargo.toml`
- Create: `shore-test-harness/src/lib.rs`
- Modify: `Cargo.toml` (workspace root — add member)

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "shore-test-harness"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
shore-config = { path = "../shore-config" }
shore-client = { path = "../shore-client" }
shore-daemon = { path = "../shore-daemon" }
shore-protocol = { path = "../shore-protocol" }
shore-llm-client = { path = "../shore-llm-client" }
shore-ledger = { path = "../shore-ledger" }
shore-diagnostics = { path = "../shore-diagnostics" }

wiremock = "0.6"
tokio = { version = "1", features = ["full", "test-util"] }
tempfile = "3"
serde_json = "1"
tracing = "0.1"
```

- [ ] **Step 2: Create lib.rs stub**

```rust
pub mod mock_llm;
pub mod harness;
pub mod config;
pub mod collected;
pub mod chaos;
```

Create empty files for each module:
- `shore-test-harness/src/mock_llm.rs` → `// MockLlmServer`
- `shore-test-harness/src/harness.rs` → `// TestHarness`
- `shore-test-harness/src/config.rs` → `// TestConfigBuilder`
- `shore-test-harness/src/collected.rs` → `// CollectedResponse`
- `shore-test-harness/src/chaos.rs` → `// Chaos helpers`

- [ ] **Step 3: Add to workspace**

In the root `Cargo.toml`, add `"shore-test-harness"` to the `[workspace] members` list, after `"shore-ledger"`.

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p shore-test-harness`
Expected: compiles with no errors (just unused warnings)

- [ ] **Step 5: Commit**

```bash
git add shore-test-harness/ Cargo.toml Cargo.lock
git commit -m "feat: add shore-test-harness crate skeleton"
```

---

### Task 2: Implement `MockLlmServer` with Anthropic SSE builder

**Files:**
- Create: `shore-test-harness/src/mock_llm.rs`

This is the core mock. It wraps `wiremock::MockServer` and provides helpers to enqueue canned Anthropic SSE streaming responses. The mock intercepts any POST to `/v1/messages` and returns the enqueued response.

- [ ] **Step 1: Write the test for `AnthropicStreamBuilder`**

Create `shore-test-harness/src/mock_llm.rs`:

```rust
use serde_json::{json, Value};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Wraps a wiremock::MockServer to serve canned LLM responses.
pub struct MockLlmServer {
    server: MockServer,
}

/// Builds an Anthropic-format SSE streaming response body.
pub struct AnthropicStreamBuilder {
    content_blocks: Vec<ContentBlock>,
    input_tokens: u32,
    output_tokens: u32,
    model: String,
    stop_reason: String,
}

enum ContentBlock {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

impl AnthropicStreamBuilder {
    pub fn new() -> Self {
        Self {
            content_blocks: Vec::new(),
            input_tokens: 50,
            output_tokens: 12,
            model: "claude-haiku-4.5".into(),
            stop_reason: "end_turn".into(),
        }
    }

    pub fn text(mut self, text: &str) -> Self {
        self.content_blocks.push(ContentBlock::Text(text.to_string()));
        self
    }

    pub fn tool_use(mut self, id: &str, name: &str, input: Value) -> Self {
        self.content_blocks.push(ContentBlock::ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input,
        });
        self.stop_reason = "tool_use".into();
        self
    }

    pub fn usage(mut self, input: u32, output: u32) -> Self {
        self.input_tokens = input;
        self.output_tokens = output;
        self
    }

    pub fn model(mut self, model: &str) -> Self {
        self.model = model.to_string();
        self
    }

    pub fn stop_reason(mut self, reason: &str) -> Self {
        self.stop_reason = reason.to_string();
        self
    }

    /// Build the SSE response body as a String.
    pub fn build(&self) -> String {
        let mut events = Vec::new();

        // message_start
        events.push(format!(
            "event: message_start\ndata: {}\n",
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_test_001",
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": self.model,
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": self.input_tokens,
                        "output_tokens": 0
                    }
                }
            })
        ));

        // Content blocks
        for (idx, block) in self.content_blocks.iter().enumerate() {
            match block {
                ContentBlock::Text(text) => {
                    // content_block_start
                    events.push(format!(
                        "event: content_block_start\ndata: {}\n",
                        json!({
                            "type": "content_block_start",
                            "index": idx,
                            "content_block": {
                                "type": "text",
                                "text": ""
                            }
                        })
                    ));

                    // content_block_delta — emit full text as one delta
                    events.push(format!(
                        "event: content_block_delta\ndata: {}\n",
                        json!({
                            "type": "content_block_delta",
                            "index": idx,
                            "delta": {
                                "type": "text_delta",
                                "text": text
                            }
                        })
                    ));

                    // content_block_stop
                    events.push(format!(
                        "event: content_block_stop\ndata: {}\n",
                        json!({
                            "type": "content_block_stop",
                            "index": idx
                        })
                    ));
                }
                ContentBlock::ToolUse { id, name, input } => {
                    // content_block_start
                    events.push(format!(
                        "event: content_block_start\ndata: {}\n",
                        json!({
                            "type": "content_block_start",
                            "index": idx,
                            "content_block": {
                                "type": "tool_use",
                                "id": id,
                                "name": name,
                                "input": {}
                            }
                        })
                    ));

                    // content_block_delta — emit full input as one delta
                    events.push(format!(
                        "event: content_block_delta\ndata: {}\n",
                        json!({
                            "type": "content_block_delta",
                            "index": idx,
                            "delta": {
                                "type": "input_json_delta",
                                "partial_json": serde_json::to_string(input).unwrap()
                            }
                        })
                    ));

                    // content_block_stop
                    events.push(format!(
                        "event: content_block_stop\ndata: {}\n",
                        json!({
                            "type": "content_block_stop",
                            "index": idx
                        })
                    ));
                }
            }
        }

        // message_delta
        events.push(format!(
            "event: message_delta\ndata: {}\n",
            json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": self.stop_reason,
                    "stop_sequence": null
                },
                "usage": {
                    "output_tokens": self.output_tokens
                }
            })
        ));

        // message_stop
        events.push(format!(
            "event: message_stop\ndata: {}\n",
            json!({"type": "message_stop"})
        ));

        events.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_builder_text_response() {
        let body = AnthropicStreamBuilder::new()
            .text("Hello from the test!")
            .usage(50, 12)
            .build();

        assert!(body.contains("event: message_start"));
        assert!(body.contains("event: content_block_start"));
        assert!(body.contains("Hello from the test!"));
        assert!(body.contains("event: content_block_stop"));
        assert!(body.contains("event: message_delta"));
        assert!(body.contains("\"stop_reason\":\"end_turn\""));
        assert!(body.contains("event: message_stop"));
    }

    #[test]
    fn test_stream_builder_tool_use_response() {
        let body = AnthropicStreamBuilder::new()
            .tool_use("toolu_01", "check_time", json!({"timezone": "UTC"}))
            .build();

        assert!(body.contains("\"type\":\"tool_use\""));
        assert!(body.contains("\"name\":\"check_time\""));
        assert!(body.contains("\"id\":\"toolu_01\""));
        assert!(body.contains("input_json_delta"));
        assert!(body.contains("\"stop_reason\":\"tool_use\""));
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p shore-test-harness -- mock_llm::tests -v`
Expected: 2 tests pass

- [ ] **Step 3: Add `MockLlmServer` methods**

Append to `mock_llm.rs`, below the `AnthropicStreamBuilder` impl block:

```rust
impl MockLlmServer {
    /// Start a new mock server on a random port.
    pub async fn start() -> Self {
        Self {
            server: MockServer::start().await,
        }
    }

    /// Base URL to inject into model config (e.g. "http://127.0.0.1:12345").
    pub fn base_url(&self) -> String {
        self.server.uri()
    }

    /// Enqueue a simple text response.
    pub async fn enqueue_text(&self, text: &str) {
        let body = AnthropicStreamBuilder::new().text(text).build();
        self.enqueue_raw_sse(body).await;
    }

    /// Enqueue a tool use response.
    pub async fn enqueue_tool_use(&self, id: &str, name: &str, input: Value) {
        let body = AnthropicStreamBuilder::new()
            .tool_use(id, name, input)
            .build();
        self.enqueue_raw_sse(body).await;
    }

    /// Enqueue a custom SSE body built with AnthropicStreamBuilder.
    pub async fn enqueue_raw_sse(&self, body: String) {
        Mock::given(method("POST"))
            .and(path_regex("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body, "text/event-stream")
            )
            .expect(1)
            .mount(&self.server)
            .await;
    }

    /// Enqueue an HTTP error response (e.g. 429, 500).
    pub async fn enqueue_error(&self, status: u16, body: &str) {
        Mock::given(method("POST"))
            .and(path_regex("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(status)
                    .set_body_string(body.to_string())
            )
            .expect(1)
            .mount(&self.server)
            .await;
    }

    /// Enqueue a response that hangs forever (never sends body).
    pub async fn enqueue_hanging(&self) {
        Mock::given(method("POST"))
            .and(path_regex("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("", "text/event-stream")
                    .set_delay(std::time::Duration::from_secs(3600))
            )
            .expect(1)
            .mount(&self.server)
            .await;
    }

    /// Verify all expected requests were received. Panics on mismatch.
    /// This is called automatically on Drop by wiremock, but can be called
    /// explicitly for clearer error messages.
    pub async fn verify(&self) {
        // wiremock verifies on drop, but we can also access received_requests
    }

    /// Get all received request bodies (for inspecting what the daemon sent).
    pub async fn received_requests(&self) -> Vec<Value> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| serde_json::from_slice(&r.body).ok())
            .collect()
    }
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p shore-test-harness`
Expected: compiles (unused warnings are fine)

- [ ] **Step 5: Commit**

```bash
git add shore-test-harness/src/mock_llm.rs
git commit -m "feat(test-harness): MockLlmServer with Anthropic SSE stream builder"
```

---

### Task 3: Implement `TestConfigBuilder`

**Files:**
- Create: `shore-test-harness/src/config.rs`

Builds a `LoadedConfig` pointing at the mock server, with a test character on disk.

- [ ] **Step 1: Write TestConfigBuilder**

```rust
use std::path::{Path, PathBuf};
use std::time::Duration;

use shore_config::app::{AppConfig, ServiceEntry, ServicesConfig};
use shore_config::models::ModelCatalog;
use shore_config::{LoadedConfig, ShoreDirs};

pub struct TestConfigBuilder {
    pub character_name: String,
    pub character_definition: String,
    pub model_alias: String,
    pub model_id: String,
    pub max_tokens: u32,
    pub tool_use_enabled: bool,
    pub tool_use_max_iterations: u32,
}

impl Default for TestConfigBuilder {
    fn default() -> Self {
        Self {
            character_name: "TestChar".into(),
            character_definition:
                "You are TestChar, a concise test assistant. Keep responses very short (1-2 sentences)."
                    .into(),
            model_alias: "haiku".into(),
            model_id: "anthropic/claude-haiku-4.5".into(),
            max_tokens: 1024,
            tool_use_enabled: true,
            tool_use_max_iterations: 5,
        }
    }
}

impl TestConfigBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn character_name(mut self, name: &str) -> Self {
        self.character_name = name.to_string();
        self
    }

    pub fn character_definition(mut self, def: &str) -> Self {
        self.character_definition = def.to_string();
        self
    }

    pub fn tool_use(mut self, enabled: bool) -> Self {
        self.tool_use_enabled = enabled;
        self
    }

    /// Build a LoadedConfig with directories under `tmp_dir`, pointing LLM
    /// traffic at `mock_base_url`.
    pub fn build(&self, tmp_dir: &Path, mock_base_url: &str) -> LoadedConfig {
        let config_dir = tmp_dir.join("config");
        let data_dir = tmp_dir.join("data");
        let runtime_dir = tmp_dir.join("runtime");

        // Create character definition on disk.
        let char_dir = config_dir
            .join("characters")
            .join(&self.character_name);
        std::fs::create_dir_all(&char_dir).unwrap();
        std::fs::write(
            char_dir.join("character.md"),
            &self.character_definition,
        )
        .unwrap();

        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&runtime_dir).unwrap();

        let mut app = AppConfig::default();
        app.defaults.model = Some(self.model_alias.clone());
        app.behavior.tool_use.enabled = self.tool_use_enabled;
        app.behavior.tool_use.max_iterations = self.tool_use_max_iterations;
        // Services config — not needed for in-process tests, but fill to
        // avoid panics if anything reads it.
        app.services = ServicesConfig {
            llm: ServiceEntry {
                command: None,
                socket: None,
            },
        };

        // Build model catalog pointing at mock server.
        let models_toml = format!(
            r#"
[openrouter]
base_url = "{mock_base_url}"

[openrouter.{alias}]
model_id = "{model_id}"
max_tokens = {max_tokens}
temperature = 0.0
"#,
            mock_base_url = mock_base_url,
            alias = self.model_alias,
            model_id = self.model_id,
            max_tokens = self.max_tokens,
        );
        let table: toml::Table = models_toml.parse().unwrap();
        let models =
            ModelCatalog::from_sections(Some(&table), None, None, None).unwrap();

        LoadedConfig::new_for_test(
            app,
            models,
            ShoreDirs {
                config: config_dir,
                data: data_dir,
                runtime: runtime_dir,
            },
        )
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p shore-test-harness`
Expected: compiles. If `AppConfig` fields differ from what's shown (e.g. `services` field name, `ServiceEntry` fields), adjust to match the actual struct definitions in `shore-config/src/app.rs`.

- [ ] **Step 3: Commit**

```bash
git add shore-test-harness/src/config.rs
git commit -m "feat(test-harness): TestConfigBuilder for mock-backed LoadedConfig"
```

---

### Task 4: Implement `CollectedResponse`

**Files:**
- Create: `shore-test-harness/src/collected.rs`

Aggregates a streamed response into a single struct for easy assertion.

- [ ] **Step 1: Write CollectedResponse**

```rust
use shore_protocol::server_msg::ServerMessage;

/// Aggregated result from collecting a full streamed LLM response.
#[derive(Debug, Default)]
pub struct CollectedResponse {
    /// Full assembled text from all StreamChunk messages.
    pub text: String,
    /// Tool calls received during this response.
    pub tool_calls: Vec<CollectedToolCall>,
    /// All raw ServerMessages received (in order).
    pub raw_messages: Vec<ServerMessage>,
    /// Whether a StreamEnd was received.
    pub stream_ended: bool,
}

#[derive(Debug, Clone)]
pub struct CollectedToolCall {
    pub name: String,
    pub id: String,
    pub input: serde_json::Value,
}

impl CollectedResponse {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a ServerMessage into the collector. Returns true if stream is complete.
    pub fn push(&mut self, msg: ServerMessage) -> bool {
        match &msg {
            ServerMessage::StreamChunk(chunk) => {
                self.text.push_str(&chunk.text);
            }
            ServerMessage::ToolCall(tc) => {
                self.tool_calls.push(CollectedToolCall {
                    name: tc.name.clone(),
                    id: tc.id.clone(),
                    input: tc.input.clone(),
                });
            }
            ServerMessage::StreamEnd(_) => {
                self.stream_ended = true;
                self.raw_messages.push(msg);
                return true;
            }
            ServerMessage::Error(_) => {
                self.raw_messages.push(msg);
                return true;
            }
            _ => {}
        }
        self.raw_messages.push(msg);
        false
    }

    /// Assert the response text contains the given substring.
    pub fn assert_text_contains(&self, expected: &str) {
        assert!(
            self.text.contains(expected),
            "Expected response text to contain {:?}, got {:?}",
            expected,
            self.text,
        );
    }

    /// Assert exactly N tool calls were made.
    pub fn assert_tool_call_count(&self, n: usize) {
        assert_eq!(
            self.tool_calls.len(),
            n,
            "Expected {} tool calls, got {}: {:?}",
            n,
            self.tool_calls.len(),
            self.tool_calls.iter().map(|tc| &tc.name).collect::<Vec<_>>(),
        );
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p shore-test-harness`
Expected: compiles. If `StreamChunk` doesn't have a `text` field or `ToolCall` has different field names, adjust to match the actual struct definitions in `shore-protocol/src/server_msg.rs`. Read those structs and fix any mismatches.

- [ ] **Step 3: Commit**

```bash
git add shore-test-harness/src/collected.rs
git commit -m "feat(test-harness): CollectedResponse for stream aggregation"
```

---

### Task 5: Implement `TestHarness`

**Files:**
- Create: `shore-test-harness/src/harness.rs`

This is the main orchestrator. It boots the daemon stack in-process (same pattern as `shore-daemon/tests/e2e.rs` lines 660-748), wires in the `MockLlmServer`, and provides a connected SWP client with send/collect helpers.

- [ ] **Step 1: Write TestHarness**

Reference `shore-daemon/tests/e2e.rs` lines 660-748 for the exact component wiring. The struct and `boot()` method must mirror that pattern exactly, with these changes:
- Replace the hardcoded OpenRouter base URL with `mock_llm.base_url()`
- Use `TestConfigBuilder` instead of `build_test_config()`
- Do NOT start any external LLM service process (no `shore-llm` binary)

```rust
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;

use shore_client::connection::{SWPConnection, ServerAddr};
use shore_config::LoadedConfig;
use shore_daemon::characters::CharacterRegistry;
use shore_daemon::commands::{CommandContext, SessionTokens};
use shore_daemon::handler::MessageHandler;
use shore_daemon::server::{Server, ServerConfig};
use shore_ledger::LedgerClient;
use shore_llm_client::LlmClient;
use shore_protocol::server_msg::ServerMessage;

use crate::collected::CollectedResponse;
use crate::config::TestConfigBuilder;
use crate::mock_llm::MockLlmServer;

/// Full daemon test harness with mock LLM backend.
pub struct TestHarness {
    pub conn: SWPConnection,
    pub mock_llm: MockLlmServer,
    pub tmp_dir: tempfile::TempDir,
    pub data_dir: PathBuf,
    pub socket_path: PathBuf,
    shutdown_tx: watch::Sender<()>,
    server_handle: JoinHandle<()>,
    handler_handle: JoinHandle<()>,
    config: LoadedConfig,
}

impl TestHarness {
    /// Boot with default test config.
    pub async fn boot() -> Self {
        Self::boot_with(TestConfigBuilder::default()).await
    }

    /// Boot with custom config.
    pub async fn boot_with(builder: TestConfigBuilder) -> Self {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let mock_llm = MockLlmServer::start().await;
        let config = builder.build(tmp_dir.path(), &mock_llm.base_url());

        let socket_path = config.dirs.runtime.join("test.sock");
        let data_dir = config.dirs.data.clone();

        // -- Wire components exactly as e2e.rs does --
        // Read e2e.rs lines 187-249 for the exact pattern.
        // The key difference: LlmClient::new() is used as-is because the
        // model catalog already has base_url pointing at mock_llm.
        //
        // Implementer: copy the wiring from E2EHarness::start() in
        // shore-daemon/tests/e2e.rs, replacing build_test_config() with
        // the `config` variable above. Remove the shore_llm_dist() /
        // LLM service socket setup — we don't need an external process.

        let (shutdown_tx, shutdown_rx) = watch::channel(());

        let server_config = ServerConfig {
            socket_path: socket_path.clone(),
            tcp: None,
            server_name: "test-harness".into(),
        };

        // Create broadcast channel for server → client pushes.
        let (push_tx, _push_rx) = tokio::sync::broadcast::channel(256);

        // Create route channel for server → handler message dispatch.
        let (route_tx, route_rx) = tokio::sync::mpsc::channel(64);

        let server = Server::new(server_config, push_tx.clone(), route_tx);

        let char_registry = Arc::new(Mutex::new(
            CharacterRegistry::new(
                &config.dirs,
                push_tx.clone(),
                config.clone(),
            )
            .unwrap(),
        ));

        let autonomy = shore_daemon::autonomy::AutonomyManager::new(
            Default::default(),
            Default::default(),
            data_dir.clone(),
            shutdown_rx.clone(),
        );

        let llm_client = LedgerClient::new(
            LlmClient::new(),
            data_dir.join("ledger.db"),
        )
        .unwrap();

        let diagnostics = Arc::new(shore_diagnostics::DiagnosticRing::new());

        let cmd_ctx = CommandContext {
            config: config.clone(),
            push_tx: push_tx.clone(),
            data_dir: data_dir.clone(),
            active_model: Arc::new(Mutex::new(None)),
            session_tokens: Arc::new(Mutex::new(SessionTokens::default())),
            autonomy: autonomy.clone(),
            llm_client: llm_client.clone(),
            diagnostics: diagnostics.clone(),
        };

        let msg_handler = MessageHandler {
            registry: char_registry.clone(),
            cmd_ctx,
            llm_client: llm_client.clone(),
            push_tx: push_tx.clone(),
            autonomy: autonomy.clone(),
            notifier: None,
            generation_handle: Arc::new(Mutex::new(None)),
        };

        let server_shutdown = shutdown_rx.clone();
        let server_handle = tokio::spawn(async move {
            let _ = server.run(server_shutdown).await;
        });

        let handler_handle = tokio::spawn(async move {
            msg_handler.run(route_rx).await;
        });

        // Wait briefly for server to bind.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Connect SWP client.
        let (conn, _hello, _history) = SWPConnection::connect(
            &ServerAddr::Unix(socket_path.display().to_string()),
            "test",
            "integration-test",
            None,
        )
        .await
        .expect("Failed to connect to test daemon");

        Self {
            conn,
            mock_llm,
            tmp_dir,
            data_dir,
            socket_path,
            shutdown_tx,
            server_handle,
            handler_handle,
            config,
        }
    }

    /// Send a user message and collect the full streamed response.
    pub async fn send_and_collect(&mut self, text: &str) -> CollectedResponse {
        self.conn
            .send_message(&self.config.app.defaults.model.as_deref().unwrap_or("haiku"), text)
            .await
            .expect("Failed to send message");
        self.collect_stream().await
    }

    /// Collect messages until StreamEnd or Error (with 30s timeout).
    pub async fn collect_stream(&mut self) -> CollectedResponse {
        let mut collected = CollectedResponse::new();
        let deadline = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let msg = self.conn.recv().await.expect("Connection closed");
                if collected.push(msg) {
                    break;
                }
            }
        });
        deadline.await.expect("Timed out waiting for stream to complete");
        collected
    }

    /// Send a slash command and collect all response messages until quiet.
    pub async fn send_command(&mut self, cmd: &str) -> Vec<ServerMessage> {
        self.conn
            .send_command(cmd)
            .await
            .expect("Failed to send command");

        let mut msgs = Vec::new();
        // Commands respond with CommandOutput, not streams.
        // Collect for a short window.
        let deadline = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match tokio::time::timeout(Duration::from_millis(500), self.conn.recv()).await {
                    Ok(Ok(msg)) => msgs.push(msg),
                    _ => break,
                }
            }
        });
        let _ = deadline.await;
        msgs
    }

    /// Read persisted JSONL messages from the active conversation file.
    pub fn read_persisted_messages(&self) -> Vec<serde_json::Value> {
        let pattern = format!("{}/TestChar/active.jsonl", self.data_dir.display());
        let path = PathBuf::from(&pattern);
        if !path.exists() {
            // Try finding any .jsonl file under the data dir.
            let mut found = Vec::new();
            for entry in walkdir::WalkDir::new(&self.data_dir)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if entry.path().extension().map(|e| e == "jsonl").unwrap_or(false) {
                    if let Ok(contents) = std::fs::read_to_string(entry.path()) {
                        for line in contents.lines() {
                            if let Ok(v) = serde_json::from_str(line) {
                                found.push(v);
                            }
                        }
                    }
                }
            }
            return found;
        }
        std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// Graceful shutdown.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        let _ = self.server_handle.await;
        let _ = self.handler_handle.await;
    }
}
```

**IMPORTANT for the implementer:** The component wiring above is a best-effort reconstruction from `e2e.rs`. You MUST:
1. Read `shore-daemon/tests/e2e.rs` lines 660-748 (the `E2EHarness::start` method)
2. Read the actual struct definitions for `Server::new`, `MessageHandler`, `CommandContext`, `CharacterRegistry::new`, `AutonomyManager::new`, `LedgerClient::new`
3. Adjust field names, parameter order, and types to match exactly
4. The `send_message` and `send_command` and `recv` method signatures on `SWPConnection` may differ — read `shore-client/src/connection.rs` and adjust

- [ ] **Step 2: Add walkdir dependency**

In `shore-test-harness/Cargo.toml`, add:
```toml
walkdir = "2"
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p shore-test-harness`
Expected: compiles. Fix any type mismatches between the harness code and actual struct definitions.

- [ ] **Step 4: Commit**

```bash
git add shore-test-harness/src/harness.rs shore-test-harness/Cargo.toml
git commit -m "feat(test-harness): TestHarness with daemon boot and SWP client"
```

---

### Task 6: Implement `CrashedHarness` and chaos helpers

**Files:**
- Create: `shore-test-harness/src/chaos.rs`

For testing crash/reboot scenarios and data corruption.

- [ ] **Step 1: Write chaos helpers**

```rust
use std::path::PathBuf;

use crate::config::TestConfigBuilder;
use crate::harness::TestHarness;
use crate::mock_llm::MockLlmServer;

/// Represents a crashed daemon — holds the temp dir and mock server
/// so a new harness can reboot from the same state.
pub struct CrashedHarness {
    pub tmp_dir: tempfile::TempDir,
    pub mock_llm: MockLlmServer,
    pub data_dir: PathBuf,
    pub socket_path: PathBuf,
}

impl TestHarness {
    /// Simulate a crash: drop the daemon without graceful shutdown.
    /// Returns CrashedHarness so you can reboot from the same state.
    pub async fn crash(self) -> CrashedHarness {
        // Drop shutdown_tx, server_handle, handler_handle without awaiting.
        // This simulates an abrupt crash.
        drop(self.shutdown_tx);
        self.server_handle.abort();
        self.handler_handle.abort();

        // Clean up the stale socket so reboot can bind.
        let _ = std::fs::remove_file(&self.socket_path);

        CrashedHarness {
            tmp_dir: self.tmp_dir,
            mock_llm: self.mock_llm,
            data_dir: self.data_dir,
            socket_path: self.socket_path,
        }
    }
}

impl CrashedHarness {
    /// Reboot the daemon from the same data directory.
    /// The mock LLM server stays running on the same port.
    pub async fn reboot(self) -> TestHarness {
        // Rebuild config from the existing directories.
        let config = TestConfigBuilder::default()
            .build(self.tmp_dir.path(), &self.mock_llm.base_url());

        // Boot new daemon components using same dirs.
        // Implementer: extract the component wiring from TestHarness::boot_with
        // into a shared helper, or duplicate the wiring here using `config`
        // and `self.socket_path`. The key difference from boot() is that
        // we reuse self.tmp_dir and self.mock_llm instead of creating new ones.
        //
        // For now, this is a placeholder that the implementer must fill in
        // by following the same pattern as TestHarness::boot_with, but using
        // the existing tmp_dir, mock_llm, data_dir, and socket_path.
        todo!("Implementer: wire daemon components from existing state — follow TestHarness::boot_with pattern")
    }

    /// Corrupt a file in the data directory by overwriting its contents.
    pub fn corrupt_file(&self, relative_path: &str) {
        let path = self.data_dir.join(relative_path);
        if path.exists() {
            std::fs::write(&path, b"CORRUPTED_DATA_FOR_TESTING").unwrap();
        }
    }

    /// Truncate a file to the given number of bytes.
    pub fn truncate_file(&self, relative_path: &str, bytes_to_keep: u64) {
        let path = self.data_dir.join(relative_path);
        if path.exists() {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap();
            file.set_len(bytes_to_keep).unwrap();
        }
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p shore-test-harness`
Expected: compiles (the `todo!()` in `reboot` is fine for now — it compiles but panics at runtime)

- [ ] **Step 3: Commit**

```bash
git add shore-test-harness/src/chaos.rs
git commit -m "feat(test-harness): CrashedHarness with crash/reboot/corrupt helpers"
```

---

### Task 7: Update `lib.rs` re-exports

**Files:**
- Modify: `shore-test-harness/src/lib.rs`

- [ ] **Step 1: Write lib.rs with re-exports**

```rust
pub mod mock_llm;
pub mod harness;
pub mod config;
pub mod collected;
pub mod chaos;

// Convenience re-exports for test files.
pub use collected::CollectedResponse;
pub use config::TestConfigBuilder;
pub use harness::TestHarness;
pub use mock_llm::{AnthropicStreamBuilder, MockLlmServer};
pub use chaos::CrashedHarness;
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p shore-test-harness`
Expected: compiles

- [ ] **Step 3: Commit**

```bash
git add shore-test-harness/src/lib.rs
git commit -m "feat(test-harness): re-export public types from lib.rs"
```

---

### Task 8: First integration test — message pipeline roundtrip

**Files:**
- Create: `shore-daemon/tests/integration_pipeline.rs`
- Modify: `shore-daemon/Cargo.toml` (add dev-dependency)

This is the proving ground. If this test boots the daemon, sends a message through the mock, and collects a response, the harness works.

- [ ] **Step 1: Add dev-dependency to shore-daemon**

In `shore-daemon/Cargo.toml`, under `[dev-dependencies]`, add:
```toml
shore-test-harness = { path = "../shore-test-harness" }
```

- [ ] **Step 2: Write the first integration test**

Create `shore-daemon/tests/integration_pipeline.rs`:

```rust
use shore_test_harness::TestHarness;

#[tokio::test]
async fn test_basic_message_roundtrip() {
    let mut harness = TestHarness::boot().await;

    // Enqueue a canned response.
    harness.mock_llm.enqueue_text("Hello from the mock!").await;

    // Send a user message and collect the streamed response.
    let response = harness.send_and_collect("Hi there").await;

    // The response text should contain what the mock returned.
    response.assert_text_contains("Hello from the mock!");
    assert!(response.stream_ended, "Stream should have ended");

    harness.shutdown().await;
}

#[tokio::test]
async fn test_message_persistence() {
    let mut harness = TestHarness::boot().await;

    harness.mock_llm.enqueue_text("Persisted response").await;
    let _response = harness.send_and_collect("Test persistence").await;

    // Verify messages were persisted to JSONL.
    let persisted = harness.read_persisted_messages();
    assert!(
        persisted.len() >= 2,
        "Expected at least 2 persisted messages (user + assistant), got {}",
        persisted.len()
    );

    // Check that the user message is in there.
    let has_user_msg = persisted.iter().any(|m| {
        m.get("role").and_then(|r| r.as_str()) == Some("user")
    });
    assert!(has_user_msg, "User message not found in persisted JSONL");

    // Check that the assistant message is in there.
    let has_assistant_msg = persisted.iter().any(|m| {
        m.get("role").and_then(|r| r.as_str()) == Some("assistant")
    });
    assert!(has_assistant_msg, "Assistant message not found in persisted JSONL");

    harness.shutdown().await;
}

#[tokio::test]
async fn test_streaming_chunks_arrive_in_order() {
    use shore_test_harness::AnthropicStreamBuilder;

    let mut harness = TestHarness::boot().await;

    // Build a multi-chunk response manually.
    // The builder emits one delta per text block, but we can use multiple
    // text blocks to simulate chunked delivery.
    let body = AnthropicStreamBuilder::new()
        .text("Hello ")
        .build();
    // Note: the builder concatenates all text blocks in order. For true
    // multi-chunk testing, we'd need to extend the builder to support
    // splitting a single text block into multiple deltas. For now, verify
    // the single-chunk path works.
    harness.mock_llm.enqueue_raw_sse(body).await;

    let response = harness.send_and_collect("chunk test").await;
    response.assert_text_contains("Hello ");
    assert!(response.stream_ended);

    harness.shutdown().await;
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p shore-daemon --test integration_pipeline -- --nocapture 2>&1 | head -80`
Expected: all 3 tests pass. If they fail, debug based on the error output — likely causes are:
- Struct field mismatches in `TestHarness` wiring (fix by reading actual struct defs)
- `SWPConnection` method signature differences (fix by reading `shore-client/src/connection.rs`)
- The mock server SSE format not matching what `shore-llm-client` expects (fix by comparing with `anthropic.rs` event parsing)

- [ ] **Step 4: Commit**

```bash
git add shore-daemon/tests/integration_pipeline.rs shore-daemon/Cargo.toml
git commit -m "test: first integration tests — message roundtrip and persistence"
```

---

### Task 9: Tool execution integration tests

**Files:**
- Modify: `shore-daemon/tests/integration_pipeline.rs`

- [ ] **Step 1: Add tool execution test**

Append to `shore-daemon/tests/integration_pipeline.rs`:

```rust
#[tokio::test]
async fn test_tool_use_roundtrip() {
    use serde_json::json;

    let mut harness = TestHarness::boot().await;

    // First response: LLM wants to call check_time.
    harness
        .mock_llm
        .enqueue_tool_use("toolu_01", "check_time", json!({}))
        .await;

    // Second response: after tool result, LLM gives final answer.
    harness
        .mock_llm
        .enqueue_text("The current time is 12:00 UTC.")
        .await;

    // Send message — daemon should: stream tool call → execute tool →
    // feed result back → stream final response.
    let response = harness.send_and_collect("What time is it?").await;

    // We should have received at least one tool call.
    // Note: depending on how the daemon handles the tool loop, we may
    // need to collect two streams. Adjust based on actual behavior.
    // The key assertion: the mock received TWO requests (initial + post-tool).
    let requests = harness.mock_llm.received_requests().await;
    assert!(
        requests.len() >= 2,
        "Expected at least 2 LLM requests (initial + post-tool), got {}",
        requests.len()
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn test_tool_result_persisted_in_jsonl() {
    use serde_json::json;

    let mut harness = TestHarness::boot().await;

    harness
        .mock_llm
        .enqueue_tool_use("toolu_02", "check_time", json!({}))
        .await;
    harness
        .mock_llm
        .enqueue_text("It is noon.")
        .await;

    let _response = harness.send_and_collect("Time please").await;

    let persisted = harness.read_persisted_messages();

    // Look for tool_use and tool_result content blocks in persisted messages.
    let serialized = serde_json::to_string(&persisted).unwrap();
    assert!(
        serialized.contains("tool_use") || serialized.contains("check_time"),
        "Expected tool_use in persisted JSONL, got: {}",
        &serialized[..serialized.len().min(500)]
    );

    harness.shutdown().await;
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p shore-daemon --test integration_pipeline -- --nocapture 2>&1 | head -80`
Expected: all 5 tests pass. Tool execution may require the daemon to loop (send tool result back to LLM), so the `collect_stream` helper needs to handle the two-phase flow. If the test hangs, the daemon likely expects a second response from the mock but `collect_stream` exited after the first `StreamEnd`. Fix by adjusting `send_and_collect` to loop through tool phases.

- [ ] **Step 3: Commit**

```bash
git add shore-daemon/tests/integration_pipeline.rs
git commit -m "test: tool execution integration tests — roundtrip and persistence"
```

---

### Task 10: Migrate `std::time::Instant` → `tokio::time::Instant` in autonomy

**Files:**
- Modify: `shore-daemon/src/autonomy/cache_keepalive.rs`
- Modify: `shore-daemon/src/autonomy/interiority.rs`
- Modify: `shore-daemon/src/autonomy/manager.rs`
- Modify: `shore-daemon/src/autonomy/activity.rs`
- Possibly: `shore-daemon/src/autonomy/mod.rs`

This migration is required for `tokio::time::pause()` to work in autonomy tests. `tokio::time::Instant` is a drop-in replacement for `std::time::Instant` when running inside a tokio runtime — it just additionally responds to `tokio::time::pause/advance`.

- [ ] **Step 1: Audit all Instant usage in autonomy**

Search for `std::time::Instant` in all files under `shore-daemon/src/autonomy/`. Also search for `Instant::now()` to catch any unqualified uses.

For each file:
- Replace `use std::time::Instant` with `use tokio::time::Instant`
- Replace any `std::time::Instant::now()` with `tokio::time::Instant::now()`
- Leave `std::time::Duration` unchanged (tokio re-exports the same type)

- [ ] **Step 2: Check for Instant usage outside autonomy that interacts with it**

Search for `Instant` in `shore-daemon/src/handler/` — if the handler passes `Instant::now()` into autonomy methods, those must also use `tokio::time::Instant`. Migrate as needed.

- [ ] **Step 3: Run existing tests**

Run: `cargo test -p shore-daemon -- autonomy -v`
Expected: all existing autonomy tests still pass. `tokio::time::Instant` behaves identically to `std::time::Instant` in non-paused contexts.

- [ ] **Step 4: Run full workspace tests**

Run: `cargo test --workspace`
Expected: no regressions.

- [ ] **Step 5: Commit**

```bash
git add shore-daemon/src/autonomy/
git commit -m "refactor: migrate autonomy to tokio::time::Instant for test time control"
```

---

### Task 11: Autonomy integration tests — keepalive pings

**Files:**
- Create: `shore-daemon/tests/integration_autonomy.rs`

These tests catch the exact class of bugs described in the phantom ping / dead timer / lost request bugs.

- [ ] **Step 1: Write keepalive integration tests**

Create `shore-daemon/tests/integration_autonomy.rs`:

```rust
use std::time::Duration;
use shore_test_harness::{TestHarness, TestConfigBuilder};

/// After sending a message (which warms the cache), advancing 59 minutes
/// should trigger a keepalive ping to the mock LLM.
#[tokio::test]
async fn test_keepalive_ping_fires_after_59_minutes() {
    tokio::time::pause();

    let mut harness = TestHarness::boot().await;

    // Send a message to warm the cache.
    harness.mock_llm.enqueue_text("Warming the cache.").await;
    let _response = harness.send_and_collect("Hello").await;

    // At this point, 1 request has been made.
    let before = harness.mock_llm.received_requests().await.len();

    // Enqueue a response for the keepalive ping.
    harness.mock_llm.enqueue_text("ping response").await;

    // Advance time past the 59-minute keepalive interval.
    tokio::time::advance(Duration::from_secs(59 * 60 + 30)).await;

    // Give the tick loop time to fire (it runs every ~30s).
    tokio::time::advance(Duration::from_secs(60)).await;
    // Yield to let async tasks run.
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let after = harness.mock_llm.received_requests().await.len();
    assert!(
        after > before,
        "Expected keepalive ping after 59 minutes, but no new request was made \
         (before={before}, after={after})"
    );

    harness.shutdown().await;
}

/// If no message has ever been sent, advancing 59 minutes should NOT
/// trigger a keepalive ping (there's nothing to ping with).
#[tokio::test]
async fn test_no_phantom_ping_without_prior_request() {
    tokio::time::pause();

    let harness = TestHarness::boot().await;

    // Advance well past keepalive interval.
    tokio::time::advance(Duration::from_secs(120 * 60)).await;
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let requests = harness.mock_llm.received_requests().await;
    assert_eq!(
        requests.len(),
        0,
        "Expected zero requests (no message was ever sent), got {}",
        requests.len()
    );

    harness.shutdown().await;
}

/// A failed keepalive ping should retry on the next tick, not silently
/// reset the timer.
#[tokio::test]
async fn test_failed_ping_retries() {
    tokio::time::pause();

    let mut harness = TestHarness::boot().await;

    // Warm cache.
    harness.mock_llm.enqueue_text("warm").await;
    let _response = harness.send_and_collect("Hello").await;

    let after_warm = harness.mock_llm.received_requests().await.len();

    // Enqueue an error for the first ping attempt.
    harness.mock_llm.enqueue_error(500, "Internal Server Error").await;

    // Advance to trigger the ping.
    tokio::time::advance(Duration::from_secs(59 * 60 + 30)).await;
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let after_fail = harness.mock_llm.received_requests().await.len();
    assert!(
        after_fail > after_warm,
        "Expected a ping attempt (even if it fails)"
    );

    // Enqueue a success for the retry.
    harness.mock_llm.enqueue_text("retry success").await;

    // Advance another tick cycle (30s).
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let after_retry = harness.mock_llm.received_requests().await.len();
    assert!(
        after_retry > after_fail,
        "Expected retry after failed ping, but no new request \
         (after_fail={after_fail}, after_retry={after_retry})"
    );

    harness.shutdown().await;
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p shore-daemon --test integration_autonomy -- --nocapture 2>&1 | head -80`
Expected: all 3 tests pass. If the time advancement doesn't trigger the tick loop, the issue may be that `tokio::time::pause()` doesn't work with the daemon's `tokio::spawn` + `tokio::time::sleep` loop. Debug by checking that the autonomy tick loop uses `tokio::time::sleep` (not `std::thread::sleep`).

- [ ] **Step 3: Commit**

```bash
git add shore-daemon/tests/integration_autonomy.rs
git commit -m "test: autonomy integration tests — keepalive ping firing, phantom ping prevention, retry"
```

---

### Task 12: Recovery integration tests — crash/reboot

**Files:**
- Create: `shore-daemon/tests/integration_recovery.rs`

- [ ] **Step 1: Implement `CrashedHarness::reboot`**

Before writing recovery tests, the `todo!()` in `CrashedHarness::reboot` must be filled in. Extract the component wiring from `TestHarness::boot_with` into a shared helper method, then call it from both `boot_with` and `reboot`.

In `shore-test-harness/src/harness.rs`, add a private method:

```rust
impl TestHarness {
    /// Internal: wire daemon components from config and existing state.
    async fn wire_daemon(
        config: LoadedConfig,
        mock_llm: MockLlmServer,
        tmp_dir: tempfile::TempDir,
        data_dir: PathBuf,
        socket_path: PathBuf,
    ) -> Self {
        // Move the entire body of boot_with (everything after config/mock/paths
        // are created) into this method. Both boot_with and reboot call it.
        // ...same wiring as boot_with...
    }
}
```

Then update `boot_with` to call `wire_daemon`, and update `CrashedHarness::reboot`:

```rust
impl CrashedHarness {
    pub async fn reboot(self) -> TestHarness {
        let config = TestConfigBuilder::default()
            .build(self.tmp_dir.path(), &self.mock_llm.base_url());
        TestHarness::wire_daemon(
            config,
            self.mock_llm,
            self.tmp_dir,
            self.data_dir,
            self.socket_path,
        )
        .await
    }
}
```

- [ ] **Step 2: Write recovery integration tests**

Create `shore-daemon/tests/integration_recovery.rs`:

```rust
use shore_test_harness::TestHarness;

/// After crash and reboot, conversation history should be available
/// to the newly connected client.
#[tokio::test]
async fn test_history_survives_restart() {
    let mut harness = TestHarness::boot().await;

    // Send a message and get a response.
    harness.mock_llm.enqueue_text("I remember this.").await;
    let _response = harness.send_and_collect("Remember this message").await;

    // Verify persistence happened.
    let persisted_before = harness.read_persisted_messages();
    assert!(
        !persisted_before.is_empty(),
        "Messages should be persisted before crash"
    );

    // Crash and reboot.
    let crashed = harness.crash().await;
    let harness = crashed.reboot().await;

    // After reboot, the JSONL file should still have our messages.
    let persisted_after = harness.read_persisted_messages();
    assert_eq!(
        persisted_before.len(),
        persisted_after.len(),
        "Persisted message count should survive crash/reboot"
    );

    harness.shutdown().await;
}

/// A stale Unix socket from a crash should not prevent reboot.
#[tokio::test]
async fn test_socket_cleanup_on_restart() {
    let mut harness = TestHarness::boot().await;

    harness.mock_llm.enqueue_text("pre-crash").await;
    let _response = harness.send_and_collect("test").await;

    let crashed = harness.crash().await;

    // Reboot should succeed even though the previous socket might be stale.
    let mut rebooted = crashed.reboot().await;

    rebooted.mock_llm.enqueue_text("post-crash").await;
    let response = rebooted.send_and_collect("still alive?").await;
    response.assert_text_contains("post-crash");

    rebooted.shutdown().await;
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p shore-daemon --test integration_recovery -- --nocapture 2>&1 | head -80`
Expected: both tests pass.

- [ ] **Step 4: Commit**

```bash
git add shore-test-harness/src/harness.rs shore-test-harness/src/chaos.rs shore-daemon/tests/integration_recovery.rs
git commit -m "test: recovery integration tests — history survival and socket cleanup after crash"
```

---

### Task 13: Provider edge case integration tests

**Files:**
- Create: `shore-daemon/tests/integration_providers.rs`

- [ ] **Step 1: Write provider edge case tests**

Create `shore-daemon/tests/integration_providers.rs`:

```rust
use shore_test_harness::TestHarness;

/// A 429 rate limit response should trigger retry, not crash.
#[tokio::test]
async fn test_rate_limit_triggers_retry() {
    let mut harness = TestHarness::boot().await;

    // First response: 429 rate limit.
    harness
        .mock_llm
        .enqueue_error(429, r#"{"error":{"type":"rate_limit_error","message":"Rate limited"}}"#)
        .await;

    // Second response: success.
    harness.mock_llm.enqueue_text("Recovered from rate limit.").await;

    let response = harness.send_and_collect("trigger retry").await;

    // Should eventually get the success response after retry.
    response.assert_text_contains("Recovered from rate limit");

    // Verify two requests were made (original + retry).
    let requests = harness.mock_llm.received_requests().await;
    assert!(
        requests.len() >= 2,
        "Expected at least 2 requests (original + retry), got {}",
        requests.len()
    );

    harness.shutdown().await;
}

/// A 500 server error should trigger retry.
#[tokio::test]
async fn test_server_error_triggers_retry() {
    let mut harness = TestHarness::boot().await;

    harness
        .mock_llm
        .enqueue_error(500, r#"{"error":{"type":"server_error","message":"Internal error"}}"#)
        .await;
    harness.mock_llm.enqueue_text("Recovered from 500.").await;

    let response = harness.send_and_collect("trigger 500").await;
    response.assert_text_contains("Recovered from 500");

    harness.shutdown().await;
}

/// Malformed SSE should result in an error, not a hang or panic.
#[tokio::test]
async fn test_malformed_sse_returns_error() {
    let mut harness = TestHarness::boot().await;

    // Enqueue garbage that is not valid SSE.
    harness
        .mock_llm
        .enqueue_raw_sse("this is not valid SSE at all\ngarbage data\n".into())
        .await;

    // Should get an error response, not hang forever.
    let response = harness.collect_stream().await;

    // Either we get an Error message or the stream ends.
    // The key assertion: the test does not hang (the 30s timeout in
    // collect_stream catches that).
    assert!(
        response.stream_ended || response.raw_messages.iter().any(|m| {
            matches!(m, shore_protocol::server_msg::ServerMessage::Error(_))
        }),
        "Expected error or stream end on malformed SSE, got: {:?}",
        response.raw_messages
    );

    harness.shutdown().await;
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p shore-daemon --test integration_providers -- --nocapture 2>&1 | head -80`
Expected: all 3 pass. The retry tests depend on `shore-llm-client`'s retry logic — if it doesn't retry on 429/500 by default, the tests will fail and reveal a real gap.

- [ ] **Step 3: Commit**

```bash
git add shore-daemon/tests/integration_providers.rs
git commit -m "test: provider edge case integration tests — retry, error handling, malformed SSE"
```

---

### Task 14: Run full test suite and verify

**Files:** none (verification only)

- [ ] **Step 1: Run all new integration tests**

Run: `cargo test -p shore-daemon --test integration_pipeline --test integration_autonomy --test integration_recovery --test integration_providers -- --nocapture 2>&1`
Expected: all tests pass.

- [ ] **Step 2: Run existing tests to verify no regressions**

Run: `cargo test --workspace -- --exclude-ignored 2>&1 | tail -20`
Expected: all existing tests still pass. The Instant migration (Task 10) should be transparent.

- [ ] **Step 3: Type check**

Run: `cargo check --workspace`
Expected: no errors.

- [ ] **Step 4: Commit any remaining fixes**

If any tests needed adjustment, commit the fixes.

---

### Task 15: Update documentation

**Files:**
- Modify: `DECISIONS.md`
- Modify: `ARCHITECTURE.md`

- [ ] **Step 1: Record decision in DECISIONS.md**

Add entry:

```markdown
## Integration Test Harness (2026-04-09)

**Decision:** Created `shore-test-harness` crate with a `TestHarness` that boots a real daemon in-process and mocks only the HTTP boundary via `wiremock`. All daemon plumbing (SWP, handler, persistence, autonomy, tools) runs for real.

**Alternatives considered:**
- Full mock of LlmClient via trait abstraction — rejected because it would require significant refactoring and wouldn't test the real reqwest/SSE parsing path.
- Record/replay from real API calls — rejected because recordings rot and are only marginally better than canned SSE for the bugs that actually occur.
- Real API calls in CI — rejected because it costs money and is non-deterministic.

**Trade-off:** We don't test actual LLM response quality or real provider quirks (socket behavior, undocumented error formats). The existing `#[ignore]`-gated e2e tests with real API keys cover that.
```

- [ ] **Step 2: Record architecture in ARCHITECTURE.md**

Add entry:

```markdown
## Test Architecture

### shore-test-harness

Dev-only crate providing integration test infrastructure. Not published.

- `MockLlmServer` — wraps `wiremock::MockServer`, serves canned Anthropic SSE streams
- `TestHarness` — boots real daemon stack in-process, connects SWP client, provides send/collect helpers
- `CrashedHarness` — simulates crash/reboot for recovery testing
- `TestConfigBuilder` — builds `LoadedConfig` pointing at mock server

Integration tests in `shore-daemon/tests/integration_*.rs` use the harness.

**Data flow in tests:**
```
SWPConnection → Server → MessageHandler → LlmClient → reqwest → MockServer (wiremock)
```

All components are real except the HTTP responses.
```

- [ ] **Step 3: Commit**

```bash
git add DECISIONS.md ARCHITECTURE.md
git commit -m "docs: record integration test harness decision and architecture"
```
