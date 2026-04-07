use shore_protocol::server_msg::{ServerMessage, StreamChunk, StreamEnd, StreamStart};
use tracing::{debug, trace, warn};

use crate::error::{ClientError, Result};

/// Callbacks invoked during stream consumption.
///
/// All callbacks receive `&mut self` so they can accumulate state.
/// Implement only the callbacks you care about; defaults are no-ops.
pub trait StreamCallbacks: Send {
    /// Called when a `stream_start` is received.
    fn on_start(&mut self, _start: &StreamStart) {}

    /// Called for each `stream_chunk`.
    fn on_chunk(&mut self, _chunk: &StreamChunk) {}

    /// Called when `stream_end` is received with the final content + metadata.
    fn on_end(&mut self, _end: &StreamEnd) {}
}

/// Accumulates streaming chunks into a final string.
///
/// This is the default handler — it collects all chunk text and exposes
/// the assembled content when the stream completes.
pub struct StreamHandler {
    /// Whether we are currently inside a stream sequence.
    active: bool,
    /// Is this a regen stream?
    regen: bool,
    /// Accumulated chunk text.
    chunks: Vec<String>,
    /// Final content from `stream_end`, if received.
    final_content: Option<String>,
    /// Metadata from stream_end, if received.
    metadata: Option<shore_protocol::types::StreamMetadata>,
}

impl StreamHandler {
    pub fn new() -> Self {
        Self {
            active: false,
            regen: false,
            chunks: Vec::new(),
            final_content: None,
            metadata: None,
        }
    }

    /// Reset state for reuse.
    pub fn reset(&mut self) {
        self.active = false;
        self.regen = false;
        self.chunks.clear();
        self.final_content = None;
        self.metadata = None;
    }

    /// Whether the handler is currently inside a stream sequence.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Whether this stream is a regen.
    pub fn is_regen(&self) -> bool {
        self.regen
    }

    /// Text assembled from chunks so far.
    pub fn assembled_text(&self) -> String {
        self.chunks.join("")
    }

    /// Final content from `stream_end`, if the stream has completed.
    pub fn final_content(&self) -> Option<&str> {
        self.final_content.as_deref()
    }

    /// Metadata from `stream_end`, if the stream has completed.
    pub fn metadata(&self) -> Option<&shore_protocol::types::StreamMetadata> {
        self.metadata.as_ref()
    }

    /// Feed a server message into the handler.
    ///
    /// Returns `true` if the message was consumed (i.e. it was a stream message),
    /// `false` if the message is unrelated to streaming and should be handled
    /// elsewhere.
    pub fn feed(
        &mut self,
        msg: &ServerMessage,
        callbacks: Option<&mut dyn StreamCallbacks>,
    ) -> Result<bool> {
        match msg {
            ServerMessage::StreamStart(start) => {
                if self.active {
                    warn!("received stream_start while already streaming");
                    return Err(ClientError::Protocol(
                        "received stream_start while already streaming".into(),
                    ));
                }
                self.reset();
                self.active = true;
                self.regen = start.regen;
                debug!(regen = start.regen, "stream started");
                if let Some(cb) = callbacks {
                    cb.on_start(start);
                }
                Ok(true)
            }
            ServerMessage::StreamChunk(chunk) => {
                if !self.active {
                    warn!("received stream_chunk outside of a stream");
                    return Err(ClientError::Protocol(
                        "received stream_chunk outside of a stream".into(),
                    ));
                }
                trace!(bytes = chunk.text.len(), content_type = %chunk.content_type, "stream chunk");
                self.chunks.push(chunk.text.clone());
                if let Some(cb) = callbacks {
                    cb.on_chunk(chunk);
                }
                Ok(true)
            }
            ServerMessage::StreamEnd(end) => {
                if !self.active {
                    warn!("received stream_end outside of a stream");
                    return Err(ClientError::Protocol(
                        "received stream_end outside of a stream".into(),
                    ));
                }
                self.active = false;
                self.final_content = Some(end.content.clone());
                self.metadata = Some(end.metadata.clone());
                debug!(
                    finish_reason = %end.finish_reason,
                    model = %end.metadata.model,
                    input_tokens = end.metadata.tokens.input,
                    output_tokens = end.metadata.tokens.output,
                    total_ms = end.metadata.timing.total_ms,
                    "stream ended"
                );
                if let Some(cb) = callbacks {
                    cb.on_end(end);
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

impl Default for StreamHandler {
    fn default() -> Self {
        Self::new()
    }
}
