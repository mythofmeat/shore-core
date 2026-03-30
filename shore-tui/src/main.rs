mod app;
mod connection;
mod images;
mod input;
mod markdown;
mod ui;

use std::io;
use std::time::Duration;

use clap::Parser;
use crossterm::event;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use shore_protocol::client_msg::{ClientMessage, Command};
use shore_protocol::server_msg::ServerMessage;
use shore_protocol::types::{ContentBlock, Message, Role};

use app::{App, ConnectionStatus, ConversationEntry, InputState};
use connection::{ConnCommand, ConnEvent};
use input::Action;

#[derive(Parser)]
#[command(name = "shore-tui", about = "Shore terminal UI")]
struct Cli {
    /// Unix socket or TCP address of the daemon
    #[arg(long)]
    socket: Option<String>,

    /// Config path to select daemon instance
    #[arg(long)]
    config: Option<String>,

    /// Character to connect as
    #[arg(short, long)]
    character: Option<String>,
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create tokio runtime");

    rt.block_on(run_tui(cli))
}

/// Resolve the initial character: --character flag > SHORE_CHARACTER env > state file.
fn resolve_character(cli_character: Option<String>) -> Option<String> {
    if cli_character.is_some() {
        return cli_character;
    }
    if let Ok(val) = std::env::var("SHORE_CHARACTER") {
        if !val.is_empty() {
            return Some(val);
        }
    }
    // Try the CLI's active_character state file.
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let state_path = std::path::Path::new(&runtime_dir)
        .join("shore")
        .join("active_character");
    if let Ok(name) = std::fs::read_to_string(state_path) {
        let name = name.trim().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

fn prefs_path() -> std::path::PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    std::path::Path::new(&runtime_dir)
        .join("shore")
        .join("tui_prefs.json")
}

fn load_prefs(app: &mut App) {
    if let Ok(data) = std::fs::read_to_string(prefs_path()) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
            if let Some(b) = v.get("show_thinking").and_then(|v| v.as_bool()) {
                app.show_thinking = b;
            }
            if let Some(b) = v.get("show_tools").and_then(|v| v.as_bool()) {
                app.show_tools = b;
            }
        }
    }
}

fn save_prefs(app: &App) {
    let v = serde_json::json!({
        "show_thinking": app.show_thinking,
        "show_tools": app.show_tools,
    });
    let _ = std::fs::write(prefs_path(), v.to_string());
}

fn open_in_editor(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    input: &mut InputState,
) -> io::Result<()> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let tmp = std::env::temp_dir().join("shore_input.md");
    std::fs::write(&tmp, input.text.as_str())?;

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    let _ = std::process::Command::new(&editor).arg(&tmp).status();

    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    terminal.clear()?;

    if let Ok(contents) = std::fs::read_to_string(&tmp) {
        input.set_text(contents.trim_end_matches('\n').to_string());
    }
    Ok(())
}

async fn run_tui(cli: Cli) -> io::Result<()> {
    // Set up terminal
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let character = resolve_character(cli.character);

    let mut app = App {
        connection_status: ConnectionStatus::Connecting,
        ..App::default()
    };
    load_prefs(&mut app);

    // Spawn connection manager
    let (cmd_tx, mut event_rx) =
        connection::spawn_connection(cli.socket, cli.config, character);

    // Main event loop
    let result = loop {
        // Draw
        terminal.draw(|frame| ui::draw(frame, &mut app))?;

        // Poll for events (crossterm keyboard or connection events)
        tokio::select! {
            biased;
            // Connection events
            conn_event = event_rx.recv() => {
                match conn_event {
                    Some(event) => {
                        let cmds = handle_conn_event(&mut app, event);
                        for cmd in cmds {
                            let _ = cmd_tx.send(cmd).await;
                        }
                    }
                    None => {
                        // Connection task exited
                        app.connection_status = ConnectionStatus::Disconnected;
                        app.set_status("connection task exited");
                    }
                }
            }
            // Keyboard events — poll with short timeout to keep responsive
            _ = tokio::time::sleep(Duration::from_millis(16)) => {
                while event::poll(Duration::from_millis(0))? {
                    let ev = event::read()?;
                    match input::handle_event(&mut app, ev) {
                        Action::Quit => {
                            app.should_quit = true;
                            break;
                        }
                        Action::Send(cmd) => {
                            let _ = cmd_tx.send(cmd).await;
                        }
                        Action::SendMulti(cmds) => {
                            for cmd in cmds {
                                let _ = cmd_tx.send(cmd).await;
                            }
                        }
                        Action::OpenInEditor => {
                            let _ = open_in_editor(&mut terminal, &mut app.input);
                        }
                        Action::Redraw | Action::None => {}
                    }
                }
            }
        }

        if app.should_quit {
            break Ok(());
        }
    };

    // Save preferences and shutdown
    save_prefs(&app);
    let _ = cmd_tx.send(ConnCommand::Shutdown).await;

    // Restore terminal
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    result
}

