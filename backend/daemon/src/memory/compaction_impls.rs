//! Production implementations of the compaction traits.
//!
//! `RealCompactionLlm` — sends prompts to shore-llm via `LlmClient`, returns raw text.
//! `RealConversationManager` — archives with retention.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use chrono::Local;
use serde_json::json;
use tracing::debug;
use uuid::Uuid;

use crate::engine::segments::{CompactionManifest, SegmentEntry};
use shore_config::models::ResolvedModel;
use shore_config::providers::ProviderRegistry;
use shore_ledger::{CallType, LedgerClient};

use super::compaction::{CompactionError, CompactionLlm, ConversationManager, RetentionParams};

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

/// Production `CompactionLlm` backed by `LedgerClient` (ledger-tracked LLM calls).
///
/// Returns raw LLM text — the compaction library handles XML parsing.
pub struct RealCompactionLlm {
    client: LedgerClient,
    model: ResolvedModel,
    /// Snapshot of the provider registry so the request honors
    /// `[providers.<name>].keys`. Without this, compaction would only
    /// look at `model.api_key_env` and fail with `MissingApiKey` for
    /// users who configure provider-level keys.
    providers: ProviderRegistry,
    character: String,
    http: Option<Arc<crate::http::DaemonHttpState>>,
}

impl RealCompactionLlm {
    pub fn new(
        client: LedgerClient,
        model: ResolvedModel,
        providers: ProviderRegistry,
        character: String,
        http: Option<Arc<crate::http::DaemonHttpState>>,
    ) -> Self {
        Self {
            client,
            model,
            providers,
            character,
            http,
        }
    }

    fn build_compaction_request(
        &self,
        system: &str,
        messages: Vec<serde_json::Value>,
        cached_request: Option<shore_llm::types::LlmRequest>,
    ) -> Result<shore_llm::types::LlmRequest, CompactionError> {
        let mut request = match cached_request {
            Some(cached) => {
                // Cache-preserving path: keep the live conversation's cached
                // prefix, but rebuild the request shell from the resolved
                // compaction model. This preserves compaction provider/model,
                // token, sampler, provider-option, and no-tool settings instead
                // of leaking chat request settings into background compaction.
                let mut request = LedgerClient::build_request_with_provider_keys(
                    &self.model,
                    &self.providers,
                    cached.messages,
                    cached.system,
                    None,
                    None,
                )
                .map_err(|e| CompactionError::Llm(e.to_string()))?;
                for msg in messages {
                    request.messages.push(msg);
                }
                request.messages.push(json!({
                    "role": "system",
                    "content": system,
                }));
                request
            }
            None => LedgerClient::build_request_with_provider_keys(
                &self.model,
                &self.providers,
                messages,
                Some(json!(system)),
                None,
                None,
            )
            .map_err(|e| CompactionError::Llm(e.to_string()))?,
        };

        request.rid = None;
        request.forensic_character = Some(self.character.clone());

        Ok(request)
    }
}

impl CompactionLlm for RealCompactionLlm {
    fn summarize(
        &self,
        system: &str,
        messages: Vec<serde_json::Value>,
        cached_request: Option<shore_llm::types::LlmRequest>,
    ) -> Pin<Box<dyn Future<Output = Result<String, CompactionError>> + Send + '_>> {
        let system = system.to_string();
        Box::pin(async move {
            let msg_count = messages.len();
            let cached_prefix_used = cached_request.is_some();
            let mut request = self.build_compaction_request(&system, messages, cached_request)?;
            let claude_code_session = crate::claude_code::prepare_request(
                &mut request,
                self.http.as_ref(),
                None,
                crate::claude_code::empty_tool_context(),
            )
            .await
            .map_err(CompactionError::Llm)?;

            debug!(
                system_len = system.len(),
                msg_count, cached_prefix_used, "compaction: starting LLM summarize"
            );
            let t0 = std::time::Instant::now();
            let mut resp = self
                .client
                .generate(&request, CallType::Compaction, &self.character, false)
                .await
                .map_err(|e| CompactionError::Llm(e.to_string()))?;
            crate::claude_code::splice_generate_response_from_session(
                &mut resp,
                claude_code_session.as_ref(),
            )
            .await;
            debug!(elapsed = ?t0.elapsed(), content_len = resp.content.len(), "compaction: LLM summarize done");

            Ok(resp.extract_text())
        })
    }
}

/// Production `ConversationManager` that archives conversations to segment
/// files with message retention.
pub struct RealConversationManager {
    character_dir: PathBuf,
}

