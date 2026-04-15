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
    model: String,
    stop_reason: String,
}

impl AnthropicStreamBuilder {
    pub fn new() -> Self {
        Self {
            content_blocks: Vec::new(),
            input_tokens: 10,
            output_tokens: 20,
            model: "claude-3-5-sonnet-20241022".to_string(),
            stop_reason: "end_turn".to_string(),
        }
    }

    pub fn text(mut self, t: &str) -> Self {
        self.content_blocks.push(ContentBlock::Text(t.to_string()));
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
                        "output_tokens": 0
                    }
                }
            }),
        ));

        for (idx, block) in self.content_blocks.iter().enumerate() {
            let index = idx as u64;
            match block {
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
                    "output_tokens": self.output_tokens
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
    /// (used by `generate()` — e.g. the interiority tick loop).
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
    /// Returns a single embedding vector of `dimensions` zeros.  Matches
    /// POST requests to `/embeddings` (the OpenAI-compatible embedding endpoint
    /// called by `shore-llm-client`'s `embed()` function).
    pub async fn enqueue_embedding_optional(&self, dimensions: usize) {
        let zeros: Vec<f64> = vec![0.0; dimensions];
        let body = json!({
            "object": "list",
            "data": [{
                "object": "embedding",
                "index": 0,
                "embedding": zeros
            }],
            "model": "text-embedding-3-small",
            "usage": { "prompt_tokens": 8, "total_tokens": 8 }
        });
        Mock::given(method("POST"))
            .and(path_regex("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
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
