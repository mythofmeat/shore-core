use futures_util::StreamExt;
use reqwest::Response;
use tokio::io::{AsyncWriteExt, DuplexStream};

/// A parsed SSE event.
pub(crate) struct SseEvent {
    pub event: Option<String>,
    pub data: String,
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
    let mut buf = String::new();
    let mut current_event: Option<String> = None;
    let mut current_data = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(crate::LlmError::Request)?;
        buf.push_str(&String::from_utf8_lossy(&chunk));

        // Process complete lines from buffer.
        while let Some(newline_pos) = buf.find('\n') {
            let line = buf[..newline_pos].trim_end_matches('\r').to_string();
            buf = buf[newline_pos + 1..].to_string();

            if line.is_empty() {
                // Empty line = end of event. Dispatch if we have data.
                if !current_data.is_empty() {
                    let event = SseEvent {
                        event: current_event.take(),
                        data: std::mem::take(&mut current_data),
                    };
                    if let Some(ndjson_line) = callback(event) {
                        writer.write_all(ndjson_line.as_bytes()).await.map_err(|e| {
                            crate::LlmError::Provider {
                                message: format!("failed to write to stream: {e}"),
                            }
                        })?;
                        writer.write_all(b"\n").await.map_err(|e| {
                            crate::LlmError::Provider {
                                message: format!("failed to write to stream: {e}"),
                            }
                        })?;
                    }
                }
                current_event = None;
                current_data.clear();
                continue;
            }

            // Comment line — ignore.
            if line.starts_with(':') {
                continue;
            }

            if let Some(value) = line.strip_prefix("event:") {
                current_event = Some(value.trim().to_string());
            } else if let Some(value) = line.strip_prefix("data:") {
                let value = value.trim_start();
                if !current_data.is_empty() {
                    current_data.push('\n');
                }
                current_data.push_str(value);
            }
            // Ignore other field types (id:, retry:, etc.)
        }
    }

    // Process any trailing data (no final empty line).
    if !current_data.is_empty() {
        let event = SseEvent {
            event: current_event.take(),
            data: std::mem::take(&mut current_data),
        };
        if let Some(ndjson_line) = callback(event) {
            let _ = writer.write_all(ndjson_line.as_bytes()).await;
            let _ = writer.write_all(b"\n").await;
        }
    }

    Ok(())
}
