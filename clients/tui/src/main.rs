mod app;
mod clipboard;
mod connection;
mod images;
mod input;
mod markdown;
mod ui;

use std::io;
use std::time::Duration;

use clap::Parser;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, EventStream};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, DisableLineWrap, EnableLineWrap, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use shore_protocol::server_msg::ServerMessage;
use shore_protocol::types::{ContentBlock, Message, Role};
use tracing::{info, instrument};
use tracing_subscriber::EnvFilter;

use app::{App, ConnectionStatus, ConversationEntry, InputState, StreamBlock};
use connection::{ConnCommand, ConnEvent};
use input::Action;

const STREAM_FRAME_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RedrawEffect {
    None,
    Immediate,
    ImmediateFull,
    DeferredStream,
}

pub(crate) struct UiEffect {
    cmds: Vec<ConnCommand>,
    redraw: RedrawEffect,
}

impl UiEffect {
    fn redraw(redraw: RedrawEffect) -> Self {
        Self {
            cmds: vec![],
            redraw,
        }
    }

    fn none() -> Self {
        Self::redraw(RedrawEffect::None)
    }
}

#[derive(Parser)]
#[command(name = "shore-tui", about = "Shore terminal UI")]
struct Cli {
    /// TCP address of the daemon
    #[arg(long)]
    addr: Option<String>,

    /// Config path to select daemon instance
    #[arg(long)]
    config: Option<String>,

    /// Character to connect as
    #[arg(short, long)]
    character: Option<String>,
}

fn main() -> io::Result<()> {
    // TUI owns the terminal, so log to a file instead of stdout/stderr.
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let log_dir = std::path::Path::new(&runtime_dir).join("shore");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("tui.log"))
        .expect("failed to open tui.log");
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(true)
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(log_file))
        .init();

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
            if let Some(b) = v.get("show_images").and_then(|v| v.as_bool()) {
                app.show_images = b;
            }
        }
    }
}

fn save_prefs(app: &App) {
    let v = serde_json::json!({
        "show_thinking": app.show_thinking,
        "show_tools": app.show_tools,
        "show_images": app.show_images,
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

    io::stdout().execute(DisableBracketedPaste)?;
    io::stdout().execute(EnableLineWrap)?;
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    let _ = std::process::Command::new(&editor).arg(&tmp).status();

    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    io::stdout().execute(DisableLineWrap)?;
    io::stdout().execute(EnableBracketedPaste)?;
    terminal.clear()?;

    if let Ok(contents) = std::fs::read_to_string(&tmp) {
        input.set_text(contents.trim_end_matches('\n').to_string());
    }
    Ok(())
}

/// Open an external file picker and return the selected path(s).
///
/// Tries (in order): $SHORE_FILE_PICKER, yazi, fzf. Leaves the alternate
/// screen while the picker runs so it can draw freely.
fn pick_image(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    start_dir: Option<&str>,
) -> io::Result<Vec<String>> {
    let chooser_file = std::env::temp_dir().join("shore_image_pick");
    // Remove stale chooser file
    let _ = std::fs::remove_file(&chooser_file);

    let start = start_dir.unwrap_or(".");

    io::stdout().execute(DisableBracketedPaste)?;
    io::stdout().execute(EnableLineWrap)?;
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    let result = try_yazi(&chooser_file, start).or_else(|| try_fzf(&chooser_file, start));

    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    io::stdout().execute(DisableLineWrap)?;
    io::stdout().execute(EnableBracketedPaste)?;
    terminal.clear()?;

    match result {
        Some(true) => {
            // Read selected path(s) from chooser file
            if let Ok(contents) = std::fs::read_to_string(&chooser_file) {
                let paths: Vec<String> = contents
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
                Ok(paths)
            } else {
                Ok(vec![])
            }
        }
        Some(false) => Ok(vec![]), // picker ran but user cancelled
        None => {
            // No picker found — print hint to stderr before restoring screen
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no file picker found (install yazi or fzf)",
            ))
        }
    }
}

/// Try launching yazi. Returns Some(true) on success, Some(false) on
/// cancel/error, None if yazi is not installed.
fn try_yazi(chooser_file: &std::path::Path, start: &str) -> Option<bool> {
    let status = std::process::Command::new("yazi")
        .arg(start)
        .arg("--chooser-file")
        .arg(chooser_file)
        .status()
        .ok()?;
    Some(status.success() && chooser_file.exists())
}

