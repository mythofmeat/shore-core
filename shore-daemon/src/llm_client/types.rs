use serde::{Deserialize, Serialize};
pub use shore_protocol::types::ContentBlock;

/// Request body for shore-llm's POST /v1/stream and POST /v1/generate endpoints.
///
/// The daemon sends fully-resolved config per-request because shore-llm is
/// zero-config — it has no model database or API key storage.
#[derive(Debug, Clone, Serialize)]
pub struct LlmRequest {
    /// Provider identifier: "anthropic", "openai", "gemini", "openrouter", "zhipuai".
    pub provider: String,

    /// Provider's model identifier (e.g. "claude-sonnet-4-20250514").
    pub model: String,

    /// API key resolved from the environment variable specified in models.toml.
    pub api_key: String,

    /// Optional base URL override (for OpenAI-compatible providers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Conversation messages in LLM-native format.
    pub messages: Vec<serde_json::Value>,

    /// System prompt blocks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<serde_json::Value>,

    /// Tool definitions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,

    /// Maximum tokens to generate.
    pub max_tokens: u32,

    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,

    /// Nucleus sampling top-p.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,

    /// Provider-specific options (cache_ttl, thinking, budget_tokens, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_options: Option<serde_json::Value>,
}

/// Token usage counts from shore-llm's normalized response.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default)]
    pub cache_read_tokens: u32,
    #[serde(default)]
    pub cache_creation_tokens: u32,
}

/// Timing information from shore-llm's normalized response.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Timing {
    pub total_ms: u32,
    #[serde(default)]
    pub time_to_first_token_ms: u32,
}

/// A single event in shore-llm's newline-delimited JSON stream.
///
/// shore-llm emits these as one JSON object per line:
/// ```text
/// {"type":"start","model":"claude-sonnet-4-6"}
/// {"type":"text","text":"Hello"}
/// {"type":"thinking","text":"Let me consider..."}
/// {"type":"tool_use","id":"tool_01","name":"memory","input":{...}}
/// {"type":"done","content":"Hello","finish_reason":"end_turn","usage":{...},"timing":{...}}
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    Start {
        model: String,
    },
    Text {
        text: String,
    },
    Thinking {
        text: String,
    },
    ThinkingSignature {
        signature: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    Done {
        content: String,
        finish_reason: String,
        usage: Usage,
        timing: Timing,
    },
}

/// A tool_use event extracted from the stream for the engine's tool loop.
#[derive(Debug, Clone)]
pub struct ToolUseEvent {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// The accumulated result after a stream completes.
#[derive(Debug, Clone)]
pub struct StreamResult {
    /// The final assembled content from the "done" event.
    pub content: String,

    /// The model that produced this response.
    pub model: String,

    /// Why the model stopped generating.
    pub finish_reason: String,

    /// Token usage from the response.
    pub usage: Usage,

    /// Timing data from shore-llm.
    pub timing: Timing,

    /// Tool invocations encountered during the stream.
    pub tool_uses: Vec<ToolUseEvent>,

    /// Structured content blocks accumulated during streaming.
    ///
    /// Contains the full sequence of text, thinking, and tool_use blocks
    /// in the order they were received. Used for persistence.
    pub content_blocks: Vec<ContentBlock>,
}

/// Response from shore-llm's POST /v1/image/generate endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct ImageGenerateResponse {
    pub url: String,
    pub revised_prompt: String,
    pub timing: ImageGenerateTiming,
}

/// Timing for image generation.
#[derive(Debug, Clone, Deserialize)]
pub struct ImageGenerateTiming {
    pub total_ms: u32,
}

// ContentBlock is re-exported from shore_protocol::types::ContentBlock.

