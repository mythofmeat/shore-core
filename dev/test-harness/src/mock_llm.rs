use serde_json::{json, Value};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

// ── SSE helpers ──────────────────────────────────────────────────────────────

fn sse_event(event_type: &str, data: &Value) -> String {
    format!("event: {}\ndata: {}\n\n", event_type, data)
}

// ── ContentBlock ─────────────────────────────────────────────────────────────

enum ContentBlock {
    Text(String),
    Thinking {
        thinking: String,
        signature: Option<String>,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

// ── AnthropicStreamBuilder ───────────────────────────────────────────────────

pub struct AnthropicStreamBuilder {
    content_blocks: Vec<ContentBlock>,
    input_tokens: u32,
    output_tokens: u32,
    cache_read_input_tokens: u32,
    cache_creation_input_tokens: u32,
    model: String,
    stop_reason: String,
}

impl AnthropicStreamBuilder {
    pub fn new() -> Self {
        Self {
            content_blocks: Vec::new(),
            input_tokens: 10,
            output_tokens: 20,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            model: "claude-3-5-sonnet-20241022".to_string(),
            stop_reason: "end_turn".to_string(),
        }
    }

    pub fn text(mut self, t: &str) -> Self {
        self.content_blocks.push(ContentBlock::Text(t.to_string()));
        self
    }

    /// Emit a `thinking` block. Pass `None` for `signature` to skip the
    /// signature_delta event (matches what providers return when the
    /// signature is empty/absent).
    pub fn thinking(mut self, thinking: &str, signature: Option<&str>) -> Self {
        self.content_blocks.push(ContentBlock::Thinking {
            thinking: thinking.to_string(),
            signature: signature.map(String::from),
        });
        self
    }

    /// Emit a `redacted_thinking` block. The `data` is opaque to the
    /// adapter; OpenRouter relays signed reasoning content through these
    /// with a `openrouter.reasoning:` prefix.
    pub fn redacted_thinking(mut self, data: &str) -> Self {
        self.content_blocks.push(ContentBlock::RedactedThinking {
            data: data.to_string(),
        });
        self
    }

    pub fn tool_use(mut self, id: &str, name: &str, input: Value) -> Self {
        self.stop_reason = "tool_use".to_string();
        self.content_blocks.push(ContentBlock::ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input,
        });
        self
    }

    pub fn usage(mut self, input: u32, output: u32) -> Self {
        self.input_tokens = input;
        self.output_tokens = output;
        self
    }

    /// Set cache usage fields. `read` corresponds to
    /// `cache_read_input_tokens`, `creation` to
    /// `cache_creation_input_tokens` on Anthropic's wire shape.
    pub fn cache_usage(mut self, read: u32, creation: u32) -> Self {
        self.cache_read_input_tokens = read;
        self.cache_creation_input_tokens = creation;
        self
    }

    pub fn model(mut self, m: &str) -> Self {
        self.model = m.to_string();
        self
    }

    pub fn stop_reason(mut self, r: &str) -> Self {
        self.stop_reason = r.to_string();
        self
    }

    pub fn build(self) -> String {
        let mut out = String::new();

        // message_start
        out.push_str(&sse_event(
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": "msg_test01",
                    "type": "message",
                    "role": "assistant",
                    "model": self.model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": self.input_tokens,
                        "output_tokens": 0,
                        "cache_read_input_tokens": self.cache_read_input_tokens,
                        "cache_creation_input_tokens": self.cache_creation_input_tokens,
                    }
                }
            }),
        ));

        for (idx, block) in self.content_blocks.iter().enumerate() {
            let index = idx as u64;
            match block {
                ContentBlock::Thinking {
                    thinking,
                    signature,
                } => {
                    out.push_str(&sse_event(
                        "content_block_start",
                        &json!({
                            "type": "content_block_start",
                            "index": index,
                            "content_block": { "type": "thinking", "thinking": "", "signature": "" }
                        }),
                    ));
                    out.push_str(&sse_event(
                        "content_block_delta",
                        &json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": { "type": "thinking_delta", "thinking": thinking }
                        }),
                    ));
                    if let Some(sig) = signature {
                        out.push_str(&sse_event(
                            "content_block_delta",
                            &json!({
                                "type": "content_block_delta",
                                "index": index,
                                "delta": { "type": "signature_delta", "signature": sig }
                            }),
                        ));
                    }
                    out.push_str(&sse_event(
                        "content_block_stop",
                        &json!({ "type": "content_block_stop", "index": index }),
                    ));
                }
                ContentBlock::RedactedThinking { data } => {
                    out.push_str(&sse_event(
                        "content_block_start",
                        &json!({
                            "type": "content_block_start",
                            "index": index,
                            "content_block": { "type": "redacted_thinking", "data": data }
                        }),
                    ));
                    out.push_str(&sse_event(
                        "content_block_stop",
                        &json!({ "type": "content_block_stop", "index": index }),
                    ));
                }
                ContentBlock::Text(text) => {
                    // content_block_start
                    out.push_str(&sse_event(
                        "content_block_start",
                        &json!({
                            "type": "content_block_start",
                            "index": index,
                            "content_block": { "type": "text", "text": "" }
                        }),
                    ));

                    // content_block_delta
                    out.push_str(&sse_event(
                        "content_block_delta",
                        &json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": { "type": "text_delta", "text": text }
                        }),
                    ));

                    // content_block_stop
                    out.push_str(&sse_event(
                        "content_block_stop",
                        &json!({
                            "type": "content_block_stop",
                            "index": index
                        }),
                    ));
                }
                ContentBlock::ToolUse { id, name, input } => {
                    // content_block_start
                    out.push_str(&sse_event(
                        "content_block_start",
                        &json!({
                            "type": "content_block_start",
                            "index": index,
                            "content_block": {
                                "type": "tool_use",
                                "id": id,
                                "name": name,
                                "input": {}
                            }
                        }),
                    ));

                    // content_block_delta — send entire input as one chunk
                    let input_json = input.to_string();
                    out.push_str(&sse_event(
                        "content_block_delta",
                        &json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": {
                                "type": "input_json_delta",
                                "partial_json": input_json
                            }
                        }),
                    ));

                    // content_block_stop
                    out.push_str(&sse_event(
                        "content_block_stop",
                        &json!({
                            "type": "content_block_stop",
                            "index": index
                        }),
                    ));
                }
            }
        }

        // message_delta
        out.push_str(&sse_event(
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": self.stop_reason,
                    "stop_sequence": null
                },
                "usage": {
                    "output_tokens": self.output_tokens,
                    "cache_read_input_tokens": self.cache_read_input_tokens,
                    "cache_creation_input_tokens": self.cache_creation_input_tokens,
                }
            }),
        ));

        // message_stop
        out.push_str(&sse_event(
            "message_stop",
            &json!({ "type": "message_stop" }),
        ));

        out
    }
}

