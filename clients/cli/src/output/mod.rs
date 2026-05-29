pub mod commands;
pub mod spinner;
pub mod styling;
pub mod transcript;

pub use commands::*;
pub use spinner::*;
pub use styling::*;
pub use transcript::*;

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, FixedOffset, Local};
use crossterm::style::{Color, ResetColor, SetForegroundColor};

// ---------------------------------------------------------------------------
// Color control (NO_COLOR / --no-color)
// ---------------------------------------------------------------------------

static COLOR_ENABLED: AtomicBool = AtomicBool::new(true);

/// Set whether color output is enabled. Call once at startup.
pub fn set_color_enabled(enabled: bool) {
    COLOR_ENABLED.store(enabled, Ordering::Relaxed);
}

pub(crate) fn use_color() -> bool {
    COLOR_ENABLED.load(Ordering::Relaxed)
}

/// Strip trailing date suffix (`-YYYYMMDD`) from a model ID.
pub(crate) fn abbreviate_model(model_id: &str) -> &str {
    if let Some(i) = model_id.rfind('-') {
        let suffix = &model_id[i + 1..];
        if suffix.len() == 8 && suffix.bytes().all(|b| b.is_ascii_digit()) {
            return &model_id[..i];
        }
    }
    model_id
}

/// Max characters to display for a tool result before truncating.
pub(crate) const MAX_TOOL_OUTPUT: usize = 500;

/// Get terminal width, falling back to 80 columns.
pub(crate) fn term_width() -> usize {
    crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(80)
}

// ---------------------------------------------------------------------------
// Process channel: thinking, tool calls, and tool results render as one
// cohesive secondary channel — inset, each block opened by a colored sigil +
// label header with a four-column hanging indent and a dim body. Response text
// (speech) stays flush-left and plain; the channels are separated by blank
// lines.
// ---------------------------------------------------------------------------

/// Sigil marking a thinking block.
pub(crate) const SIGIL_THINKING: char = '\u{25cc}'; // ◌
/// Sigil marking a tool call. Single-width (a wide glyph like ⚙ misaligns the
/// text after it in many terminals).
pub(crate) const SIGIL_TOOL: char = '\u{2192}'; // →
/// Sigil marking a successful tool result.
pub(crate) const SIGIL_OK: char = '\u{2713}'; // ✓
/// Sigil marking a failed tool result.
pub(crate) const SIGIL_ERROR: char = '\u{2717}'; // ✗

/// Header color for a thinking block.
pub(crate) const COLOR_THINKING: Color = Color::Magenta;
/// Header color for a tool call.
pub(crate) const COLOR_TOOL: Color = Color::Yellow;
/// Header color for a successful tool result.
pub(crate) const COLOR_RESULT: Color = Color::Green;

/// The left-gutter bar that runs down the process channel.
pub(crate) const CHANNEL_BAR: char = '\u{2502}'; // │
/// Total visible width of a process line's prefix (`"│ "` gutter + two-space
/// inset), so header labels and body text both land at column 4.
pub(crate) const PROCESS_INDENT_WIDTH: usize = 4;
/// Floor for the process text column so a very narrow terminal still wraps.
pub(crate) const MIN_PROCESS_WIDTH: usize = 24;

/// Width available for wrapped process text after the gutter + inset.
pub(crate) fn process_wrap_width() -> usize {
    term_width()
        .saturating_sub(PROCESS_INDENT_WIDTH)
        .max(MIN_PROCESS_WIDTH)
}

/// Write the dim left gutter (`"│ "`) without a trailing newline. The bar is
/// always dim — it marks the channel; per-block color lives in the header.
fn write_gutter(out: &mut impl Write) {
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
    }
    let _ = write!(out, "{CHANNEL_BAR} ");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
}

/// Write a bar-only channel line (`"│"`) — used to keep the gutter continuous
/// across the blank line between two process blocks, and for blank lines within
/// a thought.
pub(crate) fn write_channel_rule(out: &mut impl Write) {
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
    }
    let _ = writeln!(out, "{CHANNEL_BAR}");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
}

