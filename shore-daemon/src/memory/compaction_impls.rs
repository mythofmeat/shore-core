//! Production implementations of the compaction traits.
//!
//! `RealCompactionLlm` — sends prompts to shore-llm via `LlmClient`, returns raw text.
//! `RealVectorIndexer` — embeds text via shore-llm's `/v1/embed` and stores in LanceDB.
//! `RealConversationManager` — archives with retention and writes recap.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use shore_config::models::ResolvedModel;
use crate::engine::segments::{CompactionManifest, SegmentEntry};
use shore_llm_client::types::ContentBlock;
use shore_llm_client::LlmClient;

use super::compaction::{CompactionError, CompactionLlm, ConversationManager, RetentionParams, VectorIndexer};
use super::vectorstore::VectorStore;

// ---------------------------------------------------------------------------
// Embedding configuration
// ---------------------------------------------------------------------------

/// Resolved embedding model configuration extracted from raw TOML catalog.
#[derive(Debug, Clone)]
pub struct EmbedConfig {
    pub provider: String,
    pub model_id: String,
    pub api_key: String,
    pub base_url: Option<String>,
    /// Embedding vector dimensions (defaults to 1536).
    pub dimensions: i32,
}

/// Resolve embedding config from the raw TOML catalog entry.
///
/// Looks up the default embedding profile name, finds the raw TOML entry,
/// extracts `model_id` and provider, and resolves the API key from the
/// environment.
pub fn resolve_embed_config(
    default_name: Option<&str>,
    embedding_catalog: &std::collections::BTreeMap<String, toml::Value>,
) -> Result<EmbedConfig, CompactionError> {
    // Determine which embedding profile to use.
    let profile_name = default_name
        .or_else(|| embedding_catalog.keys().next().map(|s| s.as_str()))
        .ok_or_else(|| {
            CompactionError::Indexing("no embedding model configured".to_string())
        })?;

    let entry = embedding_catalog.get(profile_name).ok_or_else(|| {
        CompactionError::Indexing(format!(
            "embedding profile '{}' not found in model catalog",
            profile_name
        ))
    })?;

    let model_id = entry
        .get("model_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            CompactionError::Indexing(format!(
                "embedding profile '{}' is missing model_id",
                profile_name
            ))
        })?
        .to_string();

    // Provider is the parent table key in full config, but in raw TOML it may
    // be stored as a field. Fall back to "openai" (most embedding models).
    let provider = entry
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("openai")
        .to_string();

    // Resolve API key from environment.
    let api_key_env = entry
        .get("api_key_env")
        .and_then(|v| v.as_str())
        .unwrap_or("OPENAI_API_KEY");

    let api_key = std::env::var(api_key_env).map_err(|_| {
        CompactionError::Indexing(format!(
            "embedding API key env var '{}' is not set",
            api_key_env
        ))
    })?;

    let base_url = entry
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let dimensions = entry
        .get("dimensions")
        .and_then(|v| v.as_integer())
        .unwrap_or(1536) as i32;

    Ok(EmbedConfig {
        provider,
        model_id,
        api_key,
        base_url,
        dimensions,
    })
}

// ---------------------------------------------------------------------------
// Image generation configuration
// ---------------------------------------------------------------------------

/// Resolved image generation configuration extracted from raw TOML catalog.
#[derive(Debug, Clone)]
pub struct ImageGenConfig {
    pub provider: String,
    pub model_id: String,
    pub api_key: String,
    pub base_url: Option<String>,
    /// Default size for OpenAI path (e.g. "1024x1024").
    pub size: String,
    /// Optional quality hint for OpenAI path (e.g. "hd").
    pub quality: Option<String>,
    /// OpenRouter aspect ratio (e.g. "1:1", "16:9").
    pub aspect_ratio: Option<String>,
    /// OpenRouter image size (e.g. "1K", "2K", "4K").
    pub image_size: Option<String>,
}

