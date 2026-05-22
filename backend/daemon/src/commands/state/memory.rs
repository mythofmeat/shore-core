use serde_json::json;
use shore_protocol::error::ErrorCode;
use shore_protocol::types::Role;
use tracing::{debug, info};

use crate::engine::ConversationEngine;
use crate::memory::compaction::{
    CompactionError, CompactionManager, CompactionOutcome, ConversationMessage,
    DEFAULT_COMPACT_PROMPT, DEFAULT_COMPACT_SYSTEM,
};
use crate::memory::compaction_impls::{RealCompactionLlm, RealConversationManager};
use crate::memory::markdown_query;
use crate::memory::markdown_store::MarkdownMemoryStore;
use shore_config::character_memory_dir;
use shore_config::resolve_prompt_template;

use crate::commands::{CommandContext, CommandResult};

async fn open_markdown_store(
    ctx: &CommandContext,
    char_name: &str,
) -> Result<MarkdownMemoryStore, (ErrorCode, String)> {
    MarkdownMemoryStore::open(character_memory_dir(&ctx.config.dirs.config, char_name))
        .await
        .map_err(|e| {
            (
                ErrorCode::InternalError,
                format!("Failed to open markdown store: {e}"),
            )
        })
}

/// Show recent memory changelog entries.
pub fn memory_changelog(
    engine: &ConversationEngine,
    ctx: &CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);

    let char_name = engine.character_name();
    let dreams_path = crate::memory::dreams_log::dreams_log_path(&ctx.config.dirs.data, char_name);
    if !dreams_path.exists() {
        return Ok(json!({ "changelog": [], "character": char_name }));
    }

    let content = std::fs::read_to_string(&dreams_path).map_err(|e| {
        (
            ErrorCode::InternalError,
            format!("failed to read dreams log: {e}"),
        )
    })?;
    let mut sections = content
        .split("\n## ")
        .filter_map(|section| {
            let trimmed = section.trim();
            if trimmed.is_empty() || trimmed.starts_with("# Dreams") {
                return None;
            }
            let prefixed = if trimmed.starts_with("## ") {
                trimmed.to_string()
            } else {
                format!("## {trimmed}")
            };
            let mut lines = prefixed.lines();
            let heading = lines.next()?.trim_start_matches("## ").trim();
            let description = lines.collect::<Vec<_>>().join("\n").trim().to_string();
            let (timestamp, operation) = heading
                .strip_prefix("Dream Cycle - ")
                .map(|ts| (ts.to_string(), "dream_cycle".to_string()))
                .or_else(|| {
                    heading
                        .split_once(" - ")
                        .map(|(ts, op)| (ts.to_string(), op.to_string()))
                })
                .unwrap_or_else(|| (String::new(), heading.to_string()));
            Some(json!({
                "timestamp": timestamp,
                "operation": operation,
                "description": description,
            }))
        })
        .collect::<Vec<_>>();
    sections.reverse();
    sections.truncate(limit.max(0) as usize);

    debug!(
        character = char_name,
        count = sections.len(),
        "Memory changelog queried"
    );
    Ok(json!({ "changelog": sections, "character": char_name }))
}

/// Print recent entries from the dreams audit log (data-dir-rooted).
pub fn memory_dreams(
    engine: &ConversationEngine,
    ctx: &CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(10);
    let char_name = engine.character_name();
    let path = crate::memory::dreams_log::dreams_log_path(&ctx.config.dirs.data, char_name);
    if !path.exists() {
        return Ok(json!({
            "character": char_name,
            "entries": [],
            "path": path.display().to_string(),
            "exists": false,
        }));
    }
    let content = std::fs::read_to_string(&path).map_err(|e| {
        (
            ErrorCode::InternalError,
            format!("failed to read dreams log: {e}"),
        )
    })?;

    let mut sections = content
        .split("\n## ")
        .filter_map(|section| {
            let trimmed = section.trim();
            if trimmed.is_empty() || trimmed.starts_with("# Dreams") {
                None
            } else {
                Some(format!("## {trimmed}"))
            }
        })
        .collect::<Vec<_>>();
    sections.reverse();
    sections.truncate(limit);

    Ok(json!({
        "character": char_name,
        "entries": sections,
        "path": path.display().to_string(),
        "exists": true,
    }))
}

