use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, FixedOffset, Local};
use crossterm::style::{Attribute, Color, ResetColor, SetAttribute, SetForegroundColor};
use shore_protocol::server_msg::{CommandOutput, NewMessage, Phase, SendImage, StreamChunk, StreamEnd, ToolCall, ToolResult};
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

/// Print a stream chunk to stdout. Thinking chunks are shown dimmed.
pub fn print_chunk(chunk: &StreamChunk) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    if chunk.content_type == "thinking" && use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
        let _ = write!(out, "{}", chunk.text);
        let _ = crossterm::execute!(out, ResetColor);
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

/// Print command output. Formats JSON data in a readable way.
pub fn print_command_output(output: &CommandOutput) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    // Command name in bold
    if use_color() {
        let _ = crossterm::execute!(out, SetAttribute(Attribute::Bold));
    }
    let _ = write!(out, "{}", output.name);
    if use_color() {
        let _ = crossterm::execute!(out, SetAttribute(Attribute::Reset));
    }
    let _ = writeln!(out);

    // Pretty-print the JSON data
    if let Ok(pretty) = serde_json::to_string_pretty(&output.data) {
        let _ = writeln!(out, "{pretty}");
    } else {
        let _ = writeln!(out, "{}", output.data);
    }
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

/// Print a tool call notification.
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
        {
            let mut s = self.state.lock().unwrap();
            s.start = Instant::now();
            s.active = true;
        }

        let state = Arc::clone(&self.state);
        self.handle = Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
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
                if use_color() {
                    let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
                }
                let _ = write!(out, "\r{line}");
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
            // Overwrite spinner line with spaces and return to start.
            let _ = write!(out, "\r{}\r", " ".repeat(60));
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
                for block in blocks {
                    match block["type"].as_str().unwrap_or("text") {
                        "text" => {
                            let text = block["text"].as_str().unwrap_or("");
                            if !text.is_empty() {
                                let _ = writeln!(out, "{text}");
                            }
                        }
                        "thinking" => {
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
                        "tool_use" => {
                            let name = block["name"].as_str().unwrap_or("?");
                            if use_color() {
                                let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkYellow));
                            }
                            let _ = write!(out, "[tool: {name}]");
                            if use_color() {
                                let _ = crossterm::execute!(out, ResetColor);
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

/// Write a label-value row: `  Label        Value`
fn write_row(out: &mut impl Write, label: &str, value: &str) {
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
    }
    let _ = write!(out, "  {label:<13}");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = writeln!(out, "{value}");
}

/// Write a label-value row with the value in a specific color.
fn write_row_colored(out: &mut impl Write, label: &str, value: &str, color: Color) {
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
    }
    let _ = write!(out, "  {label:<13}");
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(color));
    }
    let _ = write!(out, "{value}");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = writeln!(out);
}

/// Translate a heartbeat state string to a human-readable description.
fn heartbeat_description(state: &str, data: &serde_json::Value) -> String {
    // The daemon sends Debug-formatted state strings like "Session",
    // "Deferred { fire_at: ..., reasoning: ... }", etc.
    if state.starts_with("Deferred") {
        // Try to extract fire_at for display.
        if let Some(fire_at) = data["autonomy"]["deferred_fire_at"].as_str() {
            return format!("plans to reach out at {fire_at}");
        }
        return "plans to reach out later".to_string();
    }
    match state {
        "Session" => "in conversation".to_string(),
        "PostSessionProbe" => "deciding whether to reach out...".to_string(),
        "SocialNeed" => "may reach out spontaneously".to_string(),
        "Dormant" => "quiet — waiting for you".to_string(),
        other => other.to_string(),
    }
}

