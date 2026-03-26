use super::{ToolCategory, ToolContext, ToolDef, ToolError};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

pub fn tool_defs() -> Vec<ToolDef> {
    vec![ToolDef {
        name: "memory",
        description: "Search or save a memory. Pass a natural language request describing what to search for or what to remember.",
        parameters: json!({
            "type": "object",
            "properties": {
                "request": {
                    "type": "string",
                    "description": "Natural language query to search memories, or a statement to save."
                }
            },
            "required": ["request"]
        }),
        category: ToolCategory::MemoryWrite,
    }]
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Handle the `memory` tool — search or save via the memory researcher/agent.
///
/// Routing (matching V1):
/// 1. If researcher available → researcher.research(request, ...)
/// 2. Else → agent.ask(request, ...)
///
/// Returns the synthesis text as the tool result (natural language, same as V1).
pub async fn handle_memory(input: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
    let request = input
        .get("request")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'request' field".to_string()))?;

    let agent = ctx.memory_agent();
    let db = ctx.memory_db();
    let agent_llm = ctx.agent_llm();
    let agent_model = ctx.agent_model();
    let indexer = ctx.indexer();

    let result_text = if let Some(researcher) = ctx.memory_researcher() {
        // Tier 2: cheap model drives the inner agent
        let researcher_llm = ctx
            .researcher_llm()
            .ok_or_else(|| ToolError::InvalidArgs("researcher LLM not configured".into()))?;
        let researcher_model = ctx
            .researcher_model()
            .ok_or_else(|| ToolError::InvalidArgs("researcher model not configured".into()))?;

        researcher
            .research(
                request,
                researcher_llm,
                researcher_model,
                agent,
                agent_llm,
                agent_model,
                db,
                indexer,
            )
            .await
            .map_err(|e| ToolError::Agent(e))?
    } else {
        // Direct agent query (no researcher)
        agent
            .ask(request, agent_llm, db, indexer, agent_model)
            .await
            .map_err(|e| ToolError::Agent(e))?
    };

    Ok(json!(result_text))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::models::{ResolvedModel, Sdk};
    use crate::llm_client::types::ContentBlock;
    use crate::memory::agent::types::{AgentError, AgentIndexer, AgentRag, RagHit};
    use crate::memory::agent::{CallerIdentity, MemoryAgent};
    use crate::memory::agent_llm::{AgentLlmResponse, MockAgentLlm};
    use crate::memory::db::MemoryDB;
    use std::future::Future;
    use std::pin::Pin;

    fn test_model() -> ResolvedModel {
        ResolvedModel {
            name: "test".into(),
            qualified_name: "chat.test".into(),
            category: "chat".into(),
            provider_key: "anthropic".into(),
            sdk: Sdk::Anthropic,
            model_id: "claude-test".into(),
            api_key_env: Some("TEST_KEY".into()),
            base_url: None,
            max_context_tokens: None,
            max_tokens: Some(4096),
            temperature: Some(0.7),
            top_p: None,
            reasoning_effort: None,
            budget_tokens: None,
            cache_ttl: None,
            cache_control_depth: None,
            keepalive_enabled: None,
            openrouter_provider: None,
            vertex_project: None,
            vertex_location: None,
            gemini_generation: None,
            gemini_web_search: None,
        }
    }

    struct MockRag;

    impl AgentRag for MockRag {
        fn query(
            &self,
            _query: &str,
            _top_k: usize,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<RagHit>, AgentError>> + Send + '_>> {
            Box::pin(async { Ok(vec![]) })
        }
    }

    struct TestContext {
        db: MemoryDB,
        agent: MemoryAgent,
        agent_llm: MockAgentLlm,
        model: ResolvedModel,
        rag: MockRag,
    }

    impl ToolContext for TestContext {
        fn memory_db(&self) -> &MemoryDB {
            &self.db
        }
        fn memory_agent(&self) -> &MemoryAgent {
            &self.agent
        }
        fn agent_llm(&self) -> &dyn crate::memory::agent_llm::AgentLlm {
            &self.agent_llm
        }
        fn agent_model(&self) -> &ResolvedModel {
            &self.model
        }
        fn researcher_llm(&self) -> Option<&dyn crate::memory::agent_llm::AgentLlm> {
            None
        }
        fn researcher_model(&self) -> Option<&ResolvedModel> {
            None
        }
        fn memory_researcher(&self) -> Option<&crate::memory::researcher::MemoryResearcher> {
            None
        }
        fn indexer(&self) -> Option<&dyn AgentIndexer> {
            None
        }
        fn rag(&self) -> &dyn AgentRag {
            &self.rag
        }
        fn image_dir(&self) -> &str {
            "/tmp/test_images"
        }
    }

    #[test]
    fn test_memory_tool_def() {
        let defs = tool_defs();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "memory");
        assert_eq!(defs[0].category, ToolCategory::MemoryWrite);
    }

    #[tokio::test]
    async fn test_handle_memory_returns_text() {
        let agent_llm = MockAgentLlm::new(vec![AgentLlmResponse {
            text: "No relevant memories found.".into(),
            content_blocks: vec![ContentBlock::Text {
                text: "No relevant memories found.".into(),
            }],
            finish_reason: "end_turn".into(),
        }]);

        let ctx = TestContext {
            db: MemoryDB::open_in_memory().unwrap(),
            agent: MemoryAgent::one_shot(CallerIdentity::Char, "Alice", "Bob"),
            agent_llm,
            model: test_model(),
            rag: MockRag,
        };

        let result = handle_memory(json!({"request": "What do I like?"}), &ctx)
            .await
            .unwrap();

        // New handler returns synthesis text, not structured JSON
        assert_eq!(result.as_str().unwrap(), "No relevant memories found.");
    }

    #[tokio::test]
    async fn test_handle_memory_missing_request() {
        let ctx = TestContext {
            db: MemoryDB::open_in_memory().unwrap(),
            agent: MemoryAgent::one_shot(CallerIdentity::Char, "Alice", "Bob"),
            agent_llm: MockAgentLlm::new(vec![]),
            model: test_model(),
            rag: MockRag,
        };

        let result = handle_memory(json!({}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }
}