impl RealConversationManager {
    pub fn new(character_dir: &Path) -> Self {
        Self {
            character_dir: character_dir.to_path_buf(),
        }
    }

    pub fn archive_and_retain(
        &self,
        _conversation_id: &str,
        params: RetentionParams,
    ) -> Result<String, CompactionError> {
        let started = std::time::Instant::now();
        let active_path = self.character_dir.join("active.jsonl");

        // Use pre-read content from params to avoid TOCTOU race — the file
        // may have changed since compact() parsed the messages.
        let lines: Vec<&str> = params
            .active_content
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
                CompactionError::ConversationManager(format!("failed to create segments dir: {e}"))
            })?;

            // Write segment (archived messages only).
            let segment_content = archive_lines.join("\n") + "\n";
            std::fs::write(segments_dir.join(&segment_file), &segment_content).map_err(|e| {
                CompactionError::ConversationManager(format!("failed to write segment file: {e}"))
            })?;

            manifest.segments.push(SegmentEntry {
                file: segment_file,
                message_count: archive_lines.len(),
                compacted_at: Local::now().to_rfc3339(),
            });
            manifest.total_compacted_messages += archive_lines.len();

            let manifest_json = serde_json::to_string_pretty(&manifest).map_err(|e| {
                CompactionError::ConversationManager(format!("failed to serialize manifest: {e}"))
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
        crate::engine::atomic::atomic_write(&active_path, retained_content.as_bytes()).map_err(
            |e| {
                CompactionError::ConversationManager(format!(
                    "failed to write retained messages: {e}"
                ))
            },
        )?;

        debug!(
            archived = archive_lines.len(),
            retained = retained_lines.len(),
            elapsed = ?started.elapsed(),
            "compaction: archive/retain file mutation complete"
        );
        Ok(Uuid::new_v4().to_string())
    }
}

