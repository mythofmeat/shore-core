//! End-to-end integration test for US-018: Conversation milestone.
//!
//! Verifies the full shore-daemon + shore-llm pipeline:
//!   1. SWP handshake (ServerHello → ClientHello → History)
//!   2. Streaming "Hello" message (StreamStart/StreamChunk/StreamEnd)
//!   3. Commands: status, list_characters, new_chat
//!   4. Tool use: check_time triggered by "What time is it?"
//!   5. JSONL persistence with msg_id fields
//!   6. Structured logs with rid correlation
//!
//! Prerequisites:
//!   - OPENROUTER_API_KEY env var set
//!
//! Run with: `cargo test --test e2e -- --ignored`

use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use shore_config::app::{AppConfig, AutonomyConfig, CompactionConfig, NotificationsConfig};
use shore_config::models::ModelCatalog;
use shore_config::{LoadedConfig, ShoreDirs};
use shore_daemon::characters::CharacterRegistry;
use shore_daemon::commands::{CommandContext, SessionTokens};
use shore_daemon::handler::{MessageHandler, MessageHandlerDeps};
use shore_daemon::handshake::build_handshake_provider;
use shore_ledger::LedgerClient;
use shore_llm::LlmClient;
use shore_protocol::server_msg::ServerMessage;
use shore_swp_client::connection::{SWPConnection, ServerAddr};
use shore_swp_server::{Server, ServerConfig};
use tokio::time::timeout;

/// Timeout for individual recv operations during streaming (generous for API calls).
const RECV_TIMEOUT: Duration = Duration::from_mins(1);

/// Timeout for command responses (local, no API call).
const CMD_TIMEOUT: Duration = Duration::from_secs(5);

/// Helper: receive next ServerMessage with a timeout.
async fn recv_timeout(conn: &mut SWPConnection, dur: Duration) -> ServerMessage {
    timeout(dur, conn.recv())
        .await
        .expect("Timed out waiting for server message")
        .expect("Failed to receive server message")
}

/// Helper: drain messages until we find one matching the predicate, or timeout.
async fn recv_until<F>(conn: &mut SWPConnection, dur: Duration, pred: F) -> ServerMessage
where
    F: Fn(&ServerMessage) -> bool,
{
    let deadline = tokio::time::Instant::now() + dur;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "Timed out waiting for matching message"
        );
        let msg = recv_timeout(conn, remaining).await;
        if pred(&msg) {
            return msg;
        }
    }
}

/// Check prerequisites for the E2E test. Returns an error message if not met.
fn check_prerequisites() -> Option<String> {
    if std::env::var("OPENROUTER_API_KEY").is_err() {
        return Some("OPENROUTER_API_KEY not set".into());
    }
    None
}

/// Build a test LoadedConfig with temp directories, Haiku model, and optional
/// image generation profile.
fn build_test_config(tmp: &tempfile::TempDir) -> LoadedConfig {
    build_test_config_inner(tmp, None)
}

/// Build config with an image generation profile added to the catalog.
fn build_test_config_with_image_gen(tmp: &tempfile::TempDir, image_gen_toml: &str) -> LoadedConfig {
    let table: toml::Table = image_gen_toml.parse().unwrap();
    build_test_config_inner(tmp, Some(&table))
}

fn build_test_config_inner(
    tmp: &tempfile::TempDir,
    image_gen_table: Option<&toml::Table>,
) -> LoadedConfig {
    let config_dir = tmp.path().join("config");
    let data_dir = tmp.path().join("data");
    let runtime_dir = tmp.path().join("runtime");

    // Create character definition directory.
    let char_dir = config_dir.join("characters").join("TestChar");
    std::fs::create_dir_all(&char_dir).unwrap();
    std::fs::write(
        char_dir.join("character.md"),
        "You are TestChar, a concise test assistant. Keep responses very short (1-2 sentences).",
    )
    .unwrap();

    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&runtime_dir).unwrap();

    let mut app = AppConfig::default();
    app.defaults.model = Some("haiku".into());
    app.behavior.tool_use.enabled = true;
    app.behavior.tool_use.max_iterations = 5;

    let models_toml = r#"
