use super::{ToolCategory, ToolContext, ToolDef, ToolError};
use crate::memory::markdown_query;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

pub fn tool_defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "memory",
            description: "Synthesize an answer from your markdown memory files. Use this when a question needs reasoning across several files after direct file search would be cumbersome. It reads your memory files and answers from them only; for explicit file discovery, prefer `memory_search` + `memory_read`, and for saving facts, prefer `memory_write`.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "request": {
                        "type": "string",
                        "description": "Natural-language question to answer from markdown memory files."
                    }
                },
                "required": ["request"]
            }),
            category: ToolCategory::MemoryRead,
        },
        ToolDef {
            name: "memory_read",
            description: "Read the full content of a single memory file by its relative path from your memory directory. Use this AFTER `memory_search` points you to a relevant file, or when you already know the exact path and need the complete content. Do not use this for discovery — use `memory_search` or `memory_list` first.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path within the memory directory (e.g., 'topics/gaming/doom.md')."
                    }
                },
                "required": ["path"]
            }),
            category: ToolCategory::MemoryRead,
        },
        ToolDef {
            name: "memory_write",
            description: "Write or overwrite a memory file in your memory directory. Use this to save new facts, update existing memory files, or reorganize your knowledge. Prefer updating existing files over creating new ones. Auto-creates parent directories.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path within the memory directory (e.g., 'people/ren.md')."
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
            description: "List all memory files in your memory directory, optionally filtered by a subdirectory. Use this to get an overview of what memories you have, to discover files in a specific category, or when you're unsure whether a topic has already been saved.",
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

/// Handle the `memory` tool — answer a question from markdown files only.
pub async fn handle_memory(input: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
    let request = input
        .get("request")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'request' field".to_string()))?;

    let store = ctx
        .markdown_store()
        .ok_or_else(|| ToolError::InvalidArgs("markdown memory store not available".to_string()))?;
    let result_text = markdown_query::answer_query(
        request,
        ctx.character_name(),
        "the user",
        store,
        ctx.memory_llm(),
        ctx.memory_model(),
    )
    .await
    .map_err(|e| ToolError::Io(e.to_string()))?;

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

    let entry = store
        .read(path)
        .await
        .map_err(|e| ToolError::Io(e.to_string()))?;

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
            let excerpt =
                crate::memory::markdown_query::excerpt_for_query(&entry.content, query, 400);
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
    use crate::memory::markdown_store::MarkdownMemoryStore;
    use crate::memory::memory_llm::{MemoryLlmResponse, MockMemoryLlm};
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
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();
        store
            .write("people/user.md", "# User\n\n- Likes chocolate")
            .await
            .unwrap();

        let memory_llm = MockMemoryLlm::new(vec![MemoryLlmResponse {
            text: "The user likes chocolate.".into(),
            content_blocks: vec![ContentBlock::Text {
                text: "The user likes chocolate.".into(),
            }],
            finish_reason: "end_turn".into(),
        }]);

        let ctx = TestToolContext::new()
            .with_memory_llm(memory_llm)
            .with_markdown_store(store);

        let result = handle_memory(json!({"request": "What do I like?"}), &ctx)
            .await
            .unwrap();

        assert_eq!(result.as_str().unwrap(), "The user likes chocolate.");
    }

    #[tokio::test]
    async fn test_handle_memory_missing_request() {
        let ctx = TestToolContext::new();

        let result = handle_memory(json!({}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }

    // -- Markdown memory tools tests ------------------------------------------

    #[tokio::test]
    async fn test_memory_write_and_read() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();
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
    async fn test_handle_memory_reads_existing_markdown_files() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();
        store
            .write("people/alice.md", "# Alice\n\nLikes chocolate.")
            .await
            .unwrap();

        let memory_llm = MockMemoryLlm::new(vec![MemoryLlmResponse {
            text: "No relevant memories found.".into(),
            content_blocks: vec![ContentBlock::Text {
                text: "No relevant memories found.".into(),
            }],
            finish_reason: "end_turn".into(),
        }]);

        let ctx = TestToolContext::new()
            .with_markdown_store(store)
            .with_memory_llm(memory_llm);

        handle_memory(json!({"request": "What does Alice like?"}), &ctx)
            .await
            .unwrap();
        assert_eq!(ctx.memory_llm.call_count(), 1);
    }

    #[tokio::test]
    async fn test_memory_search_finds_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();
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
    async fn test_memory_search_truncates_unicode_excerpt_safely() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();
        let ctx = TestToolContext::new().with_markdown_store(store);

        let content = "é".repeat(401);
        handle_memory_write(json!({"path": "unicode.md", "content": content}), &ctx)
            .await
            .unwrap();

        let result = handle_memory_search(json!({"query": "é"}), &ctx)
            .await
            .unwrap();
        let results = result["results"].as_array().unwrap();
        let excerpt = results[0]["excerpt"].as_str().unwrap();
        assert!(excerpt.ends_with("..."));
    }

    #[tokio::test]
    async fn test_memory_list_all_and_filtered() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();
        let ctx = TestToolContext::new().with_markdown_store(store);

        handle_memory_write(
            json!({"path": "topics/gaming/doom.md", "content": "Doom"}),
            &ctx,
        )
        .await
        .unwrap();
        handle_memory_write(
            json!({"path": "topics/food/tea.md", "content": "Tea"}),
            &ctx,
        )
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
