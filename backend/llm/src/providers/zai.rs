use std::collections::HashMap;
use std::time::Instant;

use serde_json::{json, Value};
use tokio::io::DuplexStream;

use shore_protocol::types::ContentBlock;

use crate::types::{GenerateResponse, LlmRequest, Timing, Usage};
use crate::LlmError;

use super::sse::{read_sse_events, SseEvent};
use super::stream_helpers::{
    apply_common_params, build_done_event, build_start_event, build_tool_use_event,
    extract_openai_usage, normalize_finish_reason, StreamTiming,
};

// ── Constants ──────────────────────────────────────────────────────────

const ZAI_BASE_URL: &str = "https://api.z.ai/api/paas/v4";
const ZAI_CODING_BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";

// ── Helpers ────────────────────────────────────────────────────────────

/// Extract a bool from provider_options by key.
fn opt_bool(request: &LlmRequest, key: &str) -> Option<bool> {
    request
        .provider_options
        .as_ref()
        .and_then(|opts| opts.get(key))
        .and_then(|v| v.as_bool())
}

/// Resolve the base URL: explicit override → subscription toggle → default.
fn resolve_base_url(request: &LlmRequest) -> &str {
    if let Some(ref url) = request.base_url {
        return url.as_str();
    }
    if opt_bool(request, "zai_subscription").unwrap_or(false) {
        ZAI_CODING_BASE_URL
    } else {
        ZAI_BASE_URL
    }
}

// ── Message + tool translation ─────────────────────────────────────────
//
// Z.AI speaks the OpenAI chat-completions wire format and previously
// duplicated `openai::translate_messages` / `translate_tools` outright.
// The only differences were:
//
// 1. Z.AI accepts raw `role: "system"` mid-history (OpenAI-routed
//    backends need the `<system_instruction>` wrapper).
// 2. Z.AI's `zai_clear_thinking` provider option, when set, drops prior
//    `thinking` blocks instead of replaying them as `reasoning_content`.
//
// Both are now flags on `ProviderContext` (`wrap_inline_system` and
// `drop_prior_thinking`), and the field name `reasoning_content` is
// already produced by `reasoning_field_for("zai")`. So the entire body
// of `translate_messages` collapses into a call into `openai`.

fn translate_messages(request: &LlmRequest) -> Vec<Value> {
    let ctx = super::context::build_provider_context(request);
    super::openai::translate_messages(request, &ctx)
}

fn translate_tools(tools: &Option<Vec<Value>>) -> Option<Vec<Value>> {
    super::openai::translate_tools(tools)
}

// ── Request body builder ────────────────────────────────────────────────

/// Build common request headers.
fn build_headers(request: &LlmRequest) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {}", request.api_key)
            .parse()
            .expect("valid header value"),
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        "application/json".parse().unwrap(),
    );
    if let Some(ref rid) = request.rid {
        if let Ok(hv) = rid.parse::<reqwest::header::HeaderValue>() {
            headers.insert("X-Request-ID", hv);
        }
    }
    headers
}

/// Build the JSON body for Z.AI chat completions.
fn build_chat_body(request: &LlmRequest, streaming: bool) -> Value {
    let clear_thinking = opt_bool(request, "zai_clear_thinking").unwrap_or(false);
    let messages = translate_messages(request);
    let tools = translate_tools(&request.tools);

    let mut body = json!({
        "model": request.model,
        "messages": messages,
        "max_tokens": request.max_tokens,
        "stream": streaming,
        "thinking": {"type": "enabled"},
        "clear_thinking": clear_thinking,
    });

    if streaming {
        body["stream_options"] = json!({"include_usage": true});
    }

    if let Some(tools) = tools {
        body["tools"] = Value::Array(tools);
    }
    apply_common_params(&mut body, request);

    body
}

// ── Streaming ──────────────────────────────────────────────────────────

