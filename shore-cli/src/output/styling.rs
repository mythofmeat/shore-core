use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::style::{Color, ResetColor, SetForegroundColor};
use shore_protocol::server_msg::{Phase, SendImage, StreamChunk, StreamEnd, ToolCall, ToolResult};
use shore_protocol::types::ImageRef;

use super::{abbreviate_model, use_color, MAX_TOOL_OUTPUT};
use crate::images;

// Track whether the previous chunk was thinking, so we can insert a separator
// when transitioning from thinking -> text.
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

    // Insert a separator when transitioning from thinking -> text.
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

/// Format a tool input value for display. Compact single-line for simple
/// values, truncated if too long.
pub(crate) fn format_tool_input(input: &serde_json::Value) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::set_color_enabled;

    #[test]
    fn print_error_does_not_panic() {
        print_error(&"test error");
    }

    #[test]
    fn print_server_error_does_not_panic() {
        print_server_error("busy", "engine is busy");
    }

    // ── format_tool_input content assertions ────────────────────────

    #[test]
    fn format_tool_input_empty_object_returns_none() {
        let input = serde_json::json!({});
        assert!(format_tool_input(&input).is_none());
    }

    #[test]
    fn format_tool_input_simple_object() {
        let input = serde_json::json!({"query": "weather"});
        let result = format_tool_input(&input).unwrap();
        assert!(result.contains("query"));
        assert!(result.contains("weather"));
    }

    #[test]
    fn format_tool_input_truncates_large_input() {
        let big = "x".repeat(MAX_TOOL_OUTPUT + 100);
        let input = serde_json::json!({"data": big});
        let result = format_tool_input(&input).unwrap();
        assert!(result.ends_with("..."), "large input should be truncated with ...");
        assert!(result.len() <= MAX_TOOL_OUTPUT + 50, "truncated output should be bounded");
    }

    #[test]
    fn format_tool_input_small_input_not_truncated() {
        let input = serde_json::json!({"key": "value", "num": 42});
        let result = format_tool_input(&input).unwrap();
        assert!(!result.ends_with("..."), "small input should not be truncated");
        assert!(result.contains("42"));
    }

    // ── reset_chunk_state ───────────────────────────────────────────

    #[test]
    fn reset_chunk_state_clears_thinking() {
        set_color_enabled(false);
        WAS_THINKING.store(true, Ordering::Relaxed);
        reset_chunk_state();
        assert!(!WAS_THINKING.load(Ordering::Relaxed));
    }
}