impl ConversationManager for RealConversationManager {
    fn archive_and_retain(
        &self,
        conversation_id: &str,
        params: RetentionParams,
    ) -> Pin<Box<dyn Future<Output = Result<String, CompactionError>> + Send + '_>> {
        let character_dir = self.character_dir.clone();
        let conversation_id = conversation_id.to_string();

        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let mgr = RealConversationManager { character_dir };
                mgr.archive_and_retain(&conversation_id, params)
            })
            .await
            .map_err(|e| {
                CompactionError::ConversationManager(format!(
                    "archive_and_retain task failed to join: {e}"
                ))
            })?
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use shore_config::models::{ModelConfigFields, Sdk};
    use tempfile::TempDir;

    fn test_compaction_model(api_key_env: &str) -> ResolvedModel {
        ResolvedModel::from_parts(
            "compact".to_string(),
            "chat.anthropic.compact".to_string(),
            "chat".to_string(),
            "anthropic".to_string(),
            "compaction-model".to_string(),
            Sdk::Anthropic,
            ModelConfigFields {
                sdk: Some(Sdk::Anthropic),
                api_key_env: Some(api_key_env.to_string()),
                base_url: Some("http://compaction.example".to_string()),
                max_tokens: Some(777),
                temperature: Some(0.25),
                reasoning_effort: Some("medium".to_string()),
                ..Default::default()
            },
        )
    }

    #[test]
    fn cached_compaction_request_keeps_prefix_but_uses_compaction_settings() {
        let api_key_env = format!("SHORE_TEST_COMPACTION_{}", uuid::Uuid::new_v4().simple());
        std::env::set_var(&api_key_env, "compaction-secret");
        let model = test_compaction_model(&api_key_env);
        let ledger_tmp = TempDir::new().unwrap();
        let llm = RealCompactionLlm::new(
            LedgerClient::new(
                shore_llm::LlmClient::new(),
                &ledger_tmp.path().join("ledger.db"),
            )
            .unwrap(),
            model,
            ProviderRegistry::default(),
            "alice".to_string(),
            None,
        );
        let cached = shore_llm::types::LlmRequest {
            sdk: Sdk::Anthropic,
            model: "chat-model".to_string(),
            api_key: "chat-secret".to_string(),
            base_url: Some("http://chat.example".to_string()),
            messages: vec![
                json!({"role": "user", "content": "cached user"}),
                json!({"role": "assistant", "content": "cached assistant"}),
            ],
            system: Some(json!("cached system")),
            tools: Some(vec![json!({
                "name": "read",
                "description": "chat tool that compaction must not inherit",
                "input_schema": { "type": "object" }
            })]),
            max_tokens: 42,
            temperature: Some(0.9),
            top_p: Some(0.8),
            provider_options: Some(json!({
                "cache_ttl": "1h",
                "chat_only": true
            })),
            provider_key: Some("anthropic".to_string()),
            rid: Some("rid-chat".to_string()),
            forensic_character: Some("chat-forensics".to_string()),
        };

        let request = llm
            .build_compaction_request(
                "compaction system",
                vec![json!({"role": "user", "content": "compact now"})],
                Some(cached),
            )
            .unwrap();

        std::env::remove_var(&api_key_env);

        assert_eq!(request.model, "compaction-model");
        assert_eq!(request.api_key, "compaction-secret");
        assert_eq!(
            request.base_url.as_deref(),
            Some("http://compaction.example")
        );
        assert_eq!(request.max_tokens, 777);
        assert_eq!(request.temperature, Some(0.25));
        assert_eq!(request.top_p, None);
        assert!(request.tools.is_none());
        assert_eq!(request.rid, None);
        assert_eq!(request.forensic_character.as_deref(), Some("alice"));
        assert_eq!(request.system, Some(json!("cached system")));
        assert_eq!(request.messages.len(), 4);
        assert_eq!(request.messages[0]["content"], "cached user");
        assert_eq!(request.messages[1]["content"], "cached assistant");
        assert_eq!(request.messages[2]["content"], "compact now");
        assert_eq!(request.messages[3]["role"], "system");
        assert_eq!(request.messages[3]["content"], "compaction system");

        let provider_options = request.provider_options.expect("provider options");
        assert_eq!(provider_options["reasoning_effort"], "medium");
        assert!(provider_options.get("chat_only").is_none());
    }

    // -- RealConversationManager: archive_and_retain --------------------------

    #[test]
    fn archive_and_retain_splits_messages() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        let msg1 =
            r#"{"msg_id":"m1","role":"User","content":"hello","images":[],"timestamp":"t1"}"#;
        let msg2 =
            r#"{"msg_id":"m2","role":"Assistant","content":"hi","images":[],"timestamp":"t2"}"#;
        let msg3 = r#"{"msg_id":"m3","role":"User","content":"bye","images":[],"timestamp":"t3"}"#;
        let content = format!("{msg1}\n{msg2}\n{msg3}\n");
        std::fs::write(dir.join("active.jsonl"), &content).unwrap();

        let mgr = RealConversationManager::new(dir);
        let new_id = mgr
            .archive_and_retain(
                "test-conv",
                RetentionParams {
                    keep_last_n: 1,
                    active_content: content,
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
        let manifest: CompactionManifest =
            serde_json::from_str(&std::fs::read_to_string(dir.join("compaction.json")).unwrap())
                .unwrap();
        assert_eq!(manifest.segments.len(), 1);
        assert_eq!(manifest.segments[0].message_count, 2);
        assert_eq!(manifest.total_compacted_messages, 2);
    }

    #[test]
    fn archive_and_retain_keep_all() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        let msg1 =
            r#"{"msg_id":"m1","role":"User","content":"hello","images":[],"timestamp":"t1"}"#;
        let msg2 =
            r#"{"msg_id":"m2","role":"User","content":"world","images":[],"timestamp":"t2"}"#;
        let content = format!("{msg1}\n{msg2}\n");
        std::fs::write(dir.join("active.jsonl"), &content).unwrap();

        let mgr = RealConversationManager::new(dir);
        mgr.archive_and_retain(
            "conv",
            RetentionParams {
                keep_last_n: 5, // more than available
                active_content: content,
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
        let content = format!("{msg}\n");
        std::fs::write(dir.join("active.jsonl"), &content).unwrap();

        let mgr = RealConversationManager::new(dir);
        mgr.archive_and_retain(
            "conv",
            RetentionParams {
                keep_last_n: 0,
                active_content: content,
            },
        )
        .unwrap();

        assert!(dir.join("segments/0002.jsonl").exists());

        let manifest: CompactionManifest =
            serde_json::from_str(&std::fs::read_to_string(dir.join("compaction.json")).unwrap())
                .unwrap();
        assert_eq!(manifest.segments.len(), 2);
        assert_eq!(manifest.segments[1].file, "0002.jsonl");
        assert_eq!(manifest.total_compacted_messages, 6);
    }

    #[test]
    fn archive_with_malformed_jsonl_lines() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        let valid1 =
            r#"{"msg_id":"m1","role":"User","content":"hello","images":[],"timestamp":"t1"}"#;
        let garbage = r#"corrupted{{{not valid json at all"#;
        let valid2 =
            r#"{"msg_id":"m2","role":"User","content":"bye","images":[],"timestamp":"t2"}"#;
        let content = format!("{valid1}\n{garbage}\n{valid2}\n");
        std::fs::write(dir.join("active.jsonl"), &content).unwrap();

        let mgr = RealConversationManager::new(dir);
        let new_id = mgr
            .archive_and_retain(
                "conv",
                RetentionParams {
                    keep_last_n: 1,
                    active_content: content,
                },
            )
            .unwrap();
        assert!(!new_id.is_empty());

        // Segment should contain the first 2 lines (valid + garbage).
        let seg = std::fs::read_to_string(dir.join("segments/0001.jsonl")).unwrap();
        assert!(seg.contains("m1"));
        assert!(seg.contains("corrupted{{{"));
        assert!(!seg.contains("m2"));

        // active.jsonl retains only the last valid message.
        let active = std::fs::read_to_string(dir.join("active.jsonl")).unwrap();
        assert!(active.contains("m2"));
        assert!(!active.contains("m1"));

        // Manifest reflects line count (not JSON validity).
        let manifest: CompactionManifest =
            serde_json::from_str(&std::fs::read_to_string(dir.join("compaction.json")).unwrap())
                .unwrap();
        assert_eq!(manifest.segments[0].message_count, 2);
    }

    #[test]
    fn archive_segment_write_fails_on_readonly_dir() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        let msg1 =
            r#"{"msg_id":"m1","role":"User","content":"hello","images":[],"timestamp":"t1"}"#;
        let msg2 =
            r#"{"msg_id":"m2","role":"User","content":"world","images":[],"timestamp":"t2"}"#;
        let content = format!("{msg1}\n{msg2}\n");
        std::fs::write(dir.join("active.jsonl"), &content).unwrap();

        // Pre-create the segments dir as read-only so the write fails.
        let segments_dir = dir.join("segments");
        std::fs::create_dir_all(&segments_dir).unwrap();
        std::fs::set_permissions(&segments_dir, std::fs::Permissions::from_mode(0o444)).unwrap();

        let mgr = RealConversationManager::new(dir);
        let result = mgr.archive_and_retain(
            "conv",
            RetentionParams {
                keep_last_n: 1,
                active_content: content,
            },
        );

        // Restore permissions so TempDir cleanup succeeds.
        std::fs::set_permissions(&segments_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("failed to write segment file"),
            "Expected segment write error, got: {err_msg}"
        );
    }

    #[test]
    fn archive_manifest_write_fails_on_readonly_parent() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        let msg1 =
            r#"{"msg_id":"m1","role":"User","content":"hello","images":[],"timestamp":"t1"}"#;
        let msg2 =
            r#"{"msg_id":"m2","role":"User","content":"world","images":[],"timestamp":"t2"}"#;
        let content = format!("{msg1}\n{msg2}\n");
        std::fs::write(dir.join("active.jsonl"), &content).unwrap();

        // Segments dir is writable (so segment write succeeds), but make the
        // parent dir read-only AFTER writing active.jsonl so manifest write fails.
        // Actually, we need to let segments dir creation succeed then block the
        // manifest write. Easiest: pre-create segments dir, pre-create
        // compaction.json as a directory so the write fails.
        std::fs::create_dir_all(dir.join("segments")).unwrap();
        // Create compaction.json as a directory — writing to it will fail.
        std::fs::create_dir_all(dir.join("compaction.json")).unwrap();

        let mgr = RealConversationManager::new(dir);
        let result = mgr.archive_and_retain(
            "conv",
            RetentionParams {
                keep_last_n: 1,
                active_content: content,
            },
        );

        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("compaction.json"),
            "Expected compaction.json error, got: {err_msg}"
        );
    }

    #[test]
    fn archive_empty_content_is_noop() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        // Empty active_content means nothing to archive or retain.
        let mgr = RealConversationManager::new(dir);
        let result = mgr.archive_and_retain(
            "conv",
            RetentionParams {
                keep_last_n: 1,
                active_content: String::new(),
            },
        );

        // Should succeed — no segments created, empty active.jsonl written.
        assert!(result.is_ok());
        assert!(!dir.join("segments").exists());
    }
}
