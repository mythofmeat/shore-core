use std::time::Instant;

use serde_json::{json, Value};

use crate::types::{LlmRequest, Usage};

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

// ── Shared translation helpers ─────────────────────────────────────────

/// Extract plain text from a value that is either a JSON string or an array
/// of `{type: "text", text: "..."}` content blocks.  All text blocks are
/// concatenated into a single string.
pub(crate) fn extract_system_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| {
                if b.get("type").and_then(Value::as_str) == Some("text") {
                    b.get("text").and_then(Value::as_str).map(String::from)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Extract token usage from an OpenAI usage object, including cached tokens
/// from `prompt_tokens_details.cached_tokens`.
pub(crate) fn extract_openai_usage(u: &Value) -> Usage {
    let cached = u
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    Usage {
        input_tokens: u
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        output_tokens: u
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        cache_read_tokens: cached,
        cache_creation_tokens: 0,
    }
}

/// Normalize a provider-specific finish reason into a canonical string.
///
/// Covers OpenAI (lowercase), Gemini (UPPERCASE), and Anthropic (already
/// canonical) values in a single collision-free match table.
pub(crate) fn normalize_finish_reason(reason: Option<&str>) -> &'static str {
    match reason {
        // OpenAI
        Some("stop") => "end_turn",
        Some("tool_calls") => "tool_use",
        Some("length") => "max_tokens",
        Some("content_filter") => "content_filter",
        // Gemini
        Some("STOP") => "end_turn",
        Some("MAX_TOKENS") => "max_tokens",
        Some("SAFETY") => "safety",
        Some("RECITATION") => "recitation",
        Some("MALFORMED_FUNCTION_CALL") => "tool_use",
        // Z.AI
        Some("sensitive") => "content_filter",
        Some("model_context_window_exceeded") => "max_tokens",
        Some("network_error") => "end_turn",
        // Already canonical (Anthropic passthrough)
        Some("end_turn") => "end_turn",
        Some("max_tokens") => "max_tokens",
        Some("tool_use") => "tool_use",
        _ => "end_turn",
    }
}

/// Apply common sampling parameters (`temperature`, `top_p`) to a JSON body.
///
/// Uses snake_case keys — suitable for Anthropic and OpenAI.  Gemini uses
/// different key names (`topP`) on a sub-object, so it is NOT covered here.
pub(crate) fn apply_common_params(body: &mut Value, request: &LlmRequest) {
    if let Some(temp) = request.temperature {
        body["temperature"] = json!(temp);
    }
    if let Some(top_p) = request.top_p {
        body["top_p"] = json!(top_p);
    }
}

/// Translate Anthropic-format tool definitions into provider-neutral
/// declarations with `name`, `description`, and `parameters` (from
/// `input_schema`).  Returns `None` when tools are absent or empty.
///
/// Each provider wraps the result in its own envelope:
/// - OpenAI: `{type: "function", function: <decl>}`
/// - Gemini: `[{functionDeclarations: <decls>}]`
pub(crate) fn translate_tool_declarations(tools: &Option<Vec<Value>>) -> Option<Vec<Value>> {
    let tools = tools.as_ref()?;
    if tools.is_empty() {
        return None;
    }
    Some(
        tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                    "description": t.get("description").and_then(|d| d.as_str()).unwrap_or(""),
                    "parameters": t.get("input_schema").cloned().unwrap_or(json!({})),
                })
            })
            .collect(),
    )
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

    // ── extract_system_text ────────────────────────────────────────────

    #[test]
    fn test_extract_system_text_string() {
        let val = json!("You are a helpful assistant.");
        assert_eq!(extract_system_text(&val), "You are a helpful assistant.");
    }

    #[test]
    fn test_extract_system_text_array() {
        let val = json!([
            {"type": "text", "text": "Part one. "},
            {"type": "text", "text": "Part two."}
        ]);
        assert_eq!(extract_system_text(&val), "Part one. Part two.");
    }

    #[test]
    fn test_extract_system_text_empty_array() {
        assert_eq!(extract_system_text(&json!([])), "");
    }

    #[test]
    fn test_extract_system_text_null() {
        assert_eq!(extract_system_text(&Value::Null), "");
    }

    #[test]
    fn test_extract_system_text_skips_non_text_blocks() {
        let val = json!([
            {"type": "text", "text": "Hello"},
            {"type": "image", "data": "..."},
            {"type": "text", "text": " world"}
        ]);
        assert_eq!(extract_system_text(&val), "Hello world");
    }

    // ── extract_openai_usage ───────────────────────────────────────────

    #[test]
    fn test_extract_openai_usage() {
        let u = json!({
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "prompt_tokens_details": {"cached_tokens": 30}
        });
        let usage = extract_openai_usage(&u);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_tokens, 30);
        assert_eq!(usage.cache_creation_tokens, 0);
    }

    #[test]
    fn test_extract_openai_usage_no_cache() {
        let u = json!({"prompt_tokens": 80, "completion_tokens": 20});
        let usage = extract_openai_usage(&u);
        assert_eq!(usage.input_tokens, 80);
        assert_eq!(usage.output_tokens, 20);
        assert_eq!(usage.cache_read_tokens, 0);
    }

    // ── normalize_finish_reason ────────────────────────────────────────

    #[test]
    fn test_normalize_finish_reason_openai() {
        assert_eq!(normalize_finish_reason(Some("stop")), "end_turn");
        assert_eq!(normalize_finish_reason(Some("tool_calls")), "tool_use");
        assert_eq!(normalize_finish_reason(Some("length")), "max_tokens");
        assert_eq!(normalize_finish_reason(Some("content_filter")), "content_filter");
    }

    #[test]
    fn test_normalize_finish_reason_gemini() {
        assert_eq!(normalize_finish_reason(Some("STOP")), "end_turn");
        assert_eq!(normalize_finish_reason(Some("MAX_TOKENS")), "max_tokens");
        assert_eq!(normalize_finish_reason(Some("SAFETY")), "safety");
        assert_eq!(normalize_finish_reason(Some("RECITATION")), "recitation");
        assert_eq!(normalize_finish_reason(Some("MALFORMED_FUNCTION_CALL")), "tool_use");
    }

    #[test]
    fn test_normalize_finish_reason_canonical_passthrough() {
        assert_eq!(normalize_finish_reason(Some("end_turn")), "end_turn");
        assert_eq!(normalize_finish_reason(Some("max_tokens")), "max_tokens");
        assert_eq!(normalize_finish_reason(Some("tool_use")), "tool_use");
    }

    #[test]
    fn test_normalize_finish_reason_none() {
        assert_eq!(normalize_finish_reason(None), "end_turn");
    }

    // ── translate_tool_declarations ────────────────────────────────────

    #[test]
    fn test_translate_tool_declarations() {
        let tools = Some(vec![json!({
            "name": "get_weather",
            "description": "Get the weather",
            "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}
        })]);
        let decls = translate_tool_declarations(&tools).unwrap();
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0]["name"], "get_weather");
        assert_eq!(decls[0]["description"], "Get the weather");
        assert_eq!(decls[0]["parameters"]["type"], "object");
    }

    #[test]
    fn test_translate_tool_declarations_empty() {
        assert!(translate_tool_declarations(&Some(vec![])).is_none());
    }

    #[test]
    fn test_translate_tool_declarations_none() {
        assert!(translate_tool_declarations(&None).is_none());
    }

    #[test]
    fn test_translate_tool_declarations_missing_fields() {
        let tools = Some(vec![json!({})]);
        let decls = translate_tool_declarations(&tools).unwrap();
        assert_eq!(decls[0]["name"], "");
        assert_eq!(decls[0]["description"], "");
        assert_eq!(decls[0]["parameters"], json!({}));
    }
}
