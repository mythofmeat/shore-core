use std::io::{self, Write};

use crossterm::style::{Attribute, Color, ResetColor, SetAttribute, SetForegroundColor};
use shore_protocol::server_msg::{CommandOutput, NewMessage, SendImage, StreamChunk, StreamEnd};
use shore_protocol::types::ImageRef;

use crate::images;

/// Print a stream chunk to stdout. Thinking chunks are shown dimmed.
pub fn print_chunk(chunk: &StreamChunk) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    if chunk.content_type == "thinking" {
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
    let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
    let _ = write!(
        out,
        "[{} | in:{} out:{} cache_r:{} cache_w:{} | ttft:{}ms total:{}ms]",
        end.metadata.model,
        end.metadata.tokens.input,
        end.metadata.tokens.output,
        end.metadata.tokens.cache_read,
        end.metadata.tokens.cache_write,
        end.metadata.timing.ttft_ms,
        end.metadata.timing.total_ms,
    );
    let _ = crossterm::execute!(out, ResetColor);
    let _ = writeln!(out);
}

/// Print command output. Formats JSON data in a readable way.
pub fn print_command_output(output: &CommandOutput) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    // Command name in bold
    let _ = crossterm::execute!(out, SetAttribute(Attribute::Bold));
    let _ = write!(out, "{}", output.name);
    let _ = crossterm::execute!(out, SetAttribute(Attribute::Reset));
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

    let _ = crossterm::execute!(out, SetForegroundColor(Color::Red));
    let _ = write!(out, "error");
    let _ = crossterm::execute!(out, ResetColor);
    let _ = writeln!(out, ": {err}");
}

/// Print a server protocol error.
pub fn print_server_error(code: &str, message: &str) {
    let stderr = io::stderr();
    let mut out = stderr.lock();

    let _ = crossterm::execute!(out, SetForegroundColor(Color::Red));
    let _ = write!(out, "server error");
    let _ = crossterm::execute!(out, ResetColor);
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

/// Print a push NewMessage (used in subscribe mode).
pub fn print_new_message(msg: &NewMessage) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let _ = crossterm::execute!(out, SetForegroundColor(Color::Cyan));
    let _ = write!(out, "[{:?}]", msg.message.role);
    let _ = crossterm::execute!(out, ResetColor);
    let _ = writeln!(out, " {}", msg.message.content);

    // Render any attached images.
    print_image_refs(&msg.message.images);
}

/// Print the "thinking..." indicator when streaming starts.
pub fn print_stream_start(regen: bool) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
    if regen {
        let _ = write!(out, "(regenerating...) ");
    }
    let _ = crossterm::execute!(out, ResetColor);
    let _ = out.flush();
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
}