/// Try launching fzf with image-aware preview. Returns Some(true) on
/// success, Some(false) on cancel, None if fzf is not installed.
fn try_fzf(chooser_file: &std::path::Path, start: &str) -> Option<bool> {
    // Build a list of image files and pipe into fzf
    let find = std::process::Command::new("find")
        .arg(start)
        .arg("-type")
        .arg("f")
        .arg("(")
        .args(["-iname", "*.png", "-o"])
        .args(["-iname", "*.jpg", "-o"])
        .args(["-iname", "*.jpeg", "-o"])
        .args(["-iname", "*.webp", "-o"])
        .args(["-iname", "*.gif", "-o"])
        .args(["-iname", "*.bmp"])
        .arg(")")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;

    // Detect best preview command: chafa > kitty icat > file
    let preview_cmd = if which_exists("chafa") {
        "chafa -s ${FZF_PREVIEW_COLUMNS}x${FZF_PREVIEW_LINES} {}".to_string()
    } else if which_exists("kitty") {
        "kitty icat --clear --transfer-mode=memory --stdin=no {}".to_string()
    } else {
        "file {}".to_string()
    };

    let status = std::process::Command::new("fzf")
        .arg("--preview")
        .arg(&preview_cmd)
        .arg("--preview-window=right:50%")
        .stdin(find.stdout.unwrap())
        .stdout(std::fs::File::create(chooser_file).ok()?)
        .status()
        .ok()?;

    Some(status.success() && chooser_file.exists())
}

/// Check whether a command exists on PATH.
fn which_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn send_conn_commands(
    cmd_tx: &tokio::sync::mpsc::Sender<ConnCommand>,
    cmds: Vec<ConnCommand>,
) {
    for cmd in cmds {
        let _ = cmd_tx.send(cmd).await;
    }
}

async fn handle_conn_event_and_send(
    app: &mut App,
    cmd_tx: &tokio::sync::mpsc::Sender<ConnCommand>,
    event: ConnEvent,
) -> UiEffect {
    let effect = handle_conn_event(app, event);
    send_conn_commands(cmd_tx, effect.cmds).await;
    UiEffect {
        cmds: vec![],
        redraw: effect.redraw,
    }
}

fn apply_redraw_effect(
    effect: RedrawEffect,
    needs_redraw: &mut bool,
    deferred_stream_dirty: &mut bool,
    needs_full_redraw: &mut bool,
) {
    match effect {
        RedrawEffect::None => {}
        // Stream chunks are painted by the next stream frame tick.
        RedrawEffect::DeferredStream => *deferred_stream_dirty = true,
        RedrawEffect::Immediate => *needs_redraw = true,
        RedrawEffect::ImmediateFull => {
            *needs_redraw = true;
            *needs_full_redraw = true;
        }
    }
}

async fn process_conn_event(
    app: &mut App,
    cmd_tx: &tokio::sync::mpsc::Sender<ConnCommand>,
    event: ConnEvent,
    needs_redraw: &mut bool,
    deferred_stream_dirty: &mut bool,
    needs_full_redraw: &mut bool,
) {
    let effect = handle_conn_event_and_send(app, cmd_tx, event).await;
    apply_redraw_effect(
        effect.redraw,
        needs_redraw,
        deferred_stream_dirty,
        needs_full_redraw,
    );
}

fn mark_connection_task_exited(app: &mut App, conn_events_open: &mut bool) {
    if !*conn_events_open {
        return;
    }
    *conn_events_open = false;
    app.connection_status = ConnectionStatus::Disconnected;
    app.set_status("connection task exited");
}

