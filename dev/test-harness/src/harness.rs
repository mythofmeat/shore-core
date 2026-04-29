use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use shore_config::LoadedConfig;
use shore_daemon::autonomy::manager::AutonomyManager;
use shore_daemon::characters::CharacterRegistry;
use shore_daemon::commands::{CommandContext, SessionTokens};
use shore_daemon::handler::{MessageHandler, MessageHandlerDeps};
use shore_daemon::handshake::build_handshake_provider;
use shore_ledger::LedgerClient;
use shore_llm::LlmClient;
use shore_protocol::server_msg::ServerMessage;
use shore_swp_client::connection::{SWPConnection, ServerAddr};
use shore_swp_server::{Server, ServerConfig};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::info;

use crate::collected::CollectedResponse;
use crate::config::TestConfigBuilder;
use crate::mock_llm::MockLlmServer;

/// Timeout for collecting a full streamed response.
const COLLECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Timeout for command responses (local, no LLM call).
const CMD_TIMEOUT: Duration = Duration::from_secs(5);

/// Boots a real daemon in-process with a mock LLM backend and provides
/// a connected SWP client with send/collect helpers.
pub struct TestHarness {
    pub conn: SWPConnection,
    pub mock_llm: MockLlmServer,
    pub tmp_dir: tempfile::TempDir,
    pub data_dir: PathBuf,
    pub addr: String,
    shutdown_tx: watch::Sender<()>,
    server_handle: JoinHandle<()>,
    handler_handle: JoinHandle<()>,
    pub config: LoadedConfig,
    /// Exposed so integration tests can drive heartbeat ticks deterministically
    /// (e.g. `autonomy.heartbeat_tick_now(character)` followed by a virtual-time
    /// advance to fire the tick loop).
    pub autonomy: AutonomyManager,
    // Stored for `trigger_compaction_now`.
    llm_client: LedgerClient,
    notifier: shore_daemon::notifications::NotificationService,
}

impl TestHarness {
    /// Boot with default `TestConfigBuilder` settings.
    pub async fn boot() -> Self {
        Self::boot_with(TestConfigBuilder::new()).await
    }

    /// Boot with a custom `TestConfigBuilder`.
    ///
    /// Follows the exact wiring pattern from `backend/daemon/tests/e2e.rs`
    /// (`E2EHarness::start`), but replaces the real LLM service with a
    /// `MockLlmServer` whose `base_url` is injected into the model catalog.
    pub async fn boot_with(builder: TestConfigBuilder) -> Self {
        // Start mock LLM server (wiremock HTTP).
        let mock_llm = MockLlmServer::start().await;

        // Create temp directory tree and build config.
        let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let config = builder.build(tmp_dir.path(), &mock_llm.base_url());

        let addr = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            format!("127.0.0.1:{port}")
        };
        let data_dir = config.dirs.data.clone();

