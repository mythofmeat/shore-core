use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, FixedOffset, Local};
use crossterm::style::{Attribute, Color, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use shore_protocol::server_msg::{NewMessage, Phase, SendImage, StreamChunk, StreamEnd, ToolCall, ToolResult};
use shore_protocol::types::ImageRef;
use tokio::task::JoinHandle;

use crate::images;

// ---------------------------------------------------------------------------
// Color control (NO_COLOR / --no-color)
// ---------------------------------------------------------------------------

static COLOR_ENABLED: AtomicBool = AtomicBool::new(true);

/// Set whether color output is enabled. Call once at startup.
pub fn set_color_enabled(enabled: bool) {
    COLOR_ENABLED.store(enabled, Ordering::Relaxed);
}

fn use_color() -> bool {
    COLOR_ENABLED.load(Ordering::Relaxed)
}

/// Strip trailing date suffix (`-YYYYMMDD`) from a model ID.
fn abbreviate_model(model_id: &str) -> &str {
    if let Some(i) = model_id.rfind('-') {
        let suffix = &model_id[i + 1..];
        if suffix.len() == 8 && suffix.bytes().all(|b| b.is_ascii_digit()) {
            return &model_id[..i];
        }
    }
    model_id
}

/// Max characters to display for a tool result before truncating.
const MAX_TOOL_OUTPUT: usize = 500;

// Track whether the previous chunk was thinking, so we can insert a separator
// when transitioning from thinking → text.
static WAS_THINKING: AtomicBool = AtomicBool::new(false);

/// Reset stream chunk state (call at the start of each new stream).
pub fn reset_chunk_state() {
    WAS_THINKING.store(false, Ordering::Relaxed);
}

/// Print a stream chunk to stdout. Thinking chunks are shown dimmed.
/// A separator is printed when transitioning from thinking to text.
pub fn print_chunk(chunk: &StreamChunk) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let is_thinking = chunk.content_type == "thinking";

    // Insert a separator when transitioning from thinking → text.
    if !is_thinking && WAS_THINKING.swap(false, Ordering::Relaxed) {
        let _ = writeln!(out);
        if use_color() {
            let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
            let _ = write!(out, "---");
            let _ = crossterm::execute!(out, ResetColor);
        } else {
            let _ = write!(out, "---");
        }
        let _ = writeln!(out);
        let _ = writeln!(out); // breathing room before response
    }

    if is_thinking {
        WAS_THINKING.store(true, Ordering::Relaxed);
        if use_color() {
            let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
            let _ = write!(out, "{}", chunk.text);
            let _ = crossterm::execute!(out, ResetColor);
        } else {
            let _ = write!(out, "{}", chunk.text);
        }
    } else {
        let _ = write!(out, "{}", chunk.text);
    }
    let _ = out.flush();
}

/// Print stream metadata after stream_end.
pub fn print_stream_end(end: &StreamEnd) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    // Newline after streamed content
    let _ = writeln!(out);

    // Metadata line in dim
    let model = abbreviate_model(&end.metadata.model);
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
    }
    let _ = write!(
        out,
        "[{} | in:{} out:{} cache_r:{} cache_w:{} | ttft:{}ms total:{}ms]",
        model,
        end.metadata.tokens.input,
        end.metadata.tokens.output,
        end.metadata.tokens.cache_read,
        end.metadata.tokens.cache_write,
        end.metadata.timing.ttft_ms,
        end.metadata.timing.total_ms,
    );
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = writeln!(out);
    let _ = writeln!(out); // blank line after metadata
}


/// Print an error in red to stderr.
pub fn print_error(err: &dyn std::fmt::Display) {
    let stderr = io::stderr();
    let mut out = stderr.lock();

    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::Red));
    }
    let _ = write!(out, "error");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = writeln!(out, ": {err}");
}

/// Print a server protocol error.
pub fn print_server_error(code: &str, message: &str) {
    let stderr = io::stderr();
    let mut out = stderr.lock();

    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::Red));
    }
    let _ = write!(out, "server error");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = writeln!(out, " [{code}]: {message}");
}

/// Render an inline image from a SendImage server message.
pub fn print_send_image(img: &SendImage) {
    images::render_image(&img.path, img.caption.as_deref());
}

/// Render inline images from a message's image references.
pub fn print_image_refs(refs: &[ImageRef]) {
    for img in refs {
        images::render_image(&img.path, img.caption.as_deref());
    }
}

/// Print a push NewMessage in the transcript format (used in follow mode).
pub fn print_new_message(msg: &NewMessage, character_name: &str) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    let time_str = parse_timestamp(&msg.message.timestamp)
        .map(|dt| dt.format("%H:%M").to_string())
        .unwrap_or_default();

    let (name, color) = match msg.message.role {
        shore_protocol::types::Role::User => ("You", Color::Cyan),
        shore_protocol::types::Role::Assistant => (character_name, character_color(character_name)),
        shore_protocol::types::Role::System => ("system", Color::DarkGrey),
    };

    write_header(&mut out, name, &time_str, color, width);
    let _ = writeln!(out, "{}", msg.message.content);
    let _ = writeln!(out);

    // Render any attached images.
    print_image_refs(&msg.message.images);
}

/// Print a transcript header for the assistant before streaming begins.
pub fn print_follow_stream_start(character_name: &str) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();
    let time_str = chrono::Local::now().format("%H:%M").to_string();
    let color = character_color(character_name);
    write_header(&mut out, character_name, &time_str, color, width);
}

/// Format a tool input value for display. Compact single-line for simple
/// values, truncated if too long.
fn format_tool_input(input: &serde_json::Value) -> Option<String> {
    // Skip empty objects (no arguments).
    if input.as_object().is_some_and(|o| o.is_empty()) {
        return None;
    }
    let s = serde_json::to_string(input).ok()?;
    if s.len() > MAX_TOOL_OUTPUT {
        let end = s.floor_char_boundary(MAX_TOOL_OUTPUT);
        Some(format!("{}...", &s[..end]))
    } else {
        Some(s)
    }
}

/// Print a tool call notification with its input arguments.
pub fn print_tool_call(call: &ToolCall) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let _ = writeln!(out);
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkYellow));
    }
    let _ = write!(out, "[tool: {}]", call.tool_name);
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    if let Some(input) = format_tool_input(&call.input) {
        if use_color() {
            let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
        }
        let _ = write!(out, " {input}");
        if use_color() {
            let _ = crossterm::execute!(out, ResetColor);
        }
    }
    let _ = writeln!(out);
}

/// Print a tool result. Long outputs are truncated.
pub fn print_tool_result(result: &ToolResult) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let color = if result.is_error { Color::Red } else { Color::DarkGrey };
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(color));
    }

    let label = if result.is_error { "error" } else { "result" };
    let output = &result.output;
    if output.len() > MAX_TOOL_OUTPUT {
        let end = output.floor_char_boundary(MAX_TOOL_OUTPUT);
        let _ = write!(out, "[{label}: {}... truncated, {} bytes total]", &output[..end], output.len());
    } else {
        let _ = write!(out, "[{label}: {output}]");
    }

    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = writeln!(out);
}

/// Print the "thinking..." indicator when streaming starts.
pub fn print_stream_start(regen: bool) {
    if !regen {
        return;
    }
    let stdout = io::stdout();
    let mut out = stdout.lock();

    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
    }
    let _ = write!(out, "(regenerating...) ");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = out.flush();
}

/// Print a phase indicator (e.g. "thinking...") during streaming.
pub fn print_phase(phase: &Phase) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let label = match phase.phase.as_str() {
        "thinking" => "thinking...",
        other => other,
    };

    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
    }
    let _ = write!(out, "({label}) ");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = out.flush();
}

// ---------------------------------------------------------------------------
// Stream spinner — live-updating elapsed time during streaming
// ---------------------------------------------------------------------------

