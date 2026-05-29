use std::io::{self, Write};
use std::sync::Mutex;

use crossterm::style::{Color, ResetColor, SetForegroundColor};
use shore_protocol::server_msg::{
    Phase, ProviderFallbackWarning, SendImage, StreamChunk, StreamEnd, ToolCall, ToolResult,
    UsageWarning,
};
use shore_protocol::tool_display::{format_tool_input_with_limit, format_tool_output_with_limit};
use shore_protocol::types::ImageRef;

use super::{
    abbreviate_model, thinking_wrap_width, use_color, write_thinking_logical_line, MAX_TOOL_OUTPUT,
};
use crate::images;

/// Running state for the chunk stream. Thinking is word-wrapped to fit under
/// the gutter, but the wrap width is only known per complete logical line, so
/// thinking text is buffered until a newline (or a transition out of thinking)
/// and then emitted wrapped + gutter-barred — matching the transcript renderer.
struct ChunkState {
    /// Whether the previous chunk was thinking content.
    was_thinking: bool,
    /// Whether any chunk has been printed yet (the first chunk never gets
    /// leading breathing room).
    has_emitted: bool,
    /// Whether the cursor is at the start of a line (so a transition knows
    /// whether it must close an open line before the breathing-room blank).
    at_line_start: bool,
    /// Buffer for the in-progress thinking logical line, accumulated across
    /// chunks until a `\n` completes it.
    thinking_line: String,
}

impl ChunkState {
    /// Initial state: nothing emitted, and the cursor sits at column 0 (the
    /// assistant header line ends with a newline before streaming begins).
    const INITIAL: Self = Self {
        was_thinking: false,
        has_emitted: false,
        at_line_start: true,
        thinking_line: String::new(),
    };
}

impl Default for ChunkState {
    fn default() -> Self {
        Self::INITIAL
    }
}

static CHUNK_STATE: Mutex<ChunkState> = Mutex::new(ChunkState::INITIAL);

/// Reset stream chunk state (call at the start of each new stream).
pub fn reset_chunk_state() {
    *CHUNK_STATE.lock().unwrap() = ChunkState::INITIAL;
}

/// Print a stream chunk to stdout. Thinking is shown dimmed and gutter-barred,
/// with a blank line of breathing room straddling each thinking/text boundary.
pub fn print_chunk(chunk: &StreamChunk) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut state = CHUNK_STATE.lock().unwrap();
    print_chunk_to(&mut out, &mut state, chunk);
    let _ = out.flush();
}

/// Flush any buffered thinking that did not end in a newline (called on a
/// transition out of thinking and at stream end). Emits the partial line
/// wrapped + gutter-barred, leaving the cursor at the start of a fresh line.
fn flush_thinking(out: &mut impl Write, state: &mut ChunkState) {
    if state.thinking_line.is_empty() {
        return;
    }
    let line = std::mem::take(&mut state.thinking_line);
    write_thinking_logical_line(out, &line, thinking_wrap_width());
    state.at_line_start = true;
}

/// Flush any buffered thinking line to stdout (used at stream end).
pub fn flush_pending_thinking() {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut state = CHUNK_STATE.lock().unwrap();
    flush_thinking(&mut out, &mut state);
    let _ = out.flush();
}

/// Render a single chunk to `out`. Thinking is buffered per logical line and
/// emitted dim + word-wrapped + gutter-barred; transitions between thinking and
/// non-thinking get a blank line of breathing room. Response text is written
/// verbatim (the terminal soft-wraps it; it has no gutter to preserve).
fn print_chunk_to(out: &mut impl Write, state: &mut ChunkState, chunk: &StreamChunk) {
    let is_thinking = chunk.content_type == "thinking";
    let had_output = state.has_emitted;
    let prev_thinking = state.was_thinking;
    state.has_emitted = true;
    state.was_thinking = is_thinking;

    // Breathing room on any transition between thinking and non-thinking.
    if had_output && prev_thinking != is_thinking {
        if prev_thinking {
            flush_thinking(out, state); // commit any partial thinking line first
        }
        if !state.at_line_start {
            let _ = writeln!(out); // close the open content line
            state.at_line_start = true;
        }
        let _ = writeln!(out); // one blank line between sections
    }

    if chunk.text.is_empty() {
        return;
    }

    if is_thinking {
        let width = thinking_wrap_width();
        for ch in chunk.text.chars() {
            if ch == '\n' {
                // A complete logical line (possibly empty) — emit it now.
                let line = std::mem::take(&mut state.thinking_line);
                write_thinking_logical_line(out, &line, width);
                state.at_line_start = true;
            } else {
                state.thinking_line.push(ch);
            }
        }
    } else {
        let _ = write!(out, "{}", chunk.text);
        state.at_line_start = chunk.text.ends_with('\n');
    }
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

/// Print a provider key fallback warning. Emitted when the daemon
/// rotates away from a credential-flagged key (e.g. an exhausted budget
/// key) so the user sees the rotation immediately.
pub fn print_provider_fallback_warning(w: &ProviderFallbackWarning) {
    let stderr = io::stderr();
    let mut out = stderr.lock();

    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::Yellow));
    }
    let _ = write!(out, "warning");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = writeln!(out, ": {}", w.message);
}