/// Write a process-block header line: dim `"│ "` gutter, then a colored
/// `"⟨sigil⟩ ⟨text⟩"` (label at column 4).
pub(crate) fn write_sigil_header(out: &mut impl Write, sigil: char, text: &str, color: Color) {
    write_gutter(out);
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(color));
    }
    let _ = writeln!(out, "{sigil} {text}");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
}

/// Write a tool body inset into the process channel: dim, gutter-barred, text
/// at column 4. Not word-wrapped — tool I/O is often structured or code, so it
/// is left to soft-wrap.
pub(crate) fn write_process_body(out: &mut impl Write, body: &str) {
    if body.is_empty() {
        return;
    }
    for line in body.lines() {
        if line.is_empty() {
            write_channel_rule(out);
        } else {
            if use_color() {
                let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
            }
            let _ = writeln!(out, "{CHANNEL_BAR}   {line}");
            if use_color() {
                let _ = crossterm::execute!(out, ResetColor);
            }
        }
    }
}

/// Write one logical line of thinking *content* (everything below the
/// `◌ Thinking` header): dim, gutter-barred, word-wrapped to `width`, text at
/// column 4. Blank lines render as a bar-only line.
pub(crate) fn write_thinking_content_line(out: &mut impl Write, line: &str, width: usize) {
    if line.trim().is_empty() {
        write_channel_rule(out);
        return;
    }
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
    }
    for wrapped in wrap_line(line, width) {
        let _ = writeln!(out, "{CHANNEL_BAR}   {wrapped}");
    }
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
}

/// Extract a short, human-meaningful "primary argument" from a tool input
/// object (the path/command/query/… most worth showing on the sigil line).
pub(crate) fn primary_tool_arg(input: &serde_json::Value) -> Option<String> {
    const KEYS: &[&str] = &[
        "path",
        "file_path",
        "command",
        "cmd",
        "query",
        "pattern",
        "url",
        "name",
        "key",
    ];
    let obj = input.as_object()?;
    for key in KEYS {
        if let Some(s) = obj.get(*key).and_then(|v| v.as_str()) {
            let s = s.trim();
            if !s.is_empty() {
                return Some(truncate_chars(s, 60));
            }
        }
    }
    None
}

/// Truncate `s` to at most `max` chars, appending `…` when shortened.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}\u{2026}")
    }
}

/// Greedy word-wrap of a single logical line to `width` columns (counted in
/// chars). Whitespace runs collapse to single spaces. A word longer than
/// `width` is emitted on its own line rather than hard-split. An empty/blank
/// input yields a single empty line so callers can still emit a gutter for it.
pub(crate) fn wrap_line(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for word in text.split_whitespace() {
        let wlen = word.chars().count();
        if cur_len == 0 {
            cur.push_str(word);
            cur_len = wlen;
        } else if cur_len + 1 + wlen <= width {
            cur.push(' ');
            cur.push_str(word);
            cur_len += 1 + wlen;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
            cur_len = wlen;
        }
    }
    lines.push(cur);
    lines
}

/// Parse an RFC 3339 timestamp to local time.
pub(crate) fn parse_timestamp(ts: &str) -> Option<DateTime<Local>> {
    DateTime::<FixedOffset>::parse_from_rfc3339(ts)
        .map(|dt| dt.with_timezone(&Local))
        .ok()
}

/// Write text in a specific foreground color (respects use_color()).
pub(crate) fn write_fg(out: &mut impl Write, color: Color, text: &str) {
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(color));
    }
    let _ = write!(out, "{text}");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
}

/// Write text in dim (DarkGrey) color.
pub(crate) fn write_dim(out: &mut impl Write, text: &str) {
    write_fg(out, Color::DarkGrey, text);
}

/// Print a dimmed line (for empty states).
pub(crate) fn print_dim_line(out: &mut impl Write, text: &str) {
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
    }
    let _ = writeln!(out, "  {text}");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
}