struct SpinnerState {
    phase: String,
    model: Option<String>,
    start: Instant,
    active: bool,
}

/// Live-updating status line shown during LLM streaming.
///
/// Displays elapsed time and current phase (e.g. `(thinking... 2.3s)`),
/// updated every 200ms. Automatically disabled when stdout is not a terminal.
pub struct StreamSpinner {
    state: Arc<Mutex<SpinnerState>>,
    handle: Option<JoinHandle<()>>,
    is_terminal: bool,
    cleared: bool,
}

/// Format the spinner display line from current state.
fn format_spinner_line(phase: &str, model: Option<&str>, elapsed_secs: f64) -> String {
    let label = match phase {
        "thinking" => "thinking...",
        "" => "generating...",
        other => other,
    };
    match model {
        Some(m) => format!("({label} {elapsed_secs:.1}s · {m}) "),
        None => format!("({label} {elapsed_secs:.1}s) "),
    }
}

impl StreamSpinner {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(SpinnerState {
                phase: String::new(),
                model: None,
                start: Instant::now(),
                active: false,
            })),
            handle: None,
            is_terminal: io::stdout().is_terminal(),
            cleared: false,
        }
    }

    /// Start the spinner render loop. No-op if stdout is not a terminal.
    pub fn start(&mut self) {
        if !self.is_terminal {
            return;
        }
        self.cleared = false;
        {
            let mut s = self.state.lock().unwrap();
            s.start = Instant::now();
            s.active = true;
        }

        let state = Arc::clone(&self.state);
        self.handle = Some(tokio::spawn(async move {
            let mut first = true;
            loop {
                if first {
                    first = false;
                } else {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                let line = {
                    let s = state.lock().unwrap();
                    if !s.active {
                        break;
                    }
                    let elapsed = s.start.elapsed().as_secs_f64();
                    let model_abbrev = s.model.as_deref().map(abbreviate_model);
                    format_spinner_line(&s.phase, model_abbrev, elapsed)
                };
                let stdout = io::stdout();
                let mut out = stdout.lock();
                let _ = write!(out, "\r");
                let _ = crossterm::execute!(out, Clear(ClearType::CurrentLine));
                if use_color() {
                    let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
                }
                let _ = write!(out, "{line}");
                if use_color() {
                    let _ = crossterm::execute!(out, ResetColor);
                }
                let _ = out.flush();
            }
        }));
    }

    pub fn set_phase(&self, phase: &str) {
        let mut s = self.state.lock().unwrap();
        s.phase = phase.to_string();
    }

    pub fn set_model(&self, model: Option<String>) {
        let mut s = self.state.lock().unwrap();
        s.model = model;
    }

    /// Whether the spinner render loop is running.
    pub fn is_active(&self) -> bool {
        self.state.lock().unwrap().active
    }

    /// Clear the spinner line and stop the render task.
    pub async fn clear(&mut self) {
        if self.cleared {
            return;
        }
        self.cleared = true;
        {
            let mut s = self.state.lock().unwrap();
            s.active = false;
        }
        if let Some(h) = self.handle.take() {
            let _ = h.await;
        }
        if self.is_terminal {
            let stdout = io::stdout();
            let mut out = stdout.lock();
            let _ = write!(out, "\r");
            let _ = crossterm::execute!(out, Clear(ClearType::CurrentLine));
            let _ = out.flush();
        }
    }

    /// Stop the spinner (alias for clear). Use when streaming ends without chunks.
    pub async fn stop(&mut self) {
        self.clear().await;
    }

    /// Restart the spinner for a new LLM round (e.g. after tool execution).
    pub fn restart(&mut self) {
        self.cleared = false;
        self.start();
    }
}

// ---------------------------------------------------------------------------
// Log formatter — human-readable chat transcript (Option B)
// ---------------------------------------------------------------------------

/// Terminal colors that look distinct on both dark and light backgrounds.
const CHARACTER_PALETTE: &[Color] = &[
    Color::Magenta,
    Color::Green,
    Color::DarkYellow,
    Color::Blue,
    Color::DarkCyan,
    Color::Red,
    Color::DarkMagenta,
    Color::DarkGreen,
];

/// Deterministic color derived from a character name.
fn character_color(name: &str) -> Color {
    let hash = name.bytes().fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    CHARACTER_PALETTE[(hash as usize) % CHARACTER_PALETTE.len()]
}

/// Get terminal width, falling back to 80 columns.
fn term_width() -> usize {
    crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80)
}

/// Parse an RFC 3339 timestamp to local time.
fn parse_timestamp(ts: &str) -> Option<DateTime<Local>> {
    DateTime::<FixedOffset>::parse_from_rfc3339(ts)
        .map(|dt| dt.with_timezone(&Local))
        .ok()
}

/// Format a timestamp for display. Uses "HH:MM" normally,
/// "Mon DD · HH:MM" if the date differs from `prev_date`.
fn format_time(dt: &DateTime<Local>, prev_date: Option<&str>) -> String {
    let date = dt.format("%Y-%m-%d").to_string();
    let time = dt.format("%H:%M").to_string();
    match prev_date {
        Some(prev) if prev == date => time,
        None => time, // first message, just show time
        Some(_) => dt.format("%b %d · %H:%M").to_string(),
    }
}

/// Write a colored header line: `── Name · HH:MM ─────────────────────`
fn write_header(
    out: &mut impl Write,
    name: &str,
    time_str: &str,
    color: Color,
    width: usize,
) {
    // "── Name · HH:MM " = 4 + name.len() + 3 + time.len() + 1
    let prefix = format!("── {} · {} ", name, time_str);
    let prefix_len = prefix.chars().count();
    let trail = if width > prefix_len { width - prefix_len } else { 0 };
    let rule: String = "─".repeat(trail);

    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(color));
    }
    let _ = write!(out, "{prefix}{rule}");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = writeln!(out);
}

