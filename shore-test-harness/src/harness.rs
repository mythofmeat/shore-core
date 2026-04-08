use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use shore_client::connection::{SWPConnection, ServerAddr};
use shore_config::LoadedConfig;
use shore_daemon::autonomy::manager::AutonomyManager;
use shore_daemon::characters::CharacterRegistry;
use shore_daemon::commands::{CommandContext, SessionTokens};
use shore_daemon::handler::MessageHandler;
use shore_daemon::server::{Server, ServerConfig};
use shore_ledger::LedgerClient;
use shore_llm_client::LlmClient;
use shore_protocol::server_msg::ServerMessage;
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
    pub socket_path: PathBuf,
    shutdown_tx: watch::Sender<()>,
    server_handle: JoinHandle<()>,
    handler_handle: JoinHandle<()>,
    pub config: LoadedConfig,
}

impl TestHarness {
    /// Boot with default `TestConfigBuilder` settings.
    pub async fn boot() -> Self {
        Self::boot_with(TestConfigBuilder::new()).await
    }

    /// Boot with a custom `TestConfigBuilder`.
    ///
    /// Follows the exact wiring pattern from `shore-daemon/tests/e2e.rs`
    /// (`E2EHarness::start`), but replaces the real LLM service with a
    /// `MockLlmServer` whose `base_url` is injected into the model catalog.
    pub async fn boot_with(builder: TestConfigBuilder) -> Self {
        // Start mock LLM server (wiremock HTTP).
        let mock_llm = MockLlmServer::start().await;

        // Create temp directory tree and build config.
        let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let config = builder.build(tmp_dir.path(), &mock_llm.base_url());

        let socket_path = tmp_dir.path().join("runtime").join("daemon.sock");
        let data_dir = config.dirs.data.clone();

        Self::wire_daemon(config, mock_llm, tmp_dir, data_dir, socket_path).await
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
        socket_path: PathBuf,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(());

        // ── SWP Server ────────────────────────────────────────────────
        let server_config = ServerConfig {
            socket_path: socket_path.clone(),
            tcp: None,
            server_name: "shore-test-harness".into(),
        };
        let server = Server::new(server_config);
        let push_tx = server.push_sender();
        let route_rx = server.take_route_rx();

        // ── Character Registry ────────────────────────────────────────
        let char_registry = Arc::new(tokio::sync::Mutex::new(CharacterRegistry::new(
            config.dirs.config.clone(),
            config.dirs.data.clone(),
            push_tx.clone(),
            config.clone(),
        )));

        // ── Autonomy Manager ──────────────────────────────────────────
        let (mut autonomy, _compaction_rx) = AutonomyManager::new(
            Default::default(),
            Default::default(),
            config.dirs.data.clone(),
            shutdown_rx.clone(),
        );

        // ── Ledger-wrapped LLM Client ────────────────────────────────
        let llm_client =
            LedgerClient::new(LlmClient::new(), &config.dirs.data.join("ledger.db"))
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
            push_tx: push_tx.clone(),
            data_dir: config.dirs.data.clone(),
            active_model: config.app.defaults.model.clone(),
            session_tokens: Arc::new(std::sync::Mutex::new(SessionTokens::default())),
            autonomy: autonomy.clone(),
            llm_client: llm_client.clone(),
            diagnostics: Arc::new(std::sync::Mutex::new(
                shore_diagnostics::Diagnostics::default(),
            )),
            memory_shell_sessions: HashMap::new(),
        };

        // ── Message Handler ──────────────────────────────────────────
        let mut msg_handler = MessageHandler {
            registry: char_registry,
            cmd_ctx,
            llm_client,
            push_tx: push_tx.clone(),
            is_first_after_restart: Arc::new(AtomicBool::new(true)),
            has_seen_cache_read: Arc::new(AtomicBool::new(false)),
            compaction_occurred: Arc::new(AtomicBool::new(false)),
            autonomy,
            notifier,
            generation_handle: None,
        };

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
        let (conn, server_hello, _history) = SWPConnection::connect(
            &ServerAddr::Unix(socket_path.display().to_string()),
            "test",
            "harness",
            None,
        )
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
            socket_path,
            shutdown_tx,
            server_handle,
            handler_handle,
            config,
        }
    }

    /// Simulate a crash: abort server and handler tasks without graceful shutdown,
    /// remove the stale socket file, and return a `CrashedHarness` for rebooting.
    pub async fn crash(self) -> crate::chaos::CrashedHarness {
        // Drop shutdown_tx without sending — no graceful shutdown signal.
        drop(self.shutdown_tx);

        // Abort both tasks immediately.
        self.server_handle.abort();
        self.handler_handle.abort();

        // Wait briefly so the socket FD is released before we remove it.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Remove stale socket so the next bind succeeds.
        let _ = std::fs::remove_file(&self.socket_path);

        crate::chaos::CrashedHarness {
            tmp_dir: self.tmp_dir,
            mock_llm: self.mock_llm,
            data_dir: self.data_dir,
            socket_path: self.socket_path,
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

    /// Graceful shutdown: signal the server and handler, then await both tasks.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        let _ = self.server_handle.await;
        let _ = self.handler_handle.await;
    }
}
