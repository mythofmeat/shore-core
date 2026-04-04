use std::io::{self, Write};

use chrono::{DateTime, Local};
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use shore_protocol::server_msg::NewMessage;

use super::styling::{format_tool_input, print_image_refs};
use super::{
    parse_timestamp, print_dim_line, term_width, use_color, write_section_header, MAX_TOOL_OUTPUT,
};

// ---------------------------------------------------------------------------
// Log formatter -- human-readable chat transcript (Option B)
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
pub(crate) fn character_color(name: &str) -> Color {
    let hash = name
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    CHARACTER_PALETTE[(hash as usize) % CHARACTER_PALETTE.len()]
}

/// Format a timestamp for display. Uses "HH:MM" normally,
/// "Mon DD . HH:MM" if the date differs from `prev_date`.
pub(crate) fn format_time(dt: &DateTime<Local>, prev_date: Option<&str>) -> String {
    let date = dt.format("%Y-%m-%d").to_string();
    let time = dt.format("%H:%M").to_string();
    match prev_date {
        Some(prev) if prev == date => time,
        None => time, // first message, just show time
        Some(_) => dt.format("%b %d \u{00b7} %H:%M").to_string(),
    }
}

/// Write a colored header line: `-- Name . HH:MM --------------------`
pub(crate) fn write_header(
    out: &mut impl Write,
    name: &str,
    time_str: &str,
    color: Color,
    width: usize,
) {
    // "-- Name . HH:MM " = 4 + name.len() + 3 + time.len() + 1
    let prefix = format!("\u{2500}\u{2500} {} \u{00b7} {} ", name, time_str);
    let prefix_len = prefix.chars().count();
    let trail = width.saturating_sub(prefix_len);
    let rule: String = "\u{2500}".repeat(trail);

    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(color));
    }
    let _ = write!(out, "{prefix}{rule}");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = writeln!(out);
}

/// Render the content of a single message: content blocks if present, plain text fallback otherwise.
fn render_message_content(
    out: &mut impl Write,
    content_blocks: Option<&Vec<serde_json::Value>>,
    content: &str,
    is_tool_result_msg: bool,
) {
    if let Some(blocks) = content_blocks {
        if !blocks.is_empty() {
            let mut was_thinking = false;
            for block in blocks {
                let block_type = block["type"].as_str().unwrap_or("text");
                // Insert separator when transitioning from thinking -> non-thinking.
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
                                let _ =
                                    crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
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
                                let _ =
                                    crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
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
                        let color = if is_error {
                            Color::Red
                        } else {
                            Color::DarkGrey
                        };
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
            // Empty content_blocks -- fall back to content string.
            if !content.is_empty() {
                let _ = writeln!(out, "{content}");
            }
        }
    } else {
        // No content_blocks field -- legacy message, show content string.
        if !content.is_empty() || !is_tool_result_msg {
            let _ = writeln!(out, "{content}");
        }
    }
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
            && content_blocks.is_some_and(|blocks| {
                !blocks.is_empty()
                    && blocks
                        .iter()
                        .all(|b| b["type"].as_str() == Some("tool_result"))
            });

        // Write header (skip for tool result messages -- they're continuations).
        if !is_tool_result_msg {
            match role_str {
                "user" => write_header(&mut out, "You", &time_str, Color::Cyan, width),
                "assistant" => write_header(&mut out, character_name, &time_str, char_color, width),
                "system" => {
                    if use_color() {
                        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
                    }
                    let prefix = format!("\u{2500}\u{2500} system \u{00b7} {} ", time_str);
                    let prefix_len = prefix.chars().count();
                    let trail = width.saturating_sub(prefix_len);
                    let _ = write!(out, "{prefix}{}", "\u{2500}".repeat(trail));
                    let _ = writeln!(out);
                }
                _ => {}
            }
        }

        render_message_content(&mut out, content_blocks, content, is_tool_result_msg);

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
                    .or_else(|| img["path"].as_str().and_then(|p| p.rsplit('/').next()))
                    .unwrap_or("image");

                if use_color() {
                    let _ = crossterm::execute!(out, SetForegroundColor(Color::Yellow));
                }
                let _ = write!(out, "  \u{1f4ce} {label}");
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

/// Print a single message in the same transcript format as print_log.
pub fn print_single_message(data: &serde_json::Value, character_name: &str) {
    print_log(std::slice::from_ref(data), character_name);
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

    write_section_header(
        &mut out,
        "Interiority Log",
        &format!("{} events", events.len()),
        width,
    );

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
    use crate::output::set_color_enabled;

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
    fn write_header_contains_name_and_time() {
        set_color_enabled(false);
        let mut buf = Vec::new();
        write_header(&mut buf, "Alice", "14:30", Color::Cyan, 40);
        let output = String::from_utf8(buf).unwrap();

        assert!(
            output.contains("Alice"),
            "header should contain character name"
        );
        assert!(output.contains("14:30"), "header should contain time");
        assert!(
            output.contains("\u{00b7}"),
            "header should contain middle dot separator"
        );
        assert!(
            output.contains("\u{2500}"),
            "header should contain box-drawing chars"
        );
    }

    #[test]
    fn write_header_pads_to_width() {
        set_color_enabled(false);
        let mut buf = Vec::new();
        write_header(&mut buf, "X", "00:00", Color::Cyan, 60);
        let output = String::from_utf8(buf).unwrap();
        let line = output.trim_end_matches('\n');
        // Width should roughly match requested width (all ASCII/box-drawing).
        assert!(
            line.chars().count() >= 50,
            "header should pad to fill width, got {} chars",
            line.chars().count()
        );
    }

    #[test]
    fn format_time_date_boundary() {
        // When prev_date differs, should include month abbreviation.
        let dt = chrono::Local::now();
        let result = format_time(&dt, Some("2020-01-01"));
        // Should contain something like "Apr 04" (month day).
        assert!(
            result.contains("\u{00b7}"),
            "cross-day format should contain middle dot"
        );
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
                "content": "[compaction] Compacted 42 -> 12 messages",
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
}