fn handle_conn_event(app: &mut App, event: ConnEvent) -> Vec<ConnCommand> {
    match event {
        ConnEvent::Connected {
            characters,
            history,
            config,
            ..
        } => {
            app.connection_status = ConnectionStatus::Connected;
            app.characters = characters.clone();

            // Set character name from first character
            if let Some(ch) = characters.first() {
                app.character_name = ch.name.clone();
            }

            // Check private flag from config
            if let Some(private) = config.get("private").and_then(|v| v.as_bool()) {
                app.is_private = private;
            }

            // Load any history from the handshake
            app.entries.clear();
            for msg in history {
                expand_msg(msg, &mut app.entries);
            }
            transmit_entry_images(app);

            app.set_status("connected");

            // Request full history, status, and model list from daemon
            vec![
                ConnCommand::Send(ClientMessage::Command(Command {
                    rid: None,
                    name: "log".into(),
                    args: serde_json::json!({}),
                })),
                ConnCommand::Send(ClientMessage::Command(Command {
                    rid: None,
                    name: "status".into(),
                    args: serde_json::json!({}),
                })),
                ConnCommand::Send(ClientMessage::Command(Command {
                    rid: None,
                    name: "list_models".into(),
                    args: serde_json::json!({}),
                })),
            ]
        }

        ConnEvent::Disconnected(reason) => {
            app.connection_status = ConnectionStatus::Connecting;
            app.stream.reset(); // clear stale streaming state
            app.set_status(format!("reconnecting: {reason}"));
            vec![]
        }

        ConnEvent::Message(msg) => {
            handle_server_message(app, msg)
        }
    }
}

/// Expand a protocol Message into one or more ConversationEntry items.
///
/// Assistant messages with content_blocks are expanded so that thinking,
/// tool_use, and tool_result blocks become distinct entries (matching the
/// visual style used during streaming), with the text becoming the final
/// Assistant entry.
fn expand_msg(msg: Message, entries: &mut Vec<ConversationEntry>) {
    // Non-assistant or no content_blocks: simple conversion.
    if msg.role != Role::Assistant || msg.content_blocks.is_empty() {
        entries.push(match msg.role {
            Role::User => ConversationEntry::User {
                content: msg.content,
                images: msg.images,
                timestamp: msg.timestamp,
            },
            Role::Assistant => ConversationEntry::Assistant {
                content: msg.content,
                images: msg.images,
                timestamp: msg.timestamp,
                metadata: None,
            },
            Role::System => ConversationEntry::System {
                content: msg.content,
                timestamp: msg.timestamp,
            },
        });
        return;
    }

    // Build tool_use_id → tool_name map for ToolResult labels.
    let tool_names: std::collections::HashMap<&str, &str> = msg
        .content_blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, .. } => Some((id.as_str(), name.as_str())),
            _ => None,
        })
        .collect();

    let mut text_parts: Vec<String> = Vec::new();

    for block in &msg.content_blocks {
        match block {
            ContentBlock::Thinking { thinking, .. } => {
                if !thinking.is_empty() {
                    entries.push(ConversationEntry::Thinking {
                        content: thinking.clone(),
                    });
                }
            }
            ContentBlock::RedactedThinking { .. } => {
                entries.push(ConversationEntry::Thinking {
                    content: "[redacted thinking]".into(),
                });
            }
            ContentBlock::ToolUse { id, name, input } => {
                entries.push(ConversationEntry::ToolCall {
                    tool_id: id.clone(),
                    tool_name: name.clone(),
                    input: input.clone(),
                });
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let name = tool_names
                    .get(tool_use_id.as_str())
                    .unwrap_or(&"tool");
                entries.push(ConversationEntry::ToolResult {
                    tool_id: tool_use_id.clone(),
                    tool_name: name.to_string(),
                    output: content.clone(),
                    is_error: *is_error,
                });
            }
            ContentBlock::Text { text } => {
                text_parts.push(text.clone());
            }
        }
    }

    // Emit the text content as the final Assistant entry.
    let content = text_parts.join("\n");
    if !content.trim().is_empty() {
        entries.push(ConversationEntry::Assistant {
            content,
            images: msg.images,
            timestamp: msg.timestamp,
            metadata: None,
        });
    }
}

