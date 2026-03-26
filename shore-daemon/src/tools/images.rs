use super::{ToolCategory, ToolContext, ToolDef, ToolError};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

pub fn tool_defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "send_image",
            description: "Send an image from memory to the conversation.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the image file in memory storage."
                    },
                    "caption": {
                        "type": "string",
                        "description": "Optional caption for the image."
                    }
                },
                "required": ["path"]
            }),
            category: ToolCategory::MemoryWrite,
        },
        ToolDef {
            name: "list_images",
            description: "List image memories. Optionally pass a query for semantic search via RAG (top-32).",
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Optional natural language query to search image memories."
                    }
                }
            }),
            category: ToolCategory::MemoryRead,
        },
        ToolDef {
            name: "recall_image",
            description: "View an image at full resolution by path.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the image file to recall."
                    }
                },
                "required": ["path"]
            }),
            category: ToolCategory::MemoryRead,
        },
        ToolDef {
            name: "generate_image",
            description: "Generate an image using DALL-E 3 or compatible endpoint.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "Text prompt for image generation."
                    },
                    "size": {
                        "type": "string",
                        "description": "Image dimensions (e.g. '1024x1024').",
                        "default": "1024x1024"
                    }
                },
                "required": ["prompt"]
            }),
            category: ToolCategory::MemoryWrite,
        },
    ]
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Handle `send_image` — send an image file from memory storage.
pub async fn handle_send_image(input: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
    let path = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'path' field".to_string()))?;

    let caption = input.get("caption").and_then(|v| v.as_str());

    // Resolve path relative to image directory.
    let full_path = std::path::Path::new(ctx.image_dir()).join(path);

    if !full_path.exists() {
        return Err(ToolError::Io(format!("image not found: {}", full_path.display())));
    }

    Ok(json!({
        "path": full_path.to_string_lossy(),
        "caption": caption,
        "sent": true,
    }))
}

/// Handle `list_images` — list image entries, optionally filtered by RAG query.
pub async fn handle_list_images(input: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
    let query = input.get("query").and_then(|v| v.as_str());

    if let Some(q) = query {
        // Use memory agent to query image entries via RAG.
        let agent = ctx.memory_agent();
        let result = agent
            .query(q, ctx.rag(), ctx.memory_db())
            .await
            .map_err(ToolError::Agent)?;

        let images: Vec<Value> = result
            .entries
            .iter()
            .filter(|e| e.memory_type == "image")
            .map(|e| {
                json!({
                    "entry_id": e.entry_id,
                    "summary": e.summary_text,
                    "relevance": e.relevance_score,
                })
            })
            .collect();

        Ok(json!({ "images": images, "query": q }))
    } else {
        // List all image entries from DB.
        let entries = ctx
            .memory_db()
            .get_entries_by_type("image")
            .map_err(|e| ToolError::Io(e.to_string()))?;

        let images: Vec<Value> = entries
            .iter()
            .map(|e| {
                json!({
                    "entry_id": e.id,
                    "summary": e.summary_text,
                    "image_path": e.image_path,
                })
            })
            .collect();

        Ok(json!({ "images": images }))
    }
}

/// Handle `recall_image` — return the path for full-resolution viewing.
pub async fn handle_recall_image(input: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
    let path = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'path' field".to_string()))?;

    let full_path = std::path::Path::new(ctx.image_dir()).join(path);

    if !full_path.exists() {
        return Err(ToolError::Io(format!("image not found: {}", full_path.display())));
    }

    Ok(json!({
        "path": full_path.to_string_lossy(),
        "exists": true,
    }))
}

