use super::{ToolCategory, ToolContext, ToolDef, ToolError};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::{json, Value};
use shore_llm_client::types::ImageGenerateParams;
use tracing::info;

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

pub fn tool_defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "send_image",
            description: "Attach an image from your saved memories to your reply so {{user}} sees it alongside your words. Use when the conversation calls back to a specific image, when a visual reference would clarify what you mean, or when surfacing a saved image adds warmth or humor. Pair with `list_images` to find the right one if you're not sure which path you need. Accepts either a relative path or an entry ID beginning with `img_`.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path or entry ID (e.g. 'img_...') of the image to send."
                    },
                    "caption": {
                        "type": "string",
                        "description": "Optional caption for the image."
                    }
                },
                "required": ["path"]
            }),
            category: ToolCategory::MemoryRead,
        },
        ToolDef {
            name: "list_images",
            description: "List saved image memories, optionally filtered by a natural-language query. Use when you want to find a specific image to send or recall — e.g. 'photos of Alex's cat', 'screenshots {{user}} shared last month'. Without a query, returns the most recent entries. Returns paths, IDs, and stored descriptions; use `recall_image` to actually view the contents of one.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Optional natural-language query to filter image memories."
                    }
                }
            }),
            category: ToolCategory::MemoryRead,
        },
        ToolDef {
            name: "recall_image",
            description: "Load and view an image at full resolution so you can see its contents yourself. Use when you need to reason about what an image actually depicts — a saved image came back from `list_images` and you want to look at it before referencing it, or {{user}} asked you about something visual you've forgotten the specifics of. This is for your own inspection, not for sending to {{user}}; use `send_image` for that. Accepts either a relative path or an entry ID beginning with `img_`.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path or entry ID (e.g. 'img_...') of the image to view."
                    }
                },
                "required": ["path"]
            }),
            category: ToolCategory::MemoryRead,
        },
        ToolDef {
            name: "remember_image",
            description: "Save an image {{user}} has shared to your memory database with a rich contextual description. Call this whenever {{user}} sends you an image worth remembering — most of the time, the answer is yes. The conversational context is the most valuable part of the description: 'a photo of Alex's cat Whiskers, shared the day she adopted him' is far more useful than 'a photo of a cat'. Include who shared it, why, what it means to you both. The `path` comes from the `[Attached image saved as: ...]` annotation that accompanies the user's message.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path from the [Attached image saved as: ...] annotation."
                    },
                    "description": {
                        "type": "string",
                        "description": "Rich contextual description — who shared it, why, what it means."
                    }
                },
                "required": ["path", "description"]
            }),
            category: ToolCategory::MemoryWrite,
        },
        ToolDef {
            name: "generate_image",
            description: "Generate an image from a text description via a separate image-generation model and send it to {{user}}. Feel free to use this any time the conversation paints a vivid picture, when you're describing something that would land better as a visual, when a moment feels worth illustrating, or just when it would be amusing. A specific prompt produces a better image — include mood, composition, and any visual details that matter, not just the subject. Larger sizes are higher-fidelity but slower and more expensive; `1024x1024` is a sensible default.",
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
// Path resolution
// ---------------------------------------------------------------------------

/// Resolve an image path from either a relative file path or an entry ID.
///
/// If the input starts with `img_`, looks up the entry in the DB and uses its
/// `image_path`. Otherwise treats the input as a relative path directly.
/// Returns `(relative_path, full_path)`.
fn resolve_image_path(
    input_path: &str,
    ctx: &dyn ToolContext,
) -> Result<(String, std::path::PathBuf), ToolError> {
    let relative_path = if input_path.starts_with("img_") {
        let entry = ctx
            .memory_db()
            .get_entry(input_path)
            .map_err(|e| ToolError::Io(format!("DB error: {e}")))?
            .ok_or_else(|| ToolError::Io(format!("no memory entry found: {input_path}")))?;
        if entry.image_path.is_empty() {
            return Err(ToolError::Io(format!(
                "entry {input_path} has no image_path"
            )));
        }
        entry.image_path
    } else {
        input_path.to_string()
    };
    let full_path = std::path::Path::new(ctx.image_dir()).join(&relative_path);
    if !full_path.exists() {
        return Err(ToolError::Io(format!(
            "image not found: {}",
            full_path.display()
        )));
    }
    Ok((relative_path, full_path))
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
    let (_relative, full_path) = resolve_image_path(path, ctx)?;

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

    let (_relative, full_path) = resolve_image_path(path, ctx)?;

    Ok(json!({
        "path": full_path.to_string_lossy(),
        "exists": true,
    }))
}

/// Handle `remember_image` — save a user-shared image to memory with context.
pub async fn handle_remember_image(
    input: Value,
    ctx: &dyn ToolContext,
) -> Result<Value, ToolError> {
    let path = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'path' field".to_string()))?;

    let description = input
        .get("description")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'description' field".to_string()))?;

    // Verify the file exists relative to image_dir.
    let full_path = std::path::Path::new(ctx.image_dir()).join(path);
    if !full_path.exists() {
        return Err(ToolError::Io(format!(
            "image not found: {}",
            full_path.display()
        )));
    }

    let now = chrono::Local::now().to_rfc3339();
    let entry = crate::memory::db::Entry {
        id: format!("img_{}", uuid::Uuid::new_v4()),
        memory_type: "image".into(),
        source: "user".into(),
        reason: "remember_image".into(),
        status: "active".into(),
        confidence: 1.0,
        summary_text: description.to_string(),
        topic_tags: "image,received".into(),
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
        image_path: path.to_string(),
        collated_at: String::new(),
    };

    ctx.memory_db()
        .create_entry(&entry)
        .map_err(|e| ToolError::Io(format!("failed to create memory entry: {e}")))?;

    info!(path = %path, description = %description, "Saved image memory via remember_image");

    Ok(json!({
        "entry_id": entry.id,
        "path": path,
        "description": description,
        "saved": true,
    }))
}

/// Handle `generate_image` — calls shore-llm, downloads the result, and saves to disk.
pub async fn handle_generate_image(
    input: Value,
    ctx: &dyn ToolContext,
) -> Result<Value, ToolError> {
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
    let params = ImageGenerateParams {
        provider_key: &config.provider,
        model: &config.model_id,
        api_key: &config.api_key,
        base_url: config.base_url.as_deref(),
        prompt,
        size: Some(size),
        quality: config.quality.as_deref(),
        aspect_ratio: config.aspect_ratio.as_deref(),
        image_size: config.image_size.as_deref(),
    };
    let result = client
        .image_generate(&params)
        .await
        .map_err(|e| ToolError::Http(format!("image generation failed: {e}")))?;

    info!(
        url_len = result.url.len(),
        revised_prompt = %result.revised_prompt,
        timing_ms = result.timing.total_ms,
        "Image generated via shore-llm"
    );

    // 2. Get image bytes — either decode base64 data URL or download from HTTP URL.
    let (image_bytes, extension) = if result.url.starts_with("data:") {
        decode_data_url(&result.url)?
    } else {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| ToolError::Http(format!("failed to create HTTP client: {e}")))?;

        let bytes = http_client
            .get(&result.url)
            .send()
            .await
            .map_err(|e| ToolError::Http(format!("failed to download image: {e}")))?
            .bytes()
            .await
            .map_err(|e| ToolError::Http(format!("failed to read image bytes: {e}")))?;

        (bytes.to_vec(), "png".to_string())
    };

    // 3. Save to image directory.
    let image_dir = std::path::Path::new(ctx.image_dir());
    let generated_dir = image_dir.join("generated");
    std::fs::create_dir_all(&generated_dir)
        .map_err(|e| ToolError::Io(format!("failed to create directory: {e}")))?;

    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let filename = format!("{timestamp}.{extension}");
    let save_path = generated_dir.join(&filename);

    std::fs::write(&save_path, &image_bytes)
        .map_err(|e| ToolError::Io(format!("failed to save image: {e}")))?;

    let relative_path = format!("generated/{filename}");

    // 4. Create memory entry for the generated image.
    let summary = if result.revised_prompt.is_empty() {
        prompt.to_string()
    } else {
        result.revised_prompt.clone()
    };
    let now = chrono::Local::now().to_rfc3339();
    let entry = crate::memory::db::Entry {
        id: format!("img_{}", uuid::Uuid::new_v4()),
        memory_type: "image".into(),
        source: "tool".into(),
        reason: "generate_image".into(),
        status: "active".into(),
        confidence: 1.0,
        summary_text: summary,
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
        collated_at: String::new(),
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

/// Decode a `data:image/{format};base64,{data}` URL into raw bytes and file extension.
fn decode_data_url(url: &str) -> Result<(Vec<u8>, String), ToolError> {
    let rest = url
        .strip_prefix("data:image/")
        .ok_or_else(|| ToolError::Io("data URL is not an image".into()))?;

    let (mime_subtype, b64_data) = rest
        .split_once(";base64,")
        .ok_or_else(|| ToolError::Io("data URL missing ;base64, separator".into()))?;

    let extension = match mime_subtype {
        "jpeg" => "jpg",
        other => other,
    }
    .to_string();

    let bytes = BASE64
        .decode(b64_data)
        .map_err(|e| ToolError::Io(format!("failed to decode base64 image: {e}")))?;

    Ok((bytes, extension))
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
        assert_eq!(defs.len(), 5);

        let names: Vec<&str> = defs.iter().map(|d| d.name).collect();
        assert!(names.contains(&"send_image"));
        assert!(names.contains(&"list_images"));
        assert!(names.contains(&"recall_image"));
        assert!(names.contains(&"remember_image"));
        assert!(names.contains(&"generate_image"));

        // remember_image and generate_image are MemoryWrite (they produce side effects).
        assert_eq!(
            defs.iter()
                .find(|d| d.name == "remember_image")
                .unwrap()
                .category,
            ToolCategory::MemoryWrite
        );
        assert_eq!(
            defs.iter()
                .find(|d| d.name == "generate_image")
                .unwrap()
                .category,
            ToolCategory::MemoryWrite
        );

        // send_image, list_images, and recall_image are MemoryRead.
        assert_eq!(
            defs.iter()
                .find(|d| d.name == "send_image")
                .unwrap()
                .category,
            ToolCategory::MemoryRead
        );
        assert_eq!(
            defs.iter()
                .find(|d| d.name == "list_images")
                .unwrap()
                .category,
            ToolCategory::MemoryRead
        );
        assert_eq!(
            defs.iter()
                .find(|d| d.name == "recall_image")
                .unwrap()
                .category,
            ToolCategory::MemoryRead
        );
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
        let result =
            handle_generate_image(json!({"prompt": "a cat", "size": "512x512"}), &ctx).await;
        // Without LLM client configured, should return an Io error.
        assert!(matches!(result, Err(ToolError::Io(_))));
    }

    #[tokio::test]
    async fn test_send_image_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let image_path = tmp.path().join("photo.png");
        std::fs::write(&image_path, b"fake image data").unwrap();

        let ctx = TestToolContext::new().with_image_dir(tmp.path().to_str().unwrap());

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

        let ctx = TestToolContext::new().with_image_dir(tmp.path().to_str().unwrap());

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
        let ctx = TestToolContext::new().with_rag(vec![RagHit {
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

    // ── decode_data_url tests ──────────────────────────────────────────

    #[test]
    fn test_decode_data_url_png() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let raw = b"fake png bytes";
        let encoded = STANDARD.encode(raw);
        let url = format!("data:image/png;base64,{encoded}");

        let (bytes, ext) = decode_data_url(&url).unwrap();
        assert_eq!(bytes, raw);
        assert_eq!(ext, "png");
    }

    #[test]
    fn test_decode_data_url_jpeg() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let raw = b"fake jpeg bytes";
        let encoded = STANDARD.encode(raw);
        let url = format!("data:image/jpeg;base64,{encoded}");

        let (bytes, ext) = decode_data_url(&url).unwrap();
        assert_eq!(bytes, raw);
        assert_eq!(ext, "jpg");
    }

    #[test]
    fn test_decode_data_url_webp() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let raw = b"fake webp bytes";
        let encoded = STANDARD.encode(raw);
        let url = format!("data:image/webp;base64,{encoded}");

        let (bytes, ext) = decode_data_url(&url).unwrap();
        assert_eq!(bytes, raw);
        assert_eq!(ext, "webp");
    }

    #[test]
    fn test_decode_data_url_not_image() {
        let url = "data:text/plain;base64,aGVsbG8=";
        let result = decode_data_url(url);
        assert!(matches!(result, Err(ToolError::Io(_))));
    }

    #[test]
    fn test_decode_data_url_missing_base64() {
        let url = "data:image/png,raw-data";
        let result = decode_data_url(url);
        assert!(matches!(result, Err(ToolError::Io(_))));
    }

    // ── entry ID resolution tests ─────────────────────────────────────

    #[tokio::test]
    async fn test_send_image_by_entry_id() {
        let tmp = tempfile::tempdir().unwrap();
        let image_path = tmp.path().join("sunset.png");
        std::fs::write(&image_path, b"fake image data").unwrap();

        let ctx = TestToolContext::new().with_image_dir(tmp.path().to_str().unwrap());

        let entry = make_image_entry("img_sunset_001", "A sunset photo", "sunset.png");
        ctx.db.create_entry(&entry).unwrap();

        let result = handle_send_image(json!({"path": "img_sunset_001"}), &ctx)
            .await
            .unwrap();

        assert_eq!(result["sent"], true);
        assert!(result["path"].as_str().unwrap().contains("sunset.png"));
    }

    #[tokio::test]
    async fn test_recall_image_by_entry_id() {
        let tmp = tempfile::tempdir().unwrap();
        let image_path = tmp.path().join("cat.jpg");
        std::fs::write(&image_path, b"fake image data").unwrap();

        let ctx = TestToolContext::new().with_image_dir(tmp.path().to_str().unwrap());

        let entry = make_image_entry("img_cat_001", "A cat photo", "cat.jpg");
        ctx.db.create_entry(&entry).unwrap();

        let result = handle_recall_image(json!({"path": "img_cat_001"}), &ctx)
            .await
            .unwrap();

        assert_eq!(result["exists"], true);
        assert!(result["path"].as_str().unwrap().contains("cat.jpg"));
    }

    #[tokio::test]
    async fn test_send_image_entry_id_not_found() {
        let ctx = TestToolContext::new().with_image_dir("/tmp");
        let result = handle_send_image(json!({"path": "img_nonexistent"}), &ctx).await;
        assert!(matches!(result, Err(ToolError::Io(_))));
    }
}
