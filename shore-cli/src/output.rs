use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, FixedOffset, Local};
use crossterm::style::{Attribute, Color, ResetColor, SetAttribute, SetForegroundColor};
use shore_protocol::server_msg::{CommandOutput, NewMessage, Phase, SendImage, StreamChunk, StreamEnd, ToolCall, ToolResult};
use shore_protocol::types::ImageRef;

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

        let parsed_ts = parse_timestamp(ts);
        let time_str = parsed_ts
            .as_ref()
            .map(|dt| format_time(dt, prev_date.as_deref()))
            .unwrap_or_default();

        // Update prev_date for next iteration
        if let Some(dt) = &parsed_ts {
            prev_date = Some(dt.format("%Y-%m-%d").to_string());
        }

        match role_str {
            "user" => {
                write_header(&mut out, "You", &time_str, Color::Cyan, width);
                let _ = writeln!(out, "{content}");
            }
            "assistant" => {
                write_header(&mut out, character_name, &time_str, char_color, width);
                let _ = writeln!(out, "{content}");
            }
            "system" => {
                // System messages: entirely dim
                if use_color() {
                    let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
                }
                let prefix = format!("── system · {} ", time_str);
                let prefix_len = prefix.chars().count();
                let trail = if width > prefix_len { width - prefix_len } else { 0 };
                let _ = write!(out, "{prefix}{}", "─".repeat(trail));
                let _ = writeln!(out);
                let _ = writeln!(out, "{content}");
                if use_color() {
                    let _ = crossterm::execute!(out, ResetColor);
                }
            }
            _ => {
                // Unknown role, fall through to basic display
                let _ = writeln!(out, "[{role_str}] {content}");
            }
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

/// Print a single message in the log transcript style (for `shore get`).
pub fn print_single_message(msg: &serde_json::Value, character_name: &str) {
    if let Some(arr) = msg.as_array() {
        print_log(arr, character_name);
    } else {
        print_log(&[msg.clone()], character_name);
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
}
