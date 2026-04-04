use super::{ToolCategory, ToolContext, ToolDef, ToolError};
use crate::memory::agent::RealAgentIndexer;
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
    let search_ctx = ctx.search_context();
    // Build a real indexer from the search context when available; falls back to None.
    let real_indexer = search_ctx.map(RealAgentIndexer::new);
    let indexer = real_indexer
        .as_ref()
        .map(|i| i as &dyn crate::memory::agent::AgentIndexer);

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
                search_ctx,
            )
            .await
            .map_err(ToolError::Agent)?
    } else {
        // Direct agent query (no researcher)
        agent
            .ask(request, agent_llm, db, indexer, search_ctx, agent_model)
            .await
            .map_err(ToolError::Agent)?
    };

    Ok(json!(result_text))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::agent_llm::{AgentLlmResponse, MockAgentLlm};
    use crate::test_support::TestToolContext;
    use shore_llm_client::types::ContentBlock;

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

        let ctx = TestToolContext::new().with_agent_llm(agent_llm);

        let result = handle_memory(json!({"request": "What do I like?"}), &ctx)
            .await
            .unwrap();

        // New handler returns synthesis text, not structured JSON
        assert_eq!(result.as_str().unwrap(), "No relevant memories found.");
    }

    #[tokio::test]
    async fn test_handle_memory_missing_request() {
        let ctx = TestToolContext::new();

        let result = handle_memory(json!({}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn test_handle_memory_with_researcher() {
        use crate::memory::researcher::MemoryResearcher;
        use crate::test_support::test_model;

        // Researcher LLM: calls ask_memory_agent, then synthesizes.
        let researcher_llm = MockAgentLlm::new(vec![
            AgentLlmResponse {
                text: String::new(),
                content_blocks: vec![ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "ask_memory_agent".into(),
                    input: serde_json::json!({"question": "What does Alice like?"}),
                }],
                finish_reason: "tool_use".into(),
            },
            AgentLlmResponse {
                text: "Alice likes chocolate.".into(),
                content_blocks: vec![ContentBlock::Text {
                    text: "Alice likes chocolate.".into(),
                }],
                finish_reason: "end_turn".into(),
            },
        ]);

        // Agent LLM: responds when the researcher queries the inner agent.
        let agent_llm = MockAgentLlm::new(vec![AgentLlmResponse {
            text: "Alice likes chocolate according to entry e1.".into(),
            content_blocks: vec![ContentBlock::Text {
                text: "Alice likes chocolate according to entry e1.".into(),
            }],
            finish_reason: "end_turn".into(),
        }]);

        let researcher = MemoryResearcher::new(String::new(), String::new());

        let ctx = TestToolContext::new()
            .with_agent_llm(agent_llm)
            .with_researcher(researcher, researcher_llm, test_model());

        let result = handle_memory(json!({"request": "What does Alice like?"}), &ctx)
            .await
            .unwrap();

        assert!(result.as_str().unwrap().contains("chocolate"));
    }

    #[tokio::test]
    async fn test_handle_memory_researcher_missing_llm() {
        use crate::memory::researcher::MemoryResearcher;

        // Build context with researcher but NO researcher LLM.
        let mut ctx = TestToolContext::new();
        ctx.researcher = Some(MemoryResearcher::new(String::new(), String::new()));
        // researcher_llm_val and researcher_model_val remain None.

        let result = handle_memory(json!({"request": "test"}), &ctx).await;
        assert!(
            matches!(result, Err(ToolError::InvalidArgs(_))),
            "Expected InvalidArgs for missing researcher LLM, got {:?}",
            result
        );
    }
}