/// Send a streaming chat completion request to the Z.AI API.
///
/// Returns a `DuplexStream` that yields NDJSON `StreamEvent` lines. A background
/// tokio task reads SSE from the upstream API and writes translated events.
pub async fn stream(
    client: &reqwest::Client,
    request: &LlmRequest,
) -> Result<DuplexStream, LlmError> {
    let base_url = resolve_base_url(request);
    let url = format!("{base_url}/chat/completions");
    let headers = build_headers(request);
    let body = build_chat_body(request, true);

    let response = client
        .post(&url)
        .headers(headers)
        .json(&body)
        .send()
        .await?;

    let response = super::check_response(response).await?;

    let (mut writer, reader) = tokio::io::duplex(64 * 1024);
    let model_fallback = request.model.clone();

    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;

        let mut timing = StreamTiming::new();
        let mut text_content = String::new();
        let mut finish_reason: &str = "end_turn";
        let mut usage = Usage::default();
        let mut model = model_fallback;
        let mut start_sent = false;

        // Tool call accumulation: index → (id, name, argument_chunks).
        let mut tool_calls: HashMap<u64, (String, String, Vec<String>)> = HashMap::new();

        let result = read_sse_events(
            response,
            |event: SseEvent| {
                let data = event.data.trim();

                if data == "[DONE]" {
                    return None;
                }

                let chunk: Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => return None,
                };

                let choice = chunk.get("choices").and_then(|c| c.get(0));
                let mut lines_out: Vec<String> = Vec::new();

                // Emit start event on first chunk (no early return — continue
                // processing so the first content delta is not dropped).
                if !start_sent {
                    if let Some(m) = chunk.get("model").and_then(|m| m.as_str()) {
                        model = m.to_string();
                    }
                    start_sent = true;
                    lines_out.push(build_start_event(&model));
                }

                if let Some(choice) = choice {
                    let delta = choice.get("delta");

                    // Reasoning content (Z.AI uses `reasoning_content` field).
                    if let Some(delta) = delta {
                        if let Some(reasoning) =
                            delta.get("reasoning_content").and_then(|r| r.as_str())
                        {
                            if !reasoning.is_empty() {
                                timing.record_first_token();
                                let ev = json!({"type": "thinking", "text": reasoning});
                                if let Ok(line) = serde_json::to_string(&ev) {
                                    lines_out.push(line);
                                }
                            }
                        }
                    }

                    // Text content.
                    if let Some(content) = delta
                        .and_then(|d| d.get("content"))
                        .and_then(|c| c.as_str())
                    {
                        if !content.is_empty() {
                            timing.record_first_token();
                            text_content.push_str(content);
                            let ev = json!({"type": "text", "text": content});
                            if let Ok(line) = serde_json::to_string(&ev) {
                                lines_out.push(line);
                            }
                        }
                    }

                    // Tool calls (streamed in fragments).
                    if let Some(tcs) = delta
                        .and_then(|d| d.get("tool_calls"))
                        .and_then(|t| t.as_array())
                    {
                        for tc in tcs {
                            let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                            let entry = tool_calls
                                .entry(index)
                                .or_insert_with(|| (String::new(), String::new(), Vec::new()));
                            if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                                if !id.is_empty() {
                                    entry.0 = id.to_string();
                                }
                            }
                            if let Some(name) = tc
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|n| n.as_str())
                            {
                                if !name.is_empty() {
                                    entry.1 = name.to_string();
                                }
                            }
                            if let Some(args) = tc
                                .get("function")
                                .and_then(|f| f.get("arguments"))
                                .and_then(|a| a.as_str())
                            {
                                entry.2.push(args.to_string());
                            }
                        }
                    }

                    // Finish reason.
                    if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
                        finish_reason = normalize_finish_reason(Some(reason));
                    }
                }

                // Usage (final chunk with stream_options.include_usage).
                if let Some(u) = chunk.get("usage") {
                    usage = extract_openai_usage(u);
                }

                if lines_out.is_empty() {
                    None
                } else {
                    Some(lines_out.join("\n"))
                }
            },
            &mut writer,
        )
        .await;

        if let Err(e) = result {
            tracing::warn!(error = %e, "Z.AI SSE stream read error");
        }

        // Ensure start was emitted (empty stream edge case).
        if !start_sent {
            let line = build_start_event(&model);
            let _ = writer.write_all(line.as_bytes()).await;
            let _ = writer.write_all(b"\n").await;
        }

        // Emit accumulated tool calls.
        let mut indices: Vec<u64> = tool_calls.keys().copied().collect();
        indices.sort();
        for idx in indices {
            if let Some((id, name, arg_chunks)) = tool_calls.remove(&idx) {
                let raw = arg_chunks.join("");
                let input: Value = if raw.is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(&raw).unwrap_or(json!({}))
                };
                let line = build_tool_use_event(&id, &name, &input);
                let _ = writer.write_all(line.as_bytes()).await;
                let _ = writer.write_all(b"\n").await;
            }
        }

        // Done event.
        let done = build_done_event(
            &text_content,
            finish_reason,
            &usage,
            timing.total_ms(),
            timing.ttft_ms(),
        );
        let _ = writer.write_all(done.as_bytes()).await;
        let _ = writer.write_all(b"\n").await;

        drop(writer);
    });

    Ok(reader)
}