impl Default for AnthropicStreamBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ── AnthropicJsonBuilder ─────────────────────────────────────────────────────

/// Build the non-streaming Anthropic JSON response shape (the body
/// returned by `POST /v1/messages` without `stream: true`). Lets tests
/// exercise `LlmClient::generate` against the mock with a structured
/// response that includes thinking/redacted_thinking blocks.
pub struct AnthropicJsonBuilder {
    content_blocks: Vec<ContentBlock>,
    input_tokens: u32,
    output_tokens: u32,
    cache_read_input_tokens: u32,
    cache_creation_input_tokens: u32,
    model: String,
    stop_reason: String,
}

impl AnthropicJsonBuilder {
    pub fn new() -> Self {
        Self {
            content_blocks: Vec::new(),
            input_tokens: 10,
            output_tokens: 20,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            model: "claude-3-5-sonnet-20241022".to_string(),
            stop_reason: "end_turn".to_string(),
        }
    }

    pub fn text(mut self, t: &str) -> Self {
        self.content_blocks.push(ContentBlock::Text(t.to_string()));
        self
    }

    pub fn thinking(mut self, thinking: &str, signature: Option<&str>) -> Self {
        self.content_blocks.push(ContentBlock::Thinking {
            thinking: thinking.to_string(),
            signature: signature.map(String::from),
        });
        self
    }