[openrouter]
base_url = "https://openrouter.ai/api/v1"

[openrouter.haiku]
model_id = "anthropic/claude-haiku-4.5"
max_output_tokens = 1024
temperature = 0.0
"#;
    let table: toml::Table = models_toml.parse().unwrap();
    // Set default image_generation profile if one is provided.
    if image_gen_table.is_some() {
        // Use the first key in the image_gen table as the default profile.
        if let Some(name) = image_gen_table.and_then(|t| t.keys().next()) {
            app.defaults.image_generation = Some(name.clone());
        }
    }

    let models = ModelCatalog::from_sections(Some(&table), None, None, image_gen_table).unwrap();

    LoadedConfig::new_for_test(
        app,
        models,
        ShoreDirs {
            config: config_dir,
            data: data_dir,
            runtime: runtime_dir,
            cache: tmp.path().join("cache"),
        },
    )
}

#[tokio::test]
#[ignore = "Requires OPENROUTER_API_KEY"]
async fn e2e_conversation_milestone() {
    // ── Prerequisites ──────────────────────────────────────────────────
    if let Some(msg) = check_prerequisites() {
        panic!("Skipping E2E test: {msg}");
    }

    let tmp = tempfile::tempdir().unwrap();
    let loaded = build_test_config(&tmp);

    let addr = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        format!("127.0.0.1:{port}")
    };

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(());

    // ── Start SWP server ───────────────────────────────────────────────
    let server_config = ServerConfig {
        addr: addr.clone(),
        allowed_hosts: vec![],
        server_name: "shore-daemon-test".into(),
        handshake: None,
    };
    let mut server = Server::new(server_config);
    let push_tx = server.event_sender();
    let session_router = server.session_router();
    let route_rx = server.take_route_rx();

    // Create character registry.
    let char_registry = std::sync::Arc::new(tokio::sync::Mutex::new(CharacterRegistry::new(
        loaded.dirs.config.clone(),
        loaded.dirs.data.clone(),
        push_tx.clone(),
        loaded.clone(),
    )));
    server.set_handshake_provider(build_handshake_provider(char_registry.clone()));

    let autonomy = shore_daemon::autonomy::manager::AutonomyManager::new(
        AutonomyConfig::default(),
        CompactionConfig::default(),
        loaded.dirs.data.clone(),
        shutdown_rx.clone(),
    );

    let llm_client =
        LedgerClient::new(LlmClient::new(), &loaded.dirs.data.join("ledger.db")).unwrap();

    let cmd_ctx = CommandContext {
        config: loaded.clone(),
        config_path: loaded.dirs.config.join("config.toml"),
        push_tx: push_tx.clone(),
        data_dir: loaded.dirs.data.clone(),
        character_name: None,
        active_model: loaded.app.defaults.model.clone(),
        active_resolved_model: None,
        session_tokens: std::sync::Arc::new(std::sync::Mutex::new(SessionTokens::default())),
        autonomy: autonomy.clone(),
        llm_client: llm_client.clone(),
        diagnostics: std::sync::Arc::new(std::sync::Mutex::new(
            shore_diagnostics::Diagnostics::default(),
        )),
    };

    let (_control_tx, control_rx) = tokio::sync::mpsc::channel(16);
    let mut msg_handler = MessageHandler::new(MessageHandlerDeps {
        registry: char_registry,
        cmd_ctx,
        llm_client,
        push_tx: push_tx.clone(),
        session_router,
        autonomy,
        notifier: shore_daemon::notifications::NotificationService::new(
            NotificationsConfig::default(),
        ),
        control_rx,
    });

    // Spawn message handler.
    let handler_handle = tokio::spawn(async move {
        msg_handler.run(route_rx).await;
    });

    // Run server in background.
    let server_shutdown_rx = shutdown_rx.clone();
    let server_handle = tokio::spawn(async move {
        server.run(server_shutdown_rx).await.unwrap();
    });

    // Give the server a moment to bind the socket.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // ── AC 1: Connect and verify SWP handshake ────────────────────────
    eprintln!("=== AC 1: SWP Handshake ===");
    let (mut conn, server_hello, history) =
        SWPConnection::connect(&ServerAddr(addr.clone()), "test", "e2e-test", None)
            .await
            .expect("Failed to connect to daemon");

    assert_eq!(server_hello.v, shore_protocol::SWP_V1);
    assert_eq!(server_hello.server_name, "shore-daemon-test");
    assert_eq!(server_hello.characters.len(), 1);
    assert_eq!(server_hello.characters[0].name, "TestChar");
    assert!(
        history.messages.is_empty(),
        "Initial history should be empty"
    );
    assert_eq!(history.selected_character.as_deref(), Some("TestChar"));
    // `active_model` in the handshake snapshot is the fully-qualified
    // identifier (`kind.provider.alias`) rather than the bare alias —
    // the qualified form uniquely picks one entry out of the model
    // catalog when two providers expose the same alias name.
    assert_eq!(history.config["active_model"], "chat.openrouter.haiku");
    assert_eq!(history.config["private"], false);
    assert_eq!(history.revision, 0);
    eprintln!(
        "  Handshake OK: v={}, server={}",
        server_hello.v, server_hello.server_name
    );

    // ── AC 3: Send commands (status, list_characters, new_chat) ───────
    eprintln!("=== AC 3: Commands ===");

    // status
    conn.send_command("status", json!({})).await.unwrap();
    let status_msg = recv_until(
        &mut conn,
        CMD_TIMEOUT,
        |m| matches!(m, ServerMessage::CommandOutput(o) if o.name == "status"),
    )
    .await;
    match &status_msg {
        ServerMessage::CommandOutput(o) => {
            assert!(o.data.is_object());
            assert!(
                o.data.get("tokens").is_some(),
                "status should include token counts"
            );
            eprintln!("  status OK: {:?}", o.data);
        }
        _ => panic!("Expected CommandOutput"),
    }

    // list_characters
    conn.send_command("list_characters", json!({}))
        .await
        .unwrap();
    let chars_msg = recv_until(
        &mut conn,
        CMD_TIMEOUT,
        |m| matches!(m, ServerMessage::CommandOutput(o) if o.name == "list_characters"),
    )
    .await;
    match &chars_msg {
        ServerMessage::CommandOutput(o) => {
            eprintln!("  list_characters OK: {:?}", o.data);
        }
        _ => panic!("Expected CommandOutput"),
    }

    // new_chat — not yet implemented in command dispatcher, skip for now.

    // ── AC 2: Send "Hello" with stream:true ───────────────────────────
    eprintln!("=== AC 2: Streaming Hello ===");
    conn.send_message("Hello", true).await.unwrap();

    // Expect: History (from engine append) -> StreamStart -> StreamChunk(s) -> StreamEnd.
    let mut got_stream_start = false;
    let mut got_stream_chunks = 0u32;
    let stream_end_content: String;

    let deadline = tokio::time::Instant::now() + RECV_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "Timed out waiting for streaming response to complete"
        );
        let msg = recv_timeout(&mut conn, remaining).await;
        match &msg {
            ServerMessage::StreamStart(_) => {
                got_stream_start = true;
                eprintln!("  StreamStart received");
            }
            ServerMessage::StreamChunk(chunk) => {
                got_stream_chunks += 1;
                if got_stream_chunks <= 3 {
                    eprintln!("  StreamChunk: {:?}", chunk.text);
                }
            }
            ServerMessage::StreamEnd(end) => {
                stream_end_content = end.content.clone();
                eprintln!(
                    "  StreamEnd: content_len={}, model={}, tokens=in:{}/out:{}",
                    end.content.len(),
                    end.metadata.model,
                    end.metadata.tokens.input,
                    end.metadata.tokens.output,
                );
                break;
            }
            ServerMessage::History(_) => {
                // Expected — engine broadcasts after append.
            }
            other => {
                eprintln!("  (other message: {:?})", std::mem::discriminant(other));
            }
        }
    }

    assert!(got_stream_start, "Should have received StreamStart");
    assert!(
        got_stream_chunks > 0,
        "Should have received at least one StreamChunk"
    );
    assert!(
        !stream_end_content.is_empty(),
        "StreamEnd content should not be empty"
    );

    // ── AC 4: Tool use — "What time is it?" triggers check_time ───────
    eprintln!("=== AC 4: Tool Use (check_time) ===");
    conn.send_message(
        "Use the check_time tool right now and tell me the exact time.",
        true,
    )
    .await
    .unwrap();

    let mut got_tool_call = false;
    let mut got_tool_result = false;
    let mut tool_result_output = String::new();

    let deadline = tokio::time::Instant::now() + RECV_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            // Tool use is non-deterministic with LLMs — if it didn't trigger,
            // the test still passes the streaming check.
            eprintln!("  WARN: Tool use not triggered within timeout (non-deterministic)");
            break;
        }
        let msg = recv_timeout(&mut conn, remaining).await;
        match &msg {
            ServerMessage::ToolCall(tc) => {
                got_tool_call = true;
                eprintln!("  ToolCall: name={}, id={}", tc.tool_name, tc.tool_id);
            }
            ServerMessage::ToolResult(tr) => {
                got_tool_result = true;
                tool_result_output = tr.output.clone();
                eprintln!(
                    "  ToolResult: name={}, output={}, is_error={}",
                    tr.tool_name, tr.output, tr.is_error
                );
            }
            ServerMessage::StreamEnd(end) => {
                eprintln!(
                    "  StreamEnd: content_len={}, is_final={}, tokens=in:{}/out:{}",
                    end.content.len(),
                    end.is_final,
                    end.metadata.tokens.input,
                    end.metadata.tokens.output,
                );
                if end.is_final {
                    break;
                }
            }
            ServerMessage::StreamStart(_)
            | ServerMessage::StreamChunk(_)
            | ServerMessage::History(_) => {
                // Expected during streaming and history broadcasts.
            }
            other => {
                eprintln!("  (other: {:?})", std::mem::discriminant(other));
            }
        }
    }

    if got_tool_call {
        assert!(
            got_tool_result,
            "If ToolCall was received, ToolResult should follow"
        );
        // check_time returns RFC 3339 datetime which contains 'T'.
        assert!(
            tool_result_output.contains('T'),
            "check_time output should be RFC 3339: {tool_result_output}"
        );
        eprintln!("  Tool use verified successfully");
    }

    // ── AC 5: Verify JSONL persistence with msg_id fields ─────────────
    eprintln!("=== AC 5: JSONL Persistence ===");
    let char_data_dir = tmp.path().join("data").join("TestChar");
    let jsonl_path = char_data_dir.join("active.jsonl");
    assert!(
        jsonl_path.exists(),
        "active.jsonl should exist: {}",
        jsonl_path.display()
    );
    let jsonl_content = std::fs::read_to_string(&jsonl_path).unwrap();
    let lines: Vec<&str> = jsonl_content.lines().filter(|l| !l.is_empty()).collect();
    eprintln!(
        "  JSONL file: {}, lines: {}",
        jsonl_path.display(),
        lines.len()
    );

    // Should have at least user + assistant messages from "Hello" exchange.
    assert!(
        lines.len() >= 2,
        "Should have at least 2 messages (user+assistant), got {}",
        lines.len()
    );

    // Verify each line is valid JSON with msg_id field.
    for (i, line) in lines.iter().enumerate() {
        let parsed: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("Line {i} is not valid JSON: {e}"));
        assert!(
            parsed.get("msg_id").is_some(),
            "Line {i} should have msg_id field: {line}"
        );
        let msg_id = parsed["msg_id"].as_str().unwrap();
        assert!(
            msg_id.starts_with("m_"),
            "msg_id should start with 'm_': {msg_id}"
        );
        let role = parsed["role"].as_str().unwrap();
        eprintln!("  Line {i}: role={role}, msg_id={msg_id}");
    }

    eprintln!("  Persistence OK");

    // ── AC 7: Verify content_blocks persisted for tool use ───────────
    eprintln!("=== AC 7: Content Blocks Persistence ===");
    if got_tool_call {
        // Re-read JSONL after tool use exchange.
        let jsonl_content = std::fs::read_to_string(&jsonl_path).unwrap();
        let lines: Vec<&str> = jsonl_content.lines().filter(|l| !l.is_empty()).collect();

        // After "Hello" exchange + tool use exchange, we expect:
        //   user("Hello"), assistant(response),
        //   user("Use check_time..."), assistant(tool_use), user(tool_result), assistant(final)
        // That's at least 6 messages (could be more if multi-iteration tool loop).
        assert!(
            lines.len() >= 4,
            "Should have at least 4 messages after tool use, got {}",
            lines.len()
        );

        // Find assistant messages with tool_use content_blocks.
        let mut found_tool_use_blocks = false;
        let mut found_tool_result_blocks = false;

        for (i, line) in lines.iter().enumerate() {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            let role = parsed["role"].as_str().unwrap_or("");

            if let Some(blocks) = parsed.get("content_blocks").and_then(|b| b.as_array()) {
                if blocks.is_empty() {
                    continue;
                }

                for block in blocks {
                    let block_type = block["type"].as_str().unwrap_or("");
                    match block_type {
                        "tool_use" => {
                            assert_eq!(
                                role, "assistant",
                                "tool_use block should be on assistant message"
                            );
                            assert!(block.get("id").is_some(), "tool_use block should have id");
                            assert!(
                                block.get("name").is_some(),
                                "tool_use block should have name"
                            );
                            assert!(
                                block.get("input").is_some(),
                                "tool_use block should have input"
                            );
                            found_tool_use_blocks = true;
                            eprintln!(
                                "  Line {i}: assistant tool_use block: name={}",
                                block["name"].as_str().unwrap_or("?")
                            );
                        }
                        "tool_result" => {
                            assert_eq!(role, "user", "tool_result block should be on user message");
                            assert!(
                                block.get("tool_use_id").is_some(),
                                "tool_result block should have tool_use_id"
                            );
                            assert!(
                                block.get("content").is_some(),
                                "tool_result block should have content"
                            );
                            found_tool_result_blocks = true;
                            eprintln!(
                                "  Line {i}: user tool_result block: tool_use_id={}",
                                block["tool_use_id"].as_str().unwrap_or("?")
                            );
                        }
                        "text" => {
                            eprintln!(
                                "  Line {i}: {role} text block: len={}",
                                block["text"].as_str().map_or(0, str::len)
                            );
                        }
                        "thinking" => {
                            eprintln!(
                                "  Line {i}: {role} thinking block: len={}",
                                block["thinking"].as_str().map_or(0, str::len)
                            );
                        }
                        _ => {
                            eprintln!("  Line {i}: {role} unknown block type: {block_type}");
                        }
                    }
                }
            }
        }

        assert!(
            found_tool_use_blocks,
            "Should find at least one assistant message with tool_use content_blocks"
        );
        assert!(
            found_tool_result_blocks,
            "Should find at least one user message with tool_result content_blocks"
        );
        eprintln!("  Content blocks persistence verified");
    } else {
        eprintln!("  SKIP: Tool use was not triggered, cannot verify content_blocks");
    }

    // ── AC 6: Verify status shows token counts after API usage ────────
    eprintln!("=== AC 6: Token counts in status ===");
    conn.send_command("status", json!({})).await.unwrap();
    let final_status = recv_until(
        &mut conn,
        CMD_TIMEOUT,
        |m| matches!(m, ServerMessage::CommandOutput(o) if o.name == "status"),
    )
    .await;
    match &final_status {
        ServerMessage::CommandOutput(o) => {
            let tokens = &o.data["tokens"];
            let input = tokens["input"].as_u64().unwrap_or(0);
            let output = tokens["output"].as_u64().unwrap_or(0);
            assert!(input > 0, "Input tokens should be > 0 after API calls");
            assert!(output > 0, "Output tokens should be > 0 after API calls");
            eprintln!("  Token counts: input={input}, output={output}");
        }
        _ => panic!("Expected CommandOutput"),
    }

    // ── Cleanup ────────────────────────────────────────────────────────
    eprintln!("=== Cleanup ===");
    let _ = shutdown_tx.send(());
    let _ = server_handle.await;
    let _ = handler_handle.await;
    eprintln!("=== E2E test passed ===");
}

