//! Production implementations of the compaction traits.
//!
//! `RealCompactionLlm` — sends prompts to shore-llm via `LlmClient`.
//! `RealVectorIndexer` — embeds text via shore-llm's `/v1/embed` and stores in LanceDB.
//! `RealConversationManager` — archives conversations to segment files and creates new ones.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::config::models::ResolvedModel;
use crate::engine::segments::{CompactionManifest, SegmentEntry};
use crate::llm_client::types::ContentBlock;
use crate::llm_client::LlmClient;

use super::compaction::{CompactedEntry, CompactionError, CompactionLlm, ConversationManager, VectorIndexer};
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
// RealCompactionLlm
// ---------------------------------------------------------------------------

/// Production `CompactionLlm` backed by `LlmClient` (Unix socket to shore-llm).
pub struct RealCompactionLlm {
    client: LlmClient,
    model: ResolvedModel,
}

impl RealCompactionLlm {
    pub fn new(client: LlmClient, model: ResolvedModel) -> Self {
        Self { client, model }
    }
}

/// Parsed JSON response from the compaction LLM call.
#[derive(Deserialize)]
struct CompactionResponse {
    entries: Vec<CompactionResponseEntry>,
}

#[derive(Deserialize)]
struct CompactionResponseEntry {
    memory_type: String,
    summary_text: String,
    topic_tags: String,
    topic_key: String,
    confidence: f64,
}

impl CompactionLlm for RealCompactionLlm {
    fn summarize(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CompactedEntry>, CompactionError>> + Send + '_>>
    {
        let prompt = prompt.to_string();
        Box::pin(async move {
            let messages = vec![json!({"role": "user", "content": prompt})];

            let request = LlmClient::build_request(&self.model, messages, None, None, None)
                .map_err(|e| CompactionError::Llm(e.to_string()))?;

            let resp = self
                .client
                .generate(&request, None)
                .await
                .map_err(|e| CompactionError::Llm(e.to_string()))?;

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

            // The LLM may wrap JSON in markdown code fences — strip them.
            let json_str = extract_json(&text);

            let parsed: CompactionResponse = serde_json::from_str(json_str)
                .map_err(|e| CompactionError::Llm(format!("failed to parse compaction JSON: {e}")))?;

            Ok(parsed
                .entries
                .into_iter()
                .map(|e| CompactedEntry {
                    memory_type: e.memory_type,
                    summary_text: e.summary_text,
                    topic_tags: e.topic_tags,
                    topic_key: e.topic_key,
                    confidence: e.confidence,
                })
                .collect())
        })
    }
}

/// Extract JSON from text that may be wrapped in markdown code fences.
fn extract_json(text: &str) -> &str {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("```json") {
        if let Some(json) = rest.strip_suffix("```") {
            return json.trim();
        }
    }
    if let Some(rest) = trimmed.strip_prefix("```") {
        if let Some(json) = rest.strip_suffix("```") {
            return json.trim();
        }
    }
    trimmed
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
                .map_err(|e| CompactionError::Indexing(e.to_string()))?;

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
/// files and creates new conversations by clearing `active.jsonl`.
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
    fn archive_conversation(&self, _conversation_id: &str) -> Result<(), CompactionError> {
        let active_path = self.character_dir.join("active.jsonl");
        let content = std::fs::read_to_string(&active_path).map_err(|e| {
            CompactionError::ConversationManager(format!("failed to read active.jsonl: {e}"))
        })?;

        // Nothing to archive if active conversation is empty.
        if content.trim().is_empty() {
            return Ok(());
        }

        // Load or create the compaction manifest.
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

        // Determine next segment file name.
        let segment_index = manifest.segments.len() + 1;
        let segment_file = format!("{:04}.jsonl", segment_index);
        let segments_dir = self.character_dir.join("segments");

        std::fs::create_dir_all(&segments_dir).map_err(|e| {
            CompactionError::ConversationManager(format!("failed to create segments dir: {e}"))
        })?;

        // Write the segment file.
        std::fs::write(segments_dir.join(&segment_file), &content).map_err(|e| {
            CompactionError::ConversationManager(format!("failed to write segment file: {e}"))
        })?;

        // Count messages in the content (one JSON object per non-empty line).
        let message_count = content.lines().filter(|l| !l.trim().is_empty()).count();

        // Update manifest.
        manifest.segments.push(SegmentEntry {
            file: segment_file,
            message_count,
            compacted_at: Utc::now().to_rfc3339(),
        });
        manifest.total_compacted_messages += message_count;

        let manifest_json = serde_json::to_string_pretty(&manifest).map_err(|e| {
            CompactionError::ConversationManager(format!("failed to serialize manifest: {e}"))
        })?;
        std::fs::write(&manifest_path, manifest_json).map_err(|e| {
            CompactionError::ConversationManager(format!("failed to write compaction.json: {e}"))
        })?;

        Ok(())
    }

    fn create_conversation(&self) -> Result<String, CompactionError> {
        let active_path = self.character_dir.join("active.jsonl");
        std::fs::write(&active_path, "").map_err(|e| {
            CompactionError::ConversationManager(format!("failed to truncate active.jsonl: {e}"))
        })?;
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

    // -- extract_json ---------------------------------------------------------

    #[test]
    fn extract_json_plain() {
        let input = r#"{"entries": []}"#;
        assert_eq!(extract_json(input), input);
    }

    #[test]
    fn extract_json_fenced() {
        let input = "```json\n{\"entries\": []}\n```";
        assert_eq!(extract_json(input), "{\"entries\": []}");
    }

    #[test]
    fn extract_json_fenced_no_lang() {
        let input = "```\n{\"entries\": []}\n```";
        assert_eq!(extract_json(input), "{\"entries\": []}");
    }

    // -- RealConversationManager ----------------------------------------------

    #[test]
    fn archive_creates_segment_and_updates_manifest() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        // Write an active conversation.
        let msg1 = r#"{"msg_id":"m1","role":"User","content":"hello","images":[],"timestamp":"2026-01-01T00:00:00Z"}"#;
        let msg2 = r#"{"msg_id":"m2","role":"Assistant","content":"hi","images":[],"timestamp":"2026-01-01T00:00:01Z"}"#;
        std::fs::write(dir.join("active.jsonl"), format!("{msg1}\n{msg2}\n")).unwrap();

        let mgr = RealConversationManager::new(dir);
        mgr.archive_conversation("test-conv").unwrap();

        // Segment file should exist.
        let seg_content = std::fs::read_to_string(dir.join("segments/0001.jsonl")).unwrap();
        assert!(seg_content.contains("m1"));
        assert!(seg_content.contains("m2"));

        // Manifest should be updated.
        let manifest_str = std::fs::read_to_string(dir.join("compaction.json")).unwrap();
        let manifest: CompactionManifest = serde_json::from_str(&manifest_str).unwrap();
        assert_eq!(manifest.segments.len(), 1);
        assert_eq!(manifest.segments[0].file, "0001.jsonl");
        assert_eq!(manifest.segments[0].message_count, 2);
        assert_eq!(manifest.total_compacted_messages, 2);
    }

    #[test]
    fn archive_empty_active_is_noop() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        std::fs::write(dir.join("active.jsonl"), "").unwrap();
        let mgr = RealConversationManager::new(dir);
        mgr.archive_conversation("test-conv").unwrap();

        // No segments dir created.
        assert!(!dir.join("segments").exists());
    }