    pub fn redacted_thinking(mut self, data: &str) -> Self {
        self.content_blocks.push(ContentBlock::RedactedThinking {
            data: data.to_string(),
        });
        self
    }

    pub fn tool_use(mut self, id: &str, name: &str, input: Value) -> Self {
        self.stop_reason = "tool_use".to_string();
        self.content_blocks.push(ContentBlock::ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input,
        });
        self
    }

    pub fn usage(mut self, input: u32, output: u32) -> Self {
        self.input_tokens = input;
        self.output_tokens = output;
        self
    }

    pub fn cache_usage(mut self, read: u32, creation: u32) -> Self {
        self.cache_read_input_tokens = read;
        self.cache_creation_input_tokens = creation;
        self
    }

    pub fn model(mut self, m: &str) -> Self {
        self.model = m.to_string();
        self
    }

    pub fn stop_reason(mut self, r: &str) -> Self {
        self.stop_reason = r.to_string();
        self
    }

    pub fn build(self) -> Value {
        let content: Vec<Value> = self
            .content_blocks
            .iter()
            .map(|b| match b {
                ContentBlock::Text(text) => json!({"type": "text", "text": text}),
                ContentBlock::Thinking {
                    thinking,
                    signature,
                } => {
                    let mut v = json!({"type": "thinking", "thinking": thinking});
                    if let Some(sig) = signature {
                        v["signature"] = json!(sig);
                    }
                    v
                }
                ContentBlock::RedactedThinking { data } => {
                    json!({"type": "redacted_thinking", "data": data})
                }
                ContentBlock::ToolUse { id, name, input } => json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": input,
                }),
            })
            .collect();
        json!({
            "id": "msg_test_json",
            "type": "message",
            "role": "assistant",
            "model": self.model,
            "content": content,
            "stop_reason": self.stop_reason,
            "stop_sequence": null,
            "usage": {
                "input_tokens": self.input_tokens,
                "output_tokens": self.output_tokens,
                "cache_read_input_tokens": self.cache_read_input_tokens,
                "cache_creation_input_tokens": self.cache_creation_input_tokens,
            }
        })
    }
}

impl Default for AnthropicJsonBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ── OpenAiResponseBuilder ────────────────────────────────────────────────────

/// Build OpenAI-compatible `chat/completions` responses (streaming and
/// non-streaming). Used for adapter wire tests against the OpenAI path
/// (DeepSeek, xAI, OpenRouter→OpenAI models, etc.).
pub struct OpenAiResponseBuilder {
    text: String,
    reasoning: Option<String>,
    tool_calls: Vec<(String, String, Value)>,
    finish_reason: String,
    model: String,
    prompt_tokens: u32,
    completion_tokens: u32,
    cached_tokens: u32,
}