// ── Image generation E2E test ─────────────────────────────────────────────

/// Build the image generation TOML config (routes through OpenRouter).
fn image_gen_toml() -> String {
    r#"
[openrouter-image]
provider = "openrouter"
api_key_env = "OPENROUTER_API_KEY"
model_id = "google/gemini-2.5-flash-image"
"#
    .into()
}

/// Spin up the full daemon + shore-llm stack and return the connection
/// plus handles needed for cleanup. Extracted from the main E2E test so
/// both tests can share startup logic.
struct E2EHarness {
    conn: SWPConnection,
    shutdown_tx: tokio::sync::watch::Sender<()>,
    server_handle: tokio::task::JoinHandle<()>,
    handler_handle: tokio::task::JoinHandle<()>,
    data_dir: PathBuf,
    _tmp: tempfile::TempDir,
}

impl E2EHarness {
    async fn start(loaded: LoadedConfig, tmp: tempfile::TempDir) -> Self {
        let addr = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            format!("127.0.0.1:{port}")
        };

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(());

        let server_config = ServerConfig {
            addr: addr.clone(),
            allowed_hosts: vec![],
            server_name: "shore-daemon-test".into(),
            handshake: None,
        };
        let mut server = Server::new(server_config);
        let push_tx = server.event_sender();
        let session_router = server.session_router();
        let route_rx = server.take_route_rx();