/// Print a usage budget warning emitted after a threshold crossing.
pub fn print_usage_warning(w: &UsageWarning) {
    let stderr = io::stderr();
    let mut out = stderr.lock();

    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::Yellow));
    }
    let _ = write!(out, "warning");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = writeln!(out, ": {}", w.message);
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
    images::render_image(&img.path, img.caption.as_deref(), img.data.as_deref());
}

/// Render inline images from a message's image references.
pub fn print_image_refs(refs: &[ImageRef]) {
    for img in refs {
        images::render_image(&img.path, img.caption.as_deref(), img.data.as_deref());
    }
}

/// Format a tool input value for display.
pub(crate) fn format_tool_input(input: &serde_json::Value) -> Option<String> {
    format_tool_input_with_limit(input, Some(MAX_TOOL_OUTPUT))
}

/// Format a tool result for display.
pub(crate) fn format_tool_output(output: &str) -> String {
    format_tool_output_with_limit(output, Some(MAX_TOOL_OUTPUT))
}

pub(crate) fn write_tool_body(out: &mut impl Write, body: &str, color: Color) {
    if body.is_empty() {
        return;
    }

    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(color));
    }
    for line in body.lines() {
        let _ = writeln!(out, "  {line}");
    }
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
}

pub(crate) fn write_tool_body_plain(out: &mut impl Write, body: &str) {
    for line in body.lines() {
        let _ = writeln!(out, "  {line}");
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
    let _ = writeln!(out);
    if let Some(input) = format_tool_input(&call.input) {
        write_tool_body(&mut out, &input, Color::DarkGrey);
    }
}

/// Print a tool result. Long outputs are truncated.
pub fn print_tool_result(result: &ToolResult) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let color = if result.is_error {
        Color::Red
    } else {
        Color::DarkGrey
    };
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(color));
    }

    let label = if result.is_error { "error" } else { "result" };
    let _ = write!(out, "[{label}: {}]", result.tool_name);

    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = writeln!(out);
    let body = format_tool_output(&result.output);
    write_tool_body(&mut out, &body, color);
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
        assert!(
            result.contains("truncated"),
            "large input should include a truncation notice"
        );
        assert!(
            result.contains("bytes total"),
            "large input should report the original display size"
        );
        assert!(
            result.len() <= MAX_TOOL_OUTPUT + 50,
            "truncated output should be bounded"
        );
    }

    #[test]
    fn format_tool_input_small_input_not_truncated() {
        let input = serde_json::json!({"key": "value", "num": 42});
        let result = format_tool_input(&input).unwrap();
        assert!(
            !result.ends_with("..."),
            "small input should not be truncated"
        );
        assert!(result.contains("42"));
    }

    // ── reset_chunk_state ───────────────────────────────────────────

    #[test]
    fn reset_chunk_state_clears_thinking() {
        set_color_enabled(false);
        {
            let mut s = CHUNK_STATE.lock().unwrap();
            s.was_thinking = true;
            s.has_emitted = true;
            s.at_line_start = false;
        }
        reset_chunk_state();
        let s = CHUNK_STATE.lock().unwrap();
        assert!(!s.was_thinking);
        assert!(!s.has_emitted);
        assert!(s.at_line_start);
    }

    fn chunk(content_type: &str, text: &str) -> StreamChunk {
        StreamChunk {
            rid: None,
            text: text.into(),
            content_type: content_type.into(),
        }
    }

    /// Visual preview harness (not an assertion). Streams interleaved thinking
    /// token-by-token through the real chunk renderer, color ON, and dumps the
    /// raw bytes so a terminal shows it exactly as live streaming would.
    ///
    /// Run it: `cargo test -p shore-cli render_preview_stream
    ///          -- --ignored --nocapture --test-threads=1`
    /// (or via `.claude/skills/run-shore-cli/preview.sh`).
    #[test]
    #[ignore = "visual preview; run explicitly with --ignored --nocapture"]
    fn render_preview_stream() {
        set_color_enabled(true);
        let mut state = ChunkState::default();
        let mut buf = Vec::new();
        // A long thinking paragraph (streamed in fragments) to show wrapping.
        for c in [
            "Let me reason about this carefully — the user's query ",
            "spans more than the wrap width, so the gutter bar has to ",
            "continue down every wrapped row instead of only marking the ",
            "first line of the paragraph.",
        ] {
            print_chunk_to(&mut buf, &mut state, &chunk("thinking", c));
        }
        for c in ["Here's ", "the first ", "answer."] {
            print_chunk_to(&mut buf, &mut state, &chunk("text", c));
        }
        for c in [
            "Now reconsidering, ",
            "having seen the result, I can refine it.",
        ] {
            print_chunk_to(&mut buf, &mut state, &chunk("thinking", c));
        }
        for c in ["And ", "the refined ", "conclusion."] {
            print_chunk_to(&mut buf, &mut state, &chunk("text", c));
        }
        set_color_enabled(false);
        let mut stdout = io::stdout();
        let _ = stdout.write_all(b"\n----- STREAMING RENDER (live tokens) -----\n");
        let _ = stdout.write_all(&buf);
        let _ = stdout.write_all(b"\n----- end -----\n");
        let _ = stdout.flush();
    }

    #[test]
    fn streaming_interleaved_thinking_gutter_both_directions() {
        set_color_enabled(false);
        let mut state = ChunkState::default();
        let mut buf = Vec::new();
        // thinking -> text -> thinking -> text, each as a single chunk.
        print_chunk_to(&mut buf, &mut state, &chunk("thinking", "T1"));
        print_chunk_to(&mut buf, &mut state, &chunk("text", "A1"));
        print_chunk_to(&mut buf, &mut state, &chunk("thinking", "T2"));
        print_chunk_to(&mut buf, &mut state, &chunk("text", "A2"));
        let output = String::from_utf8(buf).unwrap();

        // Thinking is gutter-barred; a blank line of breathing room straddles
        // every transition so the second thinking block is not glued onto the
        // end of the first answer.
        assert_eq!(output, "  \u{2502} T1\n\nA1\n\n  \u{2502} T2\n\nA2");
    }

    #[test]
    fn streaming_thinking_buffers_logical_lines_across_chunks() {
        set_color_enabled(false);
        let mut state = ChunkState::default();
        let mut buf = Vec::new();
        // A logical line is split across chunks (the newline lands mid-chunk,
        // and a word straddles the boundary). Each completed line is emitted
        // gutter-barred; the trailing partial line waits for an explicit flush
        // (stream end / transition).
        print_chunk_to(&mut buf, &mut state, &chunk("thinking", "line one\nli"));
        print_chunk_to(&mut buf, &mut state, &chunk("thinking", "ne two"));
        flush_thinking(&mut buf, &mut state); // simulate stream end
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(output, "  \u{2502} line one\n  \u{2502} line two\n");
    }

    #[test]
    fn streaming_thinking_flushed_before_tool_call() {
        set_color_enabled(false);
        let mut state = ChunkState::default();
        let mut buf = Vec::new();
        // Thinking with no trailing newline, then a flush (as happens before a
        // ToolCall or StreamEnd) must commit the buffered line.
        print_chunk_to(
            &mut buf,
            &mut state,
            &chunk("thinking", "deciding to call a tool"),
        );
        assert!(buf.is_empty(), "nothing emitted until the line is flushed");
        flush_thinking(&mut buf, &mut state);
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(output, "  \u{2502} deciding to call a tool\n");
    }

    #[test]
    fn streaming_consecutive_same_type_chunks_not_separated() {
        set_color_enabled(false);
        let mut state = ChunkState::default();
        let mut buf = Vec::new();
        print_chunk_to(&mut buf, &mut state, &chunk("text", "Hello "));
        print_chunk_to(&mut buf, &mut state, &chunk("text", "world"));
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(output, "Hello world");
    }
}