/// Handle `generate_image` — stub that returns a placeholder.
/// Full implementation requires HTTP call to an OpenAI-compatible endpoint.
pub async fn handle_generate_image(input: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
    let prompt = input
        .get("prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'prompt' field".to_string()))?;

    let size = input
        .get("size")
        .and_then(|v| v.as_str())
        .unwrap_or("1024x1024");

    // Stub — full implementation will call an image generation API.
    Err(ToolError::NotImplemented(format!(
        "generate_image (prompt={}, size={}): requires HTTP endpoint configuration",
        prompt, size
    )))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::models::{ResolvedModel, Sdk};
    use crate::memory::agent::types::{AgentError, AgentIndexer, AgentRag, RagHit};
    use crate::memory::agent::{CallerIdentity, MemoryAgent};
    use crate::memory::agent_llm::MockAgentLlm;
    use crate::memory::db::{Entry, MemoryDB};
    use chrono::Utc;
    use std::future::Future;
    use std::pin::Pin;

    struct MockRag {
        results: Vec<RagHit>,
    }

    impl AgentRag for MockRag {
        fn query(
            &self,
            _query: &str,
            _top_k: usize,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<RagHit>, AgentError>> + Send + '_>> {
            let result = Ok(self.results.clone());
            Box::pin(async move { result })
        }
    }

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

    struct TestContext {
        db: MemoryDB,
        agent: MemoryAgent,
        agent_llm: MockAgentLlm,
        model: ResolvedModel,
        rag: MockRag,
        image_dir: String,
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
            &self.image_dir
        }
    }

    fn make_ctx(image_dir: &str) -> TestContext {
        TestContext {
            db: MemoryDB::open_in_memory().unwrap(),
            agent: MemoryAgent::one_shot(CallerIdentity::Char, "Alice", "Bob"),
            agent_llm: MockAgentLlm::new(vec![]),
            model: test_model(),
            rag: MockRag { results: vec![] },
            image_dir: image_dir.to_string(),
        }
    }

    fn make_image_entry(id: &str, summary: &str, image_path: &str) -> Entry {
        let now = Utc::now().to_rfc3339();
        Entry {
            id: id.to_string(),
            memory_type: "image".to_string(),
            source: "user".to_string(),
            reason: "upload".to_string(),
            status: "active".to_string(),
            canonical: false,
            confidence: 1.0,
            summary_text: summary.to_string(),
            topic_tags: "image".to_string(),
            topic_key: "images".to_string(),
            start_timestamp: now.clone(),
            end_timestamp: now.clone(),
            message_count: 0,
            source_entry_ids: String::new(),
            related_entry_ids: String::new(),
            superseded_by: String::new(),
            created_at: now.clone(),
            updated_at: now,
            entry_type: String::new(),
            image_path: image_path.to_string(),
        }
    }

    #[test]
    fn test_image_tool_defs() {
        let defs = tool_defs();
        assert_eq!(defs.len(), 4);

        let names: Vec<&str> = defs.iter().map(|d| d.name).collect();
        assert!(names.contains(&"send_image"));
        assert!(names.contains(&"list_images"));
        assert!(names.contains(&"recall_image"));
        assert!(names.contains(&"generate_image"));

        // send_image and generate_image are MemoryWrite (they produce side effects).
        assert_eq!(defs.iter().find(|d| d.name == "send_image").unwrap().category, ToolCategory::MemoryWrite);
        assert_eq!(defs.iter().find(|d| d.name == "generate_image").unwrap().category, ToolCategory::MemoryWrite);

        // list_images and recall_image are MemoryRead.
        assert_eq!(defs.iter().find(|d| d.name == "list_images").unwrap().category, ToolCategory::MemoryRead);
        assert_eq!(defs.iter().find(|d| d.name == "recall_image").unwrap().category, ToolCategory::MemoryRead);
    }

    #[tokio::test]
    async fn test_send_image_file_not_found() {
        let ctx = make_ctx("/nonexistent");
        let result = handle_send_image(json!({"path": "test.png"}), &ctx).await;
        assert!(matches!(result, Err(ToolError::Io(_))));
    }

    #[tokio::test]
    async fn test_send_image_missing_path() {
        let ctx = make_ctx("/tmp");
        let result = handle_send_image(json!({}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn test_list_images_no_query() {
        let ctx = make_ctx("/tmp");
        let entry = make_image_entry("img1", "A sunset photo", "sunset.png");
        ctx.db.create_entry(&entry).unwrap();

        let result = handle_list_images(json!({}), &ctx).await.unwrap();
        let images = result["images"].as_array().unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0]["entry_id"], "img1");
    }

    #[tokio::test]
    async fn test_recall_image_missing_path() {
        let ctx = make_ctx("/tmp");
        let result = handle_recall_image(json!({}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn test_generate_image_stub() {
        let ctx = make_ctx("/tmp");
        let result = handle_generate_image(
            json!({"prompt": "a cat", "size": "512x512"}),
            &ctx,
        )
        .await;
        assert!(matches!(result, Err(ToolError::NotImplemented(_))));
    }
}