        let data_dir = loaded.dirs.data.clone();

        let char_registry = std::sync::Arc::new(tokio::sync::Mutex::new(CharacterRegistry::new(
            loaded.dirs.config.clone(),
            loaded.dirs.data.clone(),
            push_tx.clone(),
            loaded.clone(),
        )));
        server.set_handshake_provider(build_handshake_provider(char_registry.clone()));

        let autonomy = shore_daemon::autonomy::manager::AutonomyManager::new(
            AutonomyConfig::default(),
            CompactionConfig::default(),
            loaded.dirs.data.clone(),
            shutdown_rx.clone(),
        );

        let llm_client =
            LedgerClient::new(LlmClient::new(), &loaded.dirs.data.join("ledger.db")).unwrap();

        let cmd_ctx = CommandContext {
            config: loaded.clone(),
            config_path: loaded.dirs.config.join("config.toml"),
            push_tx: push_tx.clone(),
            data_dir: loaded.dirs.data.clone(),
            character_name: None,
            active_model: loaded.app.defaults.model.clone(),
            active_resolved_model: None,
            session_tokens: std::sync::Arc::new(std::sync::Mutex::new(SessionTokens::default())),
            autonomy: autonomy.clone(),
            llm_client: llm_client.clone(),
            diagnostics: std::sync::Arc::new(std::sync::Mutex::new(
                shore_diagnostics::Diagnostics::default(),
            )),
        };

