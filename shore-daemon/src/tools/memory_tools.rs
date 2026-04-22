use super::{ToolCategory, ToolContext, ToolDef, ToolError};
use crate::memory::agent::RealAgentIndexer;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

pub fn tool_defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "memory",
            description: "Advanced memory query through a researcher agent — use this for complex, multi-step memory investigations that require reasoning across many entries, or for updates that need structured database operations. For simple lookups, prefer `memory_search` followed by `memory_read`. For saving facts, prefer `memory_write`. This tool is powerful but slower; reach for the direct file tools first.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "request": {
                        "type": "string",
                        "description": "Natural-language query to search memories, or a statement to save or correct one."
                    }
                },
                "required": ["request"]
            }),
            category: ToolCategory::MemoryWrite,
        },
        ToolDef {
            name: "memory_read",
            description: "Read the full content of a single memory file by its relative path from your memories directory. Use this AFTER `memory_search` points you to a relevant file, or when you already know the exact path and need the complete content. Do not use this for discovery — use `memory_search` or `memory_list` first.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path within the memories directory (e.g., 'topics/gaming/doom.md')."
                    }
                },
                "required": ["path"]
            }),
            category: ToolCategory::MemoryRead,
        },
        ToolDef {
            name: "memory_write",
            description: "Write or overwrite a memory file in your memories directory. Use this to save new facts, update existing memory files, or reorganize your knowledge. Prefer updating existing files over creating new ones. Auto-creates parent directories.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path within the memories directory (e.g., 'people/ren.md')."
                    },
                    "content": {
                        "type": "string",
                        "description": "Full markdown content to write."
                    }
                },
                "required": ["path", "content"]
            }),
            category: ToolCategory::MemoryWrite,
        },
        ToolDef {
            name: "memory_search",
            description: "Search your memory files for a keyword or phrase. Returns matching files with their paths and a content excerpt. Use this as the FIRST step whenever the user asks about past conversations, facts about {{user}}, shared history, preferences, or anything that feels familiar. Before saying 'I think we talked about this', 'if I remember correctly', or making any factual claim you could verify, call this tool. Do not guess — verify.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Keyword or phrase to search for (case-insensitive)."
                    }
                },
                "required": ["query"]
            }),
            category: ToolCategory::MemoryRead,
        },
        ToolDef {
            name: "memory_list",
            description: "List all memory files in your memories directory, optionally filtered by a subdirectory. Use this to get an overview of what memories you have, to discover files in a specific category, or when you're unsure whether a topic has already been saved.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Optional subdirectory to list (e.g., 'topics/gaming'). Omit to list all memory files."
                    }
                },
                "required": []
            }),
            category: ToolCategory::MemoryRead,
        },
    ]
}

// ---------------------------------------------------------------------------
// Handlers
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

/// Read a memory file by relative path.
pub async fn handle_memory_read(input: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
    let path = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'path' field".to_string()))?;

    let store = ctx
        .markdown_store()
        .ok_or_else(|| ToolError::InvalidArgs("markdown memory store not available".to_string()))?;

    let entry = store.read(path).await.map_err(|e| ToolError::Io(e.to_string()))?;

    Ok(json!({
        "path": entry.path,
        "content": entry.content,
        "size": entry.size,
        "modified_at": entry.modified_at,
    }))
}

/// Write or overwrite a memory file.
pub async fn handle_memory_write(input: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
    let path = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'path' field".to_string()))?;
    let content = input
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'content' field".to_string()))?;

    let store = ctx
        .markdown_store()
        .ok_or_else(|| ToolError::InvalidArgs("markdown memory store not available".to_string()))?;

    store
        .write(path, content)
        .await
        .map_err(|e| ToolError::Io(e.to_string()))?;

    Ok(json!({"status": "written", "path": path, "bytes": content.len()}))
}

/// Search memory files for a keyword or phrase.
pub async fn handle_memory_search(input: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
    let query = input
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'query' field".to_string()))?;

    let store = ctx
        .markdown_store()
        .ok_or_else(|| ToolError::InvalidArgs("markdown memory store not available".to_string()))?;

    let results = store
        .search_text(query)
        .await
        .map_err(|e| ToolError::Io(e.to_string()))?;

    let hits: Vec<Value> = results
        .into_iter()
        .map(|entry| {
            // Truncate content to a reasonable excerpt for the LLM
            let excerpt = if entry.content.len() > 400 {
                format!("{}...", &entry.content[..400])
            } else {
                entry.content.clone()
            };
            json!({
                "path": entry.path,
                "excerpt": excerpt,
                "size": entry.size,
                "modified_at": entry.modified_at,
            })
        })
        .collect();

    Ok(json!({"query": query, "results": hits, "count": hits.len()}))
}

