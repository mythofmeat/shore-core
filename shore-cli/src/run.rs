use std::io::{self, IsTerminal, Read as _, Write as _};

use shore_client::{SWPConnection, ServerAddr};
use shore_protocol::server_msg::ServerMessage;
use tracing::{debug, info, instrument};

use crate::cli::{Cli, CliCommand};
use crate::output;
use crate::state;

/// Execute the CLI command by connecting to the daemon and dispatching.
#[instrument(skip(cli))]
pub async fn execute(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // config --path: query the daemon for its actual config dir, fall back to local.
    if matches!(&cli.command, CliCommand::Config { path: true, .. }) {
        return print_config_path(&cli).await;
    }
    if let CliCommand::Character {
        name: Some(name),
        new: true,
        ..
    } = &cli.command
    {
        return handle_create_character(name);
    }
    if let CliCommand::Matrix { subcommand } = &cli.command {
        return handle_matrix_command(subcommand, &cli).await;
    }

    let addr = resolve_addr(&cli);

    // Character resolution: --character flag > SHORE_CHARACTER env > state file > None (daemon auto-selects).
    let character = cli.character.clone().or_else(state::read_active_character);

    info!(character = ?character, "CLI executing command");

    let (mut conn, _server_hello, _history) =
        SWPConnection::connect(&addr, "cli", "shore-cli", character.clone()).await?;

    match &cli.command {
        CliCommand::Send {
            message,
            images,
            temperature,
            top_p,
            thinking,
            system,
        } => {
            let text = if !message.is_empty() {
                message.join(" ")
            } else if !io::stdin().is_terminal() {
                read_stdin()?
            } else {
                edit_message_in_editor()?
            };
            if text.is_empty() && images.is_empty() {
                return Ok(());
            }
            if *system {
                conn.send_command("inject_system", serde_json::json!({ "text": text }))
                    .await?;
                let data = recv_command_data(&mut conn).await?;
                output::format_command("inject_system", &data);
            } else {
                let overrides = if temperature.is_some() || top_p.is_some() || thinking.is_some() {
                    Some(shore_protocol::client_msg::MessageOverrides {
                        temperature: *temperature,
                        top_p: *top_p,
                        thinking_budget: *thinking,
                    })
                } else {
                    None
                };
                conn.send_message_full(&text, true, images.clone(), overrides)
                    .await?;
                recv_streaming_response(&mut conn).await?;
            }
        }
        CliCommand::Regen { guidance } => {
            conn.send_regen(true, guidance.clone()).await?;
            recv_streaming_response(&mut conn).await?;
        }
        CliCommand::Character {
            name,
            info: false,
            new: false,
            ..
        } => match name {
            Some(name) => handle_switch_character(&mut conn, name).await?,
            None => handle_list_characters(&mut conn).await?,
        },
        CliCommand::Log {
            subcommand: Some(sub),
            json,
            ..
        } => {
            let (name, args) = match sub {
                crate::cli::LogCommand::Edit { msg_ref, content } => (
                    "edit",
                    serde_json::json!({ "ref": msg_ref, "content": content.join(" ") }),
                ),
                crate::cli::LogCommand::Delete { msg_ref } => {
                    ("delete", serde_json::json!({ "refs": msg_ref }))
                }
            };
            conn.send_command(name, args).await?;
            let data = recv_command_data(&mut conn).await?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else {
                output::format_command(name, &data);
            }
        }
        CliCommand::Log {
            msg_ref: Some(r),
            json,
            plain,
            content,
            ..
        } => {
            conn.send_command("get", serde_json::json!({ "ref": r }))
                .await?;
            let data = recv_command_data(&mut conn).await?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else if *content {
                output::print_message_content(&data);
            } else if *plain {
                let char_name = character.as_deref().unwrap_or("Assistant");
                output::print_log_plain(std::slice::from_ref(&data), char_name);
            } else {
                let char_name = character.as_deref().unwrap_or("Assistant");
                output::print_single_message(&data, char_name);
            }
        }
        CliCommand::Log {
            heartbeat: true,
            count,
            json,
            ..
        } => {
            conn.send_command("heartbeat_log", serde_json::json!({ "count": count }))
                .await?;
            let data = recv_command_data(&mut conn).await?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else {
                output::print_heartbeat_log(&data);
            }
        }
        CliCommand::Log {
            count,
            follow,
            json,
            content,
            plain,
            ..
        } => {
            conn.send_command("log", serde_json::json!({ "count": count }))
                .await?;
            let data = recv_command_data(&mut conn).await?;

            if *json {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else if *content {
                if let Some(messages) = data["messages"].as_array() {
                    for msg in messages {
                        if let Some(c) = msg["content"].as_str() {
                            println!("{}", c);
                        }
                    }
                }
            } else if *plain {
                let char_name = character.as_deref().unwrap_or("Assistant");
                if let Some(messages) = data["messages"].as_array() {
                    output::print_log_plain(messages, char_name);
                }
            } else {
                let char_name = character.as_deref().unwrap_or("Assistant");
                if let Some(messages) = data["messages"].as_array() {
                    output::print_log(messages, char_name);
                }
            }

            if *follow {
                let follow_char = character.as_deref().unwrap_or("Assistant");
                loop {
                    let msg = conn.recv().await?;
                    match &msg {
                        ServerMessage::NewMessage(nm) => {
                            output::print_new_message(nm, follow_char);
                        }
                        ServerMessage::StreamStart(start) => {
                            output::reset_chunk_state();
                            if !start.regen {
                                output::print_follow_stream_start(follow_char);
                            } else {
                                output::print_stream_start(start.regen);
                            }
                        }
                        ServerMessage::StreamChunk(chunk) => {
                            output::print_chunk(chunk);
                        }
                        ServerMessage::StreamEnd(end) => {
                            output::print_stream_end(end);
                        }
                        ServerMessage::ToolCall(call) => {
                            output::print_tool_call(call);
                        }
                        ServerMessage::ToolResult(result) => {
                            output::print_tool_result(result);
                        }
                        ServerMessage::Phase(phase) => {
                            output::print_phase(phase);
                        }
                        ServerMessage::Shutdown(_) => break,
                        ServerMessage::Ping(_) | ServerMessage::History(_) => {}
                        _ => {}
                    }
                }
            }
        }
        CliCommand::Status {
            diagnostics: true,
            count,
            json,
            ..
        } => {
            conn.send_command("diagnostics", serde_json::json!({ "count": count }))
                .await?;
            let data = recv_command_data(&mut conn).await?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else {
                output::print_diagnostics(&data);
            }
        }
        CliCommand::Status { section, json, .. } => {
            conn.send_command("status", serde_json::json!({})).await?;
            let data = recv_command_data(&mut conn).await?;
            match section {
                Some(s) => {
                    if let Some(val) = data.get(s.as_str()) {
                        println!("{}", serde_json::to_string_pretty(val)?);
                    } else {
                        return Err(format!("Unknown status section: {s}").into());
                    }
                }
                None if *json => {
                    println!("{}", serde_json::to_string_pretty(&data)?);
                }
                None => {
                    let char_name = character.as_deref().unwrap_or("Assistant");
                    output::print_status(&data, char_name);
                }
            }
        }
        CliCommand::Memory {
            subcommand: Some(crate::cli::MemoryCommand::Shell),
            ..
        } => {
            run_memory_shell(&mut conn).await?;
        }
        other => {
            let json_mode = match other {
                CliCommand::Model { json, .. }
                | CliCommand::Character { json, .. }
                | CliCommand::Memory { json, .. }
                | CliCommand::Config { json, .. } => *json,
                _ => false,
            };
            let (name, args) = crate::cli::to_swp_command(other)
                .expect("non-send/regen/local command must map to SWP command");
            conn.send_command(name, args).await?;
            let data = recv_command_data(&mut conn).await?;
            if json_mode {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else {
                output::format_command(name, &data);
            }
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
    conn.send_command("list_characters", serde_json::json!({}))
        .await?;
    let data = recv_command_data(conn).await?;

    let characters = data["characters"]
        .as_array()
        .ok_or("invalid list_characters response")?;

    let valid = characters.iter().any(|c| c["name"].as_str() == Some(name));

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

    info!(character = name, "Switching active character");
    state::write_active_character(name)?;
    println!("Switched to character: {name}");
    println!("To override per-terminal: export SHORE_CHARACTER={name}");
    Ok(())
}

/// Handle `list-characters`: query daemon, annotate active character.
async fn handle_list_characters(
    conn: &mut SWPConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    conn.send_command("list_characters", serde_json::json!({}))
        .await?;
    let data = recv_command_data(conn).await?;

    let active = state::read_active_character();

    if let Some(chars) = data["characters"].as_array() {
        debug!(count = chars.len(), "Listed characters from daemon");
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

/// Handle `shore matrix` subcommands by delegating to shore-matrix binary.
async fn handle_matrix_command(
    subcommand: &crate::cli::MatrixCommand,
    cli: &Cli,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = std::process::Command::new("shore-matrix");

    // Pass through config path if set
    if let Some(ref config) = cli.config {
        cmd.arg("--config").arg(config);
    }
    if let Some(ref addr) = cli.addr {
        cmd.arg("--addr").arg(addr);
    }

    match subcommand {
        crate::cli::MatrixCommand::Setup => {
            cmd.arg("--setup");
        }
        crate::cli::MatrixCommand::Register { username, password } => {
            cmd.arg("--register").arg(username);
            if let Some(pw) = password {
                cmd.arg("--register-password").arg(pw);
            }
        }
    }

    let status = cmd
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                "shore-matrix binary not found. Is it installed and in your PATH?".to_string()
            } else {
                format!("failed to run shore-matrix: {e}")
            }
        })?;

    if !status.success() {
        return Err(format!("shore-matrix exited with status {status}").into());
    }
    Ok(())
}

/// Create a new character scaffold directory.
fn handle_create_character(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let config_dir = config_dir();
    let char_dir = config_dir.join("characters").join(name);
    let character_md = char_dir.join("character.md");

    if character_md.exists() {
        return Err(format!(
            "Character '{}' already exists at {}",
            name,
            char_dir.display()
        )
        .into());
    }

    std::fs::create_dir_all(&char_dir)?;
    std::fs::write(
        &character_md,
        format!("You are {name}.\n\n<!-- Edit this file to define {name}'s personality and behavior. -->\n"),
    )?;
    println!("Created character scaffold: {}", char_dir.display());
    Ok(())
}

/// Run an interactive memory shell session.
async fn run_memory_shell(conn: &mut SWPConnection) -> Result<(), Box<dyn std::error::Error>> {
    // Start the session.
    conn.send_command("memory_shell_start", serde_json::json!({}))
        .await?;
    let start_data = recv_command_data(conn).await?;
    let session_id = start_data["session_id"]
        .as_str()
        .ok_or("missing session_id in response")?
        .to_string();
    let character = start_data["character"].as_str().unwrap_or("unknown");

    info!(session_id, character, "Memory shell session started");
    output::print_memory_shell_welcome(character);

    let stdin = io::stdin();
    let mut line_buf = String::new();

    loop {
        // Print prompt.
        eprint!("memory> ");
        io::stderr().flush().ok();

        line_buf.clear();
        let bytes = stdin.read_line(&mut line_buf)?;

        // EOF (ctrl-d).
        if bytes == 0 {
            eprintln!();
            break;
        }

        let input = line_buf.trim();

        if input.is_empty() {
            continue;
        }

        if input == "/quit" || input == "/exit" {
            break;
        }

        // Send query.
        conn.send_command(
            "memory_shell_query",
            serde_json::json!({
                "session_id": session_id,
                "input": input,
            }),
        )
        .await?;

        let data = recv_command_data(conn).await?;
        let response = data["response"].as_str().unwrap_or("");
        let mutations = data["mutations"].as_str().unwrap_or("");

        output::print_memory_shell_response(response, mutations);
    }

    // End the session.
    info!(session_id, "Memory shell session ended");
    conn.send_command(
        "memory_shell_end",
        serde_json::json!({ "session_id": session_id }),
    )
    .await?;
    let _ = recv_command_data(conn).await;

    Ok(())
}


/// Resolve the Shore config directory.
fn config_dir() -> std::path::PathBuf {
    shore_config::config_dir()
}

/// Print the config directory path by querying the daemon.
/// Falls back to local resolution if the daemon is unreachable.
async fn print_config_path(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let addr = resolve_addr(cli);
    let character = cli.character.clone().or_else(state::read_active_character);

    match SWPConnection::connect(&addr, "cli", "shore-cli", character).await {
        Ok((mut conn, _hello, _history)) => {
            conn.send_command("status", serde_json::json!({})).await?;
            let data = recv_command_data(&mut conn).await?;
            if let Some(dir) = data["config_dir"].as_str() {
                println!("{dir}");
            } else {
                println!("{}", config_dir().display());
            }
            Ok(())
        }
        Err(_) => {
            eprintln!("(no daemon running — showing local config dir)");
            println!("{}", config_dir().display());
            Ok(())
        }
    }
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

    let status = std::process::Command::new(&editor).arg(&path).status()?;

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
    if let Some(addr) = &cli.addr {
        return ServerAddr(addr.clone());
    }
    shore_client::discover_or_default(cli.config.as_deref())
}

/// Receive and render a streaming response (for send/regen).
async fn recv_streaming_response(
    conn: &mut SWPConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut spinner = output::StreamSpinner::new();
    spinner.start();

    loop {
        let msg = conn.recv().await?;
        match &msg {
            ServerMessage::StreamStart(start) => {
                output::reset_chunk_state();
                output::print_stream_start(start.regen);
            }
            ServerMessage::StreamChunk(chunk) => {
                spinner.clear().await;
                output::print_chunk(chunk);
            }
            ServerMessage::StreamEnd(end) => {
                spinner.stop().await;
                debug!(finish_reason = end.finish_reason, "Stream complete");
                if end.finish_reason == "tool_use" {
                    // Tool loop: more messages will follow.
                    // Restart spinner for the next LLM round.
                    spinner.restart();
                    continue;
                }
                output::print_stream_end(end);
                return Ok(());
            }
            ServerMessage::ToolCall(call) => {
                spinner.clear().await;
                output::print_tool_call(call);
            }
            ServerMessage::ToolResult(result) => {
                output::print_tool_result(result);
            }
            ServerMessage::Error(err) => {
                spinner.stop().await;
                output::print_server_error(
                    &serde_json::to_string(&err.code).unwrap_or_default(),
                    &err.message,
                );
                return Err(err.message.clone().into());
            }
            ServerMessage::SendImage(img) => {
                output::print_send_image(img);
            }
            ServerMessage::NewMessage(_) => {
                // Ignore — the sender already knows what they sent.
                // NewMessage is for follow-mode / other clients.
            }
            ServerMessage::Phase(phase) => {
                // Update spinner instead of printing static label when active.
                if spinner.is_active() {
                    spinner.set_phase(&phase.phase);
                    if let Some(model) = &phase.model {
                        spinner.set_model(Some(model.clone()));
                    }
                } else {
                    output::print_phase(phase);
                }
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
            ServerMessage::Ping(_) | ServerMessage::History(_) => {}
            ServerMessage::SendImage(img) => {
                output::print_send_image(img);
            }
            ServerMessage::NewMessage(msg) => {
                output::print_new_message(msg, "Assistant");
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
            addr: None,
            config: None,
            character: None,
            no_color: false,
            command,
        }
    }

    /// Execute a command against a mock server and return what the server received.
    async fn execute_with_mock(cli: Cli, responses: Vec<ServerMessage>) -> ClientMessage {
        let (client_stream, server_stream) = duplex(16384);

        let server_handle = tokio::spawn(mock_server(server_stream, responses));

        // Connect using the raw stream and run the command logic
        let (mut conn, _hello, _history) = shore_client::SWPConnection::connect_raw(
            client_stream,
            "cli",
            "shore-cli",
            cli.character.clone(),
        )
        .await
        .unwrap();

        match &cli.command {
            CliCommand::Send {
                message, images, ..
            } => {
                let text = message.join(" ");
                conn.send_message_with_images(&text, true, images.clone())
                    .await
                    .unwrap();
                super::recv_streaming_response(&mut conn).await.unwrap();
            }
            CliCommand::Regen { guidance } => {
                conn.send_regen(true, guidance.clone()).await.unwrap();
                super::recv_streaming_response(&mut conn).await.unwrap();
            }
            other => {
                let (name, args) = crate::cli::to_swp_command(other).unwrap();
                conn.send_command(name, args).await.unwrap();
                let _data = super::recv_command_data(&mut conn).await.unwrap();
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
            images: vec![],
            temperature: None,
            top_p: None,
            thinking: None,
            system: false,
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
        let cli = test_cli(CliCommand::Status {
            section: None,
            diagnostics: false,
            count: 10,
            json: false,
        });
        let received = execute_with_mock(cli, command_response("status")).await;

        match received {
            ClientMessage::Command(c) => {
                assert_eq!(c.name, "status");
            }
            other => panic!("expected Command, got: {other:?}"),
        }
    }

    // ── Character is handled locally (see state.rs) ───────────────

    // ── Memory compact command ───────────────────────────────────────

    #[tokio::test]
    async fn memory_compact_sends_command() {
        let cli = test_cli(CliCommand::Memory {
            subcommand: Some(crate::cli::MemoryCommand::Compact),
            query: None,
            direct: false,
            json: false,
        });
        let received = execute_with_mock(cli, command_response("compact")).await;

        match received {
            ClientMessage::Command(c) => {
                assert_eq!(c.name, "compact");
                assert_eq!(c.args["collate"], true);
            }
            other => panic!("expected Command, got: {other:?}"),
        }
    }

    // ── Memory query command ─────────────────────────────────────────

    #[tokio::test]
    async fn memory_sends_command_with_query() {
        let cli = test_cli(CliCommand::Memory {
            subcommand: None,
            query: Some("recent topics".into()),
            direct: false,
            json: false,
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

    // ── Log edit command ─────────────────────────────────────────────

    #[tokio::test]
    async fn log_edit_sends_edit_command() {
        let cli = test_cli(CliCommand::Log {
            subcommand: Some(crate::cli::LogCommand::Edit {
                msg_ref: "m1".into(),
                content: vec!["new".into(), "text".into()],
            }),
            msg_ref: None,
            count: 20,
            follow: false,
            json: false,
            content: false,
            plain: false,
            heartbeat: false,
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

    // ── Log delete command ───────────────────────────────────────────

    #[tokio::test]
    async fn log_delete_sends_delete_command() {
        let cli = test_cli(CliCommand::Log {
            subcommand: Some(crate::cli::LogCommand::Delete {
                msg_ref: "m1".into(),
            }),
            msg_ref: None,
            count: 20,
            follow: false,
            json: false,
            content: false,
            plain: false,
            heartbeat: false,
        });
        let received = execute_with_mock(cli, command_response("delete")).await;

        match received {
            ClientMessage::Command(c) => {
                assert_eq!(c.name, "delete");
                assert_eq!(c.args["refs"], "m1");
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
            images: vec![],
            temperature: None,
            top_p: None,
            thinking: None,
            system: false,
        });
        let received = execute_with_mock(cli, responses).await;
        assert!(matches!(received, ClientMessage::Message(_)));
    }
}
