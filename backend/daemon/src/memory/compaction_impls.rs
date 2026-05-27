//! Production implementations of the compaction traits.
//!
//! `RealCompactionLlm` — sends prompts to shore-llm via `LlmClient`, returns raw text.
//! `RealConversationManager` — archives with retention.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use chrono::Local;
use tracing::debug;
use uuid::Uuid;

use crate::engine::segments::{CompactionManifest, SegmentEntry};
use shore_config::models::ResolvedModel;
use shore_config::providers::ProviderRegistry;
use shore_ledger::{CallType, LedgerClient};
use shore_llm::types::{GenerateResponse, LlmRequest};

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
// Compaction tail shape
// ---------------------------------------------------------------------------

/// Number of user messages appended to the chat prefix when building a
/// compaction request. Enforced structurally by the
/// `CompactionLlm::build_initial_request` signature (a single `Value`, not
/// an array), so a future caller cannot pass more without changing the
/// trait.
///
/// The compaction system prompt rides as `system_suffix`, not as another
/// trailing entry in `messages` — shore-llm's `preprocess_request` expands
/// the suffix into a `role: "system"` block at provider dispatch, and
/// Anthropic's `convert_inline_system_messages` then merges that block
/// into the **immediately preceding** user message (the "compact now"
/// turn). Because exactly ONE new user message is appended, the merge
/// target is always the compaction tail itself — never one of the chat
/// prefix turns. Cache-breakpoint markers placed by chat on its prefix
/// therefore survive verbatim into the compaction request.
///
/// `apply_cache_control` (Anthropic) is skipped when existing markers are
/// present, so the breakpoint positions chat warmed are preserved exactly
/// instead of being re-derived from the shifted message count.
///
/// Changing this constant requires re-deriving the breakpoint math, and
/// the regression test
/// `compaction_tail_preserves_cache_breakpoint_positions` in
/// shore-llm's `providers::anthropic::tests` must be updated in lockstep.
pub const COMPACTION_TAIL_USER_PROMPT_COUNT: usize = 1;

/// Apply the canonical compaction tail to a chat-shape request.
///
/// The caller is responsible for having rebuilt the request against the
/// compaction model (e.g. via `LedgerClient::build_request_with_provider_keys`
/// with chat's `system`/`tools`/`messages`). This helper makes the "what to
/// append" step a single named operation rather than two open-coded field
/// mutations, so the wire-shape invariant is visible at every call site.
fn append_compaction_tail(
    request: &mut shore_llm::types::LlmRequest,
    user_prompt: serde_json::Value,
    system_prompt: &str,
) {
    request.messages.push(user_prompt);
    request.system_suffix = Some(system_prompt.to_string());
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
}

impl RealCompactionLlm {
    pub fn new(
        client: LedgerClient,
        model: ResolvedModel,
        providers: ProviderRegistry,
        character: String,
    ) -> Self {
        Self {
            client,
            model,
            providers,
            character,
        }
    }

    fn build_compaction_request(
        &self,
        system: &str,
        compact_now_user: serde_json::Value,
        chat_request: LlmRequest,
    ) -> Result<LlmRequest, CompactionError> {
        // Compaction is, semantically, chat with one extra system message
        // appended. We rebuild against the compaction model+sampler
        // settings so the LLM call goes to the compaction model, but the
        // cacheable prefix — `system`, `tools`, `messages` — comes through
        // verbatim from chat. Anthropic's prompt-cache hash covers all
        // three, so this is the lever that keeps compaction's call hitting
        // the cache chat seeded for this conversation. The compaction
        // instruction rides as `system_suffix` (a trailing inline
        // `role:"system"` at provider dispatch); Anthropic's
        // `convert_inline_system_messages` merges it into the appended user
        // turn, so it never appears in the cache prefix itself.
        //
        // `chat_request` is either the live in-memory `last_request` (cache
        // is warm) or a chat-shape request rebuilt from disk via
        // `handler::build_chat_shape_request_from_disk` (cache is cold). The
        // wire shape is identical either way — that's the whole point of
        // unifying the two paths.
        let mut request = LedgerClient::build_request_with_provider_keys(
            &self.model,
            &self.providers,
            chat_request.messages,
            chat_request.system,
            chat_request.tools,
            None,
        )
        .map_err(|e| CompactionError::Llm(e.to_string()))?;
        append_compaction_tail(&mut request, compact_now_user, system);

        request.rid = None;
        request.forensic_character = Some(self.character.clone());
        // Compaction is low-frequency and high-value for cache-regression
        // forensics — route its payload log to the long-retention tier.
        request.retain_long = true;

        Ok(request)
    }
}

