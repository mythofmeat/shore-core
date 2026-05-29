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

/// Left-gutter prefix for thinking lines: two spaces, a box-drawing bar, and a
/// space — four visible columns.
pub(crate) const THINKING_GUTTER: &str = "  \u{2502} ";
/// Visible width of [`THINKING_GUTTER`].
pub(crate) const THINKING_GUTTER_WIDTH: usize = 4;
/// Floor for the thinking text column so a very narrow terminal still wraps.
pub(crate) const MIN_THINKING_WIDTH: usize = 24;

/// Width available for wrapped thinking text after the gutter prefix.
pub(crate) fn thinking_wrap_width() -> usize {
    term_width()
        .saturating_sub(THINKING_GUTTER_WIDTH)
        .max(MIN_THINKING_WIDTH)
}

/// Write one logical thinking line to `out`: dim, word-wrapped to `width`, with
/// every wrapped row prefixed by the gutter so the bar runs continuously down
/// the left edge. A blank line yields a bare gutter to keep the bar unbroken.
/// The `│` prints regardless of color so thinking stays distinguishable from
/// the response in no-color terminals; dim styling applies only with color.
pub(crate) fn write_thinking_logical_line(out: &mut impl Write, line: &str, width: usize) {
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
    }
    if line.trim().is_empty() {
        let _ = writeln!(out, "  \u{2502}");
    } else {
        for wrapped in wrap_line(line, width) {
            let _ = writeln!(out, "{THINKING_GUTTER}{wrapped}");
        }
    }
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
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
    fn write_thinking_logical_line_gutters_and_wraps() {
        set_color_enabled(false);
        let mut buf = Vec::new();
        write_thinking_logical_line(&mut buf, "the quick brown fox jumps", 10);
        let out = String::from_utf8(buf).unwrap();
        assert_eq!(
            out,
            "  \u{2502} the quick\n  \u{2502} brown fox\n  \u{2502} jumps\n"
        );
    }

    #[test]
    fn write_thinking_logical_line_blank_is_bare_gutter() {
        set_color_enabled(false);
        let mut buf = Vec::new();
        write_thinking_logical_line(&mut buf, "   ", 80);
        let out = String::from_utf8(buf).unwrap();
        assert_eq!(out, "  \u{2502}\n");
    }
}
