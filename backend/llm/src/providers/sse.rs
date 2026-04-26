use futures_util::StreamExt;
use reqwest::Response;
use tokio::io::{AsyncWriteExt, DuplexStream};

/// A parsed SSE event.
pub(crate) struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Stateful SSE line parser.
///
/// Accumulates bytes fed via `feed()`, emits complete `SseEvent`s when an
/// empty-line delimiter is encountered.  Call `flush()` after the last chunk
/// to dispatch any trailing data that wasn't followed by an empty line.
pub(crate) struct SseParser {
    buf: String,
    current_event: Option<String>,
    current_data: String,
}

impl SseParser {
    pub(crate) fn new() -> Self {
        Self {
            buf: String::new(),
            current_event: None,
            current_data: String::new(),
        }
    }

    /// Feed a chunk of text into the parser and return any complete events.
    pub(crate) fn feed(&mut self, chunk: &str) -> Vec<SseEvent> {
        self.buf.push_str(chunk);
        let mut events = Vec::new();

        while let Some(newline_pos) = self.buf.find('\n') {
            let raw: String = self.buf.drain(..=newline_pos).collect();
            let line = raw
                .trim_end_matches('\n')
                .trim_end_matches('\r')
                .to_string();

            if line.is_empty() {
                // Empty line = end of event.  Dispatch if we have data.
                if !self.current_data.is_empty() {
                    events.push(SseEvent {
                        event: self.current_event.take(),
                        data: std::mem::take(&mut self.current_data),
                    });
                }
                self.current_event = None;
                self.current_data.clear();
                continue;
            }

            // Comment line — ignore.
            if line.starts_with(':') {
                continue;
            }

            if let Some(value) = line.strip_prefix("event:") {
                self.current_event = Some(value.trim().to_string());
            } else if let Some(value) = line.strip_prefix("data:") {
                let value = value.trim_start();
                if !self.current_data.is_empty() {
                    self.current_data.push('\n');
                }
                self.current_data.push_str(value);
            }
            // Ignore other field types (id:, retry:, etc.)
        }

        events
    }

    /// Flush any trailing data (no final empty line).
    pub(crate) fn flush(self) -> Option<SseEvent> {
        if self.current_data.is_empty() {
            return None;
        }
        Some(SseEvent {
            event: self.current_event,
            data: self.current_data,
        })
    }
}

/// Read SSE events from a reqwest streaming response and yield them.
///
/// Handles `event:` lines, `data:` lines (with multi-line concatenation),
/// and empty-line delimiters. Ignores comment lines (`:` prefix).
pub(crate) async fn read_sse_events(
    response: Response,
    mut callback: impl FnMut(SseEvent) -> Option<String>,
    writer: &mut DuplexStream,
) -> Result<(), crate::LlmError> {
    let mut stream = response.bytes_stream();
    let mut parser = SseParser::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(crate::LlmError::Request)?;
        let events = parser.feed(&String::from_utf8_lossy(&chunk));

        for event in events {
            if let Some(ndjson_line) = callback(event) {
                writer
                    .write_all(ndjson_line.as_bytes())
                    .await
                    .map_err(|e| crate::LlmError::Provider {
                        message: format!("failed to write to stream: {e}"),
                    })?;
                writer
                    .write_all(b"\n")
                    .await
                    .map_err(|e| crate::LlmError::Provider {
                        message: format!("failed to write to stream: {e}"),
                    })?;
            }
        }
    }

    // Process any trailing data (no final empty line).
    if let Some(event) = parser.flush() {
        if let Some(ndjson_line) = callback(event) {
            let _ = writer.write_all(ndjson_line.as_bytes()).await;
            let _ = writer.write_all(b"\n").await;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_data_event() {
        let mut parser = SseParser::new();
        let events = parser.feed("data: hello\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
        assert!(events[0].event.is_none());
    }

    #[test]
    fn test_event_and_data() {
        let mut parser = SseParser::new();
        let events = parser.feed("event: message\ndata: payload\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("message"));
        assert_eq!(events[0].data, "payload");
    }

    #[test]
    fn test_multiline_data() {
        let mut parser = SseParser::new();
        let events = parser.feed("data: line1\ndata: line2\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "line1\nline2");
    }

    #[test]
    fn test_comment_lines_ignored() {
        let mut parser = SseParser::new();
        let events = parser.feed(":this is a comment\ndata: real\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "real");
    }

    #[test]
    fn test_empty_lines_between_events() {
        let mut parser = SseParser::new();
        // Multiple empty lines should not produce spurious events.
        let events = parser.feed("data: first\n\n\n\ndata: second\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "first");
        assert_eq!(events[1].data, "second");
    }

    #[test]
    fn test_carriage_return_handling() {
        let mut parser = SseParser::new();
        let events = parser.feed("data: hello\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn test_trailing_data_without_empty_line() {
        let mut parser = SseParser::new();
        // Feed data without a terminating empty line.
        let events = parser.feed("data: trailing\n");
        assert!(events.is_empty(), "no event yet — no empty-line delimiter");

        let flushed = parser.flush();
        assert!(flushed.is_some());
        assert_eq!(flushed.unwrap().data, "trailing");
    }

    #[test]
    fn test_chunked_delivery() {
        let mut parser = SseParser::new();

        // Split a single event across multiple feed calls.
        let e1 = parser.feed("event: msg\n");
        assert!(e1.is_empty());

        let e2 = parser.feed("data: hel");
        assert!(e2.is_empty());

        let e3 = parser.feed("lo\n\n");
        assert_eq!(e3.len(), 1);
        assert_eq!(e3[0].event.as_deref(), Some("msg"));
        assert_eq!(e3[0].data, "hello");
    }
}
