use std::time::Instant;

use serde_json::{json, Value};

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

// ── Usage extraction helpers ────────────────────────────────────────────

/// Merge Anthropic usage fields into an existing `Usage`.
///
/// `input_tokens` is only overwritten when > 0 — OpenRouter sends the real
/// input count in `message_delta` while `message_start` may report 0.
pub(crate) fn merge_anthropic_usage(usage: &mut Usage, raw: &Value) {
    if let Some(v) = raw.get("input_tokens").and_then(Value::as_u64) {
        if v > 0 {
            usage.input_tokens = v as u32;
        }
    }
    if let Some(v) = raw.get("output_tokens").and_then(Value::as_u64) {
        usage.output_tokens = v as u32;
    }
    if let Some(v) = raw.get("cache_read_input_tokens").and_then(Value::as_u64) {
        usage.cache_read_tokens = v as u32;
    }
    if let Some(v) = raw.get("cache_creation_input_tokens").and_then(Value::as_u64) {
        usage.cache_creation_tokens = v as u32;
    }
}

/// Extract Anthropic usage from a non-streaming response body.
pub(crate) fn extract_anthropic_usage(body: Option<&Value>) -> Usage {
    let mut usage = Usage::default();
    if let Some(raw) = body {
        // Non-streaming: all fields present in one object, set unconditionally.
        usage.input_tokens = raw
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        usage.output_tokens = raw
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        usage.cache_read_tokens = raw
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        usage.cache_creation_tokens = raw
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
    }
    usage
}

/// Extract Gemini usage from a `usageMetadata` object.
pub(crate) fn extract_gemini_usage(meta: Option<&Value>) -> Usage {
    let Some(m) = meta else {
        return Usage::default();
    };
    Usage {
        input_tokens: m
            .get("promptTokenCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        output_tokens: m
            .get("candidatesTokenCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        cache_read_tokens: m
            .get("cachedContentTokenCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        cache_creation_tokens: 0,
    }
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
