use serde_json::json;
use shore_protocol::error::ErrorCode;
use shore_protocol::types::{ContentBlock, Role};
use tracing::{debug, info};

use crate::engine::ConversationEngine;
use crate::memory::agent::{CallerIdentity, MemoryAgent, RealAgentIndexer};
use crate::memory::agent_llm::RealAgentLlm;
use crate::memory::collation::{
    CollationError, CollationManager, CollationOutcome, DecayConfig, DEFAULT_REFINE_PROMPT,
};
use crate::memory::collation_impls::RealCollationLlm;
use crate::memory::compaction::{
    CompactionError, CompactionManager, CompactionOutcome, ConversationMessage,
    DEFAULT_COMPACT_PROMPT,
};
use crate::memory::compaction_impls::{
    RealCompactionLlm, RealConversationManager, RealVectorIndexer,
};
use crate::memory::db::MemoryDB;
use crate::memory::researcher::MemoryResearcher;
use shore_config::resolve_prompt_template;
use shore_ledger::CallType;

use crate::commands::{
    build_collation_vars, memory_dir, open_embed_and_vectorstore, resolve_agent_model,
    setup_search_context, CommandContext, CommandResult, MemoryShellSession,
};

/// Build the memory DB path for a character and open it.
fn open_memory_db(ctx: &CommandContext, char_name: &str) -> Result<MemoryDB, (ErrorCode, String)> {
    let db_path = memory_dir(ctx, char_name).join("memory.db");
    MemoryDB::open(&db_path).map_err(|e| {
        (
            ErrorCode::InternalError,
            format!("Failed to open memory DB: {e}"),
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
    let db_path = memory_dir(ctx, char_name).join("memory.db");

    if !db_path.exists() {
        return Ok(json!({ "changelog": [], "character": char_name }));
    }

    let db = open_memory_db(ctx, char_name)?;

    let records = db
        .get_recent_changelog(limit)
        .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;

    let entries: Vec<serde_json::Value> = records
        .iter()
        .map(|r| {
            json!({
                "id": r.changelog_id,
                "operation": r.operation,
                "description": r.description,
                "timestamp": r.timestamp,
            })
        })
        .collect();

    debug!(
        character = char_name,
        count = entries.len(),
        "Memory changelog queried"
    );
    Ok(json!({ "changelog": entries, "character": char_name }))
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
        None => memory_status(engine, ctx),
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
fn memory_status(engine: &ConversationEngine, ctx: &CommandContext) -> CommandResult {
    let char_name = engine.character_name();
    let db_path = memory_dir(ctx, char_name).join("memory.db");

    if !db_path.exists() {
        return Ok(json!({
            "character": char_name,
            "entries": 0,
            "entities": 0,
            "active_entries": 0,
        }));
    }

    let db = open_memory_db(ctx, char_name)?;

    let entries = db
        .count_entries()
        .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;
    let entities = db
        .count_entities()
        .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;
    let active = db
        .count_entries_by_status("active")
        .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;

    debug!(
        character = char_name,
        entries, entities, active, "Memory status queried"
    );
    Ok(json!({
        "character": char_name,
        "entries": entries,
        "entities": entities,
        "active_entries": active,
    }))
}

/// Run a memory query through the researcher->agent pipeline.
async fn memory_query(
    engine: &ConversationEngine,
    ctx: &CommandContext,
    query: &str,
    direct: bool,
) -> CommandResult {
    let char_name = engine.character_name();
    let db = open_memory_db(ctx, char_name)?;
    let agent_model = resolve_agent_model(ctx)?;

    let display_name = ctx.config.app.defaults.resolve_display_name();
    let agent = MemoryAgent::one_shot(CallerIdentity::User, &display_name, char_name);
    let agent_llm = RealAgentLlm::new(
        ctx.llm_client.clone(),
        char_name.to_string(),
        CallType::MemoryAgent,
    );

    let search_ctx = setup_search_context(ctx, char_name).await;
    let real_indexer = search_ctx.as_ref().map(RealAgentIndexer::new);
    let indexer = real_indexer
        .as_ref()
        .map(|i| i as &dyn crate::memory::agent::AgentIndexer);

    let researcher_model = if direct {
        None
    } else {
        ctx.config
            .app
            .defaults
            .tool_model
            .as_deref()
            .and_then(|name| ctx.config.models.find_model(name).ok())
            .cloned()
    };

    let result = if let Some(ref r_model) = researcher_model {
        let char_def = shore_config::load_character_definition(&ctx.config.dirs.config, char_name)
            .unwrap_or_default();
        let user_def = shore_config::resolve_user_definition(&ctx.config.dirs.config, char_name)
            .unwrap_or_default();
        let researcher = MemoryResearcher::new(char_def, user_def);
        let researcher_llm = RealAgentLlm::new(
            ctx.llm_client.clone(),
            char_name.to_string(),
            CallType::Researcher,
        );

        researcher
            .research(
                query,
                &researcher_llm,
                r_model,
                &agent,
                &agent_llm,
                &agent_model,
                &db,
                indexer,
                search_ctx.as_ref(),
            )
            .await
            .map_err(|e| {
                (
                    ErrorCode::InternalError,
                    format!("Memory query failed: {e}"),
                )
            })?
    } else {
        agent
            .ask(
                query,
                &agent_llm,
                &db,
                indexer,
                search_ctx.as_ref(),
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

/// Start a new memory shell session.
pub async fn memory_shell_start(
    engine: &ConversationEngine,
    ctx: &mut CommandContext,
    _args: &serde_json::Value,
) -> CommandResult {
    let char_name = engine.character_name().to_string();
    let agent_model = resolve_agent_model(ctx)?;

    let display_name = ctx.config.app.defaults.resolve_display_name();
    let agent = MemoryAgent::interactive(CallerIdentity::User, &display_name, &char_name);

    let session_id = uuid::Uuid::new_v4().to_string();
    ctx.memory_shell_sessions.insert(
        session_id.clone(),
        MemoryShellSession {
            agent,
            history: Vec::new(),
            character: char_name.clone(),
            model: agent_model,
        },
    );

    info!(character = %char_name, session_id = %session_id, "Memory shell session started");
    Ok(json!({
        "session_id": session_id,
        "character": char_name,
    }))
}

/// Process a query within an existing memory shell session.
pub async fn memory_shell_query(
    ctx: &mut CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (ErrorCode::InvalidRequest, "Missing session_id".to_string()))?;

    let input = args
        .get("input")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (ErrorCode::InvalidRequest, "Missing input".to_string()))?
        .to_string();

    let char_name = ctx
        .memory_shell_sessions
        .get(session_id)
        .ok_or_else(|| (ErrorCode::NotFound, "Session not found".to_string()))?
        .character
        .clone();

    let db = open_memory_db(ctx, &char_name)?;
    let agent_llm = RealAgentLlm::new(
        ctx.llm_client.clone(),
        char_name.clone(),
        CallType::MemoryAgent,
    );
    let search_ctx = setup_search_context(ctx, &char_name).await;
    let real_indexer = search_ctx.as_ref().map(RealAgentIndexer::new);
    let indexer = real_indexer
        .as_ref()
        .map(|i| i as &dyn crate::memory::agent::AgentIndexer);

    let session = ctx
        .memory_shell_sessions
        .get_mut(session_id)
        .ok_or_else(|| (ErrorCode::NotFound, "Session not found".to_string()))?;

    let mutations = session
        .agent
        .run_query(
            &input,
            &mut session.history,
            &agent_llm,
            &db,
            indexer,
            search_ctx.as_ref(),
            &session.model,
            None,
        )
        .await
        .map_err(|e| (ErrorCode::InternalError, format!("Memory agent error: {e}")))?;

    let response = session
        .history
        .last()
        .and_then(|msg| msg.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    debug!(
        session_id,
        mutations,
        response_len = response.len(),
        "Memory shell query complete"
    );
    Ok(json!({
        "response": response,
        "mutations": mutations,
    }))
}

/// End a memory shell session.
pub fn memory_shell_end(ctx: &mut CommandContext, args: &serde_json::Value) -> CommandResult {
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (ErrorCode::InvalidRequest, "Missing session_id".to_string()))?;

    ctx.memory_shell_sessions.remove(session_id);

    info!(session_id, "Memory shell session ended");
    Ok(json!({ "ok": true }))
}

/// Run collation after a successful compaction, if enabled.
async fn run_post_compaction_collation(
    ctx: &CommandContext,
    char_name: &str,
    display_name: &str,
    db: &MemoryDB,
) -> Option<serde_json::Value> {
    let cmodel = resolve_collation_model(&ctx.config)?;

    let collation_llm =
        RealCollationLlm::new(ctx.llm_client.clone(), cmodel, char_name.to_string());
    let refine_template = resolve_prompt_template(&ctx.config.dirs.config, char_name, "refine.md")
        .unwrap_or_else(|| DEFAULT_REFINE_PROMPT.to_string());

    let collation_mgr = CollationManager::new(DecayConfig::default());
    let collation_limit = ctx.config.app.memory.collation.batch_limit;

    let collation_search_ctx = setup_search_context(ctx, char_name).await;
    let collation_indexer = collation_search_ctx.as_ref().map(RealAgentIndexer::new);

    let collation_vars = build_collation_vars(ctx, char_name, display_name);
    match collation_mgr
        .run(
            db,
            &collation_llm,
            &refine_template,
            &collation_vars,
            collation_indexer
                .as_ref()
                .map(|i| i as &dyn crate::memory::agent::AgentIndexer),
            collation_search_ctx.as_ref().map(|ctx| &*ctx.vector_store),
            Some(collation_limit),
        )
        .await
    {
        Ok(outcome) => Some(json!({
            "timestamps_backfilled": outcome.timestamps_backfilled,
            "refine_merges": outcome.refine_merges,
            "refine_splits": outcome.refine_splits,
            "refine_updates": outcome.refine_updates,
            "refine_new_entries": outcome.refine_new_entries,
            "refine_kept": outcome.refine_kept,
            "entries_decayed": outcome.entries_decayed,
            "entries_skipped": outcome.entries_skipped,
        })),
        Err(e) => {
            tracing::warn!(
                character = %char_name,
                error = %e,
                "Collation after compaction failed"
            );
            None
        }
    }
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

    let db = open_memory_db(ctx, &char_name)?;

    let prompt_template =
        resolve_prompt_template(&ctx.config.dirs.config, &char_name, "compact.md")
            .unwrap_or_else(|| DEFAULT_COMPACT_PROMPT.to_string());

    let model = resolve_compaction_model(&ctx.config)
        .ok_or_else(|| (ErrorCode::InternalError, "No model configured".to_string()))?;

    let (store, embed_config) = open_embed_and_vectorstore(ctx, &char_name).await?;

    let llm = RealCompactionLlm::new(ctx.llm_client.clone(), model, char_name.clone());
    let indexer = RealVectorIndexer::new(store, ctx.llm_client.inner().clone(), embed_config);
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
            &db,
            &indexer,
            &conv_mgr,
            dry_run,
            keep_turns_override,
        )
        .await
        .map_err(compaction_err)?;

    match outcome {
        CompactionOutcome::Compacted(result) => {
            info!(
                character = %char_name,
                entries_created = result.entries_created.len(),
                message_count = result.message_count,
                retained_count = result.retained_count,
                "Compaction complete"
            );
            engine
                .reload()
                .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;

            let do_collate = args
                .get("collate")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let collation_result = if do_collate && ctx.config.app.memory.collation.enabled {
                run_post_compaction_collation(ctx, &char_name, &display_name, &db).await
            } else {
                None
            };

            Ok(json!({
                "status": "compacted",
                "character": char_name,
                "entries_created": result.entries_created.len(),
                "entry_ids": result.entries_created,
                "message_count": result.message_count,
                "retained_count": result.retained_count,
                "retained_turns": result.retained_turns,
                "recap_generated": result.recap_generated,
                "new_conversation_id": result.new_conversation_id,
                "collation": collation_result,
            }))
        }
        CompactionOutcome::DryRun(result) => {
            let previews: Vec<serde_json::Value> = result
                .entries_preview
                .iter()
                .map(|e| {
                    json!({
                        "memory_type": e.memory_type,
                        "summary_text": e.summary_text,
                        "topic_tags": e.topic_tags,
                        "topic_key": e.topic_key,
                        "confidence": e.confidence,
                    })
                })
                .collect();

            Ok(json!({
                "status": "dry_run",
                "character": char_name,
                "would_create_entries": result.would_create_entries,
                "entries_preview": previews,
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

/// Resolve the model to use for collation.
pub fn resolve_collation_model(
    config: &shore_config::LoadedConfig,
) -> Option<shore_config::models::ResolvedModel> {
    config
        .app
        .defaults
        .collation
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

/// Resolve the model to use for compaction.
///
/// Chain: `defaults.compaction` → `defaults.memory_agent` → `defaults.model` → first chat model.
/// Must be kept in lockstep with [`resolve_collation_model`] — both background and interactive
/// compaction entry points depend on this returning the same value.
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

/// Run the 4-phase collation pipeline on the current character's memory.
pub async fn collate(
    engine: &mut ConversationEngine,
    ctx: &CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let char_name = engine.character_name().to_string();
    let full_mode = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);

    let db = open_memory_db(ctx, &char_name)?;

    let model = resolve_collation_model(&ctx.config)
        .ok_or_else(|| (ErrorCode::InternalError, "No model configured".to_string()))?;

    let llm = RealCollationLlm::new(ctx.llm_client.clone(), model, char_name.clone());

    let refine_template = resolve_prompt_template(&ctx.config.dirs.config, &char_name, "refine.md")
        .unwrap_or_else(|| DEFAULT_REFINE_PROMPT.to_string());

    let mgr = CollationManager::new(DecayConfig::default());

    let config_limit = ctx.config.app.memory.collation.batch_limit;
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(config_limit);

    let search_ctx = setup_search_context(ctx, &char_name).await;
    let indexer = search_ctx.as_ref().map(RealAgentIndexer::new);

    let display_name = ctx.config.app.defaults.resolve_display_name();
    let collation_vars = build_collation_vars(ctx, &char_name, &display_name);

    info!(character = %char_name, full_mode, limit, "Collation started");

    const MAX_PASSES: usize = 10;
    let mut total = CollationOutcome::default();
    let mut passes: usize = 0;

    loop {
        passes += 1;
        let outcome = mgr
            .run(
                &db,
                &llm,
                &refine_template,
                &collation_vars,
                indexer
                    .as_ref()
                    .map(|i| i as &dyn crate::memory::agent::AgentIndexer),
                search_ctx.as_ref().map(|ctx| &*ctx.vector_store),
                Some(limit),
            )
            .await
            .map_err(collation_err)?;

        total.timestamps_backfilled += outcome.timestamps_backfilled;
        total.refine_merges += outcome.refine_merges;
        total.refine_splits += outcome.refine_splits;
        total.refine_updates += outcome.refine_updates;
        total.refine_new_entries += outcome.refine_new_entries;
        total.refine_kept += outcome.refine_kept;
        total.entries_decayed += outcome.entries_decayed;
        total.entries_skipped += outcome.entries_skipped;

        if !full_mode
            || (outcome.refine_merges == 0
                && outcome.refine_splits == 0
                && outcome.refine_updates == 0)
            || passes >= MAX_PASSES
        {
            break;
        }
    }

    info!(
        character = %char_name,
        passes,
        merges = total.refine_merges,
        splits = total.refine_splits,
        updates = total.refine_updates,
        decayed = total.entries_decayed,
        "Collation complete"
    );
    Ok(json!({
        "status": "collated",
        "character": char_name,
        "passes": passes,
        "timestamps_backfilled": total.timestamps_backfilled,
        "refine_merges": total.refine_merges,
        "refine_splits": total.refine_splits,
        "refine_updates": total.refine_updates,
        "refine_new_entries": total.refine_new_entries,
        "refine_kept": total.refine_kept,
        "entries_decayed": total.entries_decayed,
        "entries_skipped": total.entries_skipped,
    }))
}

fn collation_err(e: CollationError) -> (ErrorCode, String) {
    (ErrorCode::InternalError, e.to_string())
}

/// Purge old superseded entries to reclaim space.
pub async fn memory_purge(
    engine: &mut ConversationEngine,
    ctx: &CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let char_name = engine.character_name().to_string();
    let older_than_str = args
        .get("older_than")
        .and_then(|v| v.as_str())
        .unwrap_or("30d");

    let days = parse_duration_days(older_than_str).ok_or_else(|| {
        (
            ErrorCode::InvalidRequest,
            format!("Invalid duration: {older_than_str}. Use format like '30d' or '7d'."),
        )
    })?;

    let db = open_memory_db(ctx, &char_name)?;

    let cutoff = (chrono::Local::now() - chrono::Duration::days(days)).to_rfc3339();

    let superseded = db
        .get_entries_by_status("superseded")
        .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;

    let mut deleted = 0u64;
    let mut skipped_image = 0u64;
    let mut skipped_no_replacement = 0u64;

    let cutoff_dt = chrono::DateTime::parse_from_rfc3339(&cutoff).ok();

    for entry in &superseded {
        let dominated_by_cutoff = match (
            chrono::DateTime::parse_from_rfc3339(&entry.updated_at),
            &cutoff_dt,
        ) {
            (Ok(entry_dt), Some(cutoff_ref)) => entry_dt < *cutoff_ref,
            _ => entry.updated_at.as_str() < cutoff.as_str(),
        };
        if !dominated_by_cutoff {
            continue;
        }

        if !entry.image_path.is_empty() {
            skipped_image += 1;
            continue;
        }

        if entry.superseded_by.is_empty() {
            skipped_no_replacement += 1;
            continue;
        }

        let replacements_ok = entry
            .superseded_by
            .split(',')
            .map(|id| id.trim())
            .filter(|id| !id.is_empty())
            .all(|id| {
                db.get_entry(id)
                    .ok()
                    .flatten()
                    .map(|e| e.status == "active")
                    .unwrap_or(false)
            });

        if !replacements_ok {
            skipped_no_replacement += 1;
            continue;
        }

        let log_id = db
            .append_changelog(
                "purge",
                &format!(
                    "Purge superseded entry: {} (replaced by {})",
                    entry.id, entry.superseded_by
                ),
            )
            .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;
        let _ = db.link_changelog_entry(log_id, &entry.id);

        db.delete_entry(&entry.id)
            .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;

        deleted += 1;
    }

    if deleted > 0 {
        db.vacuum()
            .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;
    }

    info!(
        character = %char_name,
        deleted,
        skipped_image,
        skipped_no_replacement,
        older_than = older_than_str,
        "Memory purge complete"
    );
    Ok(json!({
        "status": "purged",
        "character": char_name,
        "deleted": deleted,
        "skipped_image": skipped_image,
        "skipped_no_replacement": skipped_no_replacement,
        "older_than": older_than_str,
    }))
}

fn parse_duration_days(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Some(stripped) = s.strip_suffix('d') {
        stripped.parse::<i64>().ok().filter(|&d| d > 0)
    } else {
        s.parse::<i64>().ok().filter(|&d| d > 0)
    }
}

/// Rebuild FTS and vector indexes from existing memory entries.
pub async fn memory_reindex(engine: &ConversationEngine, ctx: &CommandContext) -> CommandResult {
    let char_name = engine.character_name().to_string();

    let db = open_memory_db(ctx, &char_name)?;

    let entries = db.get_entries_by_status("active").map_err(|e| {
        (
            ErrorCode::InternalError,
            format!("Failed to load entries: {e}"),
        )
    })?;

    if entries.is_empty() {
        return Ok(json!({ "reindexed": 0, "message": "No active entries to reindex" }));
    }

    info!(character = %char_name, entries = entries.len(), "Memory reindex started");

    db.rebuild_fts().map_err(|e| {
        (
            ErrorCode::InternalError,
            format!("Failed to rebuild FTS: {e}"),
        )
    })?;

    let (store, embed_config) = open_embed_and_vectorstore(ctx, &char_name).await?;

    const EMBED_BATCH_SIZE: usize = 50;
    let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(entries.len());
    for chunk in entries.chunks(EMBED_BATCH_SIZE) {
        let texts: Vec<&str> = chunk.iter().map(|e| e.summary_text.as_str()).collect();
        let batch = ctx
            .llm_client
            .inner()
            .embed(
                &embed_config.provider,
                &embed_config.model_id,
                &embed_config.api_key,
                embed_config.base_url.as_deref(),
                &texts,
            )
            .await
            .map_err(|e| (ErrorCode::InternalError, format!("Embedding failed: {e}")))?;
        embeddings.extend(batch);
    }

    let pairs: Vec<(&str, &[f32])> = entries
        .iter()
        .zip(embeddings.iter())
        .map(|(e, v)| (e.id.as_str(), v.as_slice()))
        .collect();

    store.reindex(&pairs).await.map_err(|e| {
        (
            ErrorCode::InternalError,
            format!("Vector reindex failed: {e}"),
        )
    })?;

    info!(character = %char_name, reindexed = entries.len(), "Memory reindex complete");
    Ok(json!({
        "reindexed": entries.len(),
        "message": format!("Reindexed {} entries (FTS + vector)", entries.len()),
    }))
}

#[cfg(test)]
mod resolver_tests {
    //! Regression tests for [`resolve_compaction_model`] and [`resolve_collation_model`].
    //!
    //! Bug history: interactive `/compact` used to call `resolve_agent_model`, which
    //! consults `defaults.memory_agent` — so a user's `defaults.compaction` setting was
    //! silently ignored by the interactive entry point while the background path honored
    //! it. Parallel resolvers with no shared helper, no test locking them together.
    //!
    //! These tests pin the resolution chain for both operations and assert that
    //! `defaults.compaction` / `defaults.collation` take precedence over
    //! `defaults.memory_agent`. Don't delete without a matching DECISIONS.md entry.
    use super::{resolve_collation_model, resolve_compaction_model};
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

    /// Parallel test: collation also honors its dedicated default over memory_agent.
    #[test]
    fn collation_prefers_defaults_collation_over_memory_agent() {
        let config = make_config(DefaultsConfig {
            collation: Some("bg".to_string()),
            memory_agent: Some("minimax".to_string()),
            model: Some("primary".to_string()),
            ..DefaultsConfig::default()
        });
        let model = resolve_collation_model(&config).expect("resolved");
        assert_eq!(model.name, "bg");
    }

    #[test]
    fn collation_falls_back_to_memory_agent_when_unset() {
        let config = make_config(DefaultsConfig {
            collation: None,
            memory_agent: Some("minimax".to_string()),
            model: Some("primary".to_string()),
            ..DefaultsConfig::default()
        });
        let model = resolve_collation_model(&config).expect("resolved");
        assert_eq!(model.name, "minimax");
    }

    /// Cross-resolver parity: given identical defaults, compaction and collation
    /// return the same fallback chain *structurally*. Locks them together so a
    /// future change to one is loud if not mirrored to the other.
    #[test]
    fn compaction_and_collation_fallback_chains_are_parallel() {
        // memory_agent set, no dedicated defaults — both should fall back to it.
        let config = make_config(DefaultsConfig {
            memory_agent: Some("minimax".to_string()),
            model: Some("primary".to_string()),
            ..DefaultsConfig::default()
        });
        let comp = resolve_compaction_model(&config).expect("resolved");
        let col = resolve_collation_model(&config).expect("resolved");
        assert_eq!(comp.name, col.name);
        assert_eq!(comp.name, "minimax");

        // Only `model` set — both fall all the way through to it.
        let config = make_config(DefaultsConfig {
            model: Some("primary".to_string()),
            ..DefaultsConfig::default()
        });
        let comp = resolve_compaction_model(&config).expect("resolved");
        let col = resolve_collation_model(&config).expect("resolved");
        assert_eq!(comp.name, col.name);
        assert_eq!(comp.name, "primary");
    }
}