async fn handle_action(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    cmd_tx: &tokio::sync::mpsc::Sender<ConnCommand>,
    action: Action,
) -> io::Result<bool> {
    match action {
        Action::Quit => {
            app.should_quit = true;
            Ok(true)
        }
        Action::Interrupt => {
            app.interrupt = true;
            app.should_quit = true;
            Ok(true)
        }
        Action::Send(cmd) => {
            let _ = cmd_tx.send(cmd).await;
            Ok(true)
        }
        Action::SendMulti(cmds) => {
            send_conn_commands(cmd_tx, cmds).await;
            Ok(true)
        }
        Action::OpenInEditor => {
            let _ = open_in_editor(terminal, &mut app.input);
            Ok(true)
        }
        Action::PickImage(start_dir) => {
            match pick_image(terminal, start_dir.as_deref()) {
                Ok(paths) if paths.is_empty() => {
                    // User cancelled; the alternate screen was still restored.
                }
                Ok(paths) => {
                    let count = paths.len();
                    app.pending_images.extend(paths);
                    app.set_status(format!(
                        "attached {count} image(s) ({} pending)",
                        app.pending_images.len()
                    ));
                }
                Err(e) => {
                    app.set_status(format!("image picker: {e}"));
                }
            }
            Ok(true)
        }
        Action::PasteImage => {
            let result = tokio::time::timeout(
                Duration::from_millis(1500),
                tokio::task::spawn_blocking(clipboard::read_image_to_temp),
            )
            .await;
            match result {
                Ok(Ok(Ok(path))) => {
                    let path_str = path.to_string_lossy().into_owned();
                    app.pending_images.push(path_str);
                    app.paste_temp_paths.push(path);
                    app.set_status(format!(
                        "pasted image ({} pending)",
                        app.pending_images.len()
                    ));
                }
                Ok(Ok(Err(e))) => app.set_status(e.to_string()),
                Ok(Err(_join)) => app.set_status("paste task panicked"),
                Err(_elapsed) => app.set_status("clipboard read timed out"),
            }
            Ok(true)
        }
        Action::Redraw => Ok(true),
        Action::None => Ok(false),
    }
}

#[instrument(skip(cli))]
async fn run_tui(cli: Cli) -> io::Result<()> {
    // Set up terminal
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    io::stdout().execute(DisableLineWrap)?;
    io::stdout().execute(EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let character = resolve_character(cli.character);
    info!(character = ?character, "TUI starting");

    let mut app = App {
        connection_status: ConnectionStatus::Connecting,
        ..App::default()
    };
    // Probe terminal for kitty graphics support (raw mode is now active).
    app.image_cache.probe_protocol();
    load_prefs(&mut app);

    // Spawn connection manager
    let (cmd_tx, mut event_rx) = connection::spawn_connection(cli.addr, cli.config, character);

    let mut terminal_events = EventStream::new();
    let mut stream_frame = tokio::time::interval(STREAM_FRAME_INTERVAL);
    stream_frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut needs_redraw = true;
    let mut deferred_stream_dirty = false;
    let mut needs_full_redraw = false;
    let mut conn_events_open = true;

    // Main event loop
    let result = loop {
        if needs_redraw {
            if needs_full_redraw {
                terminal.clear()?;
                needs_full_redraw = false;
            }
            terminal.draw(|frame| ui::draw(frame, &mut app))?;
            needs_redraw = false;
            deferred_stream_dirty = false;
        }

        // Poll for events (crossterm keyboard or connection events)
        tokio::select! {
            biased;
            // External SIGINT — safety net if the keyboard loop ever wedges
            // and in case the user kills us with `kill -INT <pid>`.
            _ = tokio::signal::ctrl_c() => {
                app.interrupt = true;
                app.should_quit = true;
            }
            // Connection events
            conn_event = event_rx.recv(), if conn_events_open => {
                match conn_event {
                    Some(event) => {
                        process_conn_event(
                            &mut app,
                            &cmd_tx,
                            event,
                            &mut needs_redraw,
                            &mut deferred_stream_dirty,
                            &mut needs_full_redraw,
                        ).await;

                        loop {
                            match event_rx.try_recv() {
                                Ok(event) => {
                                    process_conn_event(
                                        &mut app,
                                        &cmd_tx,
                                        event,
                                        &mut needs_redraw,
                                        &mut deferred_stream_dirty,
                                        &mut needs_full_redraw,
                                    ).await;
                                }
                                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                                    mark_connection_task_exited(&mut app, &mut conn_events_open);
                                    break;
                                }
                            }
                        }

                    }
                    None => {
                        mark_connection_task_exited(&mut app, &mut conn_events_open);
                        needs_redraw = true;
                    }
                }
            }
            terminal_event = terminal_events.next() => {
                match terminal_event {
                    Some(Ok(ev)) => {
                        let action = input::handle_event(&mut app, ev);
                        needs_redraw |= handle_action(&mut terminal, &mut app, &cmd_tx, action).await?;
                    }
                    Some(Err(e)) => return Err(e),
                    None => {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "terminal event stream ended",
                        ));
                    }
                }
            }
            // Keep progress indicators moving and coalesce high-rate stream chunks.
            _ = stream_frame.tick(), if app.stream.active => {
                app.spinner_frame = app.spinner_frame.wrapping_add(1);
                let scheduled_deferred_stream_paint = deferred_stream_dirty;
                needs_redraw = true;
                if scheduled_deferred_stream_paint {
                    deferred_stream_dirty = false;
                }
            }
        }

        if app.should_quit {
            break Ok(());
        }
    };

    info!("TUI exiting");
    // Save preferences and shutdown
    save_prefs(&app);
    let _ = cmd_tx.send(ConnCommand::Shutdown).await;

    // Best-effort cleanup of paste-origin temp files.
    for path in &app.paste_temp_paths {
        let _ = std::fs::remove_file(path);
    }

    // Restore terminal
    io::stdout().execute(DisableBracketedPaste)?;
    io::stdout().execute(EnableLineWrap)?;
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    // If the user interrupted us (Ctrl+C or external SIGINT), exit with the
    // conventional 128+SIGINT=130 code so supervisors see it as a real signal.
    if app.interrupt {
        result?;
        std::process::exit(130);
    }

    result
}