/// Resolve image generation config from the raw TOML catalog entry.
///
/// Looks up the default image generation profile name, finds the raw TOML
/// entry, extracts `model_id` and provider, and resolves the API key from
/// the environment.
pub fn resolve_image_gen_config(
    default_name: Option<&str>,
    image_gen_catalog: &std::collections::BTreeMap<String, toml::Value>,
) -> Result<ImageGenConfig, String> {
    let profile_name = default_name
        .or_else(|| image_gen_catalog.keys().next().map(|s| s.as_str()))
        .ok_or_else(|| "no image generation model configured".to_string())?;

    let entry = image_gen_catalog.get(profile_name).ok_or_else(|| {
        format!(
            "image generation profile '{}' not found in model catalog",
            profile_name
        )
    })?;

    let model_id = entry
        .get("model_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            format!(
                "image generation profile '{}' is missing model_id",
                profile_name
            )
        })?
        .to_string();

    let provider = entry
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("openai")
        .to_string();

    let default_api_key_env = match provider.as_str() {
        "openrouter" => "OPENROUTER_API_KEY",
        _ => "OPENAI_API_KEY",
    };
    let api_key_env = entry
        .get("api_key_env")
        .and_then(|v| v.as_str())
        .unwrap_or(default_api_key_env);

    let api_key = std::env::var(api_key_env).map_err(|_| {
        format!(
            "image generation API key env var '{}' is not set",
            api_key_env
        )
    })?;

    let base_url = entry
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let size = entry
        .get("size")
        .and_then(|v| v.as_str())
        .unwrap_or("1024x1024")
        .to_string();

    let quality = entry
        .get("quality")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let aspect_ratio = entry
        .get("aspect_ratio")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let image_size = entry
        .get("image_size")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(ImageGenConfig {
        provider,
        model_id,
        api_key,
        base_url,
        size,
        quality,
        aspect_ratio,
        image_size,
    })
}

// ---------------------------------------------------------------------------
// RealCompactionLlm
// ---------------------------------------------------------------------------

/// Production `CompactionLlm` backed by `LlmClient` (Unix socket to shore-llm).
///
/// Returns raw LLM text — the compaction library handles XML parsing.
pub struct RealCompactionLlm {
    client: LlmClient,
    model: ResolvedModel,
}

impl RealCompactionLlm {
    pub fn new(client: LlmClient, model: ResolvedModel) -> Self {
        Self { client, model }
    }
}

impl CompactionLlm for RealCompactionLlm {
    fn summarize(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, CompactionError>> + Send + '_>> {
        let prompt = prompt.to_string();
        Box::pin(async move {
            let messages = vec![json!({"role": "user", "content": prompt})];

            let request = LlmClient::build_request(&self.model, messages, None, None, None)
                .map_err(|e| CompactionError::Llm(e.to_string()))?;

            eprintln!("[compact-timing] summarize: starting LLM generate, prompt_len={}", prompt.len());
            let t0 = std::time::Instant::now();
            let resp = self
                .client
                .generate(&request, None)
                .await
                .map_err(|e| { eprintln!("[compact-timing] summarize: LLM generate FAILED in {:?}: {}", t0.elapsed(), e); CompactionError::Llm(e.to_string()) })?;
            eprintln!("[compact-timing] summarize: LLM generate done in {:?}, content_len={}", t0.elapsed(), resp.content.len());

            // Extract text from content blocks, falling back to content field.
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

            Ok(text)
        })
    }
}

// ---------------------------------------------------------------------------
// RealVectorIndexer
// ---------------------------------------------------------------------------

/// Production `VectorIndexer` that embeds text via shore-llm and stores
/// vectors in a LanceDB-backed `VectorStore`.
pub struct RealVectorIndexer {
    store: VectorStore,
    client: LlmClient,
    embed_config: EmbedConfig,
}

impl RealVectorIndexer {
    pub fn new(store: VectorStore, client: LlmClient, embed_config: EmbedConfig) -> Self {
        Self {
            store,
            client,
            embed_config,
        }
    }
}

impl VectorIndexer for RealVectorIndexer {
    fn index_entry(
        &self,
        entry_id: &str,
        text: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), CompactionError>> + Send + '_>> {
        let entry_id = entry_id.to_string();
        let text = text.to_string();

