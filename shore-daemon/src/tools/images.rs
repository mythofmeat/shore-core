use super::{ToolCategory, ToolContext, ToolDef, ToolError};
use serde_json::{json, Value};
use tracing::info;

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

/// Handle `generate_image` — calls shore-llm, downloads the result, and saves to disk.
pub async fn handle_generate_image(input: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
    let prompt = input
        .get("prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'prompt' field".to_string()))?;

    let client = ctx
        .llm_client()
        .ok_or_else(|| ToolError::Io("image generation not available: no LLM client".into()))?;
    let config = ctx
        .image_gen_config()
        .ok_or_else(|| ToolError::Io("no [image_generation] profile configured".into()))?;

    let size = input
        .get("size")
        .and_then(|v| v.as_str())
        .unwrap_or(&config.size);

    // 1. Call shore-llm to generate the image.
    let result = client
        .image_generate(
            &config.provider,
            &config.model_id,
            &config.api_key,
            config.base_url.as_deref(),
            prompt,
            Some(size),
            config.quality.as_deref(),
        )
        .await
        .map_err(|e| ToolError::Http(format!("image generation failed: {e}")))?;

    info!(
        url = %result.url,
        revised_prompt = %result.revised_prompt,
        timing_ms = result.timing.total_ms,
        "Image generated via shore-llm"
    );

    // 2. Download the image from the returned URL.
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| ToolError::Http(format!("failed to create HTTP client: {e}")))?;

    let image_bytes = http_client
        .get(&result.url)
        .send()
        .await
        .map_err(|e| ToolError::Http(format!("failed to download image: {e}")))?
        .bytes()
        .await
        .map_err(|e| ToolError::Http(format!("failed to read image bytes: {e}")))?;

    // 3. Save to image directory.
    let image_dir = std::path::Path::new(ctx.image_dir());
    let generated_dir = image_dir.join("generated");
    std::fs::create_dir_all(&generated_dir)
        .map_err(|e| ToolError::Io(format!("failed to create directory: {e}")))?;

    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let filename = format!("{timestamp}.png");
    let save_path = generated_dir.join(&filename);

    std::fs::write(&save_path, &image_bytes)
        .map_err(|e| ToolError::Io(format!("failed to save image: {e}")))?;

    let relative_path = format!("generated/{filename}");

    // 4. Create memory entry for the generated image.
    let now = chrono::Utc::now().to_rfc3339();
    let entry = crate::memory::db::Entry {
        id: format!("img_{}", uuid::Uuid::new_v4()),
        memory_type: "image".into(),
        source: "tool".into(),
        reason: "generate_image".into(),
        status: "active".into(),
        canonical: false,
        confidence: 1.0,
        summary_text: result.revised_prompt.clone(),
        topic_tags: "generated,image".into(),
        topic_key: "images".into(),
        start_timestamp: now.clone(),
        end_timestamp: now.clone(),
        message_count: 0,
        source_entry_ids: String::new(),
        related_entry_ids: String::new(),
        superseded_by: String::new(),
        created_at: now.clone(),
        updated_at: now,
        entry_type: String::new(),
        image_path: relative_path.clone(),
    };
    ctx.memory_db()
        .create_entry(&entry)
        .map_err(|e| ToolError::Io(format!("failed to create memory entry: {e}")))?;

    Ok(json!({
        "path": relative_path,
        "revised_prompt": result.revised_prompt,
        "timing_ms": result.timing.total_ms,
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{make_image_entry, TestToolContext};

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
        let ctx = TestToolContext::new().with_image_dir("/nonexistent");
        let result = handle_send_image(json!({"path": "test.png"}), &ctx).await;
        assert!(matches!(result, Err(ToolError::Io(_))));
    }

    #[tokio::test]
    async fn test_send_image_missing_path() {
        let ctx = TestToolContext::new().with_image_dir("/tmp");
        let result = handle_send_image(json!({}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn test_list_images_no_query() {
        let ctx = TestToolContext::new().with_image_dir("/tmp");
        let entry = make_image_entry("img1", "A sunset photo", "sunset.png");
        ctx.db.create_entry(&entry).unwrap();

        let result = handle_list_images(json!({}), &ctx).await.unwrap();
        let images = result["images"].as_array().unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0]["entry_id"], "img1");
    }

    #[tokio::test]
    async fn test_recall_image_missing_path() {
        let ctx = TestToolContext::new().with_image_dir("/tmp");
        let result = handle_recall_image(json!({}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn test_generate_image_no_config() {
        let ctx = TestToolContext::new().with_image_dir("/tmp");
        let result = handle_generate_image(
            json!({"prompt": "a cat", "size": "512x512"}),
            &ctx,
        )
        .await;
        // Without LLM client configured, should return an Io error.
        assert!(matches!(result, Err(ToolError::Io(_))));
    }

    #[tokio::test]
    async fn test_send_image_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let image_path = tmp.path().join("photo.png");
        std::fs::write(&image_path, b"fake image data").unwrap();

        let ctx = TestToolContext::new()
            .with_image_dir(tmp.path().to_str().unwrap());

        let result = handle_send_image(
            json!({"path": "photo.png", "caption": "A test photo"}),
            &ctx,
        )
        .await
        .unwrap();

        assert_eq!(result["sent"], true);
        assert!(result["path"].as_str().unwrap().contains("photo.png"));
        assert_eq!(result["caption"], "A test photo");
    }

    #[tokio::test]
    async fn test_recall_image_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let image_path = tmp.path().join("sunset.jpg");
        std::fs::write(&image_path, b"fake image data").unwrap();

        let ctx = TestToolContext::new()
            .with_image_dir(tmp.path().to_str().unwrap());

        let result = handle_recall_image(json!({"path": "sunset.jpg"}), &ctx)
            .await
            .unwrap();

        assert_eq!(result["exists"], true);
        assert!(result["path"].as_str().unwrap().contains("sunset.jpg"));
    }

    #[tokio::test]
    async fn test_list_images_with_query() {
        use crate::memory::agent::types::RagHit;

        let ctx = TestToolContext::new();

        // Insert image entries into the in-memory DB.
        let entry = make_image_entry("img_sunset", "A sunset photo", "sunset.png");
        ctx.db.create_entry(&entry).unwrap();
        let entry2 = make_image_entry("img_cat", "A cat photo", "cat.png");
        ctx.db.create_entry(&entry2).unwrap();

        // Configure RAG to return one hit matching the sunset entry.
        let ctx = TestToolContext::new()
            .with_rag(vec![RagHit {
                entry_id: "img_sunset".into(),
                score: 0.9,
            }]);
        // Re-insert entries into the new context's DB.
        let entry = make_image_entry("img_sunset", "A sunset photo", "sunset.png");
        ctx.db.create_entry(&entry).unwrap();
        let entry2 = make_image_entry("img_cat", "A cat photo", "cat.png");
        ctx.db.create_entry(&entry2).unwrap();

        let result = handle_list_images(json!({"query": "sunset"}), &ctx)
            .await
            .unwrap();

        let images = result["images"].as_array().unwrap();
        // Only the sunset image should be returned (filtered by type + RAG hit).
        assert_eq!(images.len(), 1);
        assert_eq!(images[0]["entry_id"], "img_sunset");
        assert_eq!(result["query"], "sunset");
    }
}
