use shore_client::{SWPConnection, ServerAddr};
use shore_protocol::server_msg::ServerMessage;

use crate::cli::{Cli, CliCommand};
use crate::output;

/// Execute the CLI command by connecting to the daemon and dispatching.
pub async fn execute(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let addr = resolve_addr(&cli);
    let (mut conn, _server_hello, _history) =
        SWPConnection::connect(&addr, "cli", "shore-cli").await?;

    match &cli.command {
        CliCommand::Send { message } => {
            let text = message.join(" ");
            conn.send_message(&text, true).await?;
            recv_streaming_response(&mut conn).await?;
        }
        CliCommand::Regen { guidance } => {
            conn.send_regen(true, guidance.clone()).await?;
            recv_streaming_response(&mut conn).await?;
        }
        other => {
            let (name, args) = crate::cli::to_swp_command(other)
                .expect("non-send/regen command must map to SWP command");
            conn.send_command(name, args).await?;
            recv_command_response(&mut conn).await?;
        }
    }

    Ok(())
}

/// Resolve the daemon address from CLI flags or discovery.
fn resolve_addr(cli: &Cli) -> ServerAddr {
    if let Some(socket) = &cli.socket {
        if shore_client::connection::is_unix_path(socket) {
            return ServerAddr::Unix(socket.clone());
        }
        return ServerAddr::Tcp(socket.clone());
    }
    shore_client::discover_or_default(cli.config.as_deref())
}

/// Receive and render a streaming response (for send/regen).
async fn recv_streaming_response(
    conn: &mut SWPConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        let msg = conn.recv().await?;
        match &msg {
            ServerMessage::StreamStart(start) => {
                output::print_stream_start(start.regen);
            }
            ServerMessage::StreamChunk(chunk) => {
                output::print_chunk(chunk);
            }
            ServerMessage::StreamEnd(end) => {
                output::print_stream_end(end);
                return Ok(());
            }
            ServerMessage::Error(err) => {
                output::print_server_error(
                    &serde_json::to_string(&err.code).unwrap_or_default(),
                    &err.message,
                );
                return Err(err.message.clone().into());
            }
            ServerMessage::Phase(_) => {
                // Phase changes are informational during streaming, ignore in CLI
            }
            ServerMessage::Ping(_) => {
                // Keepalive, ignore
            }
            _ => {
                // Other messages during streaming are unexpected but not fatal
            }
        }
    }
}

