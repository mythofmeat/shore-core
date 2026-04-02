use std::time::Instant;

use serde_json::json;

use crate::types::Usage;

/// Timing helper shared across streaming providers.
pub(crate) struct StreamTiming {
    start: Instant,
    first_token_ms: Option<u32>,
}

impl StreamTiming {
    pub(crate) fn new() -> Self {
        Self { start: Instant::now(), first_token_ms: None }
    }

    /// Record the first token time (idempotent — only records the first call).
    pub(crate) fn record_first_token(&mut self) {
        if self.first_token_ms.is_none() {
            self.first_token_ms = Some(self.start.elapsed().as_millis() as u32);
        }
    }

    pub(crate) fn total_ms(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    pub(crate) fn ttft_ms(&self) -> u32 {
        let total = self.total_ms();
        self.first_token_ms.unwrap_or(total)
    }
}

/// Build the NDJSON `done` event line.
pub(crate) fn build_done_event(
    content: &str,
    finish_reason: &str,
    usage: &Usage,
    total_ms: u32,
    ttft_ms: u32,
) -> String {
    json!({
        "type": "done",
        "content": content,
        "finish_reason": finish_reason,
        "usage": {
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "cache_read_tokens": usage.cache_read_tokens,
            "cache_creation_tokens": usage.cache_creation_tokens,
        },
        "timing": {
            "total_ms": total_ms,
            "time_to_first_token_ms": ttft_ms,
        },
    })
    .to_string()
}

/// Build the NDJSON `start` event line.
pub(crate) fn build_start_event(model: &str) -> String {
    json!({ "type": "start", "model": model }).to_string()
}

/// Build a NDJSON `tool_use` event line.
pub(crate) fn build_tool_use_event(id: &str, name: &str, input: &serde_json::Value) -> String {
    json!({
        "type": "tool_use",
        "id": id,
        "name": name,
        "input": input,
    })
    .to_string()
}