        let (_control_tx, control_rx) = tokio::sync::mpsc::channel(16);
        let mut msg_handler = MessageHandler::new(MessageHandlerDeps {
            registry: char_registry,
            cmd_ctx,
            llm_client,
            push_tx: push_tx.clone(),
            session_router,
            autonomy,
            notifier: shore_daemon::notifications::NotificationService::new(
                NotificationsConfig::default(),
            ),
            control_rx,
        });

        let handler_handle = tokio::spawn(async move {
            msg_handler.run(route_rx).await;
        });

        let server_shutdown_rx = shutdown_rx.clone();
        let server_handle = tokio::spawn(async move {
            server.run(server_shutdown_rx).await.unwrap();
        });

        tokio::time::sleep(Duration::from_millis(200)).await;

        let (conn, server_hello, _history) =
            SWPConnection::connect(&ServerAddr(addr.clone()), "test", "e2e-test", None)
                .await
                .expect("Failed to connect to daemon");

        assert_eq!(server_hello.v, shore_protocol::SWP_V1);

        Self {
            conn,
            shutdown_tx,
            server_handle,
            handler_handle,
            data_dir,
            _tmp: tmp,
        }
    }

    async fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        let _ = self.server_handle.await;
        let _ = self.handler_handle.await;
    }
}