/// Print the conversation log as a human-readable chat transcript.
///
/// `character_name` is used for assistant messages; pass the active character
/// name or a fallback like "Assistant".
pub fn print_log(messages: &[serde_json::Value], character_name: &str) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();
    let char_color = character_color(character_name);

    let mut prev_date: Option<String> = None;

    for msg in messages {
        let role_str = msg["role"].as_str().unwrap_or("user");
        let content = msg["content"].as_str().unwrap_or("");
        let ts = msg["timestamp"].as_str().unwrap_or("");
        let images = msg["images"].as_array();
        let content_blocks = msg["content_blocks"].as_array();

        let parsed_ts = parse_timestamp(ts);
        let time_str = parsed_ts
            .as_ref()
            .map(|dt| format_time(dt, prev_date.as_deref()))
            .unwrap_or_default();

        // Update prev_date for next iteration
        if let Some(dt) = &parsed_ts {
            prev_date = Some(dt.format("%Y-%m-%d").to_string());
        }

        // Detect tool-result-only "user" messages (from tool loop).
        let is_tool_result_msg = role_str == "user"
            && content_blocks.map_or(false, |blocks| {
                !blocks.is_empty()
                    && blocks.iter().all(|b| b["type"].as_str() == Some("tool_result"))
            });

        // Write header (skip for tool result messages — they're continuations).
        if !is_tool_result_msg {
            match role_str {
                "user" => write_header(&mut out, "You", &time_str, Color::Cyan, width),
                "assistant" => write_header(&mut out, character_name, &time_str, char_color, width),
                "system" => {
                    if use_color() {
                        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
                    }
                    let prefix = format!("── system · {} ", time_str);
                    let prefix_len = prefix.chars().count();
                    let trail = if width > prefix_len { width - prefix_len } else { 0 };
                    let _ = write!(out, "{prefix}{}", "─".repeat(trail));
                    let _ = writeln!(out);
                }
                _ => {}
            }
        }

        // Render content blocks if present, otherwise fall back to plain content.
        if let Some(blocks) = content_blocks {
            if !blocks.is_empty() {
                let mut was_thinking = false;
                for block in blocks {
                    let block_type = block["type"].as_str().unwrap_or("text");
                    // Insert separator when transitioning from thinking → non-thinking.
                    if was_thinking && block_type != "thinking" && block_type != "redacted_thinking" {
                        was_thinking = false;
                        if use_color() {
                            let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
                        }
                        let _ = writeln!(out, "---");
                        if use_color() {
                            let _ = crossterm::execute!(out, ResetColor);
                        }
                    }
                    match block_type {
                        "text" => {
                            let text = block["text"].as_str().unwrap_or("");
                            if !text.is_empty() {
                                let _ = writeln!(out, "{text}");
                            }
                        }
                        "thinking" => {
                            was_thinking = true;
                            let thinking = block["thinking"].as_str().unwrap_or("");
                            if !thinking.is_empty() {
                                if use_color() {
                                    let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
                                }
                                let _ = writeln!(out, "{thinking}");
                                if use_color() {
                                    let _ = crossterm::execute!(out, ResetColor);
                                }
                            }
                        }
                        "redacted_thinking" => {
                            was_thinking = true;
                            if use_color() {
                                let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
                            }
                            let _ = writeln!(out, "[redacted thinking]");
                            if use_color() {
                                let _ = crossterm::execute!(out, ResetColor);
                            }
                        }
                        "tool_use" => {
                            let name = block["name"].as_str().unwrap_or("?");
                            if use_color() {
                                let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkYellow));
                            }
                            let _ = write!(out, "[tool: {name}]");
                            if use_color() {
                                let _ = crossterm::execute!(out, ResetColor);
                            }
                            if let Some(input_str) = format_tool_input(&block["input"]) {
                                if use_color() {
                                    let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
                                }
                                let _ = write!(out, " {input_str}");
                                if use_color() {
                                    let _ = crossterm::execute!(out, ResetColor);
                                }
                            }
                            let _ = writeln!(out);
                        }
                        "tool_result" => {
                            let output = block["content"].as_str().unwrap_or("");
                            let is_error = block["is_error"].as_bool().unwrap_or(false);
                            let color = if is_error { Color::Red } else { Color::DarkGrey };
                            let label = if is_error { "error" } else { "result" };
                            if use_color() {
                                let _ = crossterm::execute!(out, SetForegroundColor(color));
                            }
                            if output.len() > MAX_TOOL_OUTPUT {
                                let end = output.floor_char_boundary(MAX_TOOL_OUTPUT);
                                let _ = write!(out, "[{label}: {}... truncated]", &output[..end]);
                            } else {
                                let _ = write!(out, "[{label}: {output}]");
                            }
                            if use_color() {
                                let _ = crossterm::execute!(out, ResetColor);
                            }
                            let _ = writeln!(out);
                        }
                        _ => {}
                    }
                }
            } else {
                // Empty content_blocks — fall back to content string.
                if !content.is_empty() {
                    let _ = writeln!(out, "{content}");
                }
            }
        } else {
            // No content_blocks field — legacy message, show content string.
            if !content.is_empty() || !is_tool_result_msg {
                let _ = writeln!(out, "{content}");
            }
        }

        // System messages: close dimming.
        if role_str == "system" && use_color() {
            let _ = crossterm::execute!(out, ResetColor);
        }

        // Images
        if let Some(imgs) = images {
            for img in imgs {
                let label = img["caption"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        img["path"].as_str().and_then(|p| {
                            p.rsplit('/').next()
                        })
                    })
                    .unwrap_or("image");

                if use_color() {
                    let _ = crossterm::execute!(out, SetForegroundColor(Color::Yellow));
                }
                let _ = write!(out, "  📎 {label}");
                if use_color() {
                    let _ = crossterm::execute!(out, ResetColor);
                }
                let _ = writeln!(out);
            }
        }

        // Blank line between messages
        let _ = writeln!(out);
    }
}


// ---------------------------------------------------------------------------
// Status formatter — human-readable dashboard
// ---------------------------------------------------------------------------

/// Write a section header: `── Title ──────────────────────`
fn write_section_header(out: &mut impl Write, title: &str, suffix: &str, width: usize) {
    let prefix = if suffix.is_empty() {
        format!("── {} ", title)
    } else {
        format!("── {} ({}) ", title, suffix)
    };
    let prefix_len = prefix.chars().count();
    let trail = if width > prefix_len { width - prefix_len } else { 0 };
    let rule: String = "─".repeat(trail);

    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::White));
    }
    let _ = write!(out, "{prefix}{rule}");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = writeln!(out);
}

/// Write a label-value row, optionally coloring the value.
fn write_row_with(out: &mut impl Write, label: &str, value: &str, color: Option<Color>) {
    write_dim(out, &format!("  {label:<13}"));
    match color {
        Some(c) => write_fg(out, c, value),
        None => { let _ = write!(out, "{value}"); }
    }
    let _ = writeln!(out);
}

/// Write a label-value row: `  Label        Value`
fn write_row(out: &mut impl Write, label: &str, value: &str) {
    write_row_with(out, label, value, None);
}

/// Write a label-value row with the value in a specific color.
fn write_row_colored(out: &mut impl Write, label: &str, value: &str, color: Color) {
    write_row_with(out, label, value, Some(color));
}

/// Translate an interiority state string to a human-readable description.
fn interiority_description(state: &str, ticks: u64, max_ticks: u64) -> String {
    match state {
        "Active" if ticks == 0 => "active — in conversation".to_string(),
        "Active" => format!("active — idle {ticks}/{max_ticks} ticks"),
        "Dormant" => "dormant — waiting for you".to_string(),
        other => other.to_string(),
    }
}

/// Map a normalized density (0.0–1.0) to a bar character.
///
/// Uses 8 Unicode block elements (▁▂▃▄▅▆▇█) for non-zero values and `░` for
/// effectively-zero values.
fn density_to_block(normalized: f64) -> char {
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if normalized < 0.05 {
        '░'
    } else {
        let idx = ((normalized * 7.0).round() as usize).min(7);
        BLOCKS[idx]
    }
}

/// Color for an hour classification label.
fn classification_color(class: &str) -> Color {
    match class {
        "peak" => Color::Cyan,
        "trough" => Color::DarkGrey,
        _ => Color::White,
    }
}

/// Write the activity heatmap section into the status dashboard.
///
/// Renders a 24-character bar chart (one block per hour) with hour labels
/// underneath, plus engagement and session stats.
fn write_activity_section(
    out: &mut impl Write,
    activity: &serde_json::Value,
    width: usize,
) {
    let histogram: Vec<f64> = match activity["hour_histogram"].as_array() {
        Some(arr) => arr.iter().filter_map(|v| v.as_f64()).collect(),
        None => return,
    };
    if histogram.len() != 24 {
        return;
    }
    let classifications: Vec<String> = activity["hour_classifications"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if classifications.len() != 24 {
        return;
    }

    let sufficient = activity["has_sufficient_heatmap"].as_bool().unwrap_or(false);
    let suffix = if sufficient { "" } else { "sparse" };
    write_section_header(out, "Activity", suffix, width);

    // -- bar chart row --
    let max_val = histogram.iter().cloned().fold(0.0_f64, f64::max);
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
    }
    let _ = write!(out, "  {:<13}", "");
    for (i, &density) in histogram.iter().enumerate() {
        let linear = if max_val > 0.0 { density / max_val } else { 0.0 };
        // Log scale: ln(1 + x·k) / ln(1+k) — spreads low values, compresses peaks.
        let normalized = (1.0 + linear * 9.0).ln() / 10.0_f64.ln();
        let ch = density_to_block(normalized);
        if use_color() {
            let color = classification_color(&classifications[i]);
            let _ = crossterm::execute!(out, SetForegroundColor(color));
        }
        let _ = write!(out, "{ch}");
    }
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = writeln!(out);

    // -- hour labels row --
    //    0  3  6  9  12 15 18 21
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
    }
    let _ = write!(out, "  {:<13}0  3  6  9  12 15 18 21", "");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = writeln!(out);

    // -- stats row --
    let engagement = activity["engagement_score"].as_f64().unwrap_or(0.0);
    let sessions = activity["sessions_per_day"].as_f64().unwrap_or(0.0);
    let msg_count = activity["message_count"].as_u64().unwrap_or(0);
    write_row(
        out,
        "Engagement",
        &format!("{engagement:.2} · {sessions:.1} sessions/day · {msg_count} msgs"),
    );

    let _ = writeln!(out);
}