fn handle_conn_event(app: &mut App, event: ConnEvent) -> UiEffect {
    match event {
        ConnEvent::Connected {
            characters,
            history,
            config,
            selected_character,
            ..
        } => {
            app.connection_status = ConnectionStatus::Connected;
            app.characters = characters.clone();

            if let Some(selected) = selected_character {
                app.character_name = selected;
            } else if let Some(ch) = characters.first() {
                app.character_name = ch.name.clone();
            }

            // Check private flag from config
            if let Some(private) = config.get("private").and_then(|v| v.as_bool()) {
                app.is_private = private;
            }
            if let Some(model) = config.get("active_model").and_then(|v| v.as_str()) {
                app.model = model.to_string();
            } else {
                app.model.clear();
            }

            // Load any history from the handshake
            app.entries.clear();
            for msg in history {
                expand_msg(msg, &mut app.entries);
            }
            transmit_entry_images(app);

            app.set_status("connected");
            UiEffect::redraw(RedrawEffect::Immediate)
        }

        ConnEvent::Disconnected(reason) => {
            app.connection_status = ConnectionStatus::Connecting;
            app.stream.reset(); // clear stale streaming state
            app.set_status(format!("reconnecting: {reason}"));
            UiEffect::redraw(RedrawEffect::Immediate)
        }

        ConnEvent::Message(msg) => handle_server_message(app, msg),
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
                msg_id: Some(msg.msg_id),
                content: msg.content,
                images: msg.images,
                timestamp: msg.timestamp,
                metadata: None,
            },
            Role::System => ConversationEntry::System {
                content: msg.content,
                count: 1,
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
                let name = tool_names.get(tool_use_id.as_str()).unwrap_or(&"tool");
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
            msg_id: Some(msg.msg_id),
            content,
            images: msg.images,
            timestamp: msg.timestamp,
            metadata: None,
        });
    }
}

/// Max display cells for images: 80% terminal width (minus indent), 50% terminal height.
fn image_max_cells() -> (u16, u16) {
    let (w, h) = crossterm::terminal::size().unwrap_or((80, 24));
    let max_cols = (w * 80 / 100).saturating_sub(4).max(1);
    let max_rows = (h * 50 / 100).max(1);
    (max_cols, max_rows)
}

/// Transmit images from conversation entries to kitty.
/// Prefers embedded base64 data; falls back to reading from path.
fn transmit_entry_images(app: &mut App) {
    let (max_cols, max_rows) = image_max_cells();
    for entry in &app.entries {
        let imgs = match entry {
            ConversationEntry::User { images, .. }
            | ConversationEntry::Assistant { images, .. } => images,
            _ => continue,
        };
        for img in imgs {
            transmit_image_ref(&mut app.image_cache, img, max_cols, max_rows);
        }
    }
}

