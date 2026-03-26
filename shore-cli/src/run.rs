use std::io::{self, IsTerminal, Read as _};

use shore_client::{SWPConnection, ServerAddr};
use shore_protocol::server_msg::ServerMessage;

use crate::cli::{Cli, CliCommand};
use crate::output;
use crate::state;

/// Execute the CLI command by connecting to the daemon and dispatching.
pub async fn execute(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let addr = resolve_addr(&cli);

    // Character resolution: --character flag > SHORE_CHARACTER env > state file > None (daemon auto-selects).
    let character = cli.character.clone().or_else(state::read_active_character);

    let (mut conn, _server_hello, _history) =
        SWPConnection::connect(&addr, "cli", "shore-cli", character.clone()).await?;

    // Config --path is handled locally (no daemon round-trip).
    if matches!(&cli.command, CliCommand::Config { path: true, .. }) {
        print_config_path();
        return Ok(());
    }

    match &cli.command {
        CliCommand::Send { message } => {
            let text = if !message.is_empty() {
                message.join(" ")
            } else if !io::stdin().is_terminal() {
                read_stdin()?
            } else {
                edit_message_in_editor()?
            };
            if text.is_empty() {
                return Ok(());
            }
            conn.send_message(&text, true).await?;
            recv_streaming_response(&mut conn).await?;
        }
        CliCommand::Regen { guidance } => {
            conn.send_regen(true, guidance.clone()).await?;
            recv_streaming_response(&mut conn).await?;
        }
        CliCommand::Character { name, info: false } => {
            match name {
                Some(name) => handle_switch_character(&mut conn, name).await?,
                None => handle_list_characters(&mut conn).await?,
            }
        }
        other => {
            let (name, args) = crate::cli::to_swp_command(other)
                .expect("non-send/regen/local command must map to SWP command");
            conn.send_command(name, args).await?;
            recv_command_response(&mut conn).await?;
        }
    }

    Ok(())
}

/// Handle `switch-character` locally: validate via daemon, write state file.
async fn handle_switch_character(
    conn: &mut SWPConnection,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Query the daemon for available characters.
    conn.send_command("list_characters", serde_json::json!({})).await?;
    let data = recv_command_data(conn).await?;

    let characters = data["characters"]
        .as_array()
        .ok_or("invalid list_characters response")?;

    let valid = characters
        .iter()
        .any(|c| c["name"].as_str() == Some(name));

    if !valid {
        let available: Vec<&str> = characters
            .iter()
            .filter_map(|c| c["name"].as_str())
            .collect();
        return Err(format!(
            "character '{}' not found (available: {})",
            name,
            available.join(", ")
        )
        .into());
    }

    state::write_active_character(name)?;
    println!("Switched to character: {name}");
    println!("To override per-terminal: export SHORE_CHARACTER={name}");
    Ok(())
}

/// Handle `list-characters`: query daemon, annotate active character.
async fn handle_list_characters(
    conn: &mut SWPConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    conn.send_command("list_characters", serde_json::json!({})).await?;
    let data = recv_command_data(conn).await?;

    let active = state::read_active_character();

    if let Some(chars) = data["characters"].as_array() {
        for ch in chars {
            if let Some(name) = ch["name"].as_str() {
                if active.as_deref() == Some(name) {
                    println!("  * {name} (active)");
                } else {
                    println!("    {name}");
                }
            }
        }
    }
    Ok(())
}

/// Print the config directory path and exit.
fn print_config_path() {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "~".into());
            std::path::PathBuf::from(home).join(".config")
        });
    println!("{}", base.join("shore").display());
}

/// Read all of stdin to a string (for piped input).
fn read_stdin() -> Result<String, Box<dyn std::error::Error>> {
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf)?;
    Ok(buf.trim().to_string())
}

/// Open `$EDITOR` (or `$VISUAL`) with a temp file and return the composed text.
/// Returns an empty string if the user saves an empty file or the editor exits non-zero.
fn edit_message_in_editor() -> Result<String, Box<dyn std::error::Error>> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());

    let tmp = tempfile::Builder::new()
        .prefix("shore-")
        .suffix(".md")
        .tempfile()?;

    let path = tmp.path().to_path_buf();

    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()?;

    if !status.success() {
        return Ok(String::new());
    }

    let content = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .trim()
        .to_string();

    Ok(content)
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
                if end.finish_reason == "tool_use" {
                    // Tool loop: more messages will follow — don't print metadata yet.
                    continue;
                }
                output::print_stream_end(end);
                return Ok(());
            }
            ServerMessage::ToolCall(call) => {
                output::print_tool_call(call);
            }
            ServerMessage::ToolResult(result) => {
                output::print_tool_result(result);
            }
            ServerMessage::Error(err) => {
                output::print_server_error(
                    &serde_json::to_string(&err.code).unwrap_or_default(),
                    &err.message,
                );
                return Err(err.message.clone().into());
            }
            ServerMessage::SendImage(img) => {
                output::print_send_image(img);
            }
            ServerMessage::NewMessage(msg) => {
                output::print_new_message(msg);
                crate::notifications::notify_new_message(msg);
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

/// Receive a command response and return the data payload.
async fn recv_command_data(
    conn: &mut SWPConnection,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    loop {
        let msg = conn.recv().await?;
        match &msg {
            ServerMessage::CommandOutput(co) => {
                return Ok(co.data.clone());
            }
            ServerMessage::Error(err) => {
                output::print_server_error(
                    &serde_json::to_string(&err.code).unwrap_or_default(),
                    &err.message,
                );
                return Err(err.message.clone().into());
            }
            ServerMessage::Ping(_)
            | ServerMessage::History(_) => {}
            ServerMessage::SendImage(img) => {
                output::print_send_image(img);
            }
            ServerMessage::NewMessage(msg) => {
                output::print_new_message(msg);
                crate::notifications::notify_new_message(msg);
            }
            _ => {}
        }
    }
}

/// Receive a command response and print it.
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
            ServerMessage::Ping(_)
            | ServerMessage::History(_) => {}
            ServerMessage::SendImage(img) => {
                output::print_send_image(img);
            }
            ServerMessage::NewMessage(msg) => {
                output::print_new_message(msg);
                crate::notifications::notify_new_message(msg);
            }
            _ => {}
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
            character: None,
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
            shore_client::SWPConnection::connect_raw(client_stream, "cli", "shore-cli", cli.character.clone())
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
                finish_reason: "end_turn".into(),
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

    // ── Character is handled locally (see state.rs) ───────────────

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
                assert_eq!(c.args["ref"], "m1");
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
                finish_reason: "end_turn".into(),
            }),
        ];

        let cli = test_cli(CliCommand::Send {
            message: vec!["test".into()],
        });
        let received = execute_with_mock(cli, responses).await;
        assert!(matches!(received, ClientMessage::Message(_)));
    }
}
