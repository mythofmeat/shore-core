mod app;
mod connection;
mod images;
mod input;
mod markdown;
mod ui;

use std::io;
use std::time::Duration;

use clap::Parser;
use crossterm::event::{self, Event};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use shore_protocol::server_msg::ServerMessage;
use shore_protocol::types::Role;

use app::{App, ConnectionStatus, ConversationEntry};
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
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create tokio runtime");

    rt.block_on(run_tui(cli))
}

async fn run_tui(cli: Cli) -> io::Result<()> {
    // Set up terminal
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App {
        connection_status: ConnectionStatus::Connecting,
        ..App::default()
    };

    // Spawn connection manager
    let (cmd_tx, mut event_rx) =
        connection::spawn_connection(cli.socket, cli.config);

    // Main event loop
    let result = loop {
        // Draw
        terminal.draw(|frame| ui::draw(frame, &app))?;

        // Poll for events (crossterm keyboard or connection events)
        tokio::select! {
            biased;
            // Connection events
            conn_event = event_rx.recv() => {
                match conn_event {
                    Some(event) => handle_conn_event(&mut app, event),
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
                        Action::Quit => break,
                        Action::Send(cmd) => {
                            let _ = cmd_tx.send(cmd).await;
                        }
                        Action::Redraw | Action::None => {}
                    }
                }
            }
        }

        if app.should_quit {
            break Ok(());
        }

        // Check for quit from the keyboard polling above
        // (handle_event may have been called in a nested context)
        if event::poll(Duration::from_millis(0))? {
            if let Event::Key(key) = event::read()? {
                match input::handle_event(&mut app, Event::Key(key)) {
                    Action::Quit => break Ok(()),
                    Action::Send(cmd) => {
                        let _ = cmd_tx.send(cmd).await;
                    }
                    _ => {}
                }
            }
        }
    };

    // Shutdown
    let _ = cmd_tx.send(ConnCommand::Shutdown).await;

    // Restore terminal
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    result
}

fn handle_conn_event(app: &mut App, event: ConnEvent) {
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

            // Load history
            app.entries.clear();
            for msg in history {
                let entry = match msg.role {
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
                };
                app.entries.push(entry);
            }

            app.set_status("connected");
        }

        ConnEvent::Disconnected(reason) => {
            app.connection_status = ConnectionStatus::Connecting;
            app.set_status(format!("reconnecting: {reason}"));
        }

        ConnEvent::Message(msg) => handle_server_message(app, msg),
    }
}

fn handle_server_message(app: &mut App, msg: ServerMessage) {
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
        }

        ServerMessage::Phase(phase) => {
            app.stream.phase = phase.phase;
            if let Some(model) = phase.model {
                app.model = model;
            }
        }

        ServerMessage::NewMessage(new_msg) => {
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
            app.entries.push(entry);
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
            // Attempt inline rendering
            images::render_image(&img.path, img.caption.as_deref());
        }

        ServerMessage::CommandOutput(co) => {
            app.set_status(format!("cmd:{} completed", co.name));
        }

        ServerMessage::Error(err) => {
            app.set_status(format!("error: {:?} - {}", err.code, err.message));
        }

        ServerMessage::CacheWarning(cw) => {
            app.set_status(format!("cache warning: {}", cw.message));
        }

        ServerMessage::History(hist) => {
            // Re-sync history
            app.entries.clear();
            for msg in hist.messages {
                let entry = match msg.role {
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
                };
                app.entries.push(entry);
            }
        }

        // Ignore unexpected messages
        _ => {}
    }
}