/// Write a section header: `-- Title ----------------------`
pub(crate) fn write_section_header(out: &mut impl Write, title: &str, suffix: &str, width: usize) {
    let prefix = if suffix.is_empty() {
        format!("\u{2500}\u{2500} {} ", title)
    } else {
        format!("\u{2500}\u{2500} {} ({}) ", title, suffix)
    };
    let prefix_len = prefix.chars().count();
    let trail = width.saturating_sub(prefix_len);
    let rule: String = "\u{2500}".repeat(trail);

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
pub(crate) fn write_row_with(out: &mut impl Write, label: &str, value: &str, color: Option<Color>) {
    write_dim(out, &format!("  {label:<13}"));
    match color {
        Some(c) => write_fg(out, c, value),
        None => {
            let _ = write!(out, "{value}");
        }
    }
    let _ = writeln!(out);
}

/// Write a label-value row: `  Label        Value`
pub(crate) fn write_row(out: &mut impl Write, label: &str, value: &str) {
    write_row_with(out, label, value, None);
}

/// Write a label-value row with the value in a specific color.
pub(crate) fn write_row_colored(out: &mut impl Write, label: &str, value: &str, color: Color) {
    write_row_with(out, label, value, Some(color));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbreviate_strips_date_suffix() {
        assert_eq!(
            abbreviate_model("claude-haiku-4-5-20251001"),
            "claude-haiku-4-5"
        );
        assert_eq!(
            abbreviate_model("claude-sonnet-4-20250514"),
            "claude-sonnet-4"
        );
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

    #[test]
    fn wrap_line_breaks_at_word_boundaries() {
        // width 10: greedy packing, break before a word that would overflow.
        assert_eq!(
            wrap_line("the quick brown fox jumps", 10),
            vec!["the quick", "brown fox", "jumps"]
        );
    }

    #[test]
    fn wrap_line_short_input_is_single_line() {
        assert_eq!(wrap_line("hello world", 80), vec!["hello world"]);
    }

    #[test]
    fn wrap_line_long_word_is_not_split() {
        // A word longer than width gets its own line rather than a hard split.
        assert_eq!(
            wrap_line("a supercalifragilistic b", 8),
            vec!["a", "supercalifragilistic", "b"]
        );
    }

    #[test]
    fn wrap_line_collapses_whitespace() {
        assert_eq!(wrap_line("  spaced   out  ", 80), vec!["spaced out"]);
        assert_eq!(wrap_line("", 80), vec![""]);
    }

    #[test]
    fn write_thinking_content_line_wraps_and_gutters() {
        set_color_enabled(false);
        let mut buf = Vec::new();
        write_thinking_content_line(&mut buf, "the quick brown fox jumps", 10);
        let out = String::from_utf8(buf).unwrap();
        // Every wrapped row is gutter-barred, text at column 4.
        assert_eq!(
            out,
            "\u{2502}   the quick\n\u{2502}   brown fox\n\u{2502}   jumps\n"
        );
    }

    #[test]
    fn write_thinking_content_line_blank_is_bar_only() {
        set_color_enabled(false);
        let mut buf = Vec::new();
        write_thinking_content_line(&mut buf, "   ", 80);
        let out = String::from_utf8(buf).unwrap();
        assert_eq!(out, "\u{2502}\n");
    }

    #[test]
    fn write_sigil_header_gutters_then_sigil_and_label() {
        set_color_enabled(false);
        let mut buf = Vec::new();
        write_sigil_header(&mut buf, SIGIL_THINKING, "Thinking", COLOR_THINKING);
        let out = String::from_utf8(buf).unwrap();
        assert_eq!(out, "\u{2502} \u{25cc} Thinking\n");
    }

    #[test]
    fn write_channel_rule_is_bar_only() {
        set_color_enabled(false);
        let mut buf = Vec::new();
        write_channel_rule(&mut buf);
        assert_eq!(String::from_utf8(buf).unwrap(), "\u{2502}\n");
    }

    #[test]
    fn primary_tool_arg_picks_known_key() {
        let input = serde_json::json!({"path": "src/main.rs", "edits": []});
        assert_eq!(primary_tool_arg(&input).as_deref(), Some("src/main.rs"));
        let input = serde_json::json!({"foo": "bar"});
        assert_eq!(primary_tool_arg(&input), None);
    }

    #[test]
    fn primary_tool_arg_truncates_long_values() {
        let long = "a".repeat(100);
        let input = serde_json::json!({ "command": long });
        let arg = primary_tool_arg(&input).unwrap();
        assert_eq!(arg.chars().count(), 60);
        assert!(arg.ends_with('\u{2026}'));
    }
}