/// Transmit a single ImageRef: prefer embedded data, fall back to path.
fn transmit_image_ref(
    cache: &mut images::ImageCache,
    img: &shore_protocol::types::ImageRef,
    max_cols: u16,
    max_rows: u16,
) {
    if let Some(b64) = &img.data {
        cache.ensure_transmitted_from_b64(&img.path, b64, max_cols, max_rows);
    } else {
        cache.ensure_transmitted(&img.path, max_cols, max_rows);
    }
}

pub(crate) fn handle_server_message(app: &mut App, msg: ServerMessage) -> UiEffect {
    match &msg {
        ServerMessage::AudioStart(_) | ServerMessage::AudioError(_) => {
            app.handle_audio_message(&msg);
            return UiEffect::redraw(RedrawEffect::Immediate);
        }
        ServerMessage::AudioChunk(_) | ServerMessage::AudioEnd(_) => {
            app.handle_audio_message(&msg);
            return UiEffect::none();
        }
        _ => {}
    }

    let redraw = match msg {
        ServerMessage::StreamStart(start) => {
            app.spinner_frame = 0;
            if start.regen {
                app.begin_regen_optimistic();
            } else if !app.stream.active {
                app.stream.reset();
                app.stream.active = true;
            } else {
                // Continuation within a multi-phase (tool-use) turn — preserve
                // accumulators so the final Assistant entry reflects the whole turn.
                app.stream.blocks.clear();
                app.stream.phase = "responding".into();
                app.stream.tool_name = None;
            }
            RedrawEffect::Immediate
        }

        ServerMessage::StreamChunk(chunk) => {
            let is_thinking = chunk.content_type == "thinking";
            match (is_thinking, app.stream.blocks.last_mut()) {
                (true, Some(StreamBlock::Thinking(ref mut s))) => s.push_str(&chunk.text),
                (false, Some(StreamBlock::Text(ref mut s))) => s.push_str(&chunk.text),
                (true, _) => app.stream.blocks.push(StreamBlock::Thinking(chunk.text)),
                (false, _) => app.stream.blocks.push(StreamBlock::Text(chunk.text)),
            }
            app.stream.phase = if is_thinking {
                "thinking"
            } else {
                "responding"
            }
            .into();
            if app.auto_scroll {
                app.scroll_to_bottom();
            }
            RedrawEffect::DeferredStream
        }

        ServerMessage::StreamEnd(end) => {
            if end.finish_reason == "cancelled" {
                app.stream.reset();
                app.set_status("generation cancelled");
                return UiEffect::redraw(RedrawEffect::ImmediateFull);
            }

            let keep_bottom = app.auto_scroll;
            app.model = end.metadata.model.clone();
            app.tokens = end.metadata.tokens.clone();

            if !end.content.is_empty() {
                if !app.stream.accumulated_text.is_empty() {
                    app.stream.accumulated_text.push_str("\n\n");
                }
                app.stream.accumulated_text.push_str(&end.content);
            }

            match app.stream.accumulated_metadata.as_mut() {
                Some(acc) => {
                    acc.model = end.metadata.model.clone();
                    acc.tokens.input += end.metadata.tokens.input;
                    acc.tokens.output += end.metadata.tokens.output;
                    acc.tokens.cache_read += end.metadata.tokens.cache_read;
                    acc.tokens.cache_write += end.metadata.tokens.cache_write;
                    acc.timing.total_ms += end.metadata.timing.total_ms;
                    // Preserve ttft_ms from the first phase.
                }
                None => {
                    app.stream.accumulated_metadata = Some(end.metadata.clone());
                }
            }

            if end.finish_reason == "tool_use" {
                app.stream.blocks.clear();
                app.stream.phase = "tool_use".into();
                app.stream.tool_name = None;
                RedrawEffect::Immediate
            } else {
                // The daemon broadcasts a History snapshot during
                // `engine.append_message` (in persist_and_notify) and only
                // then emits StreamEnd, so the assistant entry is already
                // present in `app.entries`. Attach streaming metadata to
                // that entry — pushing a new one would duplicate it.
                let metadata = app.stream.accumulated_metadata.take();
                let slot_pos = if let Some(target) = end.msg_id.as_deref() {
                    app.entries.iter().rposition(|e| {
                        matches!(
                            e,
                            ConversationEntry::Assistant {
                                msg_id: Some(entry_msg_id),
                                ..
                            } if entry_msg_id == target
                        )
                    })
                } else {
                    app.entries
                        .iter()
                        .rposition(|e| matches!(e, ConversationEntry::Assistant { .. }))
                };
                if let Some(pos) = slot_pos {
                    if let ConversationEntry::Assistant { metadata: slot, .. } =
                        &mut app.entries[pos]
                    {
                        *slot = metadata;
                    }
                } else {
                    // Fallback: History hasn't been applied yet (shouldn't
                    // happen given daemon ordering). Push so the user isn't
                    // left with a blank turn.
                    let content = std::mem::take(&mut app.stream.accumulated_text);
                    app.entries.push(ConversationEntry::Assistant {
                        msg_id: end.msg_id.clone(),
                        content,
                        images: vec![],
                        timestamp: String::new(),
                        metadata,
                    });
                }
                app.stream.reset();
                if keep_bottom {
                    app.scroll_to_bottom();
                }
                RedrawEffect::ImmediateFull
            }
        }

        ServerMessage::Phase(phase) => {
            app.stream.phase = phase.phase;
            if let Some(model) = phase.model {
                app.model = model;
            }
            RedrawEffect::Immediate
        }

        ServerMessage::NewMessage(_) => RedrawEffect::None,

        ServerMessage::ToolCall(tc) => {
            app.stream.active = true;
            app.stream.phase = "tool_use".into();
            app.stream.tool_name = Some(tc.tool_name.clone());
            app.entries.push(ConversationEntry::ToolCall {
                tool_id: tc.tool_id,
                tool_name: tc.tool_name,
                input: tc.input,
            });
            if app.auto_scroll {
                app.scroll_to_bottom();
            }
            RedrawEffect::Immediate
        }

        ServerMessage::ToolResult(tr) => {
            app.stream.tool_name = None;
            app.entries.push(ConversationEntry::ToolResult {
                tool_id: tr.tool_id,
                tool_name: tr.tool_name,
                output: tr.output,
                is_error: tr.is_error,
            });
            if app.auto_scroll {
                app.scroll_to_bottom();
            }
            RedrawEffect::Immediate
        }

        ServerMessage::SendImage(img) => {
            let (max_cols, max_rows) = image_max_cells();
            if let Some(b64) = &img.data {
                app.image_cache
                    .ensure_transmitted_from_b64(&img.path, b64, max_cols, max_rows);
            } else {
                app.image_cache
                    .ensure_transmitted(&img.path, max_cols, max_rows);
            }
            RedrawEffect::Immediate
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
                            .map(|n| {
                                if n == active {
                                    format!("  * {n}")
                                } else {
                                    format!("    {n}")
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        app.entries.push(ConversationEntry::System {
                            content: format!("Characters:\n{list}"),
                            count: 1,
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
                    }
                    if let Some(model) = co.data.get("active_model").and_then(|v| v.as_str()) {
                        app.model = model.to_string();
                    } else if co.data.get("active_model").is_some_and(|v| v.is_null()) {
                        app.model.clear();
                    }
                    if let Some(private) = co.data.get("private").and_then(|v| v.as_bool()) {
                        app.is_private = private;
                    }
                    if let Some(name) = co.data.get("character").and_then(|v| v.as_str()) {
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
                        // Only show the list if the user explicitly requested it
                        if app.show_model_list {
                            app.show_model_list = false;
                            let active = &app.model;
                            let list = names
                                .iter()
                                .map(|n| {
                                    if *n == active {
                                        format!("  * {n}")
                                    } else {
                                        format!("    {n}")
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            app.entries.push(ConversationEntry::System {
                                content: format!("Models:\n{list}"),
                                count: 1,
                                timestamp: String::new(),
                            });
                            if app.auto_scroll {
                                app.scroll_to_bottom();
                            }
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
                "set_reasoning_effort" => {
                    // Daemon returns `effective` as the value that will reach
                    // the request; show that alongside a marker for "overridden"
                    // vs "inherited from config" so the user can tell which is
                    // in force without reading the config.
                    let effective = match co.data.get("effective") {
                        Some(v) if v.is_null() => "off".to_string(),
                        Some(v) => v
                            .as_str()
                            .map(String::from)
                            .unwrap_or_else(|| v.to_string()),
                        None => "(unknown)".to_string(),
                    };
                    let overridden = co
                        .data
                        .get("override")
                        .map(|v| !v.is_null())
                        .unwrap_or(false);
                    let tag = if overridden { "override" } else { "config" };
                    app.set_status(format!("reasoning: {effective} ({tag})"));
                }
                "memory" => {
                    let summary = serde_json::to_string_pretty(&co.data)
                        .unwrap_or_else(|_| co.data.to_string());
                    app.entries.push(ConversationEntry::System {
                        content: summary,
                        count: 1,
                        timestamp: String::new(),
                    });
                    if app.auto_scroll {
                        app.scroll_to_bottom();
                    }
                }
                "delete" => {
                    if let Some(deleted) = co.data.get("deleted").and_then(|v| v.as_array()) {
                        let count = deleted.len();
                        app.set_status(format!("deleted {count} message(s)"));
                    }
                }
                "compact" => {
                    let status = co
                        .data
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("done");
                    app.set_status(format!("{}: {status}", co.name));
                }
                _ => {
                    app.set_status(format!("cmd:{} completed", co.name));
                }
            }
            RedrawEffect::Immediate
        }

        ServerMessage::Error(err) => {
            app.set_status(format!("error: {:?} - {}", err.code, err.message));
            RedrawEffect::Immediate
        }

        ServerMessage::CacheWarning(cw) => {
            app.set_status(format!("cache warning: {}", cw.message));
            RedrawEffect::Immediate
        }

        ServerMessage::ProviderFallbackWarning(w) => {
            app.set_status(w.message.clone());
            RedrawEffect::Immediate
        }

        ServerMessage::History(hist) => {
            if let Some(private) = hist.config.get("private").and_then(|v| v.as_bool()) {
                app.is_private = private;
            }
            if let Some(model) = hist.config.get("active_model").and_then(|v| v.as_str()) {
                app.model = model.to_string();
            }
            if let Some(selected) = hist.selected_character {
                app.character_name = selected;
            }
            // Re-sync history
            app.image_cache.clear();
            app.entries.clear();
            for msg in hist.messages {
                expand_msg(msg, &mut app.entries);
            }
            transmit_entry_images(app);
            RedrawEffect::Immediate
        }

        // Ignore unexpected messages
        _ => RedrawEffect::Immediate,
    };
    UiEffect::redraw(redraw)
}

#[cfg(test)]
mod redraw_tests {
    use super::*;
    use shore_protocol::server_msg::{StreamChunk, StreamEnd};
    use shore_protocol::types::{StreamMetadata, TimingInfo, TokenCounts};

    fn metadata() -> StreamMetadata {
        StreamMetadata {
            model: "test-model".into(),
            tokens: TokenCounts {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
            },
            timing: TimingInfo {
                total_ms: 1,
                ttft_ms: 1,
            },
        }
    }

    #[test]
    fn final_stream_end_requests_full_redraw() {
        let mut app = App::default();
        let effect = handle_server_message(
            &mut app,
            ServerMessage::StreamEnd(StreamEnd {
                rid: None,
                msg_id: None,
                revision: None,
                content: "done".into(),
                metadata: metadata(),
                finish_reason: "end_turn".into(),
                is_final: true,
            }),
        );

        assert_eq!(effect.redraw, RedrawEffect::ImmediateFull);
    }

    #[test]
    fn tool_use_stream_end_keeps_regular_redraw() {
        let mut app = App::default();
        let effect = handle_server_message(
            &mut app,
            ServerMessage::StreamEnd(StreamEnd {
                rid: None,
                msg_id: None,
                revision: None,
                content: String::new(),
                metadata: metadata(),
                finish_reason: "tool_use".into(),
                is_final: false,
            }),
        );

        assert_eq!(effect.redraw, RedrawEffect::Immediate);
    }

    #[test]
    fn deferred_stream_effect_marks_dirty_without_immediate_redraw() {
        let mut needs_redraw = false;
        let mut deferred_stream_dirty = false;
        let mut needs_full_redraw = false;

        apply_redraw_effect(
            RedrawEffect::DeferredStream,
            &mut needs_redraw,
            &mut deferred_stream_dirty,
            &mut needs_full_redraw,
        );

        assert!(!needs_redraw);
        assert!(deferred_stream_dirty);
        assert!(!needs_full_redraw);
    }

    #[test]
    fn immediate_full_effect_requests_full_redraw() {
        let mut needs_redraw = false;
        let mut deferred_stream_dirty = true;
        let mut needs_full_redraw = false;

        apply_redraw_effect(
            RedrawEffect::ImmediateFull,
            &mut needs_redraw,
            &mut deferred_stream_dirty,
            &mut needs_full_redraw,
        );

        assert!(needs_redraw);
        assert!(deferred_stream_dirty);
        assert!(needs_full_redraw);
    }

    #[test]
    fn stream_chunk_effect_is_deferred() {
        let mut app = App::default();
        let effect = handle_server_message(
            &mut app,
            ServerMessage::StreamChunk(StreamChunk {
                rid: None,
                text: "partial".into(),
                content_type: "text".into(),
            }),
        );

        assert_eq!(effect.redraw, RedrawEffect::DeferredStream);
    }

    #[test]
    fn final_stream_end_attaches_metadata_by_msg_id_when_available() {
        let target_meta = metadata();
        let mut app = App::default();
        app.entries.push(ConversationEntry::Assistant {
            msg_id: Some("m_target".into()),
            content: "target".into(),
            images: vec![],
            timestamp: "t1".into(),
            metadata: None,
        });
        app.entries.push(ConversationEntry::Assistant {
            msg_id: Some("m_later".into()),
            content: "later".into(),
            images: vec![],
            timestamp: "t2".into(),
            metadata: None,
        });

        handle_server_message(
            &mut app,
            ServerMessage::StreamEnd(StreamEnd {
                rid: None,
                msg_id: Some("m_target".into()),
                revision: Some(7),
                content: "target".into(),
                metadata: target_meta.clone(),
                finish_reason: "end_turn".into(),
                is_final: true,
            }),
        );

        let target = app
            .entries
            .iter()
            .find_map(|entry| match entry {
                ConversationEntry::Assistant {
                    msg_id: Some(msg_id),
                    metadata,
                    ..
                } if msg_id == "m_target" => metadata.as_ref(),
                _ => None,
            })
            .expect("target assistant metadata");
        assert_eq!(target.model, target_meta.model);

        let later_metadata = app.entries.iter().find_map(|entry| match entry {
            ConversationEntry::Assistant {
                msg_id: Some(msg_id),
                metadata,
                ..
            } if msg_id == "m_later" => Some(metadata),
            _ => None,
        });
        assert!(matches!(later_metadata, Some(None)));
    }

    #[test]
    fn final_stream_end_with_unmatched_msg_id_does_not_annotate_latest_assistant() {
        let mut app = App::default();
        app.stream.accumulated_text = "new response".into();
        app.entries.push(ConversationEntry::Assistant {
            msg_id: Some("m_existing".into()),
            content: "existing".into(),
            images: vec![],
            timestamp: "t1".into(),
            metadata: None,
        });

        handle_server_message(
            &mut app,
            ServerMessage::StreamEnd(StreamEnd {
                rid: None,
                msg_id: Some("m_missing_from_history".into()),
                revision: Some(8),
                content: String::new(),
                metadata: metadata(),
                finish_reason: "end_turn".into(),
                is_final: true,
            }),
        );

        let existing_metadata = app.entries.iter().find_map(|entry| match entry {
            ConversationEntry::Assistant {
                msg_id: Some(msg_id),
                metadata,
                ..
            } if msg_id == "m_existing" => Some(metadata),
            _ => None,
        });
        assert!(matches!(existing_metadata, Some(None)));
        assert!(app.entries.iter().any(|entry| matches!(
            entry,
            ConversationEntry::Assistant {
                msg_id: Some(msg_id),
                metadata: Some(_),
                ..
            } if msg_id == "m_missing_from_history"
        )));
    }
}