/// Print the status dashboard.
pub fn print_status(data: &serde_json::Value, character_name: &str) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    // ── Status ──────────────────────────────────────────
    write_section_header(&mut out, "Status", "", width);

    // Prefer the character name from the daemon response over the CLI fallback.
    let effective_name = data["character"].as_str().unwrap_or(character_name);
    let char_color = character_color(effective_name);
    write_row_colored(&mut out, "Character", effective_name, char_color);

    let model = data["active_model"].as_str().unwrap_or("(none)");
    write_row(&mut out, "Model", abbreviate_model(model));

    if let Some(count) = data["message_count"].as_u64() {
        write_row(&mut out, "Messages", &count.to_string());
    }

    // Memory info (if present in the response).
    if let Some(mem) = data.get("memory") {
        let total = mem["total_entries"].as_u64().unwrap_or(0);
        let active = mem["active_entries"].as_u64().unwrap_or(0);
        if total > 0 {
            write_row(&mut out, "Memory", &format!("{total} entries ({active} active)"));
        }
    }

    let _ = writeln!(out);

    // ── Clients ─────────────────────────────────────────
    if let Some(clients) = data.get("clients").and_then(|c| c.as_array()) {
        if !clients.is_empty() {
            write_section_header(&mut out, "Clients", "", width);
            for client in clients {
                let ctype = client["client_type"].as_str().unwrap_or("?");
                let cname = client["client_name"].as_str().unwrap_or("?");
                write_row(&mut out, ctype, cname);
            }
            let _ = writeln!(out);
        }
    }

    // ── Autonomy ────────────────────────────────────────
    if let Some(autonomy) = data.get("autonomy") {
        if !autonomy.is_null() {
            let paused = autonomy["paused"].as_bool().unwrap_or(false);
            let suffix = if paused { "paused" } else { "" };
            write_section_header(&mut out, "Autonomy", suffix, width);

            let int_state = autonomy["interiority_state"].as_str().unwrap_or("Active");
            let ticks = autonomy["ticks_without_user"].as_u64().unwrap_or(0);
            let max_ticks = autonomy["max_idle_ticks"].as_u64().unwrap_or(8);
            let description = interiority_description(int_state, ticks, max_ticks);

            // Interiority row: description + state label.
            if use_color() {
                let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
            }
            let _ = write!(out, "  {:<13}", "Interiority");
            if use_color() {
                let _ = crossterm::execute!(out, ResetColor);
            }
            let _ = write!(out, "{description}  ");
            if use_color() {
                let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
            }
            let _ = write!(out, "({int_state})");
            if use_color() {
                let _ = crossterm::execute!(out, ResetColor);
            }
            let _ = writeln!(out);

            // Cache keepalive (only when pings > 0).
            let pings = autonomy["cache_keepalive_pings"].as_u64().unwrap_or(0);
            if pings > 0 {
                let cache_state = autonomy["cache_keepalive_state"].as_str().unwrap_or("?");
                let label = match cache_state {
                    "Pinging" => "warm",
                    "Monitoring" => "warm",
                    s if s.starts_with("Stopped") => "stopped",
                    _ => cache_state,
                };
                write_row(&mut out, "Cache", &format!("{label} ({pings} pings)"));
            }

            let _ = writeln!(out);
        }
    }

    // ── Activity ───────────────────────────────────────
    if let Some(activity) = data.get("activity") {
        if !activity.is_null() {
            let msg_count = activity["message_count"].as_u64().unwrap_or(0);
            if msg_count > 0 {
                write_activity_section(&mut out, activity, width);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Command-specific formatters
// ---------------------------------------------------------------------------

/// Dispatch a command response to the appropriate formatter.
/// Falls back to generic JSON output for unknown command names.
pub fn format_command(name: &str, data: &serde_json::Value) {
    match name {
        "character_info" => print_character_info(data),
        "list_models" => print_model_list(data),
        "switch_model" => print_model_switched(data),
        "reset_model" => print_model_reset(data),
        "model_info" => print_model_info(data),
        "memory" => print_memory(data),
        "compact" => print_compact_result(data),
        "collate" => print_collate_result(data),
        "memory_purge" => print_purge_result(data),
        "memory_changelog" => print_changelog(data),
        "memory_reindex" => print_reindex(data),
        "config" => print_config(data),
        "config_check" => print_config_check(data),
        "config_reset" => print_config_reset(data),
        "edit" => print_edit_confirmation(data),
        "delete" => print_delete_confirmation(data),
        "inject_system" => println!("System instruction injected."),
        "diagnostics" => print_diagnostics(data),
        _ => print_command_output_fallback(name, data),
    }
}

fn print_command_output_fallback(name: &str, data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    if use_color() {
        let _ = crossterm::execute!(out, SetAttribute(Attribute::Bold));
    }
    let _ = write!(out, "{name}");
    if use_color() {
        let _ = crossterm::execute!(out, SetAttribute(Attribute::Reset));
    }
    let _ = writeln!(out);
    if let Ok(pretty) = serde_json::to_string_pretty(data) {
        let _ = writeln!(out, "{pretty}");
    }
}

/// Print a single message in the same transcript format as print_log.
pub fn print_single_message(data: &serde_json::Value, character_name: &str) {
    print_log(&[data.clone()], character_name);
}

/// Print edit confirmation.
fn print_edit_confirmation(data: &serde_json::Value) {
    let msg_ref = data["ref"].as_str().unwrap_or("?");
    println!("Edited message {msg_ref}");
}

/// Print delete confirmation.
fn print_delete_confirmation(data: &serde_json::Value) {
    if let Some(arr) = data["deleted"].as_array() {
        let n = arr.len();
        if n == 1 {
            let id = arr[0].as_str().unwrap_or("?");
            println!("Deleted message {id}");
        } else {
            println!("Deleted {n} messages");
        }
    } else if let Some(id) = data["deleted"].as_str() {
        println!("Deleted message {id}");
    }
}

/// Print model list.
fn print_model_list(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    let active = data["active"].as_str().unwrap_or("");

    write_section_header(&mut out, "Models", "", width);

    if let Some(models) = data["models"].as_array() {
        for m in models {
            let name = m["name"].as_str().unwrap_or("?");
            let provider = m["provider"].as_str().unwrap_or("?");
            let is_active = name == active
                || m["qualified_name"].as_str() == Some(active);

            let marker = if is_active { "*" } else { " " };

            if use_color() && is_active {
                let _ = crossterm::execute!(out, SetForegroundColor(Color::Cyan));
            } else if use_color() {
                let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
            }
            let _ = write!(out, "  {marker} ");
            if use_color() {
                let _ = crossterm::execute!(out, ResetColor);
            }
            let _ = write!(out, "{name:<24}");
            if use_color() {
                let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
            }
            let _ = write!(out, "{provider}");
            if use_color() {
                let _ = crossterm::execute!(out, ResetColor);
            }
            let _ = writeln!(out);
        }
    }
    let _ = writeln!(out);
}

/// Print model switch confirmation.
fn print_model_switched(data: &serde_json::Value) {
    let model = data["active"].as_str().unwrap_or("(none)");
    println!("Switched to model: {}", abbreviate_model(model));
}

/// Print model reset confirmation.
fn print_model_reset(data: &serde_json::Value) {
    let model = data["active"].as_str().unwrap_or("(none)");
    println!("Model reset to: {}", abbreviate_model(model));
}

/// Print detailed model info.
fn print_model_info(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    let name = data["name"].as_str().unwrap_or("?");
    write_section_header(&mut out, "Model", name, width);

    if let Some(qn) = data["qualified_name"].as_str() {
        write_row(&mut out, "Qualified", qn);
    }
    if let Some(mid) = data["model_id"].as_str() {
        write_row(&mut out, "Model ID", mid);
    }
    if let Some(sdk) = data["sdk"].as_str() {
        write_row(&mut out, "SDK", sdk);
    }
    if let Some(pk) = data["provider_key"].as_str() {
        write_row(&mut out, "Provider", pk);
    }
    if let Some(url) = data["base_url"].as_str() {
        write_row(&mut out, "Base URL", url);
    }
    if let Some(key) = data["api_key_env"].as_str() {
        write_row(&mut out, "API key env", &format!("${key}"));
    }

    // Cache settings
    if let Some(ttl) = data["cache_ttl_secs"].as_u64() {
        if ttl > 0 {
            write_row(&mut out, "Cache TTL", &format!("{ttl}s"));
        }
    }
    if let Some(depth) = data["cache_depth"].as_u64() {
        if depth > 0 {
            write_row(&mut out, "Cache depth", &depth.to_string());
        }
    }
    if let Some(re) = data["reasoning_effort"].as_str() {
        write_row(&mut out, "Reasoning", re);
    }
    if let Some(mt) = data["max_tokens"].as_u64() {
        write_row(&mut out, "Max tokens", &mt.to_string());
    }
    let _ = writeln!(out);
}

/// Print character info.
fn print_character_info(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    let name = data["name"].as_str().unwrap_or("?");
    let char_color = character_color(name);

    write_section_header(&mut out, "Character", "", width);
    write_row_colored(&mut out, "Name", name, char_color);

    let active = data["active"].as_bool().unwrap_or(false);
    if active {
        write_row_colored(&mut out, "Active", "yes", Color::Green);
    }

    if let Some(dir) = data["config_dir"].as_str() {
        write_row(&mut out, "Config", dir);
    }

    let has_def = data["has_definition"].as_bool().unwrap_or(false);
    let has_user = data["has_user_definition"].as_bool().unwrap_or(false);
    write_row(&mut out, "Definition", if has_def { "yes" } else { "no" });
    if has_user {
        write_row(&mut out, "User def", "yes");
    }

    if data["has_config_override"].as_bool().unwrap_or(false) {
        write_row_colored(&mut out, "Config override", "yes", Color::Yellow);
    }

    if let Some(overrides) = data["prompt_overrides"].as_array() {
        if !overrides.is_empty() {
            let names: Vec<&str> = overrides.iter().filter_map(|v| v.as_str()).collect();
            write_row(&mut out, "Prompts", &names.join(", "));
        }
    }

    if let Some(dir) = data["data_dir"].as_str() {
        write_row(&mut out, "Data", dir);
    }

    // Definition preview
    if let Some(preview) = data["definition_preview"].as_str() {
        if !preview.is_empty() {
            let _ = writeln!(out);
            write_section_header(&mut out, "Preview", "", width);
            // Show first few lines, dimmed
            if use_color() {
                let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
            }
            for line in preview.lines().take(8) {
                let _ = writeln!(out, "  {line}");
            }
            if use_color() {
                let _ = crossterm::execute!(out, ResetColor);
            }
        }
    }
    let _ = writeln!(out);
}

/// Print memory status or query result.
fn print_memory(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    // If there's a "result" field, this is a query response.
    if let Some(result) = data["result"].as_str() {
        let _ = writeln!(out, "{result}");
        return;
    }

    // Otherwise it's a status response.
    let char_name = data["character"].as_str().unwrap_or("?");
    write_section_header(&mut out, "Memory", char_name, width);

    let entries = data["entries"].as_u64().unwrap_or(0);
    let active = data["active_entries"].as_u64().unwrap_or(0);
    let entities = data["entities"].as_u64().unwrap_or(0);

    if entries > 0 {
        write_row(&mut out, "Entries", &format!("{entries} ({active} active)"));
    } else {
        write_row(&mut out, "Entries", "0");
    }
    write_row(&mut out, "Entities", &entities.to_string());
    let _ = writeln!(out);
}

/// Print memory changelog.
fn print_changelog(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    let char_name = data["character"].as_str().unwrap_or("?");
    write_section_header(&mut out, "Memory Changelog", char_name, width);

    if let Some(entries) = data["changelog"].as_array() {
        if entries.is_empty() {
            if use_color() {
                let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
            }
            let _ = writeln!(out, "  (no entries)");
            if use_color() {
                let _ = crossterm::execute!(out, ResetColor);
            }
        } else {
            for entry in entries {
                let ts = entry["timestamp"].as_str().unwrap_or("");
                let op = entry["operation"].as_str().unwrap_or("?");
                let desc = entry["description"].as_str().unwrap_or("");

                let time_display = parse_timestamp(ts)
                    .map(|dt| dt.format("%b %d %H:%M").to_string())
                    .unwrap_or_else(|| ts.to_string());

                if use_color() {
                    let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
                }
                let _ = write!(out, "  {time_display:<16}");

                let op_color = match op {
                    s if s.starts_with("create") || s.starts_with("compaction") => Color::Green,
                    s if s.starts_with("update") || s.starts_with("collation") => Color::DarkYellow,
                    s if s.starts_with("supersede") || s.starts_with("delete") || s.starts_with("decay") => Color::Red,
                    _ => Color::White,
                };
                if use_color() {
                    let _ = crossterm::execute!(out, SetForegroundColor(op_color));
                }
                let _ = write!(out, "{op:<18}");
                if use_color() {
                    let _ = crossterm::execute!(out, ResetColor);
                }
                let _ = writeln!(out, "{desc}");
            }
        }
    }
    let _ = writeln!(out);
}

/// Print compaction result.
fn print_compact_result(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    let status = data["status"].as_str().unwrap_or("?");
    let suffix = if status == "dry_run" { "dry run" } else { "" };
    write_section_header(&mut out, "Compaction", suffix, width);

    let char_name = data["character"].as_str().unwrap_or("?");
    write_row(&mut out, "Character", char_name);

    if status == "dry_run" {
        let would = data["would_create_entries"].as_u64().unwrap_or(0);
        write_row(&mut out, "Would create", &format!("{would} entries"));
        let msgs = data["message_count"].as_u64().unwrap_or(0);
        let retained_turns = data["retained_turns"].as_u64().unwrap_or(0);
        write_row(&mut out, "Messages", &format!("{msgs} compacted, {retained_turns} turns retained"));
    } else {
        let entries = data["entries_created"].as_u64().unwrap_or(0);
        write_row(&mut out, "Entries", &format!("{entries} new"));
        let msgs = data["message_count"].as_u64().unwrap_or(0);
        let retained_turns = data["retained_turns"].as_u64().unwrap_or(0);
        write_row(&mut out, "Messages", &format!("{msgs} compacted, {retained_turns} turns retained"));
        if data["recap_generated"].as_bool().unwrap_or(false) {
            write_row(&mut out, "Recap", "generated");
        }
    }

    // Collation results (if present).
    if let Some(collation) = data.get("collation").filter(|v| !v.is_null()) {
        let _ = writeln!(out);
        write_section_header(&mut out, "Collation", "", width);

        let tidy_splits = collation["tidy_splits"].as_u64().unwrap_or(0);
        let tidy_new = collation["tidy_new_entries"].as_u64().unwrap_or(0);
        if tidy_splits > 0 {
            write_row(&mut out, "Tidy", &format!("{tidy_splits} splits → {tidy_new} new"));
        }

        let merges = collation["collate_merges"].as_u64().unwrap_or(0);
        let merge_new = collation["collate_new_entries"].as_u64().unwrap_or(0);
        if merges > 0 {
            write_row(&mut out, "Merge", &format!("{merges} merges → {merge_new} new"));
        }

        let normalized = collation["entities_normalized"].as_u64().unwrap_or(0);
        if normalized > 0 {
            write_row(&mut out, "Normalize", &format!("{normalized} entities"));
        }

        let decayed = collation["entries_decayed"].as_u64().unwrap_or(0);
        if decayed > 0 {
            write_row(&mut out, "Decay", &format!("{decayed} entries"));
        }

        let skipped = collation["entries_skipped"].as_u64().unwrap_or(0);
        if skipped > 0 {
            write_row(&mut out, "Skipped", &format!("{skipped} entries"));
        }
    }
    let _ = writeln!(out);
}

/// Print reindex result.
fn print_reindex(data: &serde_json::Value) {
    let msg = data["message"].as_str().unwrap_or("Reindex complete");
    println!("{msg}");
}

/// Print standalone collation result.
fn print_collate_result(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    let char_name = data["character"].as_str().unwrap_or("unknown");
    let passes = data["passes"].as_u64().unwrap_or(1);

    write_section_header(&mut out, "Collation", char_name, width);

    if passes > 1 {
        write_row(&mut out, "Passes", &format!("{passes}"));
    }

    let backfilled = data["timestamps_backfilled"].as_u64().unwrap_or(0);
    if backfilled > 0 {
        write_row(&mut out, "Backfill", &format!("{backfilled} timestamps"));
    }

    let tidy_splits = data["tidy_splits"].as_u64().unwrap_or(0);
    let tidy_new = data["tidy_new_entries"].as_u64().unwrap_or(0);
    if tidy_splits > 0 {
        write_row(&mut out, "Tidy", &format!("{tidy_splits} splits → {tidy_new} new"));
    }

    let merges = data["collate_merges"].as_u64().unwrap_or(0);
    let merge_new = data["collate_new_entries"].as_u64().unwrap_or(0);
    if merges > 0 {
        write_row(&mut out, "Merge", &format!("{merges} merges → {merge_new} new"));
    }

    let normalized = data["entities_normalized"].as_u64().unwrap_or(0);
    if normalized > 0 {
        write_row(&mut out, "Normalize", &format!("{normalized} entities"));
    }

    let decayed = data["entries_decayed"].as_u64().unwrap_or(0);
    if decayed > 0 {
        write_row(&mut out, "Decay", &format!("{decayed} entries"));
    }

    let skipped = data["entries_skipped"].as_u64().unwrap_or(0);
    if skipped > 0 {
        write_row(&mut out, "Skipped", &format!("{skipped} entries"));
    }

    if tidy_splits == 0 && merges == 0 && normalized == 0 && decayed == 0 && backfilled == 0 {
        write_row(&mut out, "Result", "no changes");
    }

    let _ = writeln!(out);
}

/// Print purge result.
fn print_purge_result(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    let char_name = data["character"].as_str().unwrap_or("unknown");
    let older_than = data["older_than"].as_str().unwrap_or("?");

    write_section_header(&mut out, "Purge", char_name, width);

    write_row(&mut out, "Threshold", &format!("older than {older_than}"));

    let deleted = data["deleted"].as_u64().unwrap_or(0);
    write_row(&mut out, "Deleted", &format!("{deleted} entries"));

    let skipped_image = data["skipped_image"].as_u64().unwrap_or(0);
    if skipped_image > 0 {
        write_row(&mut out, "Skipped (image)", &format!("{skipped_image} entries"));
    }

    let skipped_no_repl = data["skipped_no_replacement"].as_u64().unwrap_or(0);
    if skipped_no_repl > 0 {
        write_row(&mut out, "Skipped (no repl)", &format!("{skipped_no_repl} entries"));
    }

    let _ = writeln!(out);
}

/// Print config display.
fn print_config(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    // Config set confirmation: { "set": "key", "value": ... }
    if let Some(key) = data["set"].as_str() {
        let value = &data["value"];
        let _ = writeln!(out, "Set {key} = {value}");
        return;
    }

    // Section view: { "key": "name", "config": { ... } }
    if let Some(key) = data["key"].as_str() {
        write_section_header(&mut out, "Config", key, width);
        print_config_section(&mut out, &data["config"], 1);
        let _ = writeln!(out);
        return;
    }

    // Full config: { "config": { ... } }
    if let Some(config) = data.get("config") {
        write_section_header(&mut out, "Config", "", width);
        print_config_section(&mut out, config, 1);
        let _ = writeln!(out);
    }
}

/// Recursively print config as indented key-value pairs.
fn print_config_section(out: &mut impl Write, value: &serde_json::Value, depth: usize) {
    let indent = "  ".repeat(depth);
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                match v {
                    serde_json::Value::Object(_) => {
                        if use_color() {
                            let _ = crossterm::execute!(out, SetForegroundColor(Color::White));
                        }
                        let _ = writeln!(out, "{indent}{k}:");
                        if use_color() {
                            let _ = crossterm::execute!(out, ResetColor);
                        }
                        print_config_section(out, v, depth + 1);
                    }
                    serde_json::Value::Null => {} // skip nulls
                    _ => {
                        if use_color() {
                            let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
                        }
                        let _ = write!(out, "{indent}{k:<24}");
                        if use_color() {
                            let _ = crossterm::execute!(out, ResetColor);
                        }
                        let display = match v {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Bool(b) => b.to_string(),
                            serde_json::Value::Number(n) => n.to_string(),
                            serde_json::Value::Array(arr) => {
                                let items: Vec<String> = arr.iter().map(|i| {
                                    i.as_str().map(String::from).unwrap_or_else(|| i.to_string())
                                }).collect();
                                items.join(", ")
                            }
                            _ => v.to_string(),
                        };
                        let _ = writeln!(out, "{display}");
                    }
                }
            }
        }
        _ => {
            let _ = writeln!(out, "{indent}{value}");
        }
    }
}

