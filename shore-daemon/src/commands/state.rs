use serde_json::json;
use shore_protocol::error::ErrorCode;
use shore_protocol::types::{ContentBlock, Role};
use tracing::{debug, info};

use crate::engine::ConversationEngine;
use crate::memory::agent::{CallerIdentity, MemoryAgent, RealAgentIndexer};
use crate::memory::agent_llm::RealAgentLlm;
use crate::memory::researcher::MemoryResearcher;
use shore_ledger::CallType;
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
use shore_config::resolve_prompt_template;

use crate::autonomy::activity::HourClassification;

use super::{
    build_collation_vars, memory_dir, resolve_agent_model, setup_search_context, CommandContext,
    CommandResult, MemoryShellSession,
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

/// Return system status: character, message count, model, token counts.
pub fn status(engine: &ConversationEngine, ctx: &CommandContext) -> CommandResult {
    let activity =
        ctx.autonomy
            .activity_stats(engine.character_name())
            .map(|(stats, msg_count)| {
                let classifications: Vec<&str> = stats
                    .hour_classifications
                    .iter()
                    .map(|c| match c {
                        HourClassification::Peak => "peak",
                        HourClassification::Trough => "trough",
                        HourClassification::Normal => "normal",
                    })
                    .collect();
                json!({
                    "hour_histogram": stats.hour_histogram.to_vec(),
                    "hour_classifications": classifications,
                    "has_sufficient_heatmap": stats.has_sufficient_heatmap,
                    "engagement_score": stats.engagement_score,
                    "sessions_per_day": stats.sessions_per_day,
                    "message_count": msg_count,
                })
            });

    // Show the effective model: runtime override → per-character/global default.
    let effective_model = ctx
        .active_model
        .as_deref()
        .or(ctx.config.app.defaults.model.as_deref());

    let tokens = ctx.session_tokens.lock().unwrap();
    Ok(json!({
        "character": engine.character_name(),
        "message_count": engine.message_count(),
        "active_model": effective_model,
        "config_dir": ctx.config.dirs.config.display().to_string(),
        "data_dir": ctx.config.dirs.data.display().to_string(),
        "tokens": {
            "input": tokens.input,
            "output": tokens.output,
            "cache_read": tokens.cache_read,
            "cache_write": tokens.cache_write,
        },
        "autonomy": ctx.autonomy.status(engine.character_name()),
        "activity": activity,
    }))
}

/// Return recent diagnostics from in-memory ring buffers.
pub fn diagnostics(ctx: &CommandContext, args: &serde_json::Value) -> CommandResult {
    let count = args.get("count").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let diag = ctx.diagnostics.lock().unwrap();
    Ok(diag.to_json(count))
}

/// Return heartbeat event log for the active character.
pub fn heartbeat_log(
    engine: &ConversationEngine,
    ctx: &CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let limit = args.get("count").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    let events = ctx.autonomy.heartbeat_log(engine.character_name(), limit);
    let events_json: Vec<serde_json::Value> = events
        .iter()
        .map(|e| {
            json!({
                "timestamp": e.timestamp,
                "kind": e.kind,
                "detail": e.detail,
            })
        })
        .collect();
    Ok(json!({ "events": events_json }))
}

/// Force an interiority tick to fire on the next poll (~10s).
pub fn force_tick(
    engine: &ConversationEngine,
    ctx: &CommandContext,
) -> CommandResult {
    let char_name = engine.character_name();
    let triggered = ctx.autonomy.force_tick(char_name);
    if triggered {
        Ok(json!({ "status": "scheduled", "character": char_name,
                    "note": "Tick will fire within ~10 seconds" }))
    } else {
        Err((
            ErrorCode::InvalidRequest,
            format!("No autonomy state for character '{char_name}'"),
        ))
    }
}

/// List available model profiles from the model catalog.
pub fn list_models(ctx: &CommandContext) -> CommandResult {
    let mut models: Vec<_> = ctx
        .config
        .models
        .chat
        .values()
        .map(|m| {
            json!({
                "name": m.name,
                "qualified_name": m.qualified_name,
                "sdk": m.sdk.as_str(),
                "provider": m.provider_key,
                "model_id": m.model_id,
            })
        })
        .collect();

    // Also include tool models.
    for m in ctx.config.models.tools.values() {
        models.push(json!({
            "name": m.name,
            "qualified_name": m.qualified_name,
            "sdk": m.sdk.as_str(),
            "provider": m.provider_key,
            "model_id": m.model_id,
        }));
    }

    Ok(json!({
        "models": models,
        "active": ctx.active_model,
    }))
}

/// Show detailed info for a model. If no name given, uses the active model.
pub fn model_info(ctx: &CommandContext, args: &serde_json::Value) -> CommandResult {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or(ctx.active_model.as_deref());

    let name = name.ok_or_else(|| {
        (
            ErrorCode::InvalidRequest,
            "No model specified and no active model set".into(),
        )
    })?;

    let resolved = ctx
        .config
        .models
        .find_model(name)
        .map_err(|e| (ErrorCode::NotFound, e.to_string()))?;

    let data = serde_json::to_value(resolved).map_err(|e| {
        (
            ErrorCode::InternalError,
            format!("Failed to serialize model: {e}"),
        )
    })?;

    Ok(data)
}

/// Switch model or show current. Validates against model catalog.
pub fn switch_model(ctx: &mut CommandContext, args: &serde_json::Value) -> CommandResult {
    let name = args.get("name").and_then(|v| v.as_str());

    match name {
        None => Ok(json!({ "active": ctx.active_model })),
        Some(name) => {
            if ctx.config.models.find_model(name).is_err() {
                return Err((
                    ErrorCode::NotFound,
                    format!("Model not found: {name}. Use list_models to see available models."),
                ));
            }
            ctx.active_model = Some(name.to_string());
            info!(model = name, "Model switched");
            Ok(json!({ "active": name, "changed": true }))
        }
    }
}

/// Reset model to config default.
pub fn reset_model(ctx: &mut CommandContext) -> CommandResult {
    let previous = ctx.active_model.take();
    ctx.active_model = ctx.config.app.defaults.model.clone();
    Ok(json!({
        "previous": previous,
        "active": ctx.active_model,
        "reset_to": "config default",
    }))
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

    debug!(character = char_name, count = entries.len(), "Memory changelog queried");
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
            debug!(character = engine.character_name(), query_len = q.len(), direct, "Memory query requested");
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

    debug!(character = char_name, entries, entities, active, "Memory status queried");
    Ok(json!({
        "character": char_name,
        "entries": entries,
        "entities": entities,
        "active_entries": active,
    }))
}

/// Run a memory query through the researcher→agent pipeline (matching tool-use path).
///
/// When `direct` is true, skips the researcher and queries the agent directly.
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
    let agent_llm = RealAgentLlm::new(ctx.llm_client.clone(), char_name.to_string(), CallType::MemoryAgent);

    let search_ctx = setup_search_context(ctx, char_name).await;
    let real_indexer = search_ctx.as_ref().map(RealAgentIndexer::new);
    let indexer = real_indexer
        .as_ref()
        .map(|i| i as &dyn crate::memory::agent::AgentIndexer);

    // Resolve the researcher model from defaults.tool_model (same as the handler path).
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
        // Full researcher→agent pipeline (matches tool-use path).
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
        // Direct agent query (no researcher configured, or --direct flag).
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

// ---------------------------------------------------------------------------
// Memory shell (interactive REPL)
// ---------------------------------------------------------------------------

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

    // Extract character name before mutable borrow of sessions.
    let char_name = ctx
        .memory_shell_sessions
        .get(session_id)
        .ok_or_else(|| (ErrorCode::NotFound, "Session not found".to_string()))?
        .character
        .clone();

    let db = open_memory_db(ctx, &char_name)?;
    let agent_llm = RealAgentLlm::new(ctx.llm_client.clone(), char_name.clone(), CallType::MemoryAgent);
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
            None, // no confirmation callback
        )
        .await
        .map_err(|e| (ErrorCode::InternalError, format!("Memory agent error: {e}")))?;

    // The last assistant message in history is the response.
    let response = session
        .history
        .last()
        .and_then(|msg| msg.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    debug!(session_id, mutations, response_len = response.len(), "Memory shell query complete");
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
    let collation_model = resolve_collation_model(&ctx.config);

    let cmodel = collation_model?;

    let collation_llm = RealCollationLlm::new(ctx.llm_client.clone(), cmodel, char_name.to_string());
    let refine_template = resolve_prompt_template(&ctx.config.dirs.config, char_name, "refine.md")
        .unwrap_or_else(|| DEFAULT_REFINE_PROMPT.to_string());

    let collation_mgr = CollationManager::new(DecayConfig::default());
    let collation_limit = ctx.config.app.memory.collation.batch_limit;

    // Open a second vector store for collation (the compaction one was moved).
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
///
/// Extracts memories from the active conversation via an LLM, stores them
/// in the memory database, indexes them in the vector store, and archives
/// the conversation to a segment file.
pub async fn compact(
    engine: &mut ConversationEngine,
    ctx: &CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let char_name = engine.character_name().to_string();

    // Convert engine messages to compaction format.
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

    // Open the memory database.
    let db = open_memory_db(ctx, &char_name)?;

    // Resolve the compaction prompt template.
    let prompt_template =
        resolve_prompt_template(&ctx.config.dirs.config, &char_name, "compact.md")
            .unwrap_or_else(|| DEFAULT_COMPACT_PROMPT.to_string());

    // Resolve model: memory_agent default → active model → first chat model.
    let model = super::resolve_agent_model(ctx)?;

    // Resolve embedding + vector store.
    let (store, embed_config) = super::open_embed_and_vectorstore(ctx, &char_name).await?;

    // Create trait implementations.
    let llm = RealCompactionLlm::new(ctx.llm_client.clone(), model, char_name.clone());
    let indexer = RealVectorIndexer::new(store, ctx.llm_client.inner().clone(), embed_config);
    let conv_mgr = RealConversationManager::new(engine.character_dir());

    // Create compaction manager with config.
    let mgr = CompactionManager::new(ctx.config.app.memory.compaction.clone());

    // Load existing recap for folding.
    let recap_path = memory_dir(ctx, &char_name).join("recap.md");
    let existing_recap = std::fs::read_to_string(&recap_path).ok();

    // Read the active.jsonl content while the engine lock is held, so
    // archive_and_retain uses the same snapshot the messages were parsed from.
    let active_content = std::fs::read_to_string(engine.character_dir().join("active.jsonl"))
        .map_err(|e| {
            (
                ErrorCode::InternalError,
                format!("failed to read active.jsonl: {e}"),
            )
        })?;

    // Run compaction.
    let display_name = ctx.config.app.defaults.resolve_display_name();
    let outcome = mgr
        .compact(
            &char_name,
            &messages,
            &active_content,
            false, // is_private: V2 has no per-conversation privacy flag yet
            &prompt_template,
            existing_recap.as_deref(),
            &char_name,
            &display_name,
            &llm,
            &db,
            &indexer,
            &conv_mgr,
            dry_run,
        )
        .await
        .map_err(compaction_err)?;

    // Build response and handle post-compaction engine state.
    match outcome {
        CompactionOutcome::Compacted(result) => {
            info!(
                character = %char_name,
                entries_created = result.entries_created.len(),
                message_count = result.message_count,
                retained_count = result.retained_count,
                "Compaction complete"
            );
            // Reload retained messages from disk (active.jsonl now has only kept messages).
            engine
                .reload()
                .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;

            // Run collation after compaction if enabled and requested.
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

/// Map `CompactionError` to a command error tuple.
fn compaction_err(e: CompactionError) -> (ErrorCode, String) {
    match &e {
        CompactionError::PrivateConversation | CompactionError::InsufficientMessages => {
            (ErrorCode::InvalidRequest, e.to_string())
        }
        _ => (ErrorCode::InternalError, e.to_string()),
    }
}

/// Resolve the model to use for collation.
///
/// Fallback chain: `defaults.collation` → `defaults.memory_agent` →
/// `defaults.model` → first chat model.
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

/// Run the 4-phase collation pipeline on the current character's memory.
///
/// Phases: tidy (split broad entries), collate (merge similar), normalize
/// entities, and confidence decay.
pub async fn collate(
    engine: &mut ConversationEngine,
    ctx: &CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let char_name = engine.character_name().to_string();
    let full_mode = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);

    // Open the memory database.
    let db = open_memory_db(ctx, &char_name)?;

    let model = resolve_collation_model(&ctx.config)
        .ok_or_else(|| (ErrorCode::InternalError, "No model configured".to_string()))?;

    let llm = RealCollationLlm::new(ctx.llm_client.clone(), model, char_name.clone());

    // Resolve prompt template.
    let refine_template = resolve_prompt_template(&ctx.config.dirs.config, &char_name, "refine.md")
        .unwrap_or_else(|| DEFAULT_REFINE_PROMPT.to_string());

    let mgr = CollationManager::new(DecayConfig::default());

    // Resolve batch limit: CLI --limit overrides config default.
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

/// Map `CollationError` to a command error tuple.
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

    // Parse duration string (e.g., "30d", "7d").
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

    for entry in &superseded {
        // Only purge entries older than the cutoff.
        if entry.updated_at.as_str() >= cutoff.as_str() {
            continue;
        }

        // Don't delete image entries — attachment files need separate handling.
        if !entry.image_path.is_empty() {
            skipped_image += 1;
            continue;
        }

        // Verify replacement(s) still exist and are active.
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

        // Log to changelog before deleting.
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

    // VACUUM to reclaim space after bulk deletes.
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

/// Parse a duration string like "30d" into days.
fn parse_duration_days(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Some(stripped) = s.strip_suffix('d') {
        stripped.parse::<i64>().ok().filter(|&d| d > 0)
    } else {
        // Try plain number as days.
        s.parse::<i64>().ok().filter(|&d| d > 0)
    }
}

/// Validate configuration and return warnings/info.
pub fn config_check(ctx: &CommandContext) -> CommandResult {
    let mut warnings: Vec<String> = Vec::new();
    let mut info: Vec<String> = Vec::new();

    // Check: any chat models configured?
    if ctx.config.models.chat.is_empty() {
        warnings.push("No chat models configured. Add [chat.*] sections to config.".into());
    } else {
        info.push(format!(
            "{} chat model(s) configured",
            ctx.config.models.chat.len()
        ));
    }

    // Check: default model set?
    match &ctx.config.app.defaults.model {
        Some(m) => {
            if ctx.config.models.find_model(m).is_ok() {
                info.push(format!("Default model: {m}"));
            } else {
                warnings.push(format!("Default model \"{m}\" not found in catalog"));
            }
        }
        None => {
            if !ctx.config.models.chat.is_empty() {
                warnings.push("No default model set. First chat model will be used.".into());
            }
        }
    }

    // Check: tool models
    if ctx.config.models.tools.is_empty() {
        info.push("No tool models configured (chat models will be used for tools)".into());
    } else {
        info.push(format!(
            "{} tool model(s) configured",
            ctx.config.models.tools.len()
        ));
    }

    // Check: embedding models
    if ctx.config.models.embedding.is_empty() {
        warnings.push(
            "No embedding models configured. Memory vector search will be unavailable.".into(),
        );
    } else {
        info.push(format!(
            "{} embedding model(s) configured",
            ctx.config.models.embedding.len()
        ));
    }

    // Check: default embedding reference
    if let Some(ref emb) = ctx.config.app.defaults.embedding {
        if !ctx.config.models.embedding.contains_key(emb.as_str()) {
            warnings.push(format!("Default embedding \"{emb}\" not found in catalog"));
        }
    }

    // Check: memory agent model reference
    if let Some(ref ma) = ctx.config.app.defaults.memory_agent {
        if ctx.config.models.find_model(ma).is_err() {
            warnings.push(format!(
                "Default memory_agent \"{ma}\" not found in catalog"
            ));
        }
    }

    // Check: LLM service configured?
    if ctx.config.app.services.llm.command.is_none() && ctx.config.app.services.llm.socket.is_none()
    {
        warnings
            .push("No LLM service configured. Set [services.llm] command or socket_path.".into());
    }

    // Check: API key env vars are set for configured providers
    for model in ctx.config.models.chat.values() {
        if let Some(ref key_env) = model.api_key_env {
            if std::env::var(key_env).is_err() {
                warnings.push(format!(
                    "API key env var ${} not set (needed by model {})",
                    key_env, model.qualified_name
                ));
            }
        }
    }

    let valid = warnings.is_empty();

    Ok(json!({
        "valid": valid,
        "warnings": warnings,
        "info": info,
        "config_dir": ctx.config.dirs.config.display().to_string(),
        "data_dir": ctx.config.dirs.data.display().to_string(),
        "chat_models": ctx.config.models.chat.len(),
        "tool_models": ctx.config.models.tools.len(),
        "embedding_models": ctx.config.models.embedding.len(),
    }))
}

/// Show effective configuration. Optionally filtered by section name.
/// Rebuild FTS and vector indexes from existing memory entries.
pub async fn memory_reindex(engine: &ConversationEngine, ctx: &CommandContext) -> CommandResult {
    let char_name = engine.character_name().to_string();

    // Open memory DB.
    let db = open_memory_db(ctx, &char_name)?;

    // Load all active entries.
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

    // Rebuild FTS index.
    db.rebuild_fts().map_err(|e| {
        (
            ErrorCode::InternalError,
            format!("Failed to rebuild FTS: {e}"),
        )
    })?;

    // Resolve embedding config and rebuild vector index.
    let (store, embed_config) = super::open_embed_and_vectorstore(ctx, &char_name).await?;

    // Embed entries in batches to avoid overrunning the Unix socket with a
    // single huge JSON response (the socket may close before the client
    // finishes reading).
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

pub fn config(ctx: &mut CommandContext, args: &serde_json::Value) -> CommandResult {
    let key = args.get("key").and_then(|v| v.as_str());
    let value = args.get("value").and_then(|v| v.as_str());

    // If both key and value are present, this is a config set operation.
    if let (Some(key), Some(value)) = (key, value) {
        return config_set(ctx, key, value);
    }

    // Otherwise, read-only config display.
    let app_json = serde_json::to_value(&ctx.config.app).map_err(|e| {
        (
            ErrorCode::InternalError,
            format!("Failed to serialize config: {e}"),
        )
    })?;

    match key {
        None => Ok(json!({ "config": app_json })),
        Some(name) => match app_json.get(name) {
            Some(data) => Ok(json!({ "key": name, "config": data })),
            None => Err((
                ErrorCode::NotFound,
                format!("Config section not found: {name}"),
            )),
        },
    }
}

/// Set a runtime config value. Only a focused set of keys are supported.
fn config_set(ctx: &mut CommandContext, key: &str, value: &str) -> CommandResult {
    match key {
        "defaults.model" | "model" => {
            // Validate the model exists.
            let _ = ctx.config.models.find_model(value)
                .map_err(|e| (ErrorCode::NotFound, format!("{e}")))?;
            ctx.active_model = Some(value.to_string());
            Ok(json!({ "set": key, "value": value }))
        }
        "defaults.stream" | "stream" => {
            let v: bool = value.parse()
                .map_err(|_| (ErrorCode::InvalidRequest, "expected true or false".into()))?;
            ctx.config.app.defaults.stream = v;
            Ok(json!({ "set": key, "value": v }))
        }
        "autonomy.enabled" | "behavior.autonomy.enabled" => {
            let v: bool = value.parse()
                .map_err(|_| (ErrorCode::InvalidRequest, "expected true or false".into()))?;
            ctx.config.app.behavior.autonomy.enabled = v;
            Ok(json!({ "set": "autonomy.enabled", "value": v }))
        }
        _ => Err((
            ErrorCode::InvalidRequest,
            format!("Config key not settable at runtime: {key}. Supported: defaults.model, defaults.stream, autonomy.enabled"),
        )),
    }
}

/// Reset all runtime config overrides by reloading from disk.
pub fn config_reset(ctx: &mut CommandContext) -> CommandResult {
    match shore_config::load_config(None) {
        Ok(fresh) => {
            ctx.active_model = None;
            ctx.config = fresh;
            info!("Configuration reloaded from disk");
            Ok(json!({ "reset": true, "message": "Configuration reloaded from disk" }))
        }
        Err(e) => Err((
            ErrorCode::InternalError,
            format!("Failed to reload config: {e}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CommandContext;
    use crate::engine::ConversationEngine;
    use shore_config::models::ModelCatalog;
    use shore_protocol::server_msg::ServerMessage;
    use shore_protocol::types::{Message, Role};
    use tempfile::TempDir;
    use tokio::sync::broadcast;

    fn make_ctx(
        tmp: &TempDir,
    ) -> (
        ConversationEngine,
        CommandContext,
        broadcast::Receiver<ServerMessage>,
    ) {
        make_ctx_with_models(tmp, ModelCatalog::default())
    }

    fn make_ctx_with_models(
        tmp: &TempDir,
        models: ModelCatalog,
    ) -> (
        ConversationEngine,
        CommandContext,
        broadcast::Receiver<ServerMessage>,
    ) {
        let (push_tx, push_rx) = broadcast::channel(16);
        let data_dir = tmp.path().to_path_buf();
        let engine =
            ConversationEngine::new("TestChar".to_string(), data_dir.clone(), push_tx.clone())
                .unwrap();

        let config = shore_config::LoadedConfig::new_for_test(
            shore_config::app::AppConfig::default(),
            models,
            shore_config::ShoreDirs {
                config: tmp.path().join("config"),
                data: data_dir.clone(),
                runtime: tmp.path().join("runtime"),
            },
        );

        let (_tx, rx) = tokio::sync::watch::channel(());
        let (autonomy, _compaction_rx) = crate::autonomy::manager::AutonomyManager::new(
            Default::default(),
            Default::default(),
            data_dir.clone(),
            rx,
        );

        let ctx = CommandContext {
            config,
            push_tx,
            data_dir: data_dir.clone(),
            active_model: None,
            session_tokens: std::sync::Arc::new(std::sync::Mutex::new(
                crate::commands::SessionTokens::default(),
            )),
            autonomy,
            llm_client: shore_ledger::LedgerClient::new(shore_llm_client::LlmClient::new(), &data_dir.join("ledger.db")).unwrap(),
            diagnostics: std::sync::Arc::new(std::sync::Mutex::new(
                shore_diagnostics::Diagnostics::default(),
            )),
            memory_shell_sessions: std::collections::HashMap::new(),
        };
        (engine, ctx, push_rx)
    }

    fn sample_models() -> ModelCatalog {
        let toml_str = r#"
[anthropic.claude-sonnet]
model_id = "claude-sonnet-4-20250514"

[openrouter.gpt-4o]
model_id = "gpt-4o"
"#;
        let table: toml::Table = toml_str.parse().unwrap();
        ModelCatalog::from_sections(Some(&table), None, None, None).unwrap()
    }

    fn make_msg(id: &str, role: Role, content: &str) -> Message {
        Message {
            msg_id: id.to_string(),
            role,
            content: content.to_string(),
            images: vec![],
            content_blocks: vec![],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn status_returns_state() {
        let tmp = TempDir::new().unwrap();
        let (engine, mut ctx, _rx) = make_ctx(&tmp);
        ctx.active_model = Some("claude-sonnet".into());

        let result = status(&engine, &ctx).unwrap();
        assert_eq!(result["character"], "TestChar");
        assert_eq!(result["message_count"], 0);
        assert_eq!(result["active_model"], "claude-sonnet");
    }

    #[test]
    fn status_with_messages() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, ctx, _rx) = make_ctx(&tmp);
        engine
            .append_message(make_msg("m1", Role::User, "Hi"))
            .unwrap();

        let result = status(&engine, &ctx).unwrap();
        assert_eq!(result["message_count"], 1);
    }

    #[test]
    fn list_models_empty() {
        let tmp = TempDir::new().unwrap();
        let (_engine, ctx, _rx) = make_ctx(&tmp);

        let result = list_models(&ctx).unwrap();
        assert!(result["models"].as_array().unwrap().is_empty());
        assert!(result["active"].is_null());
    }

    #[test]
    fn list_models_with_profiles() {
        let tmp = TempDir::new().unwrap();
        let (_engine, ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

        let result = list_models(&ctx).unwrap();
        let models = result["models"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0]["name"], "claude-sonnet");
        assert_eq!(models[1]["name"], "gpt-4o");
    }

    #[test]
    fn switch_model_show_current() {
        let tmp = TempDir::new().unwrap();
        let (_engine, mut ctx, _rx) = make_ctx(&tmp);
        ctx.active_model = Some("claude-sonnet".into());

        let result = switch_model(&mut ctx, &json!({})).unwrap();
        assert_eq!(result["active"], "claude-sonnet");
    }

    #[test]
    fn switch_model_valid() {
        let tmp = TempDir::new().unwrap();
        let (_engine, mut ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

        let result = switch_model(&mut ctx, &json!({"name": "gpt-4o"})).unwrap();
        assert_eq!(result["active"], "gpt-4o");
        assert_eq!(result["changed"], true);
        assert_eq!(ctx.active_model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn switch_model_not_found() {
        let tmp = TempDir::new().unwrap();
        let (_engine, mut ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

        let result = switch_model(&mut ctx, &json!({"name": "nonexistent"}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
    }

    #[test]
    fn model_info_by_name() {
        let tmp = TempDir::new().unwrap();
        let (_engine, ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

        let result = model_info(&ctx, &json!({"name": "claude-sonnet"})).unwrap();
        assert_eq!(result["name"], "claude-sonnet");
        assert_eq!(result["model_id"], "claude-sonnet-4-20250514");
        assert!(result["sdk"].is_string());
    }

    #[test]
    fn model_info_uses_active_model() {
        let tmp = TempDir::new().unwrap();
        let (_engine, mut ctx, _rx) = make_ctx_with_models(&tmp, sample_models());
        ctx.active_model = Some("gpt-4o".into());

        let result = model_info(&ctx, &json!({})).unwrap();
        assert_eq!(result["name"], "gpt-4o");
    }

    #[test]
    fn model_info_no_model() {
        let tmp = TempDir::new().unwrap();
        let (_engine, ctx, _rx) = make_ctx(&tmp);

        let result = model_info(&ctx, &json!({}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn model_info_not_found() {
        let tmp = TempDir::new().unwrap();
        let (_engine, ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

        let result = model_info(&ctx, &json!({"name": "nonexistent"}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn memory_status_no_db() {
        let tmp = TempDir::new().unwrap();
        let (engine, ctx, _rx) = make_ctx(&tmp);

        let result = memory(&engine, &ctx, &json!({})).await.unwrap();
        assert_eq!(result["character"], "TestChar");
        assert_eq!(result["entries"], 0);
        assert_eq!(result["entities"], 0);
        assert_eq!(result["active_entries"], 0);
    }

    #[tokio::test]
    async fn memory_status_with_db() {
        let tmp = TempDir::new().unwrap();
        let (engine, ctx, _rx) = make_ctx(&tmp);

        // Create a memory DB with some entries.
        let db_dir = ctx.data_dir.join("TestChar").join("memory");
        std::fs::create_dir_all(&db_dir).unwrap();
        let db = MemoryDB::open(&db_dir.join("memory.db")).unwrap();
        db.create_entry(&crate::memory::db::Entry {
            id: "test_1".into(),
            memory_type: "semantic".into(),
            source: "test".into(),
            reason: "test".into(),
            status: "active".into(),
            confidence: 0.9,
            summary_text: "Test entry".into(),
            topic_tags: "test".into(),
            topic_key: "test".into(),
            start_timestamp: String::new(),
            end_timestamp: String::new(),
            message_count: 0,
            source_entry_ids: String::new(),
            related_entry_ids: String::new(),
            superseded_by: String::new(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            entry_type: String::new(),
            image_path: String::new(),
            collated_at: String::new(),
        })
        .unwrap();
        drop(db);

        let result = memory(&engine, &ctx, &json!({})).await.unwrap();
        assert_eq!(result["character"], "TestChar");
        assert_eq!(result["entries"], 1);
        assert_eq!(result["active_entries"], 1);
    }

    #[tokio::test]
    async fn memory_status_null_query_is_status() {
        // Sending {"query": null} (what CLI sends with no arg) should return status.
        let tmp = TempDir::new().unwrap();
        let (engine, ctx, _rx) = make_ctx(&tmp);

        let result = memory(&engine, &ctx, &json!({"query": null}))
            .await
            .unwrap();
        assert_eq!(result["character"], "TestChar");
        assert!(result.get("entries").is_some());
    }

    #[tokio::test]
    async fn compact_empty_conversation_returns_error() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, ctx, _rx) = make_ctx(&tmp);

        let result = compact(&mut engine, &ctx, &json!({})).await;
        assert!(result.is_err());
        let (code, msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
        assert!(msg.contains("No messages"));
    }

    #[tokio::test]
    async fn collate_no_model_returns_error() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, ctx, _rx) = make_ctx(&tmp);

        // Create memory DB so it opens successfully.
        let db_dir = ctx.data_dir.join("TestChar").join("memory");
        std::fs::create_dir_all(&db_dir).unwrap();
        let _db = MemoryDB::open(&db_dir.join("memory.db")).unwrap();

        let result = collate(&mut engine, &ctx, &json!({})).await;
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InternalError);
    }

    #[test]
    fn config_check_empty_catalog() {
        let tmp = TempDir::new().unwrap();
        let (_engine, ctx, _rx) = make_ctx(&tmp);

        let result = config_check(&ctx).unwrap();
        assert!(!result["valid"].as_bool().unwrap());
        let warnings = result["warnings"].as_array().unwrap();
        assert!(warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("No chat models")));
        assert_eq!(result["chat_models"], 0);
    }

    #[test]
    fn config_check_with_models() {
        let tmp = TempDir::new().unwrap();
        let (_engine, ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

        let result = config_check(&ctx).unwrap();
        assert_eq!(result["chat_models"], 2);
        let info = result["info"].as_array().unwrap();
        assert!(info
            .iter()
            .any(|i| i.as_str().unwrap().contains("2 chat model")));
    }

    #[test]
    fn config_full() {
        let tmp = TempDir::new().unwrap();
        let (_engine, mut ctx, _rx) = make_ctx(&tmp);

        let result = config(&mut ctx, &json!({})).unwrap();
        assert!(result["config"].is_object());
    }

    #[test]
    fn config_section() {
        let tmp = TempDir::new().unwrap();
        let (_engine, mut ctx, _rx) = make_ctx(&tmp);

        let result = config(&mut ctx, &json!({"key": "defaults"})).unwrap();
        assert_eq!(result["key"], "defaults");
    }

    #[test]
    fn config_section_not_found() {
        let tmp = TempDir::new().unwrap();
        let (_engine, mut ctx, _rx) = make_ctx(&tmp);

        let result = config(&mut ctx, &json!({"key": "nonexistent"}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
    }

    // ── diagnostics / heartbeat / reset_model ──────────────────────────

    #[test]
    fn test_diagnostics_empty() {
        let tmp = TempDir::new().unwrap();
        let (_engine, ctx, _rx) = make_ctx(&tmp);

        let result = diagnostics(&ctx, &json!({})).unwrap();
        assert_eq!(result["api_calls"]["count"], 0);
        assert_eq!(result["tool_calls"]["count"], 0);
        assert_eq!(result["errors"]["count"], 0);
        assert!(result["api_calls"]["recent"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_heartbeat_log_empty() {
        let tmp = TempDir::new().unwrap();
        let (engine, ctx, _rx) = make_ctx(&tmp);

        let result = heartbeat_log(&engine, &ctx, &json!({})).unwrap();
        assert!(result["events"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_reset_model_clears_override() {
        let tmp = TempDir::new().unwrap();
        let (_engine, mut ctx, _rx) = make_ctx(&tmp);

        ctx.active_model = Some("custom-override".to_string());
        let result = reset_model(&mut ctx).unwrap();

        assert_eq!(result["previous"], "custom-override");
        assert_eq!(result["reset_to"], "config default");
        // AppConfig::default() has no defaults.model, so active_model should be None.
        assert!(ctx.active_model.is_none());
    }

    // ── memory_purge ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn memory_purge_deletes_old_superseded_entries() {
        use crate::memory::db::Entry;

        let tmp = TempDir::new().unwrap();
        let (mut engine, ctx, _rx) = make_ctx(&tmp);

        // Create memory DB on disk at the path memory_purge expects.
        let mem_dir = ctx.data_dir.join("TestChar").join("memory");
        std::fs::create_dir_all(&mem_dir).unwrap();
        let db = MemoryDB::open(&mem_dir.join("memory.db")).unwrap();

        let old_ts = "2020-01-01T00:00:00Z".to_string();
        let recent_ts = chrono::Local::now().to_rfc3339();
        let empty = String::new();

        let make_entry =
            |id: &str, status: &str, superseded_by: &str, image_path: &str, ts: &str| Entry {
                id: id.into(),
                memory_type: "observation".into(),
                source: "test".into(),
                reason: empty.clone(),
                status: status.into(),
                confidence: 1.0,
                summary_text: format!("Entry {id}"),
                topic_tags: empty.clone(),
                topic_key: empty.clone(),
                start_timestamp: empty.clone(),
                end_timestamp: empty.clone(),
                message_count: 0,
                source_entry_ids: empty.clone(),
                related_entry_ids: empty.clone(),
                superseded_by: superseded_by.into(),
                created_at: ts.into(),
                updated_at: ts.into(),
                entry_type: empty.clone(),
                image_path: image_path.into(),
                collated_at: empty.clone(),
            };

        // Active entry that serves as the replacement target.
        db.create_entry(&make_entry("active-1", "active", "", "", &old_ts))
            .unwrap();
        // Old superseded entry with valid replacement → should be deleted.
        db.create_entry(&make_entry(
            "old-superseded",
            "superseded",
            "active-1",
            "",
            &old_ts,
        ))
        .unwrap();
        // Recent superseded entry → should be skipped (not old enough).
        db.create_entry(&make_entry(
            "recent-superseded",
            "superseded",
            "active-1",
            "",
            &recent_ts,
        ))
        .unwrap();
        // Superseded entry with image_path → should be skipped.
        db.create_entry(&make_entry(
            "image-superseded",
            "superseded",
            "active-1",
            "/some/image.png",
            &old_ts,
        ))
        .unwrap();
        // Superseded entry with empty superseded_by → should be skipped.
        db.create_entry(&make_entry("no-replacement", "superseded", "", "", &old_ts))
            .unwrap();

        drop(db); // Close so memory_purge can open it.

        let result = memory_purge(&mut engine, &ctx, &json!({"older_than": "1d"}))
            .await
            .unwrap();

        assert_eq!(result["deleted"], 1);
        assert_eq!(result["skipped_image"], 1);
        assert_eq!(result["skipped_no_replacement"], 1);

        // Verify the entry was actually removed from the DB.
        let db = MemoryDB::open(&mem_dir.join("memory.db")).unwrap();
        assert!(db.get_entry("old-superseded").unwrap().is_none());
        assert!(db.get_entry("recent-superseded").unwrap().is_some());
        assert!(db.get_entry("image-superseded").unwrap().is_some());
        assert!(db.get_entry("no-replacement").unwrap().is_some());
    }

    // ── config_reset ────────────────────────────────────────────────────

    #[test]
    fn config_reset_clears_active_model_and_reloads() {
        let tmp = TempDir::new().unwrap();
        let (_engine, mut ctx, _rx) = make_ctx(&tmp);

        // Simulate a runtime model override.
        ctx.active_model = Some("custom-override".into());
        // Mutate a config value so we can detect that it was replaced.
        ctx.config.app.defaults.stream = false;

        let result = config_reset(&mut ctx).unwrap();

        assert_eq!(result["reset"], true);
        assert!(ctx.active_model.is_none(), "active_model should be cleared");
        // load_config(None) returns defaults when no config file exists,
        // so stream should be back to the default (true).
        assert!(
            ctx.config.app.defaults.stream,
            "config should be reloaded from defaults"
        );
    }

    // ── memory_reindex ──────────────────────────────────────────────────

    #[tokio::test]
    async fn memory_reindex_empty_returns_zero() {
        let tmp = TempDir::new().unwrap();
        let (engine, ctx, _rx) = make_ctx(&tmp);

        let db_dir = ctx.data_dir.join("TestChar").join("memory");
        std::fs::create_dir_all(&db_dir).unwrap();
        let _db = MemoryDB::open(&db_dir.join("memory.db")).unwrap();

        let result = memory_reindex(&engine, &ctx).await.unwrap();
        assert_eq!(result["reindexed"], 0);
        assert!(result["message"]
            .as_str()
            .unwrap()
            .contains("No active entries"));
    }

    #[tokio::test]
    #[ignore = "Requires OPENROUTER_SHORE_EMBEDDING"]
    async fn memory_reindex_rebuilds_fts_and_vectors() {
        if std::env::var("OPENROUTER_SHORE_EMBEDDING").is_err() {
            panic!("OPENROUTER_SHORE_EMBEDDING not set");
        }

        let tmp = TempDir::new().unwrap();

        let embed_toml: toml::Table = r#"
[text-embed]
model_id = "openai/text-embedding-3-small"
provider = "openai"
api_key_env = "OPENROUTER_SHORE_EMBEDDING"
base_url = "https://openrouter.ai/api/v1"
dimensions = 1536
"#
        .parse()
        .unwrap();
        let models = ModelCatalog::from_sections(None, None, Some(&embed_toml), None).unwrap();
        let (engine, ctx, _rx) = make_ctx_with_models(&tmp, models);

        let db_dir = ctx.data_dir.join("TestChar").join("memory");
        std::fs::create_dir_all(&db_dir).unwrap();
        let db = MemoryDB::open(&db_dir.join("memory.db")).unwrap();

        for i in 0..3 {
            db.create_entry(&crate::memory::db::Entry {
                id: format!("entry_{i}"),
                memory_type: "semantic".into(),
                source: "test".into(),
                reason: "reindex test".into(),
                status: "active".into(),
                confidence: 0.9,
                summary_text: format!("Test memory entry number {i} about various topics"),
                topic_tags: "test".into(),
                topic_key: "test".into(),
                start_timestamp: String::new(),
                end_timestamp: String::new(),
                message_count: 0,
                source_entry_ids: String::new(),
                related_entry_ids: String::new(),
                superseded_by: String::new(),
                created_at: "2026-01-01T00:00:00Z".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
                entry_type: String::new(),
                image_path: String::new(),
                collated_at: String::new(),
            })
            .unwrap();
        }
        drop(db);

        let result = memory_reindex(&engine, &ctx).await.unwrap();
        assert_eq!(result["reindexed"], 3);
        assert!(result["message"]
            .as_str()
            .unwrap()
            .contains("Reindexed 3 entries"));
    }
}