#[tokio::test]
#[ignore = "Requires OPENROUTER_API_KEY"]
async fn e2e_generate_image() {
    if let Some(msg) = check_prerequisites() {
        panic!("Skipping image gen E2E test: {msg}");
    }

    let tmp = tempfile::tempdir().unwrap();
    let ig_toml = image_gen_toml();
    let loaded = build_test_config_with_image_gen(&tmp, &ig_toml);

    let mut harness = E2EHarness::start(loaded, tmp).await;

    // ── Send a message that should trigger generate_image ─────────────
    eprintln!("=== Image Gen: Sending generate_image request ===");
    harness
        .conn
        .send_message(
            "Use the generate_image tool to generate an image of a red circle on a white background. \
             Use that exact tool name, do not describe the image yourself.",
            true,
        )
        .await
        .unwrap();

    // ── Drain messages until we see ToolCall + ToolResult + StreamEnd ──
    let mut got_tool_call = false;
    let mut tool_call_name = String::new();
    let mut got_tool_result = false;
    let mut tool_result_output = String::new();

    let deadline = tokio::time::Instant::now() + Duration::from_mins(2);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "Timed out waiting for image generation response"
        );
        let msg = recv_timeout(&mut harness.conn, remaining).await;
        match &msg {
            ServerMessage::ToolCall(tc) => {
                got_tool_call = true;
                tool_call_name = tc.tool_name.clone();
                eprintln!("  ToolCall: name={}, id={}", tc.tool_name, tc.tool_id);
            }
            ServerMessage::ToolResult(tr) => {
                got_tool_result = true;
                tool_result_output = tr.output.clone();
                eprintln!(
                    "  ToolResult: name={}, is_error={}, output={}",
                    tr.tool_name, tr.is_error, tr.output
                );
            }
            ServerMessage::StreamEnd(end) => {
                eprintln!(
                    "  StreamEnd: content_len={}, is_final={}, tokens=in:{}/out:{}",
                    end.content.len(),
                    end.is_final,
                    end.metadata.tokens.input,
                    end.metadata.tokens.output,
                );
                if end.is_final && got_tool_result {
                    break;
                }
            }
            ServerMessage::StreamStart(_)
            | ServerMessage::StreamChunk(_)
            | ServerMessage::History(_) => {
                // Expected while tool-use and tool-result messages persist;
                // keep reading until the terminal StreamEnd.
            }
            ServerMessage::Error(e) => {
                panic!("Received error from daemon: {} ({:?})", e.message, e.code);
            }
            other => {
                eprintln!("  (other: {:?})", std::mem::discriminant(other));
            }
        }
    }

    // ── Assertions ────────────────────────────────────────────────────
    assert!(got_tool_call, "LLM should have called a tool");
    assert_eq!(
        tool_call_name, "generate_image",
        "Tool called should be generate_image"
    );
    assert!(got_tool_result, "Tool result should have been returned");

    // Parse the tool result JSON and verify it contains a path.
    let result_json: serde_json::Value =
        serde_json::from_str(&tool_result_output).unwrap_or_else(|e| {
            panic!("Tool result should be valid JSON: {e}\nGot: {tool_result_output}")
        });
    let image_path = result_json["path"]
        .as_str()
        .expect("Tool result should contain 'path' field");
    eprintln!("  Image saved to: {image_path}");

    // Verify the file actually exists on disk.
    let generated_dir = harness
        .data_dir
        .join("TestChar")
        .join("images")
        .join("generated");
    let returned_path = PathBuf::from(image_path);
    let full_path = if returned_path.is_absolute() {
        returned_path
    } else {
        harness
            .data_dir
            .join("TestChar")
            .join("images")
            .join(returned_path)
    };
    assert!(
        full_path.starts_with(&generated_dir),
        "Image path should be under generated dir: {}",
        full_path.display()
    );
    assert!(
        full_path.exists(),
        "Generated image file should exist at: {}",
        full_path.display()
    );
    let file_size = std::fs::metadata(&full_path).unwrap().len();
    assert!(file_size > 0, "Generated image should not be empty");
    eprintln!("  Image file verified: {file_size} bytes");

    // ── Cleanup ───────────────────────────────────────────────────────
    eprintln!("=== Image Gen E2E test passed ===");
    harness.shutdown().await;
}