/// Print config check results.
fn print_config_check(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    let valid = data["valid"].as_bool().unwrap_or(false);
    let suffix = if valid { "valid" } else { "warnings" };
    write_section_header(&mut out, "Config Check", suffix, width);

    if let Some(dir) = data["config_dir"].as_str() {
        write_row(&mut out, "Config dir", dir);
    }
    if let Some(dir) = data["data_dir"].as_str() {
        write_row(&mut out, "Data dir", dir);
    }

    let chat = data["chat_models"].as_u64().unwrap_or(0);
    let tool = data["tool_models"].as_u64().unwrap_or(0);
    let embed = data["embedding_models"].as_u64().unwrap_or(0);
    write_row(&mut out, "Models", &format!("{chat} chat, {tool} tool, {embed} embedding"));

    let _ = writeln!(out);

    // Warnings
    if let Some(warnings) = data["warnings"].as_array() {
        for w in warnings {
            if let Some(msg) = w.as_str() {
                if use_color() {
                    let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkYellow));
                }
                let _ = write!(out, "  ! ");
                if use_color() {
                    let _ = crossterm::execute!(out, ResetColor);
                }
                let _ = writeln!(out, "{msg}");
            }
        }
    }

    // Info
    if let Some(info) = data["info"].as_array() {
        for i in info {
            if let Some(msg) = i.as_str() {
                if use_color() {
                    let _ = crossterm::execute!(out, SetForegroundColor(Color::Green));
                }
                let _ = write!(out, "  ");
                if use_color() {
                    let _ = crossterm::execute!(out, ResetColor);
                }
                let _ = writeln!(out, "{msg}");
            }
        }
    }
    let _ = writeln!(out);
}