/// Non-streaming response from POST /v1/generate.
#[derive(Debug, Clone, Deserialize)]
pub struct GenerateResponse {
    pub content: String,
    #[serde(default)]
    pub content_blocks: Vec<ContentBlock>,
    pub finish_reason: String,
    pub usage: Usage,
    pub timing: Timing,
    pub model: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_request_omits_none_fields() {
        let req = LlmRequest {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-20250514".into(),
            api_key: "sk-test".into(),
            base_url: None,
            messages: vec![serde_json::json!({"role": "user", "content": "Hello"})],
            system: None,
            tools: None,
            max_tokens: 4096,
            temperature: Some(0.7),
            top_p: None,
            provider_options: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(!json.as_object().unwrap().contains_key("base_url"));
        assert!(!json.as_object().unwrap().contains_key("system"));
        assert!(!json.as_object().unwrap().contains_key("tools"));
        assert!(!json.as_object().unwrap().contains_key("top_p"));
        assert!(!json.as_object().unwrap().contains_key("provider_options"));
        assert_eq!(json["temperature"], 0.7);
        assert_eq!(json["max_tokens"], 4096);
    }

    #[test]
    fn deserialize_stream_start() {
        let json = r#"{"type":"start","model":"claude-sonnet-4-6"}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::Start { model } => assert_eq!(model, "claude-sonnet-4-6"),
            other => panic!("Expected Start, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_stream_text() {
        let json = r#"{"type":"text","text":"Hello world"}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::Text { text } => assert_eq!(text, "Hello world"),
            other => panic!("Expected Text, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_stream_thinking() {
        let json = r#"{"type":"thinking","text":"Let me consider..."}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::Thinking { text } => assert_eq!(text, "Let me consider..."),
            other => panic!("Expected Thinking, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_stream_tool_use() {
        let json = r#"{"type":"tool_use","id":"tool_01","name":"memory","input":{"q":"test"}}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::ToolUse { id, name, input } => {
                assert_eq!(id, "tool_01");
                assert_eq!(name, "memory");
                assert_eq!(input["q"], "test");
            }
            other => panic!("Expected ToolUse, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_stream_done() {
        let json = r#"{
            "type": "done",
            "content": "Hello there",
            "finish_reason": "end_turn",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_read_tokens": 80,
                "cache_creation_tokens": 20
            },
            "timing": {
                "total_ms": 1500,
                "time_to_first_token_ms": 200
            }
        }"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::Done {
                content,
                finish_reason,
                usage,
                timing,
            } => {
                assert_eq!(content, "Hello there");
                assert_eq!(finish_reason, "end_turn");
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.output_tokens, 50);
                assert_eq!(usage.cache_read_tokens, 80);
                assert_eq!(usage.cache_creation_tokens, 20);
                assert_eq!(timing.total_ms, 1500);
                assert_eq!(timing.time_to_first_token_ms, 200);
            }
            other => panic!("Expected Done, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_done_with_missing_cache_fields() {
        let json = r#"{
            "type": "done",
            "content": "Hi",
            "finish_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5},
            "timing": {"total_ms": 100}
        }"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::Done { usage, timing, .. } => {
                assert_eq!(usage.cache_read_tokens, 0);
                assert_eq!(usage.cache_creation_tokens, 0);
                assert_eq!(timing.time_to_first_token_ms, 0);
            }
            other => panic!("Expected Done, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_generate_response() {
        let json = r#"{
            "content": "Response text",
            "finish_reason": "end_turn",
            "usage": {"input_tokens": 50, "output_tokens": 25},
            "timing": {"total_ms": 800, "time_to_first_token_ms": 0},
            "model": "claude-sonnet-4-6"
        }"#;
        let resp: GenerateResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content, "Response text");
        assert_eq!(resp.model, "claude-sonnet-4-6");
        assert_eq!(resp.usage.input_tokens, 50);
        // content_blocks defaults to empty when absent.
        assert!(resp.content_blocks.is_empty());
    }

    #[test]
    fn deserialize_generate_response_with_content_blocks() {
        let json = r#"{
            "content": "I'll check the weather.",
            "content_blocks": [
                {"type": "text", "text": "I'll check the weather."},
                {"type": "tool_use", "id": "toolu_01", "name": "get_weather", "input": {"city": "NYC"}}
            ],
            "finish_reason": "tool_use",
            "usage": {"input_tokens": 50, "output_tokens": 25},
            "timing": {"total_ms": 800},
            "model": "claude-sonnet-4-6"
        }"#;
        let resp: GenerateResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.finish_reason, "tool_use");
        assert_eq!(resp.content_blocks.len(), 2);
        match &resp.content_blocks[0] {
            ContentBlock::Text { text } => assert_eq!(text, "I'll check the weather."),
            other => panic!("Expected Text, got {:?}", other),
        }
        match &resp.content_blocks[1] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_01");
                assert_eq!(name, "get_weather");
                assert_eq!(input["city"], "NYC");
            }
            other => panic!("Expected ToolUse, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_generate_response_with_thinking() {
        let json = r#"{
            "content": "The answer is 42.",
            "content_blocks": [
                {"type": "thinking", "thinking": "Let me think..."},
                {"type": "text", "text": "The answer is 42."}
            ],
            "finish_reason": "end_turn",
            "usage": {"input_tokens": 30, "output_tokens": 15},
            "timing": {"total_ms": 500},
            "model": "claude-sonnet-4-6"
        }"#;
        let resp: GenerateResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content_blocks.len(), 2);
        match &resp.content_blocks[0] {
            ContentBlock::Thinking { thinking, signature } => {
                assert_eq!(thinking, "Let me think...");
                assert!(signature.is_none());
            }
            other => panic!("Expected Thinking, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_stream_thinking_signature() {
        let json = r#"{"type":"thinking_signature","signature":"sig_abc123"}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::ThinkingSignature { signature } => {
                assert_eq!(signature, "sig_abc123");
            }
            other => panic!("Expected ThinkingSignature, got {:?}", other),
        }
    }
}