impl CompactionLlm for RealCompactionLlm {
    fn build_initial_request(
        &self,
        system: &str,
        compact_now_user: serde_json::Value,
        chat_request: LlmRequest,
    ) -> Result<LlmRequest, CompactionError> {
        let chat_msg_count = chat_request.messages.len();
        let chat_tools_count = chat_request.tools.as_ref().map(Vec::len).unwrap_or(0);
        let request = self.build_compaction_request(system, compact_now_user, chat_request)?;
        debug!(
            system_len = system.len(),
            chat_msg_count, chat_tools_count, "compaction: initial request built from chat prefix"
        );
        Ok(request)
    }

    fn generate<'a>(
        &'a self,
        request: &'a mut LlmRequest,
    ) -> Pin<Box<dyn Future<Output = Result<GenerateResponse, CompactionError>> + Send + 'a>> {
        Box::pin(async move {
            let t0 = std::time::Instant::now();
            let (resp, _fallback_events) = self
                .client
                .generate_with_credential_fallback(
                    request,
                    &self.model,
                    &self.providers,
                    CallType::Compaction,
                    &self.character,
                    false,
                )
                .await
                .map_err(|e| CompactionError::Llm(e.to_string()))?;
            debug!(
                elapsed = ?t0.elapsed(),
                content_len = resp.content.len(),
                content_blocks = resp.content_blocks.len(),
                finish_reason = %resp.finish_reason,
                "compaction: LLM generate round done"
            );
            Ok(resp)
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
        let active_path = self.character_dir.join(shore_config::ACTIVE_JSONL_FILE);

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
            let manifest_path = self
                .character_dir
                .join(shore_config::COMPACTION_MANIFEST_FILE);
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
            let segments_dir = self.character_dir.join(shore_config::SEGMENTS_DIR);

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
    use serde_json::json;
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

    fn test_openai_compaction_model(api_key_env: &str) -> ResolvedModel {
        ResolvedModel::from_parts(
            "gpt".to_string(),
            "chat.openai.gpt".to_string(),
            "chat".to_string(),
            "openai".to_string(),
            "gpt-4o".to_string(),
            Sdk::Openai,
            ModelConfigFields {
                sdk: Some(Sdk::Openai),
                api_key_env: Some(api_key_env.to_string()),
                base_url: Some("http://compaction-openai.example".to_string()),
                max_tokens: Some(777),
                ..Default::default()
            },
        )
    }

    /// Helper: build an Anthropic-style chat-shape request to feed into the
    /// compaction request builder. Mirrors what `prepare_chat_context` +
    /// `build_request_with_provider_keys` would produce (or what chat's
    /// in-memory `last_request` would carry).
    fn chat_shape_request(
        sdk: Sdk,
        system: Option<serde_json::Value>,
        tools: Option<Vec<serde_json::Value>>,
        messages: Vec<serde_json::Value>,
    ) -> shore_llm::types::LlmRequest {
        shore_llm::types::LlmRequest {
            sdk,
            model: "chat-model".to_string(),
            api_key: "chat-secret".to_string(),
            api_key_name: None,
            base_url: Some("http://chat.example".to_string()),
            messages,
            system,
            tools,
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
            system_suffix: None,
            retain_long: false,
        }
    }

    #[test]
    fn compaction_request_keeps_chat_prefix_but_uses_compaction_settings() {
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
        );
        let chat_request = chat_shape_request(
            Sdk::Anthropic,
            Some(json!("cached system")),
            Some(vec![json!({
                "name": "read",
                "description": "chat tool — compaction inherits it to preserve the cache prefix hash",
                "input_schema": { "type": "object" }
            })]),
            vec![
                json!({"role": "user", "content": "cached user"}),
                json!({"role": "assistant", "content": "cached assistant"}),
            ],
        );

        let request = llm
            .build_compaction_request(
                "compaction system",
                json!({"role": "user", "content": "compact now"}),
                chat_request,
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
        let tools = request
            .tools
            .as_ref()
            .expect("chat tools must pass through so the cache prefix hash matches");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "read");
        assert_eq!(request.rid, None);
        assert_eq!(request.forensic_character.as_deref(), Some("alice"));
        assert_eq!(request.system, Some(json!("cached system")));
        // The compaction prompt rides as `system_suffix`, not as a
        // trailing entry in `messages`. The chat prefix (cached user +
        // cached assistant) plus the compaction-specific user turn is
        // all that should appear in `messages`. shore-llm expands the
        // suffix into the wire-format trailing system message just
        // before provider dispatch.
        assert_eq!(request.messages.len(), 3);
        assert_eq!(request.messages[0]["content"], "cached user");
        assert_eq!(request.messages[1]["content"], "cached assistant");
        assert_eq!(request.messages[2]["content"], "compact now");
        assert_eq!(request.system_suffix.as_deref(), Some("compaction system"));

        let provider_options = request.provider_options.expect("provider options");
        assert_eq!(provider_options["reasoning_effort"], "medium");
        assert!(provider_options.get("chat_only").is_none());
    }

    /// Regression pin: every byte that contributes to Anthropic's cache-prefix
    /// hash (system, tools, first N messages, max_tokens-style sampler config
    /// when emitted in the body) must match between the chat request that
    /// seeded the cache and the compaction request the daemon issues moments
    /// later. If a future refactor changes any of these, this test fails
    /// immediately rather than going silently undetected by manifesting as a
    /// ~40% API-spend regression in production.
    #[test]
    fn compaction_request_matches_chat_prefix_byte_for_byte() {
        let api_key_env = format!(
            "SHORE_TEST_COMPACTION_PREFIX_{}",
            uuid::Uuid::new_v4().simple()
        );
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
        );

        let chat_tools = vec![json!({
            "name": "read",
            "description": "exact-byte tool definition",
            "input_schema": { "type": "object", "properties": {} }
        })];
        let chat_messages = vec![
            json!({"role": "user", "content": "cached user 1"}),
            json!({"role": "assistant", "content": "cached assistant 1"}),
            json!({"role": "user", "content": "cached user 2"}),
        ];
        let chat_system = Some(json!("chat system prompt"));

        let chat_request = chat_shape_request(
            Sdk::Anthropic,
            chat_system.clone(),
            Some(chat_tools.clone()),
            chat_messages.clone(),
        );

        let request = llm
            .build_compaction_request(
                "compaction system prompt",
                json!({"role": "user", "content": "compact now"}),
                chat_request,
            )
            .unwrap();

        std::env::remove_var(&api_key_env);

        // Prefix portion: system + tools + first len(chat_messages) entries.
        assert_eq!(
            request.system, chat_system,
            "system block diverged — would invalidate Anthropic cache prefix"
        );
        assert_eq!(
            request.tools.as_deref(),
            Some(chat_tools.as_slice()),
            "tools array diverged — Anthropic hashes tools into the cache prefix"
        );
        for (i, expected) in chat_messages.iter().enumerate() {
            assert_eq!(
                &request.messages[i], expected,
                "message {i} diverged from chat prefix — cache invalidation"
            );
        }
        // The compaction-specific tail rides after the prefix, plus its
        // prompt rides as `system_suffix` so it doesn't enter the prefix at all.
        assert_eq!(request.messages.len(), chat_messages.len() + 1);
        assert_eq!(request.messages.last().unwrap()["content"], "compact now");
        assert_eq!(
            request.system_suffix.as_deref(),
            Some("compaction system prompt")
        );
    }

    /// The unified compaction path must work for non-Anthropic SDKs too:
    /// the compaction prompt rides as `system_suffix` regardless of
    /// provider, and shore-llm's per-provider dispatch logic decides how
    /// to emit it on the wire (Anthropic merges into the trailing user
    /// turn; OpenAI/Gemini emit an inline `<system_instruction>` wrap;
    /// Z.AI emits a raw trailing `role:"system"`). The previous fresh
    /// path used to special-case the top-level `system` block here; the
    /// unification dropped that branch because chat's prefix is the same
    /// no matter the provider.
    #[test]
    fn compaction_request_routes_system_through_suffix_on_openai_sdk() {
        let api_key_env = format!(
            "SHORE_TEST_OPENAI_COMPACTION_{}",
            uuid::Uuid::new_v4().simple()
        );
        std::env::set_var(&api_key_env, "openai-secret");
        let model = test_openai_compaction_model(&api_key_env);
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
        );

        let chat_request = chat_shape_request(
            Sdk::Openai,
            Some(json!("chat system prompt")),
            None,
            vec![json!({"role": "user", "content": "hi"})],
        );

        let request = llm
            .build_compaction_request(
                "compaction system",
                json!({"role": "user", "content": "compact now"}),
                chat_request,
            )
            .unwrap();

        std::env::remove_var(&api_key_env);

        assert_eq!(request.sdk, Sdk::Openai);
        // Chat's system block passes through verbatim, regardless of SDK.
        assert_eq!(request.system, Some(json!("chat system prompt")));
        // The compaction instruction rides as system_suffix — shore-llm
        // expands it to the wire-format trailing system message at
        // dispatch time.
        assert_eq!(request.system_suffix.as_deref(), Some("compaction system"));
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0]["content"], "hi");
        assert_eq!(request.messages[1]["content"], "compact now");
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