// ── Non-streaming generate ─────────────────────────────────────────────

/// Send a non-streaming chat completion request to the Z.AI API.
pub async fn generate(
    client: &reqwest::Client,
    request: &LlmRequest,
) -> Result<GenerateResponse, LlmError> {
    let base_url = resolve_base_url(request);
    let url = format!("{base_url}/chat/completions");
    let headers = build_headers(request);
    let body = build_chat_body(request, false);

    let start = Instant::now();

    let response = client
        .post(&url)
        .headers(headers)
        .json(&body)
        .send()
        .await?;

    let response = super::check_response(response).await?;

    let total_ms = start.elapsed().as_millis() as u32;

    let resp_body: Value = response.json().await.map_err(LlmError::Request)?;

    let choice = resp_body.get("choices").and_then(|c| c.get(0));
    let message = choice.and_then(|c| c.get("message"));

    // Build content blocks.
    let mut typed_blocks: Vec<ContentBlock> = Vec::new();

    // Reasoning / thinking (Z.AI uses `reasoning_content`).
    if let Some(reasoning) = message
        .and_then(|m| m.get("reasoning_content"))
        .and_then(|r| r.as_str())
    {
        if !reasoning.is_empty() {
            typed_blocks.push(ContentBlock::Thinking {
                thinking: reasoning.to_string(),
                signature: None,
            });
        }
    }

    // Text content.
    let text_content = message
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    if !text_content.is_empty() {
        typed_blocks.push(ContentBlock::Text {
            text: text_content.to_string(),
        });
    }

    // Tool calls.
    if let Some(tcs) = message
        .and_then(|m| m.get("tool_calls"))
        .and_then(|t| t.as_array())
    {
        for tc in tcs {
            let tc_type = tc.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if tc_type != "function" {
                continue;
            }
            let id = tc
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or("")
                .to_string();
            let func = tc.get("function");
            let name = func
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let args_str = func
                .and_then(|f| f.get("arguments"))
                .and_then(|a| a.as_str())
                .unwrap_or("{}");
            let input: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
            typed_blocks.push(ContentBlock::ToolUse { id, name, input });
        }
    }

    let finish_reason_raw = choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(|r| r.as_str());

    let usage = resp_body
        .get("usage")
        .map(extract_openai_usage)
        .unwrap_or_default();

    let resp_model = resp_body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or(&request.model);

    Ok(GenerateResponse {
        content: text_content.to_string(),
        content_blocks: typed_blocks,
        finish_reason: normalize_finish_reason(finish_reason_raw).to_string(),
        usage,
        timing: Timing {
            total_ms,
            time_to_first_token_ms: total_ms,
        },
        model: resp_model.to_string(),
    })
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use shore_config::models::Sdk;

    fn test_request() -> LlmRequest {
        LlmRequest {
            sdk: Sdk::Zai,
            model: "glm-5".into(),
            api_key: "test-key".into(),
            api_key_name: None,
            base_url: None,
            messages: vec![],
            system: None,
            tools: None,
            max_tokens: 4096,
            temperature: Some(0.7),
            top_p: None,
            provider_options: None,
            provider_key: Some("zai".into()),
            rid: None,
            forensic_character: None,
            system_suffix: None,
            retain_long: false,
        }
    }

    // ── resolve_base_url ──────────────────────────────────────────

    #[test]
    fn resolve_base_url_default() {
        let req = test_request();
        assert_eq!(resolve_base_url(&req), ZAI_BASE_URL);
    }

    #[test]
    fn resolve_base_url_subscription() {
        let mut req = test_request();
        req.provider_options = Some(json!({"zai_subscription": true}));
        assert_eq!(resolve_base_url(&req), ZAI_CODING_BASE_URL);
    }

    #[test]
    fn resolve_base_url_explicit_override() {
        let mut req = test_request();
        req.base_url = Some("https://custom.example.com/v4".into());
        req.provider_options = Some(json!({"zai_subscription": true}));
        // Explicit base_url wins over subscription toggle.
        assert_eq!(resolve_base_url(&req), "https://custom.example.com/v4");
    }

    // ── translate_messages ────────────────────────────────────────

    #[test]
    fn translate_messages_basic() {
        let mut req = test_request();
        req.system = Some(json!("You are helpful."));
        req.messages = vec![
            json!({"role": "user", "content": "Hello"}),
            json!({"role": "assistant", "content": "Hi there!"}),
        ];
        let msgs = translate_messages(&req);
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "You are helpful.");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[2]["content"], "Hi there!");
    }

    #[test]
    fn translate_messages_assistant_thinking_clear_true() {
        let mut req = test_request();
        req.provider_options = Some(json!({"zai_clear_thinking": true}));
        req.messages = vec![json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "let me reason..."},
                {"type": "text", "text": "The answer is 42."},
            ]
        })];

        let msgs = translate_messages(&req);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["content"], "The answer is 42.");
        // reasoning_content should NOT be present when clear_thinking is true.
        assert!(msgs[0].get("reasoning_content").is_none());
    }

    #[test]
    fn translate_messages_assistant_thinking_clear_false() {
        let mut req = test_request();
        req.messages = vec![json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "let me reason..."},
                {"type": "text", "text": "The answer is 42."},
            ]
        })];

        let msgs = translate_messages(&req);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["content"], "The answer is 42.");
        assert_eq!(msgs[0]["reasoning_content"], "let me reason...");
    }

    #[test]
    fn translate_messages_assistant_with_tool_use() {
        let mut req = test_request();
        req.messages = vec![json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Let me check."},
                {
                    "type": "tool_use",
                    "id": "call_1",
                    "name": "get_weather",
                    "input": {"city": "Tokyo"},
                },
            ]
        })];

        let msgs = translate_messages(&req);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["content"], "Let me check.");
        let tc = &msgs[0]["tool_calls"][0];
        assert_eq!(tc["id"], "call_1");
        assert_eq!(tc["type"], "function");
        assert_eq!(tc["function"]["name"], "get_weather");
    }

    #[test]
    fn translate_messages_user_with_tool_result() {
        let mut req = test_request();
        req.messages = vec![json!({
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "call_1",
                    "content": "Sunny, 25°C",
                },
                {"type": "text", "text": "What about tomorrow?"},
            ]
        })];

        let msgs = translate_messages(&req);
        assert_eq!(msgs.len(), 2);
        // Tool result first.
        assert_eq!(msgs[0]["role"], "tool");
        assert_eq!(msgs[0]["tool_call_id"], "call_1");
        assert_eq!(msgs[0]["content"], "Sunny, 25°C");
        // Remaining user text.
        assert_eq!(msgs[1]["role"], "user");
    }

    /// Z.AI accepts raw `role: "system"` mid-history; the OpenAI inline
    /// system wrapper must not kick in here. This pins the
    /// `wrap_inline_system = false` provider context flag for `zai`.
    #[test]
    fn translate_messages_zai_passes_inline_system_through_raw() {
        let mut req = test_request();
        req.messages = vec![
            json!({"role": "user", "content": "first"}),
            json!({"role": "system", "content": "behave"}),
            json!({"role": "user", "content": "second"}),
        ];
        let msgs = translate_messages(&req);
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1]["role"], "system");
        assert_eq!(msgs[1]["content"], "behave");
        assert!(
            !msgs[1]["content"]
                .as_str()
                .unwrap()
                .contains("<system_instruction>"),
            "zai should not wrap inline system messages"
        );
    }

    // ── translate_tools ──────────────────────────────────────────

    #[test]
    fn translate_tools_wraps_in_function_envelope() {
        let tools = Some(vec![json!({
            "name": "get_time",
            "description": "Get current time",
            "input_schema": {"type": "object", "properties": {}},
        })]);
        let result = translate_tools(&tools).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["type"], "function");
        assert_eq!(result[0]["function"]["name"], "get_time");
        assert_eq!(result[0]["function"]["description"], "Get current time");
    }

    #[test]
    fn translate_tools_none_input() {
        assert!(translate_tools(&None).is_none());
    }

    // ── build_chat_body ──────────────────────────────────────────

    #[test]
    fn build_chat_body_includes_thinking_params() {
        let req = test_request();
        let body = build_chat_body(&req, false);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["clear_thinking"], false);
        assert_eq!(body["model"], "glm-5");
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn build_chat_body_streaming_includes_usage_option() {
        let req = test_request();
        let body = build_chat_body(&req, true);
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn build_chat_body_clear_thinking_from_options() {
        let mut req = test_request();
        req.provider_options = Some(json!({"zai_clear_thinking": true}));
        let body = build_chat_body(&req, false);
        assert_eq!(body["clear_thinking"], true);
    }

    /// The first SSE chunk often carries both the model name AND the first
    /// content delta. The streaming callback must emit both the `start` event
    /// and the text from that first chunk. Previously the callback returned
    /// early after emitting `start`, silently dropping the first token.
    #[test]
    fn first_chunk_content_not_dropped() {
        use crate::providers::stream_helpers::build_start_event;

        let mut start_sent = false;
        let mut model = "glm-5".to_string();
        let mut text_content = String::new();
        let mut timing = StreamTiming::new();

        // First SSE chunk carries model AND a content delta.
        let first_chunk = json!({
            "model": "glm-5-plus",
            "choices": [{
                "index": 0,
                "delta": { "content": "Hello" }
            }]
        });

        let data = serde_json::to_string(&first_chunk).unwrap();
        let chunk: Value = serde_json::from_str(&data).unwrap();

        let mut lines_out: Vec<String> = Vec::new();

        // Emit start event on first chunk (no early return).
        if !start_sent {
            if let Some(m) = chunk.get("model").and_then(|m| m.as_str()) {
                model = m.to_string();
            }
            start_sent = true;
            lines_out.push(build_start_event(&model));
        }

        // Process content from the same chunk.
        let choice = chunk.get("choices").and_then(|c| c.get(0));
        if let Some(choice) = choice {
            let delta = choice.get("delta");
            if let Some(content) = delta
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
            {
                if !content.is_empty() {
                    timing.record_first_token();
                    text_content.push_str(content);
                    let ev = json!({"type": "text", "text": content});
                    if let Ok(line) = serde_json::to_string(&ev) {
                        lines_out.push(line);
                    }
                }
            }
        }

        assert!(start_sent, "Expected first chunk to emit a start event");
        assert_eq!(lines_out.len(), 2, "Expected start + text events");
        let start_ev: Value = serde_json::from_str(&lines_out[0]).unwrap();
        assert_eq!(start_ev["type"], "start");
        assert_eq!(start_ev["model"], "glm-5-plus");
        let text_ev: Value = serde_json::from_str(&lines_out[1]).unwrap();
        assert_eq!(text_ev["type"], "text");
        assert_eq!(text_ev["text"], "Hello");

        assert_eq!(
            text_content, "Hello",
            "First chunk's content delta was dropped — text_content is empty"
        );
    }
}