impl OpenAiResponseBuilder {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            reasoning: None,
            tool_calls: Vec::new(),
            finish_reason: "stop".to_string(),
            model: "gpt-test".to_string(),
            prompt_tokens: 10,
            completion_tokens: 5,
            cached_tokens: 0,
        }
    }

    pub fn text(mut self, t: &str) -> Self {
        self.text = t.to_string();
        self
    }

    pub fn reasoning(mut self, r: &str) -> Self {
        self.reasoning = Some(r.to_string());
        self
    }

    pub fn tool_call(mut self, id: &str, name: &str, args: Value) -> Self {
        self.tool_calls
            .push((id.to_string(), name.to_string(), args));
        self.finish_reason = "tool_calls".to_string();
        self
    }

    pub fn finish_reason(mut self, r: &str) -> Self {
        self.finish_reason = r.to_string();
        self
    }

    pub fn model(mut self, m: &str) -> Self {
        self.model = m.to_string();
        self
    }

    pub fn usage(mut self, prompt: u32, completion: u32, cached: u32) -> Self {
        self.prompt_tokens = prompt;
        self.completion_tokens = completion;
        self.cached_tokens = cached;
        self
    }

    /// Build a single non-streaming ChatCompletion JSON body.
    pub fn build_json(&self) -> Value {
        let tool_calls: Vec<Value> = self
            .tool_calls
            .iter()
            .map(|(id, name, args)| {
                json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(args).unwrap_or_else(|_| "{}".into()),
                    }
                })
            })
            .collect();
        let mut message = json!({"role": "assistant", "content": self.text});
        if !tool_calls.is_empty() {
            message["tool_calls"] = Value::Array(tool_calls);
        }
        if let Some(r) = &self.reasoning {
            message["reasoning"] = json!(r);
        }
        json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "created": 1_778_284_800u64,
            "model": self.model,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": self.finish_reason,
            }],
            "usage": {
                "prompt_tokens": self.prompt_tokens,
                "completion_tokens": self.completion_tokens,
                "total_tokens": self.prompt_tokens + self.completion_tokens,
                "prompt_tokens_details": {
                    "cached_tokens": self.cached_tokens,
                },
            }
        })
    }

    /// Build the streaming SSE chunks for the same response.
    pub fn build_sse(&self) -> String {
        let id = "chatcmpl-test";
        let created: u64 = 1_778_284_800;
        let mut out = String::new();

        let mut delta = json!({"role": "assistant"});
        if !self.text.is_empty() {
            delta["content"] = json!(self.text);
        }
        if let Some(r) = &self.reasoning {
            delta["reasoning"] = json!(r);
        }
        if !self.tool_calls.is_empty() {
            delta["tool_calls"] = Value::Array(
                self.tool_calls
                    .iter()
                    .enumerate()
                    .map(|(i, (id, name, args))| {
                        json!({
                            "index": i,
                            "id": id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": serde_json::to_string(args)
                                    .unwrap_or_else(|_| "{}".into()),
                            }
                        })
                    })
                    .collect(),
            );
        }

        out.push_str(&format!(
            "data: {}\n\n",
            json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": self.model,
                "choices": [{
                    "index": 0,
                    "delta": delta,
                    "finish_reason": Value::Null,
                }],
            })
        ));
        out.push_str(&format!(
            "data: {}\n\n",
            json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": self.model,
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": self.finish_reason,
                }],
                "usage": {
                    "prompt_tokens": self.prompt_tokens,
                    "completion_tokens": self.completion_tokens,
                    "total_tokens": self.prompt_tokens + self.completion_tokens,
                    "prompt_tokens_details": {"cached_tokens": self.cached_tokens},
                },
            })
        ));
        out.push_str("data: [DONE]\n\n");
        out
    }
}

impl Default for OpenAiResponseBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ── assertion helpers ────────────────────────────────────────────────────────