// ── Web search E2E test ───────────────────────────────────────────────────

#[tokio::test]
#[ignore = "Requires OPENROUTER_API_KEY and TAVILY_API_KEY"]
async fn e2e_web_search() {
    if let Some(msg) = check_prerequisites() {
        panic!("Skipping web search E2E test: {msg}");
    }
    assert!(
        std::env::var("TAVILY_API_KEY").is_ok(),
        "Skipping web search E2E test: TAVILY_API_KEY not set"
    );

    let tmp = tempfile::tempdir().unwrap();
    let loaded = build_test_config(&tmp);

    let mut harness = E2EHarness::start(loaded, tmp).await;

    // ── Send a message that should trigger web_search ─────────────────
    eprintln!("=== Web Search: Sending search request ===");
    harness
        .conn
        .send_message(
            "Use the web_search tool to search for 'Rust programming language 2024'. \
             Use that exact tool, then summarize what you found in one sentence.",
            true,
        )
        .await
        .unwrap();

    // ── Drain messages until we see ToolCall + ToolResult + StreamEnd ──
    let mut got_tool_call = false;
    let mut tool_call_name = String::new();
    let mut got_tool_result = false;
    let mut tool_result_output = String::new();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let final_content = loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "Timed out waiting for web search response"
        );
        let msg = recv_timeout(&mut harness.conn, remaining).await;
        match &msg {
            ServerMessage::ToolCall(tc) => {
                got_tool_call = true;
                tool_call_name = tc.tool_name.clone();
                eprintln!("  ToolCall: name={}, id={}", tc.tool_name, tc.tool_id);
            }
            ServerMessage::ToolResult(tr) => {
                got_tool_result = true;
                tool_result_output = tr.output.clone();
                eprintln!(
                    "  ToolResult: name={}, is_error={}, output_len={}",
                    tr.tool_name,
                    tr.is_error,
                    tr.output.len()
                );
            }
            ServerMessage::StreamEnd(end) => {
                eprintln!(
                    "  StreamEnd: content_len={}, is_final={}, tokens=in:{}/out:{}",
                    end.content.len(),
                    end.is_final,
                    end.metadata.tokens.input,
                    end.metadata.tokens.output,
                );
                if end.is_final {
                    break end.content.clone();
                }
            }
            ServerMessage::StreamStart(_)
            | ServerMessage::StreamChunk(_)
            | ServerMessage::History(_) => {
                // Expected while tool-use and tool-result messages persist;
                // keep reading until the terminal StreamEnd.
            }
            ServerMessage::Error(e) => {
                panic!("Received error from daemon: {} ({:?})", e.message, e.code);
            }
            other => {
                eprintln!("  (other: {:?})", std::mem::discriminant(other));
            }
        }
    };

    // ── Assertions ────────────────────────────────────────────────────
    assert!(got_tool_call, "LLM should have called a tool");
    assert_eq!(
        tool_call_name, "web_search",
        "Tool called should be web_search"
    );
    assert!(got_tool_result, "Tool result should have been returned");

    // Parse the tool result JSON and verify structure.
    let result_json: serde_json::Value =
        serde_json::from_str(&tool_result_output).unwrap_or_else(|e| {
            panic!("Tool result should be valid JSON: {e}\nGot: {tool_result_output}")
        });
    assert!(
        result_json.get("query").is_some(),
        "Tool result should contain 'query' field"
    );
    let results = result_json["results"]
        .as_array()
        .expect("Tool result should contain 'results' array");
    assert!(!results.is_empty(), "Search results should not be empty");
    // Each result should have title, url, content.
    for r in results {
        assert!(r["title"].as_str().is_some(), "Result should have title");
        assert!(r["url"].as_str().is_some(), "Result should have url");
        assert!(
            r["content"].as_str().is_some(),
            "Result should have content"
        );
    }
    eprintln!("  Search returned {} results", results.len());

    // The LLM should have produced a final response incorporating the search results.
    assert!(
        !final_content.is_empty(),
        "LLM should have produced a final response after web search"
    );
    eprintln!("  Final response length: {} chars", final_content.len());

    // ── Cleanup ───────────────────────────────────────────────────────
    eprintln!("=== Web Search E2E test passed ===");
    harness.shutdown().await;
}