/// Print config reset confirmation.
fn print_config_reset(data: &serde_json::Value) {
    let msg = data["message"].as_str().unwrap_or("Configuration reloaded from disk");
    println!("{msg}");
}

/// Print diagnostics from ring buffers.
pub fn print_diagnostics(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    // ── API Calls ───────────────────────
    print_diagnostics_section(&mut out, "API Calls", &data["api_calls"], width, |out, call| {
        let model = abbreviate_model(call["model"].as_str().unwrap_or("?"));
        let input = call["input_tokens"].as_u64().unwrap_or(0);
        let output_t = call["output_tokens"].as_u64().unwrap_or(0);
        let cr = call["cache_read_tokens"].as_u64().unwrap_or(0);
        let cw = call["cache_write_tokens"].as_u64().unwrap_or(0);
        let total = call["total_ms"].as_u64().unwrap_or(0);
        let secs = total as f64 / 1000.0;

        let _ = write!(out, "{model:<24}");
        write_dim(out, &format!("in:{input:<5} out:{output_t:<5} cache:{cr}/{cw}  {secs:.1}s"));

        if let Some(err) = call.get("error").filter(|v| !v.is_null()) {
            write_fg(out, Color::Red, &format!("  ERR: {}", err.as_str().unwrap_or("?")));
        }
        let _ = writeln!(out);
    });

    // ── Tool Calls ──────────────────────
    print_diagnostics_section(&mut out, "Tool Calls", &data["tool_calls"], width, |out, call| {
        let name = call["tool_name"].as_str().unwrap_or("?");
        let dur = call["duration_ms"].as_u64().unwrap_or(0);
        let ok = call["success"].as_bool().unwrap_or(true);

        let _ = write!(out, "{name:<24}");
        write_dim(out, &format!("{dur}ms  "));
        let (marker_color, marker_text) = if ok { (Color::Green, "ok") } else { (Color::Red, "FAIL") };
        write_fg(out, marker_color, marker_text);
        let _ = writeln!(out);
    });

    // ── Errors ──────────────────────────
    print_diagnostics_section(&mut out, "Errors", &data["errors"], width, |out, err| {
        let etype = err["error_type"].as_str().unwrap_or("?");
        let msg = err["message"].as_str().unwrap_or("?");

        write_fg(out, Color::Red, &format!("{etype:<12}"));
        let _ = writeln!(out, "{msg}");
    });
}

