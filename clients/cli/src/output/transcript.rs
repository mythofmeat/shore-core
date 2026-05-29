use std::io::{self, Write};

use chrono::{DateTime, Local};
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use shore_protocol::server_msg::NewMessage;

use super::styling::{
    format_tool_input, format_tool_output, print_image_refs, write_tool_body_plain,
};
use super::{
    parse_timestamp, primary_tool_arg, print_dim_line, process_wrap_width, term_width, use_color,
    write_channel_rule, write_process_body, write_section_header, write_sigil_header,
    write_thinking_content_line, COLOR_RESULT, COLOR_THINKING, COLOR_TOOL, SIGIL_ERROR, SIGIL_OK,
    SIGIL_THINKING, SIGIL_TOOL,
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

fn write_archive_boundary(out: &mut impl Write, width: usize, archived_turns: usize) {
    let label = if archived_turns == 1 {
        " 1 archived turn above · outside current context ".to_string()
    } else {
        format!(" {archived_turns} archived turns above · outside current context ")
    };
    let label_width = label.chars().count();
    let line = if width > label_width {
        let left = (width - label_width) / 2;
        let right = width.saturating_sub(label_width + left);
        format!("{}{}{}", "─".repeat(left), label, "─".repeat(right))
    } else {
        label.trim().to_string()
    };

    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
    }
    let _ = writeln!(out, "{line}");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = writeln!(out);
}

fn is_tool_result_only_value(msg: &serde_json::Value) -> bool {
    msg["role"].as_str() == Some("user")
        && msg["content_blocks"].as_array().is_some_and(|blocks| {
            !blocks.is_empty()
                && blocks
                    .iter()
                    .all(|block| block["type"].as_str() == Some("tool_result"))
        })
}

fn count_user_turn_values(messages: &[serde_json::Value]) -> usize {
    messages
        .iter()
        .filter(|msg| msg["role"].as_str() == Some("user") && !is_tool_result_only_value(msg))
        .count()
}