/// List memory files, optionally filtered by subdirectory.
pub async fn handle_memory_list(input: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
    let store = ctx
        .markdown_store()
        .ok_or_else(|| ToolError::InvalidArgs("markdown memory store not available".to_string()))?;

    let filter_path = input.get("path").and_then(|v| v.as_str());

    let all = store
        .list_all()
        .await
        .map_err(|e| ToolError::Io(e.to_string()))?;

    let files: Vec<Value> = all
        .into_iter()
        .filter(|entry| {
            if let Some(filter) = filter_path {
                entry.path.starts_with(filter)
            } else {
                true
            }
        })
        .map(|entry| {
            json!({
                "path": entry.path,
                "size": entry.size,
                "modified_at": entry.modified_at,
            })
        })
        .collect();

    Ok(json!({"files": files, "count": files.len()}))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::agent_llm::{AgentLlmResponse, MockAgentLlm};
    use crate::memory::markdown_store::MarkdownMemoryStore;
    use crate::test_support::TestToolContext;
    use shore_llm_client::types::ContentBlock;

    #[test]
    fn test_memory_tool_defs() {
        let defs = tool_defs();
        assert_eq!(defs.len(), 5);
        let names: Vec<&str> = defs.iter().map(|d| d.name).collect();
        assert!(names.contains(&"memory"));
        assert!(names.contains(&"memory_read"));
        assert!(names.contains(&"memory_write"));
        assert!(names.contains(&"memory_search"));
        assert!(names.contains(&"memory_list"));
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

    // -- Markdown memory tools tests ------------------------------------------

    #[tokio::test]
    async fn test_memory_write_and_read() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories")).await.unwrap();
        let ctx = TestToolContext::new().with_markdown_store(store);

        let result = handle_memory_write(
            json!({"path": "people/alice.md", "content": "# Alice\n\nLikes chocolate."}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "written");

        let read = handle_memory_read(json!({"path": "people/alice.md"}), &ctx)
            .await
            .unwrap();
        assert_eq!(read["content"], "# Alice\n\nLikes chocolate.");
    }

    #[tokio::test]
    async fn test_memory_search_finds_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories")).await.unwrap();
        let ctx = TestToolContext::new().with_markdown_store(store);

        handle_memory_write(
            json!({"path": "a.md", "content": "Ren likes chocolate"}),
            &ctx,
        )
        .await
        .unwrap();
        handle_memory_write(
            json!({"path": "b.md", "content": "Alice prefers tea"}),
            &ctx,
        )
        .await
        .unwrap();

        let result = handle_memory_search(json!({"query": "chocolate"}), &ctx)
            .await
            .unwrap();
        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["path"], "a.md");
    }

    #[tokio::test]
    async fn test_memory_list_all_and_filtered() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories")).await.unwrap();
        let ctx = TestToolContext::new().with_markdown_store(store);

        handle_memory_write(json!({"path": "topics/gaming/doom.md", "content": "Doom"}), &ctx)
            .await
            .unwrap();
        handle_memory_write(json!({"path": "topics/food/tea.md", "content": "Tea"}), &ctx)
            .await
            .unwrap();

        let all = handle_memory_list(json!({}), &ctx).await.unwrap();
        let files = all["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);

        let filtered = handle_memory_list(json!({"path": "topics/gaming"}), &ctx)
            .await
            .unwrap();
        let ffiles = filtered["files"].as_array().unwrap();
        assert_eq!(ffiles.len(), 1);
        assert_eq!(ffiles[0]["path"], "topics/gaming/doom.md");
    }

    #[tokio::test]
    async fn test_memory_read_missing_path() {
        let ctx = TestToolContext::new();
        let result = handle_memory_read(json!({}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn test_memory_tools_without_store() {
        let ctx = TestToolContext::new();
        let result = handle_memory_read(json!({"path": "x.md"}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }
}