pub async fn memory_dream(
    engine: &ConversationEngine,
    ctx: &CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let status = args
        .get("status")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    let char_name = engine.character_name();
    let cfg = &ctx.config.app.memory.dreaming;

    if status {
        let status = crate::memory::dreaming::dream_status(
            &ctx.data_dir,
            &ctx.config.dirs.config,
            char_name,
            cfg,
        )
        .await
        .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;
        return Ok(json!(status));
    }

    let cached_request = ctx.autonomy.cached_last_request(char_name);
    let result = match crate::memory::dreaming::run_librarian_sweep(
        &ctx.config,
        &ctx.data_dir,
        &ctx.llm_client,
        char_name,
        cached_request.as_ref(),
        dry_run,
        force,
    )
    .await
    {
        Ok(result) => result,
        Err(crate::memory::dreaming::DreamingError::Config(_)) if dry_run => {
            crate::memory::dreaming::run_legacy_diagnostic_sweep(
                &ctx.data_dir,
                &ctx.config.dirs.config,
                char_name,
                cfg,
                dry_run,
                force,
            )
            .await
            .map_err(|e| (ErrorCode::InternalError, e.to_string()))?
        }
        Err(e) => return Err((ErrorCode::InternalError, e.to_string())),
    };
    match result {
        Some(result) => Ok(json!(result)),
        None => Ok(json!({
            "character": char_name,
            "status": "not_due",
            "enabled": cfg.enabled,
            "frequency": cfg.frequency,
        })),
    }
}

/// Memory command: status (no query) or direct markdown search (with query).
pub async fn memory(
    engine: &ConversationEngine,
    ctx: &CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    match query {
        None => memory_status(engine, ctx).await,
        Some(q) => {
            debug!(
                character = engine.character_name(),
                query_len = q.len(),
                "Memory query requested"
            );
            memory_query(engine, ctx, q).await
        }
    }
}

/// Return markdown memory file counts for the current character.
async fn memory_status(engine: &ConversationEngine, ctx: &CommandContext) -> CommandResult {
    let char_name = engine.character_name();
    let store = open_markdown_store(ctx, char_name).await?;
    let status = markdown_query::memory_status(&store)
        .await
        .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;

    debug!(
        character = char_name,
        entries = status.total_files,
        daily = status.daily_files,
        images = status.image_files,
        "Memory status queried"
    );
    Ok(json!({
        "character": char_name,
        "entries": status.total_files,
        "curated_files": status.topic_files,
        "daily_files": status.daily_files,
        "image_files": status.image_files,
    }))
}

/// Run a memory query against markdown files.
async fn memory_query(
    engine: &ConversationEngine,
    ctx: &CommandContext,
    query: &str,
) -> CommandResult {
    let char_name = engine.character_name();
    let store = open_markdown_store(ctx, char_name).await?;
    let hits = store.search_text(query).await.map_err(|e| {
        (
            ErrorCode::InternalError,
            format!("Memory query failed: {e}"),
        )
    })?;
    let result = markdown_query::format_direct_response(query, &hits);

    Ok(json!({
        "character": char_name,
        "query": query,
        "result": result,
    }))
}