        Self::wire_daemon(config, mock_llm, tmp_dir, data_dir, addr).await
    }

    /// Internal: wire up daemon components and connect a SWP client.
    ///
    /// Called by both `boot_with` (fresh start) and `CrashedHarness::reboot`
    /// (restart from existing state on disk).
    pub(crate) async fn wire_daemon(
        config: LoadedConfig,
        mock_llm: MockLlmServer,
        tmp_dir: tempfile::TempDir,
        data_dir: PathBuf,
        addr: String,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(());

        // ── SWP Server ────────────────────────────────────────────────
        let server_config = ServerConfig {
            addr: addr.clone(),
            allowed_hosts: vec![],
            server_name: "shore-test-harness".into(),
            handshake: None,
        };
        let mut server = Server::new(server_config);
        let push_tx = server.event_sender();
        let session_router = server.session_router();
        let route_rx = server.take_route_rx();

        // ── Character Registry ────────────────────────────────────────
        let char_registry = Arc::new(tokio::sync::Mutex::new(CharacterRegistry::new(
            config.dirs.config.clone(),
            config.dirs.data.clone(),
            push_tx.clone(),
            config.clone(),
        )));
        server.set_handshake_provider(build_handshake_provider(char_registry.clone()));

        // ── Autonomy Manager ──────────────────────────────────────────
        let mut autonomy = AutonomyManager::new(
            config.app.behavior.autonomy.clone(),
            config.app.memory.compaction.clone(),
            config.dirs.data.clone(),
            shutdown_rx.clone(),
        );

        // ── Ledger-wrapped LLM Client ────────────────────────────────
        let llm_client = LedgerClient::new(LlmClient::new(), &config.dirs.data.join("ledger.db"))
            .expect("failed to create LedgerClient");

        let notifier = shore_daemon::notifications::NotificationService::new(Default::default());

        // Wire up autonomy with LLM resources (mirrors main.rs wiring).
        autonomy.set_resources(
            llm_client.clone(),
            push_tx.clone(),
            config.clone(),
            notifier.clone(),
        );
        autonomy.set_registry(char_registry.clone());

        // ── Command Context ──────────────────────────────────────────
        let cmd_ctx = CommandContext {
            config: config.clone(),
            config_path: config.dirs.config.join("config.toml"),
            push_tx: push_tx.clone(),
            data_dir: config.dirs.data.clone(),
            active_model: config.app.defaults.model.clone(),
            reasoning_effort_override: None,
            session_tokens: Arc::new(std::sync::Mutex::new(SessionTokens::default())),
            autonomy: autonomy.clone(),
            llm_client: llm_client.clone(),
            diagnostics: Arc::new(std::sync::Mutex::new(
                shore_diagnostics::Diagnostics::default(),
            )),
        };

        // Clone for storage in TestHarness (before ownership is moved into msg_handler).
        let stored_llm_client = llm_client.clone();
        let stored_notifier = notifier.clone();
        let stored_autonomy = autonomy.clone();

        // ── Message Handler ──────────────────────────────────────────
        let (_control_tx, control_rx) = tokio::sync::mpsc::channel(16);
        let mut msg_handler = MessageHandler::new(MessageHandlerDeps {
            registry: char_registry,
            cmd_ctx,
            llm_client,
            push_tx: push_tx.clone(),
            session_router,
            autonomy,
            notifier,
            live_speak: Arc::new(AtomicBool::new(false)),
            tts_client: None,
            control_rx,
        });

        // Spawn handler loop.
        let handler_handle = tokio::spawn(async move {
            msg_handler.run(route_rx).await;
        });

        // Spawn server loop.
        let server_shutdown_rx = shutdown_rx.clone();
        let server_handle = tokio::spawn(async move {
            server.run(server_shutdown_rx).await.unwrap();
        });

        // Give the server time to bind the socket.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // ── SWP Client Connection ────────────────────────────────────
        let (conn, server_hello, _history) =
            SWPConnection::connect(&ServerAddr(addr.clone()), "test", "harness", None)
                .await
                .expect("failed to connect to daemon");

        assert_eq!(
            server_hello.v,
            shore_protocol::SWP_V1,
            "SWP protocol version mismatch"
        );
        info!(
            server_name = %server_hello.server_name,
            "TestHarness booted"
        );

        Self {
            conn,
            mock_llm,
            tmp_dir,
            data_dir,
            addr,
            shutdown_tx,
            server_handle,
            handler_handle,
            config,
            autonomy: stored_autonomy,
            llm_client: stored_llm_client,
            notifier: stored_notifier,
        }
    }

    /// Directly trigger compaction for a character, bypassing the 30-second
    /// autonomy tick.  Useful in tests where you don't want to wait 30s.
    ///
    /// Enqueue mock responses (compaction LLM call + embedding calls) BEFORE
    /// calling this method.
    pub async fn trigger_compaction_now(&self, character: &str) {
        match shore_daemon::memory::compaction::run_compaction(
            character,
            &self.config,
            &self.llm_client,
            &self.data_dir,
            &self.notifier,
        )
        .await
        {
            Ok(retained) => {
                info!(
                    character,
                    retained, "trigger_compaction_now: compaction complete"
                );
            }
            Err(e) => {
                info!(character, error = %e, "trigger_compaction_now: compaction failed");
            }
        }
    }

    /// Simulate a crash: abort server and handler tasks without graceful shutdown,
    /// and return a `CrashedHarness` for rebooting.
    pub async fn crash(self) -> crate::chaos::CrashedHarness {
        // Drop shutdown_tx without sending — no graceful shutdown signal.
        drop(self.shutdown_tx);

        // Abort both tasks immediately.
        self.server_handle.abort();
        self.handler_handle.abort();

        // Wait briefly so the port is released before the next bind.
        tokio::time::sleep(Duration::from_millis(50)).await;

        crate::chaos::CrashedHarness {
            tmp_dir: self.tmp_dir,
            mock_llm: self.mock_llm,
            data_dir: self.data_dir,
            addr: self.addr,
        }
    }

    /// Send a user message and collect the full streamed response.
    ///
    /// Enqueues nothing on the mock — caller must call `mock_llm.enqueue_text()`
    /// (or similar) before calling this method.
    pub async fn send_and_collect(&mut self, text: &str) -> CollectedResponse {
        self.conn
            .send_message(text, true)
            .await
            .expect("failed to send message");
        self.collect_stream().await
    }

    /// Collect server messages until `StreamEnd` or `Error`, with a 30s timeout.
    pub async fn collect_stream(&mut self) -> CollectedResponse {
        let mut collected = CollectedResponse::new();
        let deadline = tokio::time::Instant::now() + COLLECT_TIMEOUT;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                panic!(
                    "collect_stream timed out after {:?}; collected {} messages so far",
                    COLLECT_TIMEOUT,
                    collected.raw_messages.len(),
                );
            }

            let msg = timeout(remaining, self.conn.recv())
                .await
                .expect("collect_stream timed out waiting for message")
                .expect("failed to recv server message");

            if collected.push(msg) {
                return collected;
            }
        }
    }

    /// Send a slash command and collect responses until `CommandOutput` or timeout.
    pub async fn send_command(&mut self, cmd: &str) -> Vec<ServerMessage> {
        self.conn
            .send_command(cmd, json!({}))
            .await
            .expect("failed to send command");

        let mut messages = Vec::new();
        let deadline = tokio::time::Instant::now() + CMD_TIMEOUT;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }

            match timeout(remaining, self.conn.recv()).await {
                Ok(Ok(msg)) => {
                    let is_terminal = matches!(
                        &msg,
                        ServerMessage::CommandOutput(_) | ServerMessage::Error(_)
                    );
                    messages.push(msg);
                    if is_terminal {
                        break;
                    }
                }
                Ok(Err(_)) => break,
                Err(_) => break,
            }
        }

        messages
    }

    /// Read all persisted JSONL messages from the data directory.
    ///
    /// Walks `{data_dir}/{character}/active.jsonl` files and parses each line.
    pub fn read_persisted_messages(&self) -> Vec<serde_json::Value> {
        let mut messages = Vec::new();

        let entries = match std::fs::read_dir(&self.data_dir) {
            Ok(e) => e,
            Err(_) => return messages,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let jsonl_path = path.join("active.jsonl");
            if !jsonl_path.exists() {
                continue;
            }
            let content = match std::fs::read_to_string(&jsonl_path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for line in content.lines() {
                if line.is_empty() {
                    continue;
                }
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                    messages.push(val);
                }
            }
        }

        messages
    }

    /// Read all ledger entries from the SQLite database at `{data_dir}/ledger.db`.
    ///
    /// Returns each row from the `calls` table as a `serde_json::Value` object.
    /// The caller should give the daemon time to flush (e.g. a short sleep) before
    /// calling this, as ledger writes happen synchronously but may lag slightly.
    pub fn read_ledger_entries(&self) -> Vec<serde_json::Value> {
        let db_path = self.data_dir.join("ledger.db");
        let conn = match rusqlite::Connection::open(&db_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        let mut stmt = match conn.prepare(
            "SELECT id, ts, character, provider, model, call_type, \
             input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, \
             cache_ttl, total_ms, ttft_ms, finish_reason, thinking_enabled, \
             cache_state, cache_anomaly, input_cost, output_cost, \
             cache_read_cost, cache_write_cost, total_cost \
             FROM calls ORDER BY id ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let rows = stmt.query_map([], |row| {
            Ok(serde_json::json!({
                "id":                 row.get::<_, i64>(0)?,
                "ts":                 row.get::<_, String>(1)?,
                "character":          row.get::<_, String>(2)?,
                "provider":           row.get::<_, String>(3)?,
                "model":              row.get::<_, String>(4)?,
                "call_type":          row.get::<_, String>(5)?,
                "input_tokens":       row.get::<_, i64>(6)?,
                "output_tokens":      row.get::<_, i64>(7)?,
                "cache_read_tokens":  row.get::<_, i64>(8)?,
                "cache_write_tokens": row.get::<_, i64>(9)?,
                "cache_ttl":          row.get::<_, Option<String>>(10)?,
                "total_ms":           row.get::<_, i64>(11)?,
                "ttft_ms":            row.get::<_, i64>(12)?,
                "finish_reason":      row.get::<_, String>(13)?,
                "thinking_enabled":   row.get::<_, i64>(14)? != 0,
                "cache_state":        row.get::<_, Option<String>>(15)?,
                "cache_anomaly":      row.get::<_, Option<String>>(16)?,
                "input_cost":         row.get::<_, Option<f64>>(17)?,
                "output_cost":        row.get::<_, Option<f64>>(18)?,
                "cache_read_cost":    row.get::<_, Option<f64>>(19)?,
                "cache_write_cost":   row.get::<_, Option<f64>>(20)?,
                "total_cost":         row.get::<_, Option<f64>>(21)?,
            }))
        });

        match rows {
            Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Connect an additional SWP client to the same daemon.
    pub async fn connect_second_client(&self) -> SWPConnection {
        let (conn, _hello, _history) = SWPConnection::connect(
            &ServerAddr(self.addr.clone()),
            "test",
            "second-client",
            None,
        )
        .await
        .expect("Failed to connect second client");
        conn
    }

    /// Graceful shutdown: signal the server and handler, then await both tasks.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        let _ = self.server_handle.await;
        let _ = self.handler_handle.await;
    }
}
