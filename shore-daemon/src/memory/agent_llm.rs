//! LLM abstraction for the memory agent loop.
//!
//! The `AgentLlm` trait decouples the agent from the concrete LLM transport,
//! enabling unit tests with canned responses via `MockAgentLlm`.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use serde_json::Value;
use tracing::warn;

use shore_config::models::ResolvedModel;
use shore_llm_client::retry::{should_retry_error, RetryDecision, RetryPolicy};
use shore_llm_client::types::ContentBlock;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors from agent LLM calls.
#[derive(Debug, thiserror::Error)]
pub enum AgentLlmError {
    /// Transport/API error.
    #[error("llm transport: {0}")]
    Transport(String),
    /// No more canned responses in mock.
    #[error("mock: no more canned responses")]
    MockExhausted,
}

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

use shore_ledger::{CallType, LedgerClient};

/// Retry config for agent-tier LLM calls.
///
/// Gemini Flash (and occasionally other providers) returns `finish_reason=content_filter`
/// with zero tokens on innocuous memory queries. Transport blips also manifest as empty
/// responses or transient 5xx/429s. Retry covers both cases so the researcher's fallback
/// path isn't triggered by a ~2% flake.
const AGENT_MAX_RETRIES: u32 = 2;
const AGENT_RETRY_BACKOFF_MS: u64 = 500;

/// Production `AgentLlm` backed by `LedgerClient` (ledger-tracked LLM calls).
pub struct RealAgentLlm {
    client: LedgerClient,
    character: String,
    call_type: CallType,
}

impl RealAgentLlm {
    pub fn new(client: LedgerClient, character: String, call_type: CallType) -> Self {
        Self {
            client,
            character,
            call_type,
        }
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
            let policy = RetryPolicy {
                max_retries: AGENT_MAX_RETRIES,
                fallback_model: None,
            };
            let mut attempt: u32 = 0;

            loop {
                let request = LedgerClient::build_request(
                    model,
                    messages.clone(),
                    system.clone(),
                    tools.clone(),
                    None,
                )
                .map_err(|e| AgentLlmError::Transport(e.to_string()))?;

                match self
                    .client
                    .generate(&request, self.call_type, &self.character, false)
                    .await
                {
                    Ok(resp) => {
                        let text = resp.extract_text();
                        let empty_output =
                            resp.content_blocks.is_empty() && text.trim().is_empty();
                        let filtered = resp.finish_reason == "content_filter"
                            || resp.finish_reason == "refusal";

                        if (empty_output || filtered) && attempt < AGENT_MAX_RETRIES {
                            let delay = std::time::Duration::from_millis(
                                AGENT_RETRY_BACKOFF_MS * 2u64.pow(attempt),
                            );
                            warn!(
                                attempt,
                                delay_ms = delay.as_millis() as u64,
                                finish_reason = %resp.finish_reason,
                                call_type = self.call_type.as_str(),
                                model = %model.qualified_name,
                                empty_output,
                                "Agent LLM returned empty/filtered response, retrying"
                            );
                            tokio::time::sleep(delay).await;
                            attempt += 1;
                            continue;
                        }

                        return Ok(AgentLlmResponse {
                            text,
                            content_blocks: resp.content_blocks,
                            finish_reason: resp.finish_reason,
                        });
                    }
                    Err(e) => match should_retry_error(&e, attempt, &policy) {
                        RetryDecision::Retry => {
                            let delay = std::time::Duration::from_millis(
                                AGENT_RETRY_BACKOFF_MS * 2u64.pow(attempt),
                            );
                            warn!(
                                attempt,
                                delay_ms = delay.as_millis() as u64,
                                error = %e,
                                call_type = self.call_type.as_str(),
                                "Retrying transient agent LLM error"
                            );
                            tokio::time::sleep(delay).await;
                            attempt += 1;
                        }
                        RetryDecision::FallbackModel(_) | RetryDecision::Fail => {
                            return Err(AgentLlmError::Transport(e.to_string()));
                        }
                    },
                }
            }
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
            .generate(
                vec![json!({"role": "user", "content": "hi"})],
                None,
                None,
                &model,
            )
            .await
            .unwrap();
        assert_eq!(r1.text, "First response");
        assert_eq!(r1.finish_reason, "end_turn");

        let r2 = mock
            .generate(
                vec![json!({"role": "user", "content": "bye"})],
                None,
                None,
                &model,
            )
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
        mock.generate(
            vec![],
            Some(json!("system prompt")),
            Some(tools.clone()),
            &model,
        )
        .await
        .unwrap();

        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls[0].system, Some(json!("system prompt")));
        assert_eq!(calls[0].tools.as_ref().unwrap().len(), 1);
    }
}