/// Render a social need bar: `████████░░  83%`
fn format_social_need_bar(value: f64) -> String {
    let clamped = value.clamp(0.0, 1.0);
    let bar_width = 10;
    let filled = (clamped * bar_width as f64).round() as usize;
    let empty = bar_width - filled;
    let bar: String = "█".repeat(filled);
    let rest: String = "░".repeat(empty);
    let pct = (clamped * 100.0).round() as u32;
    format!("{bar}{rest}  {pct}%")
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

/// Choose a color for the social need bar based on value.
fn social_need_color(value: f64) -> Color {
    if value < 0.33 {
        Color::Green
    } else if value < 0.66 {
        Color::DarkYellow
    } else {
        Color::Red
    }
}

/// Print the status dashboard.
pub fn print_status(data: &serde_json::Value, character_name: &str) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    // ── Status ──────────────────────────────────────────
    write_section_header(&mut out, "Status", "", width);

    let char_color = character_color(character_name);
    write_row_colored(&mut out, "Character", character_name, char_color);

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

            let hb_state = autonomy["heartbeat_state"].as_str().unwrap_or("Session");
            let description = heartbeat_description(hb_state, data);
            let dim_state = format!("{description}  ");

            // Heartbeat row: description + dim state label.
            if use_color() {
                let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
            }
            let _ = write!(out, "  {:<13}", "Heartbeat");
            if use_color() {
                let _ = crossterm::execute!(out, ResetColor);
            }
            let _ = write!(out, "{dim_state}");
            if use_color() {
                let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
            }
            let _ = write!(out, "({hb_state})");
            if use_color() {
                let _ = crossterm::execute!(out, ResetColor);
            }
            let _ = writeln!(out);

            // Social need bar (only in SocialNeed state or if bar > 0).
            let bar_value = autonomy["social_need_bar"].as_f64().unwrap_or(0.0);
            if bar_value > 0.0 || hb_state == "SocialNeed" {
                let bar_str = format_social_need_bar(bar_value);
                let bar_color = social_need_color(bar_value);
                write_row_colored(&mut out, "Social need", &bar_str, bar_color);
            }

            // Roll info (only meaningful in SocialNeed).
            if hb_state == "SocialNeed" {
                let tau = autonomy["tau"].as_f64().unwrap_or(0.0);
                if tau > 0.0 {
                    // Per-check probability with 30-min interval.
                    let prob = 1.0 - (-1800.0_f64 / tau).exp();
                    let pct = (prob * 100.0).min(99.9);
                    write_row(&mut out, "Roll", &format!("{pct:.1}% / check · τ {tau:.0}s"));
                }
            }

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
    fn heartbeat_description_maps_states() {
        let data = serde_json::json!({});
        assert_eq!(heartbeat_description("Session", &data), "in conversation");
        assert_eq!(heartbeat_description("SocialNeed", &data), "may reach out spontaneously");
        assert_eq!(heartbeat_description("Dormant", &data), "quiet — waiting for you");
        assert_eq!(
            heartbeat_description("PostSessionProbe", &data),
            "deciding whether to reach out..."
        );
    }

    #[test]
    fn heartbeat_description_deferred() {
        let data = serde_json::json!({
            "autonomy": { "deferred_fire_at": "8:30 PM" }
        });
        let desc = heartbeat_description("Deferred { fire_at: ..., reasoning: ... }", &data);
        assert!(desc.contains("8:30 PM"));
    }

    #[test]
    fn social_need_bar_formatting() {
        assert_eq!(format_social_need_bar(0.0), "░░░░░░░░░░  0%");
        assert_eq!(format_social_need_bar(0.5), "█████░░░░░  50%");
        assert_eq!(format_social_need_bar(1.0), "██████████  100%");
    }

    #[test]
    fn social_need_color_ranges() {
        assert!(matches!(social_need_color(0.1), Color::Green));
        assert!(matches!(social_need_color(0.5), Color::DarkYellow));
        assert!(matches!(social_need_color(0.9), Color::Red));
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
                "heartbeat_state": "SocialNeed",
                "unanswered_count": 0,
                "dormant_threshold": 1,
                "social_need_bar": 0.42,
                "tau": 15120.0,
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
                "heartbeat_state": "Session",
                "unanswered_count": 0,
                "dormant_threshold": 1,
                "social_need_bar": 0.0,
                "tau": 10800.0,
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