/// Print a diagnostics section with a header, shared timestamp formatting,
/// and a per-entry formatter.
fn print_diagnostics_section<W: Write>(
    out: &mut W,
    title: &str,
    section: &serde_json::Value,
    width: usize,
    mut format_row: impl FnMut(&mut W, &serde_json::Value),
) {
    let count = section["count"].as_u64().unwrap_or(0);
    write_section_header(out, title, &format!("{count} total"), width);

    if let Some(entries) = section["recent"].as_array() {
        if entries.is_empty() {
            print_dim_line(out, "(none)");
        } else {
            for entry in entries {
                let ts = entry["timestamp"].as_str().unwrap_or("");
                let time = parse_timestamp(ts)
                    .map(|dt| dt.format("%H:%M:%S").to_string())
                    .unwrap_or_else(|| ts.chars().take(8).collect());

                write_dim(out, &format!("  {time}  "));
                format_row(out, entry);
            }
        }
    }
    let _ = writeln!(out);
}

/// Write text in a specific foreground color (respects use_color()).
fn write_fg(out: &mut impl Write, color: Color, text: &str) {
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(color));
    }
    let _ = write!(out, "{text}");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
}

/// Write text in dim (DarkGrey) color.
fn write_dim(out: &mut impl Write, text: &str) {
    write_fg(out, Color::DarkGrey, text);
}

/// Print a dimmed line (for empty states).
fn print_dim_line(out: &mut impl Write, text: &str) {
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
    }
    let _ = writeln!(out, "  {text}");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
}

// ---------------------------------------------------------------------------
// Memory shell
// ---------------------------------------------------------------------------

/// Print the memory shell welcome banner.
pub fn print_memory_shell_welcome(character: &str) {
    let mut out = io::stderr().lock();
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::Cyan));
    }
    let _ = writeln!(out, "Memory shell for {character}");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = writeln!(out, "Type a query or command. /quit to exit.\n");
}

/// Print a memory shell response.
pub fn print_memory_shell_response(response: &str, mutations: &str) {
    if !response.is_empty() {
        println!("{response}");
    }
    if !mutations.is_empty() {
        let mut out = io::stdout().lock();
        if use_color() {
            let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
        }
        let _ = writeln!(out, "  [{mutations}]");
        if use_color() {
            let _ = crossterm::execute!(out, ResetColor);
        }
    }
    println!();
}