    #[test]
    fn create_conversation_truncates_active() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        std::fs::write(dir.join("active.jsonl"), "some content").unwrap();
        let mgr = RealConversationManager::new(dir);
        let new_id = mgr.create_conversation().unwrap();

        assert!(!new_id.is_empty());
        let content = std::fs::read_to_string(dir.join("active.jsonl")).unwrap();
        assert!(content.is_empty());
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

        std::fs::write(dir.join("active.jsonl"), r#"{"msg_id":"m3","role":"User","content":"test","images":[],"timestamp":"2026-01-01T00:00:00Z"}"#).unwrap();
        std::fs::create_dir_all(dir.join("segments")).unwrap();

        let mgr = RealConversationManager::new(dir);
        mgr.archive_conversation("test-conv").unwrap();

        // Second segment should be 0002.jsonl.
        assert!(dir.join("segments/0002.jsonl").exists());

        let manifest_str = std::fs::read_to_string(dir.join("compaction.json")).unwrap();
        let manifest: CompactionManifest = serde_json::from_str(&manifest_str).unwrap();
        assert_eq!(manifest.segments.len(), 2);
        assert_eq!(manifest.segments[1].file, "0002.jsonl");
        assert_eq!(manifest.total_compacted_messages, 6);
    }

    // -- CompactionResponse parsing -------------------------------------------

    #[test]
    fn parse_compaction_response() {
        let json = r#"{"entries":[{"memory_type":"episodic","summary_text":"User had lunch","topic_tags":"daily,food","topic_key":"daily_life","confidence":0.85}]}"#;
        let parsed: CompactionResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].summary_text, "User had lunch");
        assert_eq!(parsed.entries[0].confidence, 0.85);
    }
}