/// Collect dotted paths in a JSON value where `cache_control` is present.
/// Lets tests assert the exact 4-breakpoint slot set without caring about
/// every other field. Array indices are formatted as `[N]` so paths are
/// unambiguous (e.g. `system[1]`, `messages[3].content[2]`).
pub fn find_cache_control_paths(body: &Value) -> Vec<String> {
    fn walk(v: &Value, base: &str, out: &mut Vec<String>) {
        match v {
            Value::Array(arr) => {
                for (i, item) in arr.iter().enumerate() {
                    let next = format!("{base}[{i}]");
                    walk(item, &next, out);
                }
            }
            Value::Object(map) => {
                for (k, val) in map.iter() {
                    if k == "cache_control" {
                        out.push(base.to_string());
                        continue;
                    }
                    let next = if base.is_empty() {
                        k.clone()
                    } else {
                        format!("{base}.{k}")
                    };
                    walk(val, &next, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(body, "", &mut out);
    out
}

// ── SseResponder ─────────────────────────────────────────────────────────────

/// A wiremock `Respond` impl that returns a pre-built SSE body.
struct SseResponder(String);

impl Respond for SseResponder {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(self.0.clone())
    }
}

struct ErrorResponder {
    status: u16,
    body: String,
}

impl Respond for ErrorResponder {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        ResponseTemplate::new(self.status).set_body_string(self.body.clone())
    }
}

struct EmbeddingResponder {
    dimensions: usize,
}

impl Respond for EmbeddingResponder {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or_else(|_| json!({}));
        let input_count = match body.get("input") {
            Some(Value::Array(inputs)) => inputs.len(),
            Some(Value::String(_)) => 1,
            _ => 0,
        };
        let zeros: Vec<f64> = vec![0.0; self.dimensions];
        let data: Vec<Value> = (0..input_count)
            .map(|idx| {
                json!({
                    "object": "embedding",
                    "index": idx,
                    "embedding": zeros.clone(),
                })
            })
            .collect();
        let response = json!({
            "object": "list",
            "data": data,
            "model": "text-embedding-3-small",
            "usage": { "prompt_tokens": 8, "total_tokens": 8 }
        });
        ResponseTemplate::new(200).set_body_json(&response)
    }
}

// ── MockLlmServer ────────────────────────────────────────────────────────────

pub struct MockLlmServer {
    server: MockServer,
}

impl MockLlmServer {
    pub async fn start() -> Self {
        Self {
            server: MockServer::start().await,
        }
    }

    pub fn base_url(&self) -> String {
        self.server.uri()
    }

    pub async fn enqueue_text(&self, text: &str) {
        let body = AnthropicStreamBuilder::new().text(text).build();
        self.enqueue_raw_sse(body).await;
    }

    pub async fn enqueue_tool_use(&self, id: &str, name: &str, input: Value) {
        let body = AnthropicStreamBuilder::new()
            .tool_use(id, name, input)
            .build();
        self.enqueue_raw_sse(body).await;
    }

    pub async fn enqueue_raw_sse(&self, body: String) {
        Mock::given(method("POST"))
            .and(path_regex("/v1/messages"))
            .respond_with(SseResponder(body))
            .up_to_n_times(1)
            .expect(1)
            .mount(&self.server)
            .await;
    }

    pub async fn enqueue_error(&self, status: u16, body: &str) {
        Mock::given(method("POST"))
            .and(path_regex("/v1/messages"))
            .respond_with(ErrorResponder {
                status,
                body: body.to_string(),
            })
            .up_to_n_times(1)
            .expect(1)
            .mount(&self.server)
            .await;
    }

    /// Enqueue a non-streaming JSON response containing a `tool_use` block
    /// (used by `generate()` — e.g. the heartbeat tick loop).
    pub async fn enqueue_json_tool_use(&self, id: &str, name: &str, input: Value) {
        let body = json!({
            "id": "msg_test_json_tool",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-5-sonnet-20241022",
            "content": [{
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            }],
            "stop_reason": "tool_use",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 8
            }
        });
        Mock::given(method("POST"))
            .and(path_regex("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .up_to_n_times(1)
            .expect(1)
            .mount(&self.server)
            .await;
    }

    /// Enqueue a non-streaming JSON response (used by `generate()`, e.g. keepalive pings).
    pub async fn enqueue_json_text(&self, text: &str) {
        let body = json!({
            "id": "msg_test_json",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-5-sonnet-20241022",
            "content": [{ "type": "text", "text": text }],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 8
            }
        });
        Mock::given(method("POST"))
            .and(path_regex("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .up_to_n_times(1)
            .expect(1)
            .mount(&self.server)
            .await;
    }

    /// Enqueue the two non-streaming JSON responses a compaction tool
    /// loop needs to write one memory file end-to-end:
    ///
    /// 1. A `tool_use` round calling `write(path, content)`.
    /// 2. An `end_turn` round with a short text summary, terminating the
    ///    loop after the daemon dispatched the write and sent back the
    ///    `tool_result`.
    ///
    /// Both are mounted with `up_to_n_times(1)` and no `.expect(...)`, so
    /// callers don't fail when retrieval/index-rebuild paths happen to
    /// consume them in a different order.
    pub async fn enqueue_json_compaction_write_optional(&self, path: &str, content: &str) {
        let tool_use_body = json!({
            "id": "msg_compaction_write_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-5-sonnet-20241022",
            "content": [{
                "type": "tool_use",
                "id": "call_compact_write",
                "name": "write",
                "input": {
                    "path": path,
                    "content": content,
                },
            }],
            "stop_reason": "tool_use",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 12,
                "output_tokens": 8,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 4
            }
        });
        let end_body = json!({
            "id": "msg_compaction_write_2",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-5-sonnet-20241022",
            "content": [{ "type": "text", "text": "memory written" }],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 4,
                "output_tokens": 4,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 12
            }
        });
        Mock::given(method("POST"))
            .and(path_regex("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&tool_use_body))
            .up_to_n_times(1)
            .mount(&self.server)
            .await;
        Mock::given(method("POST"))
            .and(path_regex("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&end_body))
            .up_to_n_times(1)
            .mount(&self.server)
            .await;
    }

    /// Enqueue a non-streaming JSON response without strict expectation
    /// (won't panic if not consumed).
    pub async fn enqueue_json_text_optional(&self, text: &str) {
        let body = json!({
            "id": "msg_test_json",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-5-sonnet-20241022",
            "content": [{ "type": "text", "text": text }],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 8
            }
        });
        Mock::given(method("POST"))
            .and(path_regex("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .up_to_n_times(1)
            .mount(&self.server)
            .await;
    }

    /// Enqueue an error response without strict expectation.
    pub async fn enqueue_error_optional(&self, status: u16, body: &str) {
        Mock::given(method("POST"))
            .and(path_regex("/v1/messages"))
            .respond_with(ErrorResponder {
                status,
                body: body.to_string(),
            })
            .up_to_n_times(1)
            .mount(&self.server)
            .await;
    }

    pub async fn enqueue_hanging(&self) {
        Mock::given(method("POST"))
            .and(path_regex("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_delay(std::time::Duration::from_secs(3600)),
            )
            .up_to_n_times(1)
            .expect(1)
            .mount(&self.server)
            .await;
    }

    /// Like `enqueue_hanging` but without strict expectation — won't panic if
    /// the request is never made (e.g. because generation was aborted).
    pub async fn enqueue_hanging_optional(&self) {
        Mock::given(method("POST"))
            .and(path_regex("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_delay(std::time::Duration::from_secs(3600)),
            )
            .up_to_n_times(1)
            .mount(&self.server)
            .await;
    }

    /// Enqueue a text response without strict expectation (won't panic if not consumed).
    pub async fn enqueue_text_optional(&self, text: &str) {
        let body = AnthropicStreamBuilder::new().text(text).build();
        Mock::given(method("POST"))
            .and(path_regex("/v1/messages"))
            .respond_with(SseResponder(body))
            .up_to_n_times(1)
            .mount(&self.server)
            .await;
    }

    /// Enqueue an OpenAI-format embedding response (optional — won't panic if unused).
    ///
    /// Returns one embedding vector of `dimensions` zeros per requested input.
    /// Matches POST requests to `/embeddings` (the OpenAI-compatible embedding
    /// endpoint called by `shore-llm`'s `embed()` function).
    pub async fn enqueue_embedding_optional(&self, dimensions: usize) {
        Mock::given(method("POST"))
            .and(path_regex("/embeddings"))
            .respond_with(EmbeddingResponder { dimensions })
            .up_to_n_times(100)
            .mount(&self.server)
            .await;
    }

    pub async fn received_requests(&self) -> Vec<Value> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| serde_json::from_slice(&r.body).ok())
            .collect()
    }

    /// Enqueue a streaming Anthropic response from a prebuilt
    /// [`AnthropicStreamBuilder`]. Lets wire tests stage multi-block
    /// responses (thinking → tool_use, etc.) without juggling raw SSE.
    pub async fn enqueue_stream(&self, builder: AnthropicStreamBuilder) {
        self.enqueue_raw_sse(builder.build()).await;
    }

    /// Enqueue a non-streaming Anthropic JSON response from a prebuilt
    /// [`AnthropicJsonBuilder`].
    pub async fn enqueue_json(&self, builder: AnthropicJsonBuilder) {
        let body = builder.build();
        Mock::given(method("POST"))
            .and(path_regex("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .up_to_n_times(1)
            .expect(1)
            .mount(&self.server)
            .await;
    }

    /// Enqueue a non-streaming OpenAI-compatible chat/completions response.
    pub async fn enqueue_openai_json(&self, builder: OpenAiResponseBuilder) {
        let body = builder.build_json();
        Mock::given(method("POST"))
            .and(path_regex("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .up_to_n_times(1)
            .expect(1)
            .mount(&self.server)
            .await;
    }

    /// Enqueue a streaming OpenAI-compatible chat/completions response.
    pub async fn enqueue_openai_stream(&self, builder: OpenAiResponseBuilder) {
        let body = builder.build_sse();
        Mock::given(method("POST"))
            .and(path_regex("/chat/completions"))
            .respond_with(SseResponder(body))
            .up_to_n_times(1)
            .expect(1)
            .mount(&self.server)
            .await;
    }

    /// Like [`received_requests`], but returns paths too so callers can
    /// distinguish `/v1/messages` from `/chat/completions` etc. when a
    /// test exercises both adapters against the same mock.
    pub async fn received_requests_with_path(&self) -> Vec<(String, Value)> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| {
                let body: Value = serde_json::from_slice(&r.body).ok()?;
                Some((r.url.path().to_string(), body))
            })
            .collect()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_builder_text_response() {
        let body = AnthropicStreamBuilder::new()
            .text("Hello, world!")
            .usage(5, 10)
            .build();

        // All required event types must appear
        assert!(
            body.contains("event: message_start\n"),
            "missing message_start"
        );
        assert!(
            body.contains("event: content_block_start\n"),
            "missing content_block_start"
        );
        assert!(
            body.contains("event: content_block_delta\n"),
            "missing content_block_delta"
        );
        assert!(
            body.contains("event: content_block_stop\n"),
            "missing content_block_stop"
        );
        assert!(
            body.contains("event: message_delta\n"),
            "missing message_delta"
        );
        assert!(
            body.contains("event: message_stop\n"),
            "missing message_stop"
        );

        // The text must be present
        assert!(body.contains("Hello, world!"), "missing text content");

        // text_delta type
        assert!(body.contains("text_delta"), "missing text_delta type");

        // stop_reason end_turn
        assert!(body.contains("end_turn"), "missing end_turn stop_reason");

        // Usage tokens
        assert!(
            body.contains("\"input_tokens\":5"),
            "missing input token count"
        );
        assert!(
            body.contains("\"output_tokens\":10"),
            "missing output token count"
        );
    }

    #[test]
    fn test_stream_builder_tool_use_response() {
        let input = json!({ "query": "weather in London" });
        let body = AnthropicStreamBuilder::new()
            .tool_use("toolu_01", "search", input.clone())
            .build();

        // All required event types must appear
        assert!(
            body.contains("event: message_start\n"),
            "missing message_start"
        );
        assert!(
            body.contains("event: content_block_start\n"),
            "missing content_block_start"
        );
        assert!(
            body.contains("event: content_block_delta\n"),
            "missing content_block_delta"
        );
        assert!(
            body.contains("event: content_block_stop\n"),
            "missing content_block_stop"
        );
        assert!(
            body.contains("event: message_delta\n"),
            "missing message_delta"
        );
        assert!(
            body.contains("event: message_stop\n"),
            "missing message_stop"
        );

        // tool_use block must have the id and name
        assert!(body.contains("\"toolu_01\""), "missing tool id");
        assert!(body.contains("\"search\""), "missing tool name");

        // input_json_delta type
        assert!(
            body.contains("input_json_delta"),
            "missing input_json_delta type"
        );

        // stop_reason must be tool_use
        assert!(
            body.contains("\"tool_use\""),
            "missing tool_use stop_reason"
        );

        // Input content must be present somewhere in the body
        assert!(
            body.contains("weather in London"),
            "missing tool input content"
        );
    }
}