/// Max display columns for images (terminal width minus borders/indent).
fn image_max_cols() -> u16 {
    crossterm::terminal::size()
        .map(|(w, _)| w.saturating_sub(4))
        .unwrap_or(76)
}

/// Transmit images from a conversation entry to kitty.
fn transmit_entry_images(app: &mut App) {
    let max_cols = image_max_cols();
    for entry in &app.entries {
        let imgs = match entry {
            ConversationEntry::User { images, .. }
            | ConversationEntry::Assistant { images, .. } => images,
            _ => continue,
        };
        for img in imgs {
            app.image_cache.ensure_transmitted(&img.path, max_cols);
        }
    }
}

fn handle_server_message(app: &mut App, msg: ServerMessage) -> Vec<ConnCommand> {
    match msg {
        ServerMessage::StreamStart(start) => {
            app.stream.reset();
            app.stream.active = true;
            app.stream.regen = start.regen;

            // If regenerating, remove last assistant entry
            if start.regen {
                if let Some(pos) = app
                    .entries
                    .iter()
                    .rposition(|e| matches!(e, ConversationEntry::Assistant { .. }))
                {
                    app.entries.truncate(pos);
                }
            }
        }

        ServerMessage::StreamChunk(chunk) => {
            if chunk.content_type == "thinking" {
                app.stream.thinking.push_str(&chunk.text);
            } else {
                app.stream.text.push_str(&chunk.text);
            }
            if app.auto_scroll {
                app.scroll_to_bottom();
            }
        }

        ServerMessage::StreamEnd(end) => {
            app.model = end.metadata.model.clone();
            app.tokens = end.metadata.tokens.clone();

            app.entries.push(ConversationEntry::Assistant {
                content: end.content,
                images: vec![],
                timestamp: String::new(),
                metadata: Some(end.metadata),
            });

            app.stream.reset();

            // Re-request log to guard against stale History broadcasts
            // (e.g. from background compaction) overwriting this response.
            return vec![ConnCommand::Send(ClientMessage::Command(Command {
                rid: None,
                name: "log".into(),
                args: serde_json::json!({}),
            }))];
        }

        ServerMessage::Phase(phase) => {
            app.stream.phase = phase.phase;
            if let Some(model) = phase.model {
                app.model = model;
            }
        }

        ServerMessage::NewMessage(new_msg) => {
            // Deduplicate: engine.append_message broadcasts History (which
            // clears + rebuilds entries) AND the handler broadcasts NewMessage
            // for the same user message.  Skip if the last entry already
            // matches this timestamp (placed there by the preceding History).
            let dominated = app.entries.last().map_or(false, |last| {
                let ts = match last {
                    ConversationEntry::User { timestamp, .. }
                    | ConversationEntry::Assistant { timestamp, .. }
                    | ConversationEntry::System { timestamp, .. } => timestamp.as_str(),
                    _ => "",
                };
                !ts.is_empty() && ts == new_msg.message.timestamp
            });

            if !dominated {
                // Check if the last entry is an optimistic user echo (empty
                // timestamp) that matches this incoming NewMessage.  If so,
                // replace it with the server-authoritative version instead of
                // pushing a duplicate.
                let is_optimistic_echo = new_msg.message.role == Role::User
                    && app.entries.last().map_or(false, |last| {
                        matches!(last, ConversationEntry::User { timestamp, content, .. }
                            if timestamp.is_empty() && *content == new_msg.message.content)
                    });

                let max_cols = image_max_cols();
                for img in &new_msg.message.images {
                    app.image_cache.ensure_transmitted(&img.path, max_cols);
                }
                let entry = match new_msg.message.role {
                    Role::User => ConversationEntry::User {
                        content: new_msg.message.content,
                        images: new_msg.message.images,
                        timestamp: new_msg.message.timestamp,
                    },
                    Role::Assistant => ConversationEntry::Assistant {
                        content: new_msg.message.content,
                        images: new_msg.message.images,
                        timestamp: new_msg.message.timestamp,
                        metadata: None,
                    },
                    Role::System => ConversationEntry::System {
                        content: new_msg.message.content,
                        timestamp: new_msg.message.timestamp,
                    },
                };
                if is_optimistic_echo {
                    // Replace optimistic entry with authoritative version
                    let last = app.entries.len() - 1;
                    app.entries[last] = entry;
                } else {
                    app.entries.push(entry);
                }
            }
            if app.auto_scroll {
                app.scroll_to_bottom();
            }
        }

        ServerMessage::ToolCall(tc) => {
            app.entries.push(ConversationEntry::ToolCall {
                tool_id: tc.tool_id,
                tool_name: tc.tool_name,
                input: tc.input,
            });
        }

        ServerMessage::ToolResult(tr) => {
            app.entries.push(ConversationEntry::ToolResult {
                tool_id: tr.tool_id,
                tool_name: tr.tool_name,
                output: tr.output,
                is_error: tr.is_error,
            });
        }

        ServerMessage::SendImage(img) => {
            app.image_cache
                .ensure_transmitted(&img.path, image_max_cols());
        }

        ServerMessage::CommandOutput(co) => {
            match co.name.as_str() {
                "log" => {
                    if let Some(messages) = co.data.get("messages").and_then(|v| v.as_array()) {
                        app.image_cache.clear();
                        app.entries.clear();
                        for msg_val in messages {
                            if let Ok(msg) = serde_json::from_value::<Message>(msg_val.clone()) {
                                expand_msg(msg, &mut app.entries);
                            }
                        }
                        transmit_entry_images(app);
                        if app.auto_scroll {
                            app.scroll_to_bottom();
                        }
                    }
                }
                "status" => {
                    if let Some(character) = co.data.get("character").and_then(|v| v.as_str()) {
                        app.character_name = character.to_string();
                    }
                    if let Some(model) = co.data.get("active_model").and_then(|v| v.as_str()) {
                        app.model = model.to_string();
                    }
                }
                "list_characters" => {
                    if let Some(chars) = co.data.get("characters").and_then(|v| v.as_array()) {
                        let active = co.data.get("active").and_then(|v| v.as_str()).unwrap_or("");
                        let list = chars
                            .iter()
                            .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
                            .map(|n| if n == active { format!("  * {n}") } else { format!("    {n}") })
                            .collect::<Vec<_>>()
                            .join("\n");
                        app.entries.push(ConversationEntry::System {
                            content: format!("Characters:\n{list}"),
                            timestamp: String::new(),
                        });
                        if app.auto_scroll {
                            app.scroll_to_bottom();
                        }
                    }
                }
                "switch_character" => {
                    if let Some(name) = co.data.get("character").and_then(|v| v.as_str()) {
                        app.character_name = name.to_string();
                        app.set_status(format!("switched to {name}"));
                    }
                }
                "list_models" => {
                    if let Some(models) = co.data.get("models").and_then(|v| v.as_array()) {
                        let names: Vec<&str> = models
                            .iter()
                            .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
                            .collect();
                        // Cache model names for tab completion
                        app.model_names = names.iter().map(|n| n.to_string()).collect();
                        let active = &app.model;
                        let list = names
                            .iter()
                            .map(|n| if *n == active { format!("  * {n}") } else { format!("    {n}") })
                            .collect::<Vec<_>>()
                            .join("\n");
                        app.entries.push(ConversationEntry::System {
                            content: format!("Models:\n{list}"),
                            timestamp: String::new(),
                        });
                        if app.auto_scroll {
                            app.scroll_to_bottom();
                        }
                    }
                }
                "switch_model" => {
                    if let Some(name) = co.data.get("model").and_then(|v| v.as_str()) {
                        app.model = name.to_string();
                        app.set_status(format!("model: {name}"));
                    }
                }
                "reset_model" => {
                    app.model.clear();
                    app.set_status("model reset to default");
                }
                "memory" => {
                    let summary = serde_json::to_string_pretty(&co.data)
                        .unwrap_or_else(|_| co.data.to_string());
                    app.entries.push(ConversationEntry::System {
                        content: summary,
                        timestamp: String::new(),
                    });
                    if app.auto_scroll {
                        app.scroll_to_bottom();
                    }
                }
                "compact" | "collate" => {
                    let status = co.data.get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("done");
                    app.set_status(format!("{}: {status}", co.name));
                }
                _ => {
                    app.set_status(format!("cmd:{} completed", co.name));
                }
            }
        }

        ServerMessage::Error(err) => {
            app.set_status(format!("error: {:?} - {}", err.code, err.message));
        }

        ServerMessage::CacheWarning(cw) => {
            app.set_status(format!("cache warning: {}", cw.message));
        }

        ServerMessage::History(hist) => {
            // Re-sync history
            app.image_cache.clear();
            app.entries.clear();
            for msg in hist.messages {
                expand_msg(msg, &mut app.entries);
            }
            transmit_entry_images(app);
        }

        // Ignore unexpected messages
        _ => {}
    }
    vec![]
}