/// Render a thinking block into the process channel: a colored `◌ Thinking`
/// header, then the thought as dim, word-wrapped, inset content.
fn write_thinking(out: &mut impl Write, thinking: &str) {
    write_sigil_header(out, SIGIL_THINKING, "Thinking", COLOR_THINKING);
    let width = process_wrap_width();
    for line in thinking.lines() {
        write_thinking_content_line(out, line, width);
    }
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
            // Speech (text) is the primary, flush-left voice; thinking, tool
            // calls, and results form one inset, gutter-barred "process"
            // channel. Consecutive process blocks are joined by a bar-only rule
            // so the gutter runs unbroken; a blank line separates the channel
            // from speech on either side. `redacted_thinking` is hidden.
            let mut prev_process: Option<bool> = None;
            for block in blocks {
                let block_type = block["type"].as_str().unwrap_or("text");
                let is_process = block_type != "text";
                // Whether this block will actually emit any visible output.
                let renders = match block_type {
                    "text" => !block["text"].as_str().unwrap_or("").is_empty(),
                    "thinking" => !block["thinking"].as_str().unwrap_or("").is_empty(),
                    "tool_use" | "tool_result" => true,
                    // redacted_thinking is a content-free placeholder — hide it.
                    _ => false,
                };
                if !renders {
                    continue;
                }
                if let Some(prev) = prev_process {
                    if prev && is_process {
                        write_channel_rule(out); // keep the gutter unbroken
                    } else if prev || is_process {
                        let _ = writeln!(out); // channel ↔ speech boundary
                    }
                }
                prev_process = Some(is_process);
                match block_type {
                    "text" => {
                        let text = block["text"].as_str().unwrap_or("");
                        if !text.is_empty() {
                            let _ = writeln!(out, "{text}");
                        }
                    }
                    "thinking" => {
                        let thinking = block["thinking"].as_str().unwrap_or("");
                        if !thinking.is_empty() {
                            write_thinking(out, thinking);
                        }
                    }
                    "tool_use" => {
                        let name = block["name"].as_str().unwrap_or("?");
                        let header = match primary_tool_arg(&block["input"]) {
                            Some(arg) => format!("{name} \u{00b7} {arg}"),
                            None => name.to_string(),
                        };
                        write_sigil_header(out, SIGIL_TOOL, &header, COLOR_TOOL);
                        if let Some(input_str) = format_tool_input(&block["input"]) {
                            write_process_body(out, &input_str);
                        }
                    }
                    "tool_result" => {
                        let output = block["content"].as_str().unwrap_or("");
                        let is_error = block["is_error"].as_bool().unwrap_or(false);
                        let (sigil, label, color) = if is_error {
                            (SIGIL_ERROR, "error", Color::Red)
                        } else {
                            (SIGIL_OK, "result", COLOR_RESULT)
                        };
                        write_sigil_header(out, sigil, label, color);
                        // Bodies stay dim; the colored header carries the status.
                        let output = format_tool_output(output);
                        write_process_body(out, &output);
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
    print_log_with_boundary(messages, 0, character_name);
}

/// Print the conversation log with an optional archive/current-context boundary.
pub fn print_log_with_boundary(
    messages: &[serde_json::Value],
    active_start: usize,
    character_name: &str,
) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();
    let char_color = character_color(character_name);

    let mut prev_date: Option<String> = None;

    let active_start = active_start.min(messages.len());
    let archived_turns = count_user_turn_values(&messages[..active_start]);
    for (index, msg) in messages.iter().enumerate() {
        if active_start > 0 && index == active_start {
            write_archive_boundary(&mut out, width, archived_turns);
        }

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

    if active_start > 0 && active_start == messages.len() {
        write_archive_boundary(&mut out, width, archived_turns);
    }
}

/// Print only the text content of a single message — no role, no timestamp, no decoration.
pub fn print_message_content(data: &serde_json::Value) {
    let content = data["content"].as_str().unwrap_or("");
    let content_blocks = data["content_blocks"].as_array();

    if let Some(blocks) = content_blocks {
        if !blocks.is_empty() {
            for block in blocks {
                if block["type"].as_str() == Some("text") {
                    let text = block["text"].as_str().unwrap_or("");
                    if !text.is_empty() {
                        println!("{text}");
                    }
                }
            }
            return;
        }
    }
    if !content.is_empty() {
        println!("{content}");
    }
}

/// Print the conversation log as plain text — no colors, no box-drawing.
/// Format: `role [HH:MM]: content` with blank lines between messages.
pub fn print_log_plain(messages: &[serde_json::Value], character_name: &str) {
    print_log_plain_with_boundary(messages, 0, character_name);
}

/// Plain-text transcript with an optional archive/current-context boundary.
pub fn print_log_plain_with_boundary(
    messages: &[serde_json::Value],
    active_start: usize,
    character_name: &str,
) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let active_start = active_start.min(messages.len());
    let archived_turns = count_user_turn_values(&messages[..active_start]);
    for (index, msg) in messages.iter().enumerate() {
        if active_start > 0 && index == active_start {
            let _ = writeln!(
                out,
                "--- {archived_turns} archived turn(s) above; outside current context ---\n"
            );
        }

        let role_str = msg["role"].as_str().unwrap_or("user");
        let content = msg["content"].as_str().unwrap_or("");
        let ts = msg["timestamp"].as_str().unwrap_or("");
        let content_blocks = msg["content_blocks"].as_array();

        let name = match role_str {
            "user" => "you",
            "assistant" => character_name,
            other => other,
        };

        let time_str = parse_timestamp(ts)
            .map(|dt| dt.format("%H:%M").to_string())
            .unwrap_or_default();

        let _ = writeln!(out, "{name} [{time_str}]:");

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
                            let t = block["thinking"].as_str().unwrap_or("");
                            if !t.is_empty() {
                                let _ = writeln!(out, "[thinking] {t}");
                            }
                        }
                        // redacted_thinking is a content-free placeholder — hide it.
                        "tool_use" => {
                            let name = block["name"].as_str().unwrap_or("?");
                            let _ = writeln!(out, "[tool: {name}]");
                            if let Some(input_str) = format_tool_input(&block["input"]) {
                                write_tool_body_plain(&mut out, &input_str);
                            }
                        }
                        "tool_result" => {
                            let output = block["content"].as_str().unwrap_or("");
                            let is_error = block["is_error"].as_bool().unwrap_or(false);
                            let label = if is_error { "error" } else { "result" };
                            let _ = writeln!(out, "[{label}]");
                            let output = format_tool_output(output);
                            write_tool_body_plain(&mut out, &output);
                        }
                        _ => {}
                    }
                }
            } else if !content.is_empty() {
                let _ = writeln!(out, "{content}");
            }
        } else if !content.is_empty() {
            let _ = writeln!(out, "{content}");
        }

        let _ = writeln!(out);
    }

    if active_start > 0 && active_start == messages.len() {
        let _ = writeln!(
            out,
            "--- {archived_turns} archived turn(s) above; outside current context ---\n"
        );
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

/// Print heartbeat event log returned by `shore log --heartbeat`.
pub fn print_heartbeat_log(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    let events = match data["events"].as_array() {
        Some(arr) => arr,
        None => {
            print_dim_line(&mut out, "(no heartbeat events)");
            return;
        }
    };

    if events.is_empty() {
        print_dim_line(&mut out, "(no heartbeat events)");
        return;
    }

    write_section_header(
        &mut out,
        "Heartbeat Log",
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
            "recap_written" => Color::Green,
            "recap_missing" => Color::Yellow,
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

    /// Visual preview harness (not an assertion). Renders a representative
    /// assistant turn with interleaved thinking via the real renderer, color
    /// ON, and dumps the raw bytes (ANSI escapes included) so a terminal shows
    /// it exactly as `shore log` / `shore get` would.
    ///
    /// Run it: `cargo test -p shore-cli render_preview_log
    ///          -- --ignored --nocapture --test-threads=1`
    /// (or via `.claude/skills/run-shore-cli/preview.sh`).
    #[test]
    #[ignore = "visual preview; run explicitly with --ignored --nocapture"]
    fn render_preview_log() {
        set_color_enabled(true);
        let blocks = vec![
            serde_json::json!({"type": "thinking", "thinking": "Let me reason about this first. The user asked about a long-standing issue, and this paragraph is deliberately long so it wraps and shows the gutter bar continuing down every wrapped row.\nA second paragraph confirms blank-line handling between thoughts."}),
            serde_json::json!({"type": "text", "text": "Here's the first part of my answer."}),
            serde_json::json!({"type": "tool_use", "name": "read_file", "input": {"path": "src/main.rs"}}),
            serde_json::json!({"type": "tool_result", "content": "fn main() { ... }", "is_error": false}),
            // redacted_thinking is hidden — should produce no output and not
            // disturb the breathing room around the real thinking block.
            serde_json::json!({"type": "redacted_thinking", "data": "AAAA"}),
            serde_json::json!({"type": "thinking", "thinking": "Now that I've read the file, I can refine my answer with the concrete details I just learned."}),
            serde_json::json!({"type": "text", "text": "And here's the refined conclusion."}),
        ];
        let mut buf = Vec::new();
        render_message_content(&mut buf, Some(&blocks), "", false);
        set_color_enabled(false);
        let mut stdout = io::stdout();
        let _ = stdout.write_all(b"\n----- LOG RENDER (shore log / shore get) -----\n");
        let _ = stdout.write_all(&buf);
        let _ = stdout.write_all(b"----- end -----\n");
        let _ = stdout.flush();
    }

    #[test]
    fn interleaved_thinking_header_both_directions() {
        set_color_enabled(false);
        let blocks = vec![
            serde_json::json!({"type": "thinking", "thinking": "T1"}),
            serde_json::json!({"type": "text", "text": "A1"}),
            serde_json::json!({"type": "thinking", "thinking": "T2"}),
            serde_json::json!({"type": "text", "text": "A2"}),
        ];
        let mut buf = Vec::new();
        render_message_content(&mut buf, Some(&blocks), "", false);
        let output = String::from_utf8(buf).unwrap();

        // Each thinking block is a `◌ Thinking` header + gutter-barred content,
        // with a blank line straddling each thinking/speech boundary.
        assert_eq!(
            output,
            " \u{2502} \u{25cc} Thinking\n \u{2502}   T1\n\nA1\n\n \u{2502} \u{25cc} Thinking\n \u{2502}   T2\n\nA2\n"
        );
    }

    #[test]
    fn thinking_header_then_indented_content() {
        set_color_enabled(false);
        let blocks =
            vec![serde_json::json!({"type": "thinking", "thinking": "line one\nline two"})];
        let mut buf = Vec::new();
        render_message_content(&mut buf, Some(&blocks), "", false);
        let output = String::from_utf8(buf).unwrap();
        // Header line, then each content line gutter-barred under it.
        assert_eq!(
            output,
            " \u{2502} \u{25cc} Thinking\n \u{2502}   line one\n \u{2502}   line two\n"
        );
    }

    #[test]
    fn no_breathing_room_before_first_thinking_block() {
        set_color_enabled(false);
        let blocks = vec![
            serde_json::json!({"type": "thinking", "thinking": "T1"}),
            serde_json::json!({"type": "text", "text": "A1"}),
        ];
        let mut buf = Vec::new();
        render_message_content(&mut buf, Some(&blocks), "", false);
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(
            output,
            " \u{2502} \u{25cc} Thinking\n \u{2502}   T1\n\nA1\n"
        );
    }

    #[test]
    fn adjacent_process_blocks_joined_by_bar_rule() {
        set_color_enabled(false);
        let blocks = vec![
            serde_json::json!({"type": "thinking", "thinking": "T1"}),
            // Empty text block must not break the channel on its own.
            serde_json::json!({"type": "text", "text": ""}),
            serde_json::json!({"type": "thinking", "thinking": "T2"}),
        ];
        let mut buf = Vec::new();
        render_message_content(&mut buf, Some(&blocks), "", false);
        let output = String::from_utf8(buf).unwrap();
        // Two adjacent process blocks are joined by a bar-only rule, not a blank.
        assert_eq!(
            output,
            " \u{2502} \u{25cc} Thinking\n \u{2502}   T1\n \u{2502}\n \u{2502} \u{25cc} Thinking\n \u{2502}   T2\n"
        );
    }

    #[test]
    fn tool_call_and_result_render_with_sigils_and_separators() {
        set_color_enabled(false);
        let blocks = vec![
            serde_json::json!({"type": "text", "text": "let me check"}),
            serde_json::json!({"type": "tool_use", "name": "edit", "input": {"path": "a.md"}}),
            serde_json::json!({"type": "tool_result", "content": "done", "is_error": false}),
            serde_json::json!({"type": "text", "text": "fixed"}),
        ];
        let mut buf = Vec::new();
        render_message_content(&mut buf, Some(&blocks), "", false);
        let output = String::from_utf8(buf).unwrap();
        // Speech flush-left; the tool call + result share one unbroken gutter
        // (joined by a bar rule), with a blank line to the speech on each side.
        assert_eq!(
            output,
            "let me check\n\n \u{2502} \u{2192} edit \u{00b7} a.md\n \u{2502}   path: a.md\n \u{2502}\n \u{2502} \u{2713} result\n \u{2502}   done\n\nfixed\n"
        );
    }

    #[test]
    fn redacted_thinking_is_hidden() {
        set_color_enabled(false);
        let blocks = vec![
            serde_json::json!({"type": "text", "text": "A1"}),
            serde_json::json!({"type": "redacted_thinking", "data": "AAAA"}),
            serde_json::json!({"type": "text", "text": "A2"}),
        ];
        let mut buf = Vec::new();
        render_message_content(&mut buf, Some(&blocks), "", false);
        let output = String::from_utf8(buf).unwrap();
        // Redacted block produces nothing and leaves no breathing room.
        assert_eq!(output, "A1\nA2\n");
    }

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
                "content": "[compaction] Compacted 42 -> 12 turns",
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
