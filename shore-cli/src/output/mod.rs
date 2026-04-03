pub mod styling;
pub mod spinner;
pub mod transcript;
pub mod commands;

pub use styling::*;
pub use spinner::*;
pub use transcript::*;
pub use commands::*;

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
    crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80)
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
    let trail = if width > prefix_len { width - prefix_len } else { 0 };
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
        None => { let _ = write!(out, "{value}"); }
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
}