/// Print interiority event log returned by `shore log --heartbeat`.
pub fn print_heartbeat_log(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    let events = match data["events"].as_array() {
        Some(arr) => arr,
        None => {
            print_dim_line(&mut out, "(no interiority events)");
            return;
        }
    };

    if events.is_empty() {
        print_dim_line(&mut out, "(no interiority events)");
        return;
    }

    write_section_header(&mut out, "Interiority Log", &format!("{} events", events.len()), width);

    let mut prev_date: Option<String> = None;
    for event in events {
        let ts = event["timestamp"].as_str().unwrap_or("");
        let kind = event["kind"].as_str().unwrap_or("?");
        let detail = event["detail"].as_str().unwrap_or("");

        let time_str = parse_timestamp(ts)
            .map(|dt| {
                let formatted = format_time(&dt, prev_date.as_deref());
                prev_date = Some(dt.format("%Y-%m-%d").to_string());
                formatted
            })
            .unwrap_or_else(|| ts.chars().take(8).collect());

        // Kind label with color
        let kind_color = match kind {
            "tick_fired" => Color::Blue,
            "message_sent" => Color::Green,
            "message_skipped" => Color::DarkGrey,
            "tool_use" => Color::Cyan,
            "dormant" => Color::Red,
            "wake" => Color::Green,
            _ => Color::White,
        };

        if use_color() {
            let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
        }
        let _ = write!(out, "  {time_str:<14}");
        if use_color() {
            let _ = crossterm::execute!(out, SetForegroundColor(kind_color));
        }
        let _ = write!(out, "{kind:<18}");
        if use_color() {
            let _ = crossterm::execute!(out, ResetColor);
        }
        let _ = writeln!(out, "{detail}");
    }
    let _ = writeln!(out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_error_does_not_panic() {
        // Smoke test: ensure formatting doesn't panic
        print_error(&"test error");
    }

    #[test]
    fn print_server_error_does_not_panic() {
        print_server_error("busy", "engine is busy");
    }

    #[test]
    fn abbreviate_strips_date_suffix() {
        assert_eq!(abbreviate_model("claude-haiku-4-5-20251001"), "claude-haiku-4-5");
        assert_eq!(abbreviate_model("claude-sonnet-4-20250514"), "claude-sonnet-4");
    }

    #[test]
    fn abbreviate_preserves_short_names() {
        assert_eq!(abbreviate_model("test-model"), "test-model");
        assert_eq!(abbreviate_model("opus"), "opus");
    }

    #[test]
    fn abbreviate_preserves_non_date_suffix() {
        assert_eq!(abbreviate_model("model-latest"), "model-latest");
        assert_eq!(abbreviate_model("model-v2"), "model-v2");
    }

    // ── Log formatter tests ─────────────────────────────────────────

    #[test]
    fn character_color_is_deterministic() {
        let c1 = character_color("Sable");
        let c2 = character_color("Sable");
        assert_eq!(format!("{c1:?}"), format!("{c2:?}"));
    }

    #[test]
    fn character_color_varies_by_name() {
        let c1 = character_color("Sable");
        let c2 = character_color("Atlas");
        // Different names should usually get different colors
        // (not guaranteed but very likely with distinct names)
        assert_ne!(format!("{c1:?}"), format!("{c2:?}"));
    }

    #[test]
    fn format_time_same_day_shows_hhmm() {
        let dt = chrono::Local::now();
        let date = dt.format("%Y-%m-%d").to_string();
        let result = format_time(&dt, Some(&date));
        // Should be just HH:MM
        assert!(result.len() <= 5, "expected HH:MM, got: {result}");
    }

    #[test]
    fn format_time_first_message_shows_hhmm() {
        let dt = chrono::Local::now();
        let result = format_time(&dt, None);
        assert!(result.len() <= 5, "expected HH:MM, got: {result}");
    }

    #[test]
    fn format_time_different_day_shows_date() {
        let dt = chrono::Local::now();
        let result = format_time(&dt, Some("1999-01-01"));
        // Should contain month abbreviation
        assert!(result.len() > 5, "expected date + time, got: {result}");
    }

    #[test]
    fn parse_timestamp_handles_rfc3339() {
        let ts = "2026-01-15T10:30:00Z";
        assert!(parse_timestamp(ts).is_some());
    }

    #[test]
    fn parse_timestamp_handles_invalid() {
        assert!(parse_timestamp("not a date").is_none());
    }

    #[test]
    fn print_log_does_not_panic() {
        set_color_enabled(false);
        let messages = vec![
            serde_json::json!({
                "msg_id": "m1",
                "role": "user",
                "content": "Hello!",
                "images": [],
                "timestamp": "2026-01-15T10:30:00Z"
            }),
            serde_json::json!({
                "msg_id": "m2",
                "role": "assistant",
                "content": "Hi there!",
                "images": [],
                "timestamp": "2026-01-15T10:30:45Z"
            }),
            serde_json::json!({
                "msg_id": "m3",
                "role": "system",
                "content": "[compaction] Compacted 42 → 12 messages",
                "images": [],
                "timestamp": "2026-01-15T10:45:00Z"
            }),
        ];
        print_log(&messages, "Sable");
    }

    #[test]
    fn print_log_with_images_does_not_panic() {
        set_color_enabled(false);
        let messages = vec![serde_json::json!({
            "msg_id": "m1",
            "role": "user",
            "content": "Check this out",
            "images": [
                { "path": "/tmp/sunset.png", "caption": "A beautiful sunset" },
                { "path": "/tmp/photo.jpg" }
            ],
            "timestamp": "2026-01-15T10:30:00Z"
        })];
        print_log(&messages, "Sable");
    }

    // ── Status formatter tests ──────────────────────────────────────

    #[test]
    fn interiority_description_maps_states() {
        assert_eq!(interiority_description("Active", 0, 3), "active — in conversation");
        assert_eq!(interiority_description("Active", 2, 3), "active — idle 2/3 ticks");
        assert_eq!(interiority_description("Dormant", 4, 3), "dormant — waiting for you");
    }

    #[test]
    fn print_status_does_not_panic() {
        set_color_enabled(false);
        let data = serde_json::json!({
            "character": "Sable",
            "message_count": 142,
            "active_model": "claude-sonnet-4-20250514",
            "tokens": {
                "input": 12450,
                "output": 3218,
                "cache_read": 8100,
                "cache_write": 1024,
            },
            "autonomy": {
                "paused": false,
                "interiority_state": "Active",
                "ticks_without_user": 1,
                "max_idle_ticks": 3,
                "cache_keepalive_state": "Pinging",
                "cache_keepalive_pings": 3,
            }
        });
        print_status(&data, "Sable");
    }

    #[test]
    fn print_status_minimal_does_not_panic() {
        set_color_enabled(false);
        let data = serde_json::json!({
            "character": "Sable",
            "message_count": 5,
            "active_model": null,
        });
        print_status(&data, "Sable");
    }

    #[test]
    fn print_status_paused_does_not_panic() {
        set_color_enabled(false);
        let data = serde_json::json!({
            "character": "Sable",
            "message_count": 50,
            "active_model": "test-model",
            "autonomy": {
                "paused": true,
                "interiority_state": "Active",
                "ticks_without_user": 0,
                "max_idle_ticks": 3,
                "cache_keepalive_state": "Monitoring",
                "cache_keepalive_pings": 0,
            }
        });
        print_status(&data, "Sable");
    }

    #[test]
    fn print_status_with_activity_does_not_panic() {
        set_color_enabled(false);
        // Simulate a realistic hour histogram: busier in afternoon/evening.
        let histogram: Vec<f64> = (0..24)
            .map(|h| match h {
                0..=5 => 0.01,
                6..=8 => 0.04,
                9..=11 => 0.06,
                12..=14 => 0.08,
                15..=17 => 0.05,
                18..=21 => 0.10,
                _ => 0.02,
            })
            .collect();
        let classifications: Vec<&str> = (0..24)
            .map(|h| match h {
                0..=5 => "trough",
                18..=21 => "peak",
                _ => "normal",
            })
            .collect();

        let data = serde_json::json!({
            "character": "Sable",
            "message_count": 200,
            "active_model": "claude-sonnet-4-20250514",
            "tokens": { "input": 5000, "output": 1200, "cache_read": 0, "cache_write": 0 },
            "activity": {
                "hour_histogram": histogram,
                "hour_classifications": classifications,
                "has_sufficient_heatmap": true,
                "engagement_score": 0.72,
                "sessions_per_day": 2.3,
                "message_count": 200,
            }
        });
        print_status(&data, "Sable");
    }

    #[test]
    fn print_status_sparse_activity_does_not_panic() {
        set_color_enabled(false);
        let data = serde_json::json!({
            "character": "Sable",
            "message_count": 3,
            "active_model": "test-model",
            "activity": {
                "hour_histogram": vec![0.0_f64; 24],
                "hour_classifications": vec!["normal"; 24],
                "has_sufficient_heatmap": false,
                "engagement_score": 0.0,
                "sessions_per_day": 0.0,
                "message_count": 3,
            }
        });
        print_status(&data, "Sable");
    }

    #[test]
    fn density_to_block_ranges() {
        assert_eq!(density_to_block(0.0), '░');   // below threshold
        assert_eq!(density_to_block(0.04), '░');  // below threshold
        assert_eq!(density_to_block(0.06), '▁');  // 0.06 * 7 = 0.42 → round 0 → ▁
        assert_eq!(density_to_block(0.5), '▅');   // 0.5 * 7 = 3.5 → round 4 → ▅
        assert_eq!(density_to_block(1.0), '█');   // 1.0 * 7 = 7.0 → index 7 → █
    }

    // ── Spinner tests ──────────────────────────────────────────────────

    #[test]
    fn format_spinner_line_thinking() {
        let line = format_spinner_line("thinking", None, 2.3);
        assert_eq!(line, "(thinking... 2.3s) ");
    }

    #[test]
    fn format_spinner_line_with_model() {
        let line = format_spinner_line("thinking", Some("claude-sonnet-4"), 1.5);
        assert_eq!(line, "(thinking... 1.5s · claude-sonnet-4) ");
    }

    #[test]
    fn format_spinner_line_empty_phase() {
        let line = format_spinner_line("", None, 0.4);
        assert_eq!(line, "(generating... 0.4s) ");
    }

    #[test]
    fn format_spinner_line_custom_phase() {
        let line = format_spinner_line("analyzing", None, 5.0);
        assert_eq!(line, "(analyzing 5.0s) ");
    }

    #[test]
    fn stream_spinner_new_does_not_panic() {
        let spinner = StreamSpinner::new();
        assert!(!spinner.is_active());
    }
}