/// Receive a command response.
async fn recv_command_response(
    conn: &mut SWPConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        let msg = conn.recv().await?;
        match &msg {
            ServerMessage::CommandOutput(co) => {
                output::print_command_output(co);
                return Ok(());
            }
            ServerMessage::Error(err) => {
                output::print_server_error(
                    &serde_json::to_string(&err.code).unwrap_or_default(),
                    &err.message,
                );
                return Err(err.message.clone().into());
            }
            ServerMessage::Ping(_) => {
                // Keepalive, ignore
            }
            ServerMessage::History(_) => {
                // Some commands trigger a history push; we just print and continue
            }
            _ => {
                // Unexpected but not fatal
            }
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use tokio::io::duplex;
    use tokio::io::AsyncWriteExt;

    use shore_protocol::client_msg::ClientMessage;
    use shore_protocol::server_msg::*;
    use shore_protocol::types::*;
    use shore_protocol::SWP_V1;

    use crate::cli::{Cli, CliCommand};

    /// Helper: write a JSON line to a writer.
    async fn write_json_line<W: AsyncWriteExt + Unpin, T: serde::Serialize>(w: &mut W, val: &T) {
        let line = serde_json::to_string(val).unwrap();
        w.write_all(line.as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();
        w.flush().await.unwrap();
    }

    /// Helper: read one JSON line from a reader.
    async fn read_json_line<
        R: tokio::io::AsyncBufReadExt + Unpin,
        T: serde::de::DeserializeOwned,
    >(
        r: &mut R,
    ) -> T {
        let mut line = String::new();
        r.read_line(&mut line).await.unwrap();
        serde_json::from_str(line.trim()).unwrap()
    }

    /// Spawn a mock SWP server that completes the handshake, reads one client
    /// message, and responds with the given server messages.
    async fn mock_server(
        server_stream: tokio::io::DuplexStream,
        responses: Vec<ServerMessage>,
    ) -> ClientMessage {
        let (r, mut w) = tokio::io::split(server_stream);
        let mut reader = tokio::io::BufReader::new(r);

        // Handshake: send server hello
        let hello = ServerMessage::Hello(ServerHello {
            v: SWP_V1,
            server_name: "test-daemon".into(),
            characters: vec![],
        });
        write_json_line(&mut w, &hello).await;

        // Read client hello
        let _client_hello: ClientMessage = read_json_line(&mut reader).await;

        // Send empty history
        let history = ServerMessage::History(History {
            messages: vec![],
            config: serde_json::json!({}),
        });
        write_json_line(&mut w, &history).await;

        // Read the client's command/message
        let client_msg: ClientMessage = read_json_line(&mut reader).await;

        // Send all response messages
        for msg in &responses {
            write_json_line(&mut w, msg).await;
        }

        client_msg
    }

    /// Build a Cli struct for testing (bypasses actual socket connection).
    fn test_cli(command: CliCommand) -> Cli {
        Cli {
            socket: None,
            config: None,
            command,
        }
    }

    /// Execute a command against a mock server and return what the server received.
    async fn execute_with_mock(
        cli: Cli,
        responses: Vec<ServerMessage>,
    ) -> ClientMessage {
        let (client_stream, server_stream) = duplex(16384);

        let server_handle = tokio::spawn(mock_server(server_stream, responses));

        // Connect using the raw stream and run the command logic
        let (mut conn, _hello, _history) =
            shore_client::SWPConnection::connect_raw(client_stream, "cli", "shore-cli")
                .await
                .unwrap();

        match &cli.command {
            CliCommand::Send { message } => {
                let text = message.join(" ");
                conn.send_message(&text, true).await.unwrap();
                super::recv_streaming_response(&mut conn).await.unwrap();
            }
            CliCommand::Regen { guidance } => {
                conn.send_regen(true, guidance.clone()).await.unwrap();
                super::recv_streaming_response(&mut conn).await.unwrap();
            }
            other => {
                let (name, args) = crate::cli::to_swp_command(other).unwrap();
                conn.send_command(name, args).await.unwrap();
                super::recv_command_response(&mut conn).await.unwrap();
            }
        }

        drop(conn);
        server_handle.await.unwrap()
    }

    fn streaming_response(text: &str) -> Vec<ServerMessage> {
        vec![
            ServerMessage::StreamStart(StreamStart { regen: false }),
            ServerMessage::StreamChunk(StreamChunk {
                text: text.into(),
                content_type: "text".into(),
            }),
            ServerMessage::StreamEnd(StreamEnd {
                content: text.into(),
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
            }),
        ]
    }

    fn command_response(name: &str) -> Vec<ServerMessage> {
        vec![ServerMessage::CommandOutput(CommandOutput {
            name: name.into(),
            data: serde_json::json!({"ok": true}),
        })]
    }

    // ── Send command ─────────────────────────────────────────────────

    #[tokio::test]
    async fn send_sends_swp_message() {
        let cli = test_cli(CliCommand::Send {
            message: vec!["hello".into(), "world".into()],
        });
        let received = execute_with_mock(cli, streaming_response("Hi there!")).await;

        match received {
            ClientMessage::Message(m) => {
                assert_eq!(m.text, "hello world");
                assert!(m.stream);
            }
            other => panic!("expected Message, got: {other:?}"),
        }
    }

    // ── Regen command ────────────────────────────────────────────────

    #[tokio::test]
    async fn regen_sends_swp_regen() {
        let cli = test_cli(CliCommand::Regen {
            guidance: Some("be funny".into()),
        });
        let received = execute_with_mock(cli, streaming_response("Haha!")).await;

        match received {
            ClientMessage::Regen(r) => {
                assert!(r.stream);
                assert_eq!(r.guidance.as_deref(), Some("be funny"));
            }
            other => panic!("expected Regen, got: {other:?}"),
        }
    }

    // ── Status command ───────────────────────────────────────────────

    #[tokio::test]
    async fn status_sends_swp_command() {
        let cli = test_cli(CliCommand::Status);
        let received = execute_with_mock(cli, command_response("status")).await;

        match received {
            ClientMessage::Command(c) => {
                assert_eq!(c.name, "status");
            }
            other => panic!("expected Command, got: {other:?}"),
        }
    }

    // ── SwitchCharacter command ──────────────────────────────────────

    #[tokio::test]
    async fn switch_character_sends_command_with_args() {
        let cli = test_cli(CliCommand::SwitchCharacter {
            name: "alice".into(),
        });
        let received =
            execute_with_mock(cli, command_response("switch_character")).await;

        match received {
            ClientMessage::Command(c) => {
                assert_eq!(c.name, "switch_character");
                assert_eq!(c.args["name"], "alice");
            }
            other => panic!("expected Command, got: {other:?}"),
        }
    }

    // ── TogglePrivate command ────────────────────────────────────────

    #[tokio::test]
    async fn toggle_private_sends_command() {
        let cli = test_cli(CliCommand::TogglePrivate);
        let received =
            execute_with_mock(cli, command_response("toggle_private")).await;

        match received {
            ClientMessage::Command(c) => {
                assert_eq!(c.name, "toggle_private");
            }
            other => panic!("expected Command, got: {other:?}"),
        }
    }

    // ── Compact command ──────────────────────────────────────────────

    #[tokio::test]
    async fn compact_sends_command() {
        let cli = test_cli(CliCommand::Compact);
        let received = execute_with_mock(cli, command_response("compact")).await;

        match received {
            ClientMessage::Command(c) => {
                assert_eq!(c.name, "compact");
            }
            other => panic!("expected Command, got: {other:?}"),
        }
    }

    // ── Memory command ───────────────────────────────────────────────

    #[tokio::test]
    async fn memory_sends_command_with_query() {
        let cli = test_cli(CliCommand::Memory {
            query: Some("recent topics".into()),
        });
        let received = execute_with_mock(cli, command_response("memory")).await;

        match received {
            ClientMessage::Command(c) => {
                assert_eq!(c.name, "memory");
                assert_eq!(c.args["query"], "recent topics");
            }
            other => panic!("expected Command, got: {other:?}"),
        }
    }

    // ── ToggleAutonomy command ───────────────────────────────────────

    #[tokio::test]
    async fn toggle_autonomy_sends_command() {
        let cli = test_cli(CliCommand::ToggleAutonomy);
        let received =
            execute_with_mock(cli, command_response("toggle_autonomy")).await;

        match received {
            ClientMessage::Command(c) => {
                assert_eq!(c.name, "toggle_autonomy");
            }
            other => panic!("expected Command, got: {other:?}"),
        }
    }

    // ── Swipe command ────────────────────────────────────────────────

    #[tokio::test]
    async fn swipe_sends_command_with_direction() {
        let cli = test_cli(CliCommand::Swipe {
            direction: "prev".into(),
        });
        let received = execute_with_mock(cli, command_response("swipe")).await;

        match received {
            ClientMessage::Command(c) => {
                assert_eq!(c.name, "swipe");
                assert_eq!(c.args["direction"], "prev");
            }
            other => panic!("expected Command, got: {other:?}"),
        }
    }

    // ── Edit command ─────────────────────────────────────────────────

    #[tokio::test]
    async fn edit_sends_command_with_joined_content() {
        let cli = test_cli(CliCommand::Edit {
            msg_id: "m1".into(),
            content: vec!["new".into(), "text".into()],
        });
        let received = execute_with_mock(cli, command_response("edit")).await;

        match received {
            ClientMessage::Command(c) => {
                assert_eq!(c.name, "edit");
                assert_eq!(c.args["msg_id"], "m1");
                assert_eq!(c.args["content"], "new text");
            }
            other => panic!("expected Command, got: {other:?}"),
        }
    }

    // ── Streaming with thinking chunks ───────────────────────────────

    #[tokio::test]
    async fn streaming_with_thinking_chunks() {
        let responses = vec![
            ServerMessage::StreamStart(StreamStart { regen: false }),
            ServerMessage::StreamChunk(StreamChunk {
                text: "Let me think...".into(),
                content_type: "thinking".into(),
            }),
            ServerMessage::StreamChunk(StreamChunk {
                text: "Here's the answer.".into(),
                content_type: "text".into(),
            }),
            ServerMessage::StreamEnd(StreamEnd {
                content: "Here's the answer.".into(),
                metadata: StreamMetadata {
                    tokens: TokenCounts {
                        input: 10,
                        output: 5,
                        cache_read: 0,
                        cache_write: 0,
                    },
                    timing: TimingInfo {
                        total_ms: 200,
                        ttft_ms: 50,
                    },
                    model: "test-model".into(),
                },
            }),
        ];

        let cli = test_cli(CliCommand::Send {
            message: vec!["test".into()],
        });
        let received = execute_with_mock(cli, responses).await;
        assert!(matches!(received, ClientMessage::Message(_)));
    }
}
