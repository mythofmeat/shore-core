use serde_json::json;
use shore_protocol::error::ErrorCode;
use shore_protocol::types::{ContentBlock, Role};
use tracing::{debug, info};

use crate::engine::ConversationEngine;
use crate::memory::agent_llm::RealAgentLlm;
use crate::memory::compaction::{
    CompactionError, CompactionManager, CompactionOutcome, ConversationMessage,
    DEFAULT_COMPACT_PROMPT,
};
use crate::memory::compaction_impls::{RealCompactionLlm, RealConversationManager};
use crate::memory::markdown_query;
use crate::memory::markdown_store::MarkdownMemoryStore;
use shore_config::resolve_prompt_template;
use shore_ledger::CallType;

use crate::commands::{memory_dir, resolve_agent_model, CommandContext, CommandResult};

async fn open_markdown_store(
    ctx: &CommandContext,
    char_name: &str,
    ) -> Result<MarkdownMemoryStore, (ErrorCode, String)> {
    MarkdownMemoryStore::open(
        ctx.data_dir.join(char_name).join("memories"),
    )
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
    let dreams_path = ctx.data_dir.join(char_name).join("memories").join("DREAMS.md");
    if !dreams_path.exists() {
        return Ok(json!({ "changelog": [], "character": char_name }));
    }

    let content = std::fs::read_to_string(&dreams_path)
        .map_err(|e| (ErrorCode::InternalError, format!("failed to read DREAMS.md: {e}")))?;
    let mut sections = content
        .split("\n## ")
        .filter_map(|section| {
            let trimmed = section.trim();
            if trimmed.is_empty() || trimmed == "# Dreams" {
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
                .split_once(" - ")
                .map(|(ts, op)| (ts.to_string(), op.to_string()))
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

/// Memory command: status (no query) or agent query (with query).
pub async fn memory(
    engine: &ConversationEngine,
    ctx: &CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let direct = args
        .get("direct")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    match query {
        None => memory_status(engine, ctx).await,
        Some(q) => {
            debug!(
                character = engine.character_name(),
                query_len = q.len(),
                direct,
                "Memory query requested"
            );
            memory_query(engine, ctx, q, direct).await
        }
    }
}

/// Return memory entry/entity counts for the current character.
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
    direct: bool,
) -> CommandResult {
    let char_name = engine.character_name();
    let store = open_markdown_store(ctx, char_name).await?;
    let agent_model = resolve_agent_model(ctx)?;
    let agent_llm = RealAgentLlm::new(
        ctx.llm_client.clone(),
        char_name.to_string(),
        CallType::MemoryAgent,
    );

    let result = if direct {
        let hits = store
            .search_text(query)
            .await
            .map_err(|e| (ErrorCode::InternalError, format!("Memory query failed: {e}")))?;
        markdown_query::format_direct_response(query, &hits)
    } else {
        markdown_query::answer_query(
            query,
            char_name,
            &ctx.config.app.defaults.resolve_display_name(),
            &store,
            &agent_llm,
            &agent_model,
        )
            .await
            .map_err(|e| {
                (
                    ErrorCode::InternalError,
                    format!("Memory query failed: {e}"),
                )
            })?
    };

    Ok(json!({
        "character": char_name,
        "query": query,
        "result": result,
    }))
}

/// Legacy memory shell removed in markdown-only memory mode.
pub async fn memory_shell_start(
    _engine: &ConversationEngine,
    _ctx: &mut CommandContext,
    _args: &serde_json::Value,
) -> CommandResult {
    Err((
        ErrorCode::InvalidRequest,
        "memory shell was removed; use `shore memory <query>` or the markdown file tools instead"
            .to_string(),
    ))
}

pub async fn memory_shell_query(
    _ctx: &mut CommandContext,
    _args: &serde_json::Value,
) -> CommandResult {
    Err((
        ErrorCode::InvalidRequest,
        "memory shell was removed; use markdown memory files directly".to_string(),
    ))
}

pub fn memory_shell_end(_ctx: &mut CommandContext, _args: &serde_json::Value) -> CommandResult {
    Err((
        ErrorCode::InvalidRequest,
        "memory shell was removed; nothing to close".to_string(),
    ))
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
            is_tool_result_only: m.role == Role::User
                && !m.content_blocks.is_empty()
                && m.content_blocks
                    .iter()
                    .all(|b| matches!(b, ContentBlock::ToolResult { .. })),
        })
        .collect();

    if messages.is_empty() {
        return Err((
            ErrorCode::InvalidRequest,
            "No messages to compact".to_string(),
        ));
    }

    info!(character = %char_name, message_count = messages.len(), dry_run, "Compaction started");

    let prompt_template =
        resolve_prompt_template(&ctx.config.dirs.config, &char_name, "compact.md")
            .unwrap_or_else(|| DEFAULT_COMPACT_PROMPT.to_string());

    let model = resolve_compaction_model(&ctx.config)
        .ok_or_else(|| (ErrorCode::InternalError, "No model configured".to_string()))?;

    let llm = RealCompactionLlm::new(ctx.llm_client.clone(), model, char_name.clone());
    let conv_mgr = RealConversationManager::new(engine.character_dir());

    let mgr = CompactionManager::new(ctx.config.app.memory.compaction.clone());

    let recap_path = memory_dir(ctx, &char_name).join("recap.md");
    let existing_recap = std::fs::read_to_string(&recap_path).ok();

    let active_content = std::fs::read_to_string(engine.character_dir().join("active.jsonl"))
        .map_err(|e| {
            (
                ErrorCode::InternalError,
                format!("failed to read active.jsonl: {e}"),
            )
        })?;

    let display_name = ctx.config.app.defaults.resolve_display_name();

    let markdown_store = crate::memory::markdown_store::MarkdownMemoryStore::open(
        engine.character_dir().join("memories"),
    )
    .await
    .ok();

    let outcome = mgr
        .compact(
            &char_name,
            &messages,
            &active_content,
            false,
            &prompt_template,
            existing_recap.as_deref(),
            &char_name,
            &display_name,
            &llm,
            &conv_mgr,
            markdown_store.as_ref(),
            dry_run,
            keep_turns_override,
        )
        .await
        .map_err(compaction_err)?;

    match outcome {
        CompactionOutcome::Compacted(result) => {
            info!(
                character = %char_name,
                entries_created = result.memory_files_written.len(),
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
                "message_count": result.message_count,
                "retained_count": result.retained_count,
                "retained_turns": result.retained_turns,
                "recap_generated": result.recap_generated,
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
                "would_create_entries": result.would_create_entries,
                "file_ops_preview": previews,
                "message_count": result.message_count,
                "retained_count": result.retained_count,
                "retained_turns": result.retained_turns,
                "recap_preview": result.recap_preview,
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

/// Resolve the model to use for compaction.
///
/// Chain: `defaults.compaction` → `defaults.memory_agent` → `defaults.model` → first chat model.
/// Background and interactive compaction entry points depend on this returning
/// the same value.
pub fn resolve_compaction_model(
    config: &shore_config::LoadedConfig,
) -> Option<shore_config::models::ResolvedModel> {
    config
        .app
        .defaults
        .compaction
        .as_deref()
        .and_then(|name| config.models.find_model(name).ok())
        .or_else(|| {
            config
                .app
                .defaults
                .memory_agent
                .as_deref()
                .and_then(|name| config.models.find_model(name).ok())
        })
        .or_else(|| {
            config
                .app
                .defaults
                .model
                .as_deref()
                .and_then(|name| config.models.find_model(name).ok())
        })
        .or_else(|| config.models.first_chat_model())
        .cloned()
}

/// Legacy purge removed in markdown-only memory mode.
pub async fn memory_purge(
    _engine: &mut ConversationEngine,
    _ctx: &CommandContext,
    _args: &serde_json::Value,
) -> CommandResult {
    Err((
        ErrorCode::InvalidRequest,
        "memory_purge was removed; markdown memories are edited directly instead".to_string(),
    ))
}

/// Legacy reindex removed in markdown-only memory mode.
pub async fn memory_reindex(engine: &ConversationEngine, ctx: &CommandContext) -> CommandResult {
    let char_name = engine.character_name().to_string();
    let _ = ctx;
    Ok(json!({
        "character": char_name,
        "message": "memory_reindex is unnecessary in the markdown-only memory system"
    }))
}

#[cfg(test)]
mod resolver_tests {
    //! Regression tests for [`resolve_compaction_model`].
    //!
    //! Bug history: interactive `/compact` used to call `resolve_agent_model`, which
    //! consults `defaults.memory_agent` — so a user's `defaults.compaction` setting was
    //! silently ignored by the interactive entry point while the background path honored
    //! it. Parallel resolvers with no shared helper, no test locking them together.
    //!
    //! These tests pin the resolution chain and assert that
    //! `defaults.compaction` take precedence over `defaults.memory_agent`.
    //! Don't delete without a matching DECISIONS.md entry.
    use super::resolve_compaction_model;
    use shore_config::app::{AppConfig, DefaultsConfig};
    use shore_config::models::ModelCatalog;
    use shore_config::{LoadedConfig, ShoreDirs};
    use std::path::PathBuf;

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
        let mut app = AppConfig::default();
        app.defaults = defaults;
        LoadedConfig::new_for_test(
            app,
            catalog_with_chat_and_tools(),
            ShoreDirs {
                config: PathBuf::from("/tmp/resolver-test/config"),
                data: PathBuf::from("/tmp/resolver-test/data"),
                runtime: PathBuf::from("/tmp/resolver-test/runtime"),
                cache: PathBuf::from("/tmp/resolver-test/cache"),
            },
        )
    }

    /// The bug this test was written for: compaction must honor `defaults.compaction`
    /// even when `memory_agent` is also set.
    #[test]
    fn compaction_prefers_defaults_compaction_over_memory_agent() {
        let config = make_config(DefaultsConfig {
            compaction: Some("bg".to_string()),
            memory_agent: Some("minimax".to_string()),
            model: Some("primary".to_string()),
            ..DefaultsConfig::default()
        });
        let model = resolve_compaction_model(&config).expect("resolved");
        assert_eq!(
            model.name, "bg",
            "defaults.compaction must win over memory_agent"
        );
    }

    #[test]
    fn compaction_falls_back_to_memory_agent_when_unset() {
        let config = make_config(DefaultsConfig {
            compaction: None,
            memory_agent: Some("minimax".to_string()),
            model: Some("primary".to_string()),
            ..DefaultsConfig::default()
        });
        let model = resolve_compaction_model(&config).expect("resolved");
        assert_eq!(model.name, "minimax");
    }

    #[test]
    fn compaction_falls_back_to_model_when_compaction_and_memory_agent_unset() {
        let config = make_config(DefaultsConfig {
            compaction: None,
            memory_agent: None,
            model: Some("primary".to_string()),
            ..DefaultsConfig::default()
        });
        let model = resolve_compaction_model(&config).expect("resolved");
        assert_eq!(model.name, "primary");
    }

    #[test]
    fn compaction_falls_back_to_first_chat_model_when_nothing_set() {
        let config = make_config(DefaultsConfig::default());
        let model = resolve_compaction_model(&config).expect("resolved");
        // first_chat_model returns the first BTreeMap entry: "bg" sorts before "primary".
        assert_eq!(model.name, "bg");
    }

    #[test]
    fn compaction_returns_none_with_empty_catalog() {
        let mut app = AppConfig::default();
        app.defaults = DefaultsConfig::default();
        let config = LoadedConfig::new_for_test(
            app,
            ModelCatalog::default(),
            ShoreDirs {
                config: PathBuf::from("/tmp/resolver-test/config"),
                data: PathBuf::from("/tmp/resolver-test/data"),
                runtime: PathBuf::from("/tmp/resolver-test/runtime"),
                cache: PathBuf::from("/tmp/resolver-test/cache"),
            },
        );
        assert!(resolve_compaction_model(&config).is_none());
    }
}
