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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_build_start_event() {
        let event = build_start_event("claude-3-opus");
        let parsed: Value = serde_json::from_str(&event).unwrap();
        assert_eq!(parsed["type"], "start");
        assert_eq!(parsed["model"], "claude-3-opus");
    }

    #[test]
    fn test_build_done_event() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 25,
            cache_creation_tokens: 10,
        };
        let event = build_done_event("hello world", "end_turn", &usage, 1500, 200);
        let parsed: Value = serde_json::from_str(&event).unwrap();
        assert_eq!(parsed["type"], "done");
        assert_eq!(parsed["content"], "hello world");
        assert_eq!(parsed["finish_reason"], "end_turn");
        assert_eq!(parsed["usage"]["input_tokens"], 100);
        assert_eq!(parsed["usage"]["output_tokens"], 50);
        assert_eq!(parsed["usage"]["cache_read_tokens"], 25);
        assert_eq!(parsed["usage"]["cache_creation_tokens"], 10);
        assert_eq!(parsed["timing"]["total_ms"], 1500);
        assert_eq!(parsed["timing"]["time_to_first_token_ms"], 200);
    }

    #[test]
    fn test_build_tool_use_event() {
        let input = json!({"query": "weather today"});
        let event = build_tool_use_event("call_123", "web_search", &input);
        let parsed: Value = serde_json::from_str(&event).unwrap();
        assert_eq!(parsed["type"], "tool_use");
        assert_eq!(parsed["id"], "call_123");
        assert_eq!(parsed["name"], "web_search");
        assert_eq!(parsed["input"]["query"], "weather today");
    }

    #[test]
    fn test_merge_anthropic_usage_skips_zero_input() {
        let mut usage = Usage {
            input_tokens: 500,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        };
        // OpenRouter sends input_tokens=0 in message_start; must not overwrite.
        let raw = json!({"input_tokens": 0, "output_tokens": 42});
        merge_anthropic_usage(&mut usage, &raw);
        assert_eq!(usage.input_tokens, 500, "input_tokens=0 should not overwrite");
        assert_eq!(usage.output_tokens, 42);
    }

    #[test]
    fn test_merge_anthropic_usage_overwrites_output() {
        let mut usage = Usage {
            input_tokens: 100,
            output_tokens: 10,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        };
        let raw = json!({"output_tokens": 99});
        merge_anthropic_usage(&mut usage, &raw);
        assert_eq!(usage.output_tokens, 99);
        // input_tokens unchanged (key absent in raw).
        assert_eq!(usage.input_tokens, 100);
    }

    #[test]
    fn test_merge_anthropic_usage_cache_fields() {
        let mut usage = Usage::default();
        let raw = json!({
            "input_tokens": 200,
            "output_tokens": 50,
            "cache_read_input_tokens": 150,
            "cache_creation_input_tokens": 80
        });
        merge_anthropic_usage(&mut usage, &raw);
        assert_eq!(usage.input_tokens, 200);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_tokens, 150);
        assert_eq!(usage.cache_creation_tokens, 80);
    }

    #[test]
    fn test_extract_anthropic_usage() {
        let body = json!({
            "input_tokens": 300,
            "output_tokens": 120,
            "cache_read_input_tokens": 200,
            "cache_creation_input_tokens": 50
        });
        let usage = extract_anthropic_usage(Some(&body));
        assert_eq!(usage.input_tokens, 300);
        assert_eq!(usage.output_tokens, 120);
        assert_eq!(usage.cache_read_tokens, 200);
        assert_eq!(usage.cache_creation_tokens, 50);

        // None body returns all zeros.
        let empty = extract_anthropic_usage(None);
        assert_eq!(empty.input_tokens, 0);
        assert_eq!(empty.output_tokens, 0);
    }

    #[test]
    fn test_extract_gemini_usage() {
        let meta = json!({
            "promptTokenCount": 400,
            "candidatesTokenCount": 80,
            "cachedContentTokenCount": 100
        });
        let usage = extract_gemini_usage(Some(&meta));
        assert_eq!(usage.input_tokens, 400);
        assert_eq!(usage.output_tokens, 80);
        assert_eq!(usage.cache_read_tokens, 100);
        assert_eq!(usage.cache_creation_tokens, 0);

        // None meta returns all zeros.
        let empty = extract_gemini_usage(None);
        assert_eq!(empty.input_tokens, 0);
    }

    #[test]
    fn test_merge_anthropic_usage_all_zeros() {
        let mut usage = Usage::default();
        let raw = json!({"input_tokens": 0, "output_tokens": 0});
        merge_anthropic_usage(&mut usage, &raw);
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
    }

    #[test]
    fn test_merge_anthropic_usage_cache_only() {
        let mut usage = Usage::default();
        let raw = json!({
            "cache_read_input_tokens": 500,
            "cache_creation_input_tokens": 200
        });
        merge_anthropic_usage(&mut usage, &raw);
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.cache_read_tokens, 500);
        assert_eq!(usage.cache_creation_tokens, 200);
    }

    #[test]
    fn test_extract_anthropic_usage_missing_fields() {
        let body = json!({"input_tokens": 100});
        let usage = extract_anthropic_usage(Some(&body));
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.cache_read_tokens, 0);
        assert_eq!(usage.cache_creation_tokens, 0);
    }

    #[test]
    fn test_extract_gemini_usage_missing_fields() {
        let meta = json!({"promptTokenCount": 100});
        let usage = extract_gemini_usage(Some(&meta));
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.cache_read_tokens, 0);
    }
}
