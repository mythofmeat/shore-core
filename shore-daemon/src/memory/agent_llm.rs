//! LLM abstraction for the memory agent loop.
//!
//! The `AgentLlm` trait decouples the agent from the concrete LLM transport,
//! enabling unit tests with canned responses via `MockAgentLlm`.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use serde_json::Value;

use shore_config::models::ResolvedModel;
use shore_llm_client::types::ContentBlock;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors from agent LLM calls.
#[derive(Debug)]
pub enum AgentLlmError {
    /// Transport/API error.
    Transport(String),
    /// No more canned responses in mock.
    MockExhausted,
}

impl std::fmt::Display for AgentLlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentLlmError::Transport(e) => write!(f, "llm transport: {e}"),
            AgentLlmError::MockExhausted => write!(f, "mock: no more canned responses"),
        }
    }
}

impl std::error::Error for AgentLlmError {}

// ---------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------

/// Normalized response from an agent LLM call.
#[derive(Debug, Clone)]
pub struct AgentLlmResponse {
    /// Concatenated text content (convenience — same as joining Text blocks).
    pub text: String,
    /// Full content blocks including tool_use, thinking, etc.
    pub content_blocks: Vec<ContentBlock>,
    /// Why the model stopped: "end_turn", "tool_use", etc.
    pub finish_reason: String,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Abstraction over LLM calls for the memory agent loop.
///
/// The memory agent's inner loop calls this to generate completions with tool
/// definitions. The trait is object-safe and async-friendly via boxed futures.
pub trait AgentLlm: Send + Sync {
    fn generate<'a>(
        &'a self,
        messages: Vec<Value>,
        system: Option<Value>,
        tools: Option<Vec<Value>>,
        model: &'a ResolvedModel,
    ) -> Pin<Box<dyn Future<Output = Result<AgentLlmResponse, AgentLlmError>> + Send + 'a>>;
}

// ---------------------------------------------------------------------------
// Production implementation
// ---------------------------------------------------------------------------

use shore_llm_client::LlmClient;

/// Production `AgentLlm` backed by `LlmClient` (Unix socket to shore-llm).
pub struct RealAgentLlm {
    client: LlmClient,
}

impl RealAgentLlm {
    pub fn new(client: LlmClient) -> Self {
        Self { client }
    }
}

impl AgentLlm for RealAgentLlm {
    fn generate<'a>(
        &'a self,
        messages: Vec<Value>,
        system: Option<Value>,
        tools: Option<Vec<Value>>,
        model: &'a ResolvedModel,
    ) -> Pin<Box<dyn Future<Output = Result<AgentLlmResponse, AgentLlmError>> + Send + 'a>> {
        Box::pin(async move {
            let request = LlmClient::build_request(model, messages, system, tools, None)
                .map_err(|e| AgentLlmError::Transport(e.to_string()))?;

            let resp = self
                .client
                .generate(&request, None)
                .await
                .map_err(|e| AgentLlmError::Transport(e.to_string()))?;

            // Extract text from content_blocks, or fall back to content field.
            let text = if resp.content_blocks.is_empty() {
                resp.content.clone()
            } else {
                resp.content_blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("")
            };

            Ok(AgentLlmResponse {
                text,
                content_blocks: resp.content_blocks,
                finish_reason: resp.finish_reason,
            })
        })
    }
}

// ---------------------------------------------------------------------------
// Mock implementation (for tests)
// ---------------------------------------------------------------------------

/// Mock `AgentLlm` that returns canned responses in sequence.
///
/// Tracks call history for assertion in tests.
pub struct MockAgentLlm {
    responses: Mutex<Vec<AgentLlmResponse>>,
    call_index: AtomicUsize,
    pub calls: Mutex<Vec<MockAgentLlmCall>>,
}

/// A recorded call to the mock LLM.
#[derive(Debug, Clone)]
pub struct MockAgentLlmCall {
    pub messages: Vec<Value>,
    pub system: Option<Value>,
    pub tools: Option<Vec<Value>>,
}

impl MockAgentLlm {
    /// Create a mock that returns the given responses in order.
    pub fn new(responses: Vec<AgentLlmResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
            call_index: AtomicUsize::new(0),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// How many calls have been made.
    pub fn call_count(&self) -> usize {
        self.call_index.load(Ordering::SeqCst)
    }
}

impl AgentLlm for MockAgentLlm {
    fn generate<'a>(
        &'a self,
        messages: Vec<Value>,
        system: Option<Value>,
        tools: Option<Vec<Value>>,
        _model: &'a ResolvedModel,
    ) -> Pin<Box<dyn Future<Output = Result<AgentLlmResponse, AgentLlmError>> + Send + 'a>> {
        // Record the call.
        self.calls.lock().unwrap().push(MockAgentLlmCall {
            messages,
            system,
            tools,
        });

        let idx = self.call_index.fetch_add(1, Ordering::SeqCst);
        let responses = self.responses.lock().unwrap();
        let response = responses.get(idx).cloned();

        Box::pin(async move { response.ok_or(AgentLlmError::MockExhausted) })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_model;
    use serde_json::json;

    #[tokio::test]
    async fn mock_returns_canned_responses() {
        let mock = MockAgentLlm::new(vec![
            AgentLlmResponse {
                text: "First response".into(),
                content_blocks: vec![ContentBlock::Text {
                    text: "First response".into(),
                }],
                finish_reason: "end_turn".into(),
            },
            AgentLlmResponse {
                text: "Second response".into(),
                content_blocks: vec![ContentBlock::Text {
                    text: "Second response".into(),
                }],
                finish_reason: "end_turn".into(),
            },
        ]);

        let model = test_model();

        let r1 = mock
            .generate(vec![json!({"role": "user", "content": "hi"})], None, None, &model)
            .await
            .unwrap();
        assert_eq!(r1.text, "First response");
        assert_eq!(r1.finish_reason, "end_turn");

        let r2 = mock
            .generate(vec![json!({"role": "user", "content": "bye"})], None, None, &model)
            .await
            .unwrap();
        assert_eq!(r2.text, "Second response");

        assert_eq!(mock.call_count(), 2);
        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls[0].messages[0]["content"], "hi");
        assert_eq!(calls[1].messages[0]["content"], "bye");
    }

    #[tokio::test]
    async fn mock_exhausted_returns_error() {
        let mock = MockAgentLlm::new(vec![]);
        let model = test_model();
        let result = mock.generate(vec![], None, None, &model).await;
        assert!(matches!(result, Err(AgentLlmError::MockExhausted)));
    }

    #[tokio::test]
    async fn mock_records_tools() {
        let mock = MockAgentLlm::new(vec![AgentLlmResponse {
            text: "ok".into(),
            content_blocks: vec![],
            finish_reason: "end_turn".into(),
        }]);

        let model = test_model();
        let tools = vec![json!({"name": "search", "input_schema": {}})];
        mock.generate(vec![], Some(json!("system prompt")), Some(tools.clone()), &model)
            .await
            .unwrap();

        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls[0].system, Some(json!("system prompt")));
        assert_eq!(calls[0].tools.as_ref().unwrap().len(), 1);
    }
}