/// Run compaction on the current character's conversation.
pub async fn compact(
    engine: &mut ConversationEngine,
    ctx: &CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let keep_turns_override = args
        .get("keep_turns")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);

    let char_name = engine.character_name().to_string();
    let _compaction_guard =
        crate::memory::compaction::try_begin_compaction(&ctx.config.dirs.data, &char_name)
            .ok_or_else(|| {
                (
                    ErrorCode::Busy,
                    format!("Compaction already running for {char_name}"),
                )
            })?;

    let messages: Vec<ConversationMessage> = engine
        .messages()
        .iter()
        .map(|m| ConversationMessage {
            role: match m.role {
                Role::User => "user".to_string(),
                Role::Assistant => "assistant".to_string(),
                Role::System => "system".to_string(),
            },
            content: m.content.clone(),
            timestamp: m.timestamp.clone(),
            is_tool_result_only: m.is_tool_result_only(),
        })
        .collect();

    if messages.is_empty() {
        return Err((
            ErrorCode::InvalidRequest,
            "No messages to compact".to_string(),
        ));
    }

    info!(character = %char_name, message_count = messages.len(), dry_run, "Compaction started");

    let system_template =
        resolve_prompt_template(&ctx.config.dirs.config, &char_name, "compact_system.md")
            .unwrap_or_else(|| DEFAULT_COMPACT_SYSTEM.to_string());
    let prompt_template =
        resolve_prompt_template(&ctx.config.dirs.config, &char_name, "compact.md")
            .unwrap_or_else(|| DEFAULT_COMPACT_PROMPT.to_string());

    let model = crate::preferences::resolve_background_model(
        &ctx.config,
        shore_config::app::BackgroundTask::Compaction,
        &char_name,
    )
    .ok_or_else(|| (ErrorCode::InternalError, "No model configured".to_string()))?;

    let llm = RealCompactionLlm::new(
        ctx.llm_client.clone(),
        model,
        ctx.config.providers.clone(),
        char_name.clone(),
    );
    let conv_mgr = RealConversationManager::new(engine.character_dir());

    let mgr = CompactionManager::new(ctx.config.app.memory.compaction.clone());

    let active_content =
        std::fs::read_to_string(engine.character_dir().join(shore_config::ACTIVE_JSONL_FILE))
            .map_err(|e| {
                (
                    ErrorCode::InternalError,
                    format!("failed to read active.jsonl: {e}"),
                )
            })?;

    let display_name = ctx.config.app.defaults.resolve_display_name();

    let markdown_store = crate::memory::markdown_store::MarkdownMemoryStore::open(
        character_memory_dir(&ctx.config.dirs.config, &char_name),
    )
    .await
    .ok();

    let cached_request = ctx.autonomy.cached_last_request(&char_name);

    let outcome = mgr
        .compact(
            &char_name,
            &messages,
            &active_content,
            false,
            &system_template,
            &prompt_template,
            &char_name,
            &display_name,
            &llm,
            &conv_mgr,
            markdown_store.as_ref(),
            dry_run,
            keep_turns_override,
            cached_request,
            Some(&ctx.config.dirs.data),
        )
        .await
        .map_err(compaction_err)?;

    match outcome {
        CompactionOutcome::Compacted(result) => {
            info!(
                character = %char_name,
                memory_files_written = result.memory_files_written.len(),
                message_count = result.message_count,
                retained_count = result.retained_count,
                "Compaction complete"
            );
            engine
                .reload()
                .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;

            // Apply deferred character self-edits now that the cache has
            // been bust by the engine reload.
            let character_data_dir = ctx.config.dirs.data.join(&char_name);
            if let Err(e) = crate::memory::deferred_edits::apply_deferred_edits(
                &character_data_dir,
                &ctx.config.dirs.config,
                &char_name,
            ) {
                tracing::warn!(
                    character = %char_name,
                    error = %e,
                    "Failed to apply deferred edits after compaction"
                );
            }

            Ok(json!({
                "status": "compacted",
                "character": char_name,
                "memory_files_written": result.memory_files_written,
                "message_count": result.compacted_turns,
                "turn_count": result.compacted_turns,
                "compacted_turns": result.compacted_turns,
                "retained_count": result.retained_count,
                "retained_turns": result.retained_turns,
                "new_conversation_id": result.new_conversation_id,
            }))
        }
        CompactionOutcome::DryRun(result) => {
            let previews: Vec<serde_json::Value> = result
                .file_ops_preview
                .iter()
                .map(|op| {
                    json!({
                        "path": op.path,
                        "content_preview": op.content.chars().take(200).collect::<String>(),
                    })
                })
                .collect();

            Ok(json!({
                "status": "dry_run",
                "character": char_name,
                "would_write_files": result.would_write_files,
                "file_ops_preview": previews,
                "message_count": result.compacted_turns,
                "turn_count": result.compacted_turns,
                "compacted_turns": result.compacted_turns,
                "retained_count": result.retained_count,
                "retained_turns": result.retained_turns,
            }))
        }
    }
}