        Box::pin(async move {
            eprintln!("[compact-timing] index_entry '{}': starting embed", &entry_id);
            let t0 = std::time::Instant::now();
            let embedding = self
                .client
                .embed(
                    &self.embed_config.provider,
                    &self.embed_config.model_id,
                    &self.embed_config.api_key,
                    self.embed_config.base_url.as_deref(),
                    &[text.as_str()],
                )
                .await
                .map_err(|e| { eprintln!("[compact-timing] index_entry '{}': embed FAILED in {:?}: {}", &entry_id, t0.elapsed(), e); CompactionError::Indexing(e.to_string()) })?;
            eprintln!("[compact-timing] index_entry '{}': embed done in {:?}", &entry_id, t0.elapsed());

            let vec = embedding
                .first()
                .ok_or_else(|| CompactionError::Indexing("empty embedding response".to_string()))?;

            self.store
                .index_entry(&entry_id, vec)
                .await
                .map_err(|e| CompactionError::Indexing(e.to_string()))?;

            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// RealConversationManager
// ---------------------------------------------------------------------------

/// Production `ConversationManager` that archives conversations to segment
/// files with message retention and recap writing.
pub struct RealConversationManager {
    character_dir: PathBuf,
}

impl RealConversationManager {
    pub fn new(character_dir: &Path) -> Self {
        Self {
            character_dir: character_dir.to_path_buf(),
        }
    }
}

impl ConversationManager for RealConversationManager {
    fn archive_and_retain(
        &self,
        _conversation_id: &str,
        params: RetentionParams,
    ) -> Result<String, CompactionError> {
        let active_path = self.character_dir.join("active.jsonl");
        let content = std::fs::read_to_string(&active_path).map_err(|e| {
            CompactionError::ConversationManager(format!("failed to read active.jsonl: {e}"))
        })?;

        // Split lines into archive vs retain portions.
        let lines: Vec<&str> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();

        let keep = params.keep_last_n.min(lines.len());
        let split_at = lines.len() - keep;
        let archive_lines = &lines[..split_at];
        let retained_lines = &lines[split_at..];

        // Archive the compacted portion to a segment file.
        if !archive_lines.is_empty() {
            let manifest_path = self.character_dir.join("compaction.json");
            let mut manifest: CompactionManifest = if manifest_path.exists() {
                let mf = std::fs::read_to_string(&manifest_path).map_err(|e| {
                    CompactionError::ConversationManager(format!(
                        "failed to read compaction.json: {e}"
                    ))
                })?;
                serde_json::from_str(&mf).map_err(|e| {
                    CompactionError::ConversationManager(format!(
                        "failed to parse compaction.json: {e}"
                    ))
                })?
            } else {
                CompactionManifest::default()
            };

            let segment_index = manifest.segments.len() + 1;
            let segment_file = format!("{:04}.jsonl", segment_index);
            let segments_dir = self.character_dir.join("segments");

            std::fs::create_dir_all(&segments_dir).map_err(|e| {
                CompactionError::ConversationManager(format!(
                    "failed to create segments dir: {e}"
                ))
            })?;

            // Write segment (archived messages only).
            let segment_content = archive_lines.join("\n") + "\n";
            std::fs::write(segments_dir.join(&segment_file), &segment_content).map_err(|e| {
                CompactionError::ConversationManager(format!(
                    "failed to write segment file: {e}"
                ))
            })?;

            manifest.segments.push(SegmentEntry {
                file: segment_file,
                message_count: archive_lines.len(),
                compacted_at: Utc::now().to_rfc3339(),
            });
            manifest.total_compacted_messages += archive_lines.len();

            let manifest_json = serde_json::to_string_pretty(&manifest).map_err(|e| {
                CompactionError::ConversationManager(format!(
                    "failed to serialize manifest: {e}"
                ))
            })?;
            std::fs::write(&manifest_path, manifest_json).map_err(|e| {
                CompactionError::ConversationManager(format!(
                    "failed to write compaction.json: {e}"
                ))
            })?;
        }

        // Write retained messages back to active.jsonl.
        let retained_content = if retained_lines.is_empty() {
            String::new()
        } else {
            retained_lines.join("\n") + "\n"
        };
        std::fs::write(&active_path, &retained_content).map_err(|e| {
            CompactionError::ConversationManager(format!(
                "failed to write retained messages: {e}"
            ))
        })?;

        // Write recap if provided.
        if let Some(recap) = &params.recap {
            let memory_dir = self.character_dir.join("memory");
            std::fs::create_dir_all(&memory_dir).map_err(|e| {
                CompactionError::ConversationManager(format!(
                    "failed to create memory dir: {e}"
                ))
            })?;
            std::fs::write(memory_dir.join("recap.md"), recap.trim().to_owned() + "\n")
                .map_err(|e| {
                    CompactionError::ConversationManager(format!(
                        "failed to write recap.md: {e}"
                    ))
                })?;
        }

        Ok(Uuid::new_v4().to_string())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -- RealConversationManager: archive_and_retain --------------------------

    #[test]
    fn archive_and_retain_splits_messages() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        let msg1 = r#"{"msg_id":"m1","role":"User","content":"hello","images":[],"timestamp":"t1"}"#;
        let msg2 = r#"{"msg_id":"m2","role":"Assistant","content":"hi","images":[],"timestamp":"t2"}"#;
        let msg3 = r#"{"msg_id":"m3","role":"User","content":"bye","images":[],"timestamp":"t3"}"#;
        std::fs::write(
            dir.join("active.jsonl"),
            format!("{msg1}\n{msg2}\n{msg3}\n"),
        )
        .unwrap();

        let mgr = RealConversationManager::new(dir);
        let new_id = mgr
            .archive_and_retain(
                "test-conv",
                RetentionParams {
                    keep_last_n: 1,
                    recap: None,
                },
            )
            .unwrap();

        assert!(!new_id.is_empty());

        // Segment should have the first 2 messages.
        let seg = std::fs::read_to_string(dir.join("segments/0001.jsonl")).unwrap();
        assert!(seg.contains("m1"));
        assert!(seg.contains("m2"));
        assert!(!seg.contains("m3"));

        // active.jsonl should have only the retained message.
        let active = std::fs::read_to_string(dir.join("active.jsonl")).unwrap();
        assert!(!active.contains("m1"));
        assert!(!active.contains("m2"));
        assert!(active.contains("m3"));

        // Manifest should be updated.
        let manifest: CompactionManifest = serde_json::from_str(
            &std::fs::read_to_string(dir.join("compaction.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.segments.len(), 1);
        assert_eq!(manifest.segments[0].message_count, 2);
        assert_eq!(manifest.total_compacted_messages, 2);
    }

    #[test]
    fn archive_and_retain_writes_recap() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        let msg = r#"{"msg_id":"m1","role":"User","content":"hello","images":[],"timestamp":"t1"}"#;
        std::fs::write(dir.join("active.jsonl"), format!("{msg}\n")).unwrap();

        let mgr = RealConversationManager::new(dir);
        mgr.archive_and_retain(
            "conv",
            RetentionParams {
                keep_last_n: 0,
                recap: Some("A recap of events.".to_string()),
            },
        )
        .unwrap();

        let recap = std::fs::read_to_string(dir.join("memory/recap.md")).unwrap();
        assert_eq!(recap, "A recap of events.\n");
    }

    #[test]
    fn archive_and_retain_no_recap_leaves_file_alone() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        // Pre-existing recap.
        std::fs::create_dir_all(dir.join("memory")).unwrap();
        std::fs::write(dir.join("memory/recap.md"), "old recap").unwrap();

        let msg = r#"{"msg_id":"m1","role":"User","content":"hello","images":[],"timestamp":"t1"}"#;
        std::fs::write(dir.join("active.jsonl"), format!("{msg}\n")).unwrap();

        let mgr = RealConversationManager::new(dir);
        mgr.archive_and_retain(
            "conv",
            RetentionParams {
                keep_last_n: 0,
                recap: None,
            },
        )
        .unwrap();

        let recap = std::fs::read_to_string(dir.join("memory/recap.md")).unwrap();
        assert_eq!(recap, "old recap");
    }

    #[test]
    fn archive_and_retain_keep_all() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        let msg1 = r#"{"msg_id":"m1","role":"User","content":"hello","images":[],"timestamp":"t1"}"#;
        let msg2 = r#"{"msg_id":"m2","role":"User","content":"world","images":[],"timestamp":"t2"}"#;
        std::fs::write(dir.join("active.jsonl"), format!("{msg1}\n{msg2}\n")).unwrap();

        let mgr = RealConversationManager::new(dir);
        mgr.archive_and_retain(
            "conv",
            RetentionParams {
                keep_last_n: 5, // more than available
                recap: None,
            },
        )
        .unwrap();

        // All messages retained, no segment created.
        let active = std::fs::read_to_string(dir.join("active.jsonl")).unwrap();
        assert!(active.contains("m1"));
        assert!(active.contains("m2"));
        assert!(!dir.join("segments").exists());
    }

    #[test]
    fn archive_increments_segment_number() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        // Create an existing manifest with one segment.
        let manifest = CompactionManifest {
            segments: vec![SegmentEntry {
                file: "0001.jsonl".into(),
                message_count: 5,
                compacted_at: "2026-01-01T00:00:00Z".into(),
            }],
            total_compacted_messages: 5,
        };
        std::fs::write(
            dir.join("compaction.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("segments")).unwrap();

        let msg = r#"{"msg_id":"m3","role":"User","content":"test","images":[],"timestamp":"t"}"#;
        std::fs::write(dir.join("active.jsonl"), format!("{msg}\n")).unwrap();

        let mgr = RealConversationManager::new(dir);
        mgr.archive_and_retain(
            "conv",
            RetentionParams {
                keep_last_n: 0,
                recap: None,
            },
        )
        .unwrap();

        assert!(dir.join("segments/0002.jsonl").exists());

        let manifest: CompactionManifest = serde_json::from_str(
            &std::fs::read_to_string(dir.join("compaction.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.segments.len(), 2);
        assert_eq!(manifest.segments[1].file, "0002.jsonl");
        assert_eq!(manifest.total_compacted_messages, 6);
    }
}