fn compaction_err(e: CompactionError) -> (ErrorCode, String) {
    match &e {
        CompactionError::PrivateConversation | CompactionError::InsufficientMessages => {
            (ErrorCode::InvalidRequest, e.to_string())
        }
        _ => (ErrorCode::InternalError, e.to_string()),
    }
}

#[cfg(test)]
mod resolver_tests {
    //! Regression tests for compaction model resolution via
    //! [`crate::preferences::resolve_background_model`].
    //!
    //! Compaction is a background task and follows the
    //! `defaults.background.compaction → defaults.background.model →
    //! defaults.model → first chat model` chain. The per-character
    //! active chat model is **not** consulted, so a runtime
    //! `switch_model` does not move the compaction model.
    use crate::preferences::resolve_background_model;
    use shore_config::app::{AppConfig, BackgroundDefaultsConfig, BackgroundTask, DefaultsConfig};
    use shore_config::models::ModelCatalog;
    use shore_config::{LoadedConfig, ShoreDirs};

    fn resolve(config: &LoadedConfig) -> Option<shore_config::models::ResolvedModel> {
        resolve_background_model(config, BackgroundTask::Compaction, "test-char")
    }

    fn catalog_with_chat_and_tools() -> ModelCatalog {
        let chat: toml::Table = toml::from_str(
            r#"
[anthropic.primary]
model_id = "claude-primary"

[anthropic.bg]
model_id = "claude-bg"
"#,
        )
        .unwrap();
        let tools: toml::Table = toml::from_str(
            r#"
[openrouter.minimax]
model_id = "minimax-tool"
"#,
        )
        .unwrap();
        ModelCatalog::from_sections(Some(&chat), Some(&tools), None, None).unwrap()
    }

    fn make_config(defaults: DefaultsConfig) -> LoadedConfig {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = AppConfig {
            defaults,
            ..AppConfig::default()
        };
        LoadedConfig::new_for_test(
            app,
            catalog_with_chat_and_tools(),
            ShoreDirs {
                config: tmp.path().join("config"),
                data: tmp.path().join("data"),
                runtime: tmp.path().join("runtime"),
                cache: tmp.path().join("cache"),
            },
        )
    }

    #[test]
    fn compaction_prefers_per_task_override() {
        let config = make_config(DefaultsConfig {
            model: Some("primary".to_string()),
            background: BackgroundDefaultsConfig {
                model: Some("primary".to_string()),
                compaction: Some("bg".to_string()),
                ..BackgroundDefaultsConfig::default()
            },
            ..DefaultsConfig::default()
        });
        let model = resolve(&config).expect("resolved");
        assert_eq!(model.name, "bg");
    }

    #[test]
    fn compaction_falls_back_to_background_model() {
        let config = make_config(DefaultsConfig {
            model: Some("primary".to_string()),
            background: BackgroundDefaultsConfig {
                model: Some("bg".to_string()),
                ..BackgroundDefaultsConfig::default()
            },
            ..DefaultsConfig::default()
        });
        let model = resolve(&config).expect("resolved");
        assert_eq!(model.name, "bg");
    }

    #[test]
    fn compaction_falls_back_to_chat_default_when_background_unset() {
        let config = make_config(DefaultsConfig {
            model: Some("primary".to_string()),
            ..DefaultsConfig::default()
        });
        let model = resolve(&config).expect("resolved");
        assert_eq!(model.name, "primary");
    }

    #[test]
    fn compaction_falls_back_to_first_chat_model_when_nothing_set() {
        let config = make_config(DefaultsConfig::default());
        let model = resolve(&config).expect("resolved");
        // first_chat_model returns the first BTreeMap entry: "bg" sorts before "primary".
        assert_eq!(model.name, "bg");
    }

    #[test]
    fn compaction_returns_none_with_empty_catalog() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = AppConfig {
            defaults: DefaultsConfig::default(),
            ..AppConfig::default()
        };
        let config = LoadedConfig::new_for_test(
            app,
            ModelCatalog::default(),
            ShoreDirs {
                config: tmp.path().join("config"),
                data: tmp.path().join("data"),
                runtime: tmp.path().join("runtime"),
                cache: tmp.path().join("cache"),
            },
        );
        assert!(resolve(&config).is_none());
    }
}
