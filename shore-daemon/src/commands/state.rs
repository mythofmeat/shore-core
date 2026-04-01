use serde_json::json;
use shore_protocol::error::ErrorCode;
use shore_protocol::types::{ContentBlock, Role};

use shore_config::resolve_prompt_template;
use crate::engine::ConversationEngine;
use crate::memory::agent::{AgentSearchContext, CallerIdentity, MemoryAgent, RealAgentIndexer};
use crate::memory::agent_llm::RealAgentLlm;
use crate::memory::collation::{
    CollationConfig as LibCollationConfig, CollationError, CollationManager, CollationOutcome,
    DEFAULT_COLLATE_PROMPT, DEFAULT_NORMALIZE_PROMPT, DEFAULT_TIDY_PROMPT,
};
use crate::memory::collation_impls::RealCollationLlm;
use crate::memory::compaction::{
    CompactionConfig, CompactionError, CompactionManager, CompactionOutcome,
    ConversationMessage, DEFAULT_COMPACT_PROMPT,
};
use crate::memory::compaction_impls::{
    resolve_embed_config, RealCompactionLlm, RealConversationManager, RealVectorIndexer,
};
use crate::memory::db::MemoryDB;
use crate::memory::vectorstore::VectorStore;

use crate::autonomy::activity::HourClassification;

use super::{CommandContext, CommandResult, MemoryShellSession};

/// Return system status: character, message count, model, token counts.
pub fn status(engine: &ConversationEngine, ctx: &CommandContext) -> CommandResult {
    let activity = ctx
        .autonomy
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
    let effective_model = ctx.active_model.as_deref()
        .or(ctx.config.app.defaults.model.as_deref());

    let tokens = ctx.session_tokens.lock().unwrap();
    Ok(json!({
        "character": engine.character_name(),
        "message_count": engine.message_count(),
        "active_model": effective_model,
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
    let count = args
        .get("count")
        .and_then(|v| v.as_u64())
        .unwrap_or(10) as usize;
    let diag = ctx.diagnostics.lock().unwrap();
    Ok(diag.to_json(count))
}

/// Return heartbeat event log for the active character.
pub fn heartbeat_log(
    engine: &ConversationEngine,
    ctx: &CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let limit = args
        .get("count")
        .and_then(|v| v.as_u64())
        .unwrap_or(20) as usize;
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
                "sdk": m.sdk.as_provider_str(),
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
            "sdk": m.sdk.as_provider_str(),
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

    let data = serde_json::to_value(&resolved)
        .map_err(|e| (ErrorCode::InternalError, format!("Failed to serialize model: {e}")))?;

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
                    format!(
                        "Model not found: {name}. Use list_models to see available models."
                    ),
                ));
            }
            ctx.active_model = Some(name.to_string());
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
    let limit = args
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(20);

    let char_name = engine.character_name();
    let db_path = ctx
        .data_dir
        .join(char_name)
        .join("memory")
        .join("memory.db");

    if !db_path.exists() {
        return Ok(json!({ "changelog": [], "character": char_name }));
    }

    let db = MemoryDB::open(&db_path)
        .map_err(|e| (ErrorCode::InternalError, format!("Failed to open memory DB: {e}")))?;

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

    match query {
        None => memory_status(engine, ctx),
        Some(q) => memory_query(engine, ctx, q).await,
    }
}

/// Return memory entry/entity counts for the current character.
fn memory_status(engine: &ConversationEngine, ctx: &CommandContext) -> CommandResult {
    let char_name = engine.character_name();
    let db_path = ctx
        .data_dir
        .join(char_name)
        .join("memory")
        .join("memory.db");

    if !db_path.exists() {
        return Ok(json!({
            "character": char_name,
            "entries": 0,
            "entities": 0,
            "active_entries": 0,
        }));
    }

    let db = MemoryDB::open(&db_path)
        .map_err(|e| (ErrorCode::InternalError, format!("Failed to open memory DB: {e}")))?;

    let entries = db
        .count_entries()
        .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;
    let entities = db
        .count_entities()
        .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;
    let active = db
        .count_entries_by_status("active")
        .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;

    Ok(json!({
        "character": char_name,
        "entries": entries,
        "entities": entities,
        "active_entries": active,
    }))
}

/// Run a one-shot memory agent query and return the synthesis.
async fn memory_query(
    engine: &ConversationEngine,
    ctx: &CommandContext,
    query: &str,
) -> CommandResult {
    let char_name = engine.character_name();
    let db_path = ctx
        .data_dir
        .join(char_name)
        .join("memory")
        .join("memory.db");

    let db = MemoryDB::open(&db_path)
        .map_err(|e| (ErrorCode::InternalError, format!("Failed to open memory DB: {e}")))?;

    // Resolve agent model: configured memory_agent → active model → first available.
    let agent_model = ctx
        .config
        .app
        .defaults
        .memory_agent
        .as_deref()
        .and_then(|name| ctx.config.models.find_model(name).ok())
        .or_else(|| {
            ctx.active_model
                .as_deref()
                .and_then(|name| ctx.config.models.find_model(name).ok())
        })
        .or_else(|| ctx.config.models.first_chat_model())
        .ok_or_else(|| (ErrorCode::InternalError, "No model configured".to_string()))?
        .clone();

    let display_name = ctx.config.app.defaults.resolve_display_name();
    let agent = MemoryAgent::one_shot(CallerIdentity::User, &display_name, char_name);
    let agent_llm = RealAgentLlm::new(ctx.llm_client.clone());

    // Build semantic search context (graceful: None if no embedding model).
    let search_ctx = match resolve_embed_config(
        ctx.config.app.defaults.embedding.as_deref(),
        &ctx.config.models.embedding,
    ) {
        Ok(embed_config) => {
            let vs_path = ctx.data_dir
                .join(char_name)
                .join("memory")
                .join("vectorstore");
            VectorStore::open(&vs_path, embed_config.dimensions)
                .await
                .ok()
                .map(|vs| AgentSearchContext::new(vs, ctx.llm_client.clone(), embed_config))
        }
        Err(_) => None,
    };
    let real_indexer = search_ctx.as_ref().map(RealAgentIndexer::new);
    let indexer = real_indexer.as_ref().map(|i| i as &dyn crate::memory::agent::AgentIndexer);

    let result = agent
        .ask(query, &agent_llm, &db, indexer, search_ctx.as_ref(), &agent_model)
        .await
        .map_err(|e| (ErrorCode::InternalError, format!("Memory query failed: {e}")))?;

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

    // Resolve agent model (same logic as memory_query).
    let agent_model = ctx
        .config
        .app
        .defaults
        .memory_agent
        .as_deref()
        .and_then(|name| ctx.config.models.find_model(name).ok())
        .or_else(|| {
            ctx.active_model
                .as_deref()
                .and_then(|name| ctx.config.models.find_model(name).ok())
        })
        .or_else(|| ctx.config.models.first_chat_model())
        .ok_or_else(|| (ErrorCode::InternalError, "No model configured".to_string()))?
        .clone();

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

    let session = ctx
        .memory_shell_sessions
        .get_mut(session_id)
        .ok_or_else(|| (ErrorCode::NotFound, "Session not found".to_string()))?;

    let db_path = ctx
        .data_dir
        .join(&session.character)
        .join("memory")
        .join("memory.db");

    let db = MemoryDB::open(&db_path)
        .map_err(|e| (ErrorCode::InternalError, format!("Failed to open memory DB: {e}")))?;

    let agent_llm = RealAgentLlm::new(ctx.llm_client.clone());

    // Build semantic search context (graceful: None if no embedding model).
    let search_ctx = match resolve_embed_config(
        ctx.config.app.defaults.embedding.as_deref(),
        &ctx.config.models.embedding,
    ) {
        Ok(embed_config) => {
            let vs_path = ctx.data_dir
                .join(&session.character)
                .join("memory")
                .join("vectorstore");
            VectorStore::open(&vs_path, embed_config.dimensions)
                .await
                .ok()
                .map(|vs| AgentSearchContext::new(vs, ctx.llm_client.clone(), embed_config))
        }
        Err(_) => None,
    };
    let real_indexer = search_ctx.as_ref().map(RealAgentIndexer::new);
    let indexer = real_indexer.as_ref().map(|i| i as &dyn crate::memory::agent::AgentIndexer);

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

    Ok(json!({
        "response": response,
        "mutations": mutations,
    }))
}

/// End a memory shell session.
pub fn memory_shell_end(
    ctx: &mut CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (ErrorCode::InvalidRequest, "Missing session_id".to_string()))?;

    ctx.memory_shell_sessions.remove(session_id);

    Ok(json!({ "ok": true }))
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

    // Open the memory database.
    let db_path = ctx
        .data_dir
        .join(&char_name)
        .join("memory")
        .join("memory.db");
    let db = MemoryDB::open(&db_path)
        .map_err(|e| (ErrorCode::InternalError, format!("Failed to open memory DB: {e}")))?;

    // Resolve the compaction prompt template.
    let prompt_template = resolve_prompt_template(
        &ctx.config.dirs.config,
        &char_name,
        "compact.md",
    )
    .unwrap_or_else(|| DEFAULT_COMPACT_PROMPT.to_string());

    // Resolve model: always use the active conversation model.
    let model = ctx
        .active_model
        .as_deref()
        .and_then(|name| ctx.config.models.find_model(name).ok())
        .ok_or_else(|| (ErrorCode::InternalError, "No active model for compaction".to_string()))?
        .clone();

    // Resolve embedding config.
    let embed_config = resolve_embed_config(
        ctx.config.app.defaults.embedding.as_deref(),
        &ctx.config.models.embedding,
    )
    .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;

    // Open vector store.
    let vs_path = ctx
        .data_dir
        .join(&char_name)
        .join("memory")
        .join("vectorstore");
    let store = VectorStore::open(&vs_path, embed_config.dimensions)
        .await
        .map_err(|e| (ErrorCode::InternalError, format!("Failed to open vector store: {e}")))?;

    // Create trait implementations.
    let llm = RealCompactionLlm::new(ctx.llm_client.clone(), model);
    let collation_embed_config = embed_config.clone();
    let indexer = RealVectorIndexer::new(store, ctx.llm_client.clone(), embed_config);
    let conv_mgr = RealConversationManager::new(engine.character_dir());

    // Create compaction manager with config.
    let app_compaction = &ctx.config.app.memory.compaction;
    let config = CompactionConfig {
        idle_trigger_minutes: app_compaction.idle_trigger_minutes as u64,
        min_turns: app_compaction.min_turns,
        max_turns: app_compaction.max_turns,
        keep_recent_turns: app_compaction.keep_recent_turns,
    };
    let mgr = CompactionManager::new(config);

    // Load existing recap for folding.
    let recap_path = ctx
        .data_dir
        .join(&char_name)
        .join("memory")
        .join("recap.md");
    let existing_recap = std::fs::read_to_string(&recap_path).ok();

    // Run compaction.
    let display_name = ctx.config.app.defaults.resolve_display_name();
    let outcome = mgr
        .compact(
            &char_name,
            &messages,
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
        .map_err(|e| compaction_err(e))?;

    // Build response and handle post-compaction engine state.
    match outcome {
        CompactionOutcome::Compacted(result) => {
            // Reload retained messages from disk (active.jsonl now has only kept messages).
            engine
                .reload()
                .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;

            // Run collation after compaction if enabled and requested.
            let do_collate = args
                .get("collate")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let collation_result = if do_collate
                && ctx.config.app.memory.collation.enabled
            {
                let collation_model = resolve_collation_model(&ctx.config);

                if let Some(cmodel) = collation_model {
                    let collation_llm =
                        RealCollationLlm::new(ctx.llm_client.clone(), cmodel);
                    let tidy_template = resolve_prompt_template(
                        &ctx.config.dirs.config,
                        &char_name,
                        "tidy.md",
                    )
                    .unwrap_or_else(|| DEFAULT_TIDY_PROMPT.to_string());
                    let collate_template = resolve_prompt_template(
                        &ctx.config.dirs.config,
                        &char_name,
                        "collate.md",
                    )
                    .unwrap_or_else(|| DEFAULT_COLLATE_PROMPT.to_string());
                    let normalize_template = resolve_prompt_template(
                        &ctx.config.dirs.config,
                        &char_name,
                        "normalize.md",
                    )
                    .unwrap_or_else(|| DEFAULT_NORMALIZE_PROMPT.to_string());

                    let collation_mgr = CollationManager::new(LibCollationConfig::default());
                    let collation_limit = ctx.config.app.memory.collation.batch_limit;

                    // Open a second vector store for collation (the compaction one was moved).
                    let collation_search_ctx = {
                        let cvs_path = ctx.data_dir.join(&char_name).join("memory").join("vectorstore");
                        match VectorStore::open(&cvs_path, collation_embed_config.dimensions).await {
                            Ok(vs) => Some(AgentSearchContext::new(vs, ctx.llm_client.clone(), collation_embed_config.clone())),
                            Err(_) => None,
                        }
                    };
                    let collation_indexer = collation_search_ctx.as_ref().map(|ctx| RealAgentIndexer::new(ctx));

                    let mut collation_vars = std::collections::HashMap::new();
                    collation_vars.insert("char".to_string(), char_name.clone());
                    collation_vars.insert("user".to_string(), display_name.clone());
                    match collation_mgr
                        .run(
                            &db,
                            &collation_llm,
                            &tidy_template,
                            &collate_template,
                            &normalize_template,
                            &collation_vars,
                            collation_indexer.as_ref().map(|i| i as &dyn crate::memory::agent::AgentIndexer),
                            collation_search_ctx.as_ref().map(|ctx| &ctx.vector_store),
                            Some(collation_limit),
                        )
                        .await
                    {
                        Ok(outcome) => Some(json!({
                            "timestamps_backfilled": outcome.timestamps_backfilled,
                            "tidy_splits": outcome.tidy_splits,
                            "tidy_new_entries": outcome.tidy_new_entries,
                            "collate_merges": outcome.collate_merges,
                            "collate_new_entries": outcome.collate_new_entries,
                            "entities_normalized": outcome.entities_normalized,
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
                } else {
                    None
                }
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
    let db_path = ctx
        .data_dir
        .join(&char_name)
        .join("memory")
        .join("memory.db");
    let db = MemoryDB::open(&db_path)
        .map_err(|e| (ErrorCode::InternalError, format!("Failed to open memory DB: {e}")))?;

    let model = resolve_collation_model(&ctx.config)
        .ok_or_else(|| (ErrorCode::InternalError, "No model configured".to_string()))?;

    let llm = RealCollationLlm::new(ctx.llm_client.clone(), model);

    // Resolve prompt templates.
    let tidy_template = resolve_prompt_template(&ctx.config.dirs.config, &char_name, "tidy.md")
        .unwrap_or_else(|| DEFAULT_TIDY_PROMPT.to_string());
    let collate_template =
        resolve_prompt_template(&ctx.config.dirs.config, &char_name, "collate.md")
            .unwrap_or_else(|| DEFAULT_COLLATE_PROMPT.to_string());
    let normalize_template =
        resolve_prompt_template(&ctx.config.dirs.config, &char_name, "normalize.md")
            .unwrap_or_else(|| DEFAULT_NORMALIZE_PROMPT.to_string());

    let mgr = CollationManager::new(LibCollationConfig::default());

    // Resolve batch limit: CLI --limit overrides config default.
    let config_limit = ctx.config.app.memory.collation.batch_limit;
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(config_limit);

    // Construct vector store + indexer for clustering and indexing (optional).
    let search_ctx = match resolve_embed_config(
        ctx.config.app.defaults.embedding.as_deref(),
        &ctx.config.models.embedding,
    ) {
        Ok(embed_config) => {
            let vs_path = ctx.data_dir.join(&char_name).join("memory").join("vectorstore");
            match VectorStore::open(&vs_path, embed_config.dimensions).await {
                Ok(vs) => Some(AgentSearchContext::new(vs, ctx.llm_client.clone(), embed_config)),
                Err(e) => {
                    tracing::warn!("Vector store unavailable for collation: {e}");
                    None
                }
            }
        }
        Err(_) => None,
    };
    let indexer = search_ctx.as_ref().map(|ctx| RealAgentIndexer::new(ctx));

    let collation_display_name = ctx.config.app.defaults.resolve_display_name();
    let mut collation_vars = std::collections::HashMap::new();
    collation_vars.insert("char".to_string(), char_name.clone());
    collation_vars.insert("user".to_string(), collation_display_name);

    const MAX_PASSES: usize = 10;
    let mut total = CollationOutcome::default();
    let mut passes: usize = 0;

    loop {
        passes += 1;
        let outcome = mgr
            .run(
                &db, &llm, &tidy_template, &collate_template, &normalize_template, &collation_vars,
                indexer.as_ref().map(|i| i as &dyn crate::memory::agent::AgentIndexer),
                search_ctx.as_ref().map(|ctx| &ctx.vector_store),
                Some(limit),
            )
            .await
            .map_err(collation_err)?;

        total.timestamps_backfilled += outcome.timestamps_backfilled;
        total.tidy_splits += outcome.tidy_splits;
        total.tidy_new_entries += outcome.tidy_new_entries;
        total.collate_merges += outcome.collate_merges;
        total.collate_new_entries += outcome.collate_new_entries;
        total.entities_normalized += outcome.entities_normalized;
        total.entries_decayed += outcome.entries_decayed;
        total.entries_skipped += outcome.entries_skipped;

        if !full_mode
            || (outcome.collate_merges == 0 && outcome.tidy_splits == 0)
            || passes >= MAX_PASSES
        {
            break;
        }
    }

    Ok(json!({
        "status": "collated",
        "character": char_name,
        "passes": passes,
        "timestamps_backfilled": total.timestamps_backfilled,
        "tidy_splits": total.tidy_splits,
        "tidy_new_entries": total.tidy_new_entries,
        "collate_merges": total.collate_merges,
        "collate_new_entries": total.collate_new_entries,
        "entities_normalized": total.entities_normalized,
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

    let db_path = ctx
        .data_dir
        .join(&char_name)
        .join("memory")
        .join("memory.db");
    let db = MemoryDB::open(&db_path)
        .map_err(|e| (ErrorCode::InternalError, format!("Failed to open memory DB: {e}")))?;

    let cutoff = (chrono::Utc::now() - chrono::Duration::days(days))
        .to_rfc3339();

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
    if s.ends_with('d') {
        s[..s.len() - 1].parse::<i64>().ok().filter(|&d| d > 0)
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
        info.push(format!("{} chat model(s) configured", ctx.config.models.chat.len()));
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
        info.push(format!("{} tool model(s) configured", ctx.config.models.tools.len()));
    }

    // Check: embedding models
    if ctx.config.models.embedding.is_empty() {
        warnings.push("No embedding models configured. Memory vector search will be unavailable.".into());
    } else {
        info.push(format!("{} embedding model(s) configured", ctx.config.models.embedding.len()));
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
            warnings.push(format!("Default memory_agent \"{ma}\" not found in catalog"));
        }
    }

    // Check: LLM service configured?
    if ctx.config.app.services.llm.command.is_none() && ctx.config.app.services.llm.socket.is_none() {
        warnings.push("No LLM service configured. Set [services.llm] command or socket_path.".into());
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
    let db_path = ctx.data_dir.join(&char_name).join("memory").join("memory.db");
    let db = MemoryDB::open(&db_path)
        .map_err(|e| (ErrorCode::InternalError, format!("Failed to open memory DB: {e}")))?;

    // Load all active entries.
    let entries = db.get_entries_by_status("active")
        .map_err(|e| (ErrorCode::InternalError, format!("Failed to load entries: {e}")))?;

    if entries.is_empty() {
        return Ok(json!({ "reindexed": 0, "message": "No active entries to reindex" }));
    }

    // Rebuild FTS index.
    db.rebuild_fts()
        .map_err(|e| (ErrorCode::InternalError, format!("Failed to rebuild FTS: {e}")))?;

    // Resolve embedding config and rebuild vector index.
    let embed_config = resolve_embed_config(
        ctx.config.app.defaults.embedding.as_deref(),
        &ctx.config.models.embedding,
    )
    .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;

    let vs_path = ctx.data_dir.join(&char_name).join("memory").join("vectorstore");
    let store = VectorStore::open(&vs_path, embed_config.dimensions)
        .await
        .map_err(|e| (ErrorCode::InternalError, format!("Failed to open vector store: {e}")))?;

    // Embed entries in batches to avoid overrunning the Unix socket with a
    // single huge JSON response (the socket may close before the client
    // finishes reading).
    const EMBED_BATCH_SIZE: usize = 50;
    let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(entries.len());
    for chunk in entries.chunks(EMBED_BATCH_SIZE) {
        let texts: Vec<&str> = chunk.iter().map(|e| e.summary_text.as_str()).collect();
        let batch = ctx.llm_client
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

    store.reindex(&pairs)
        .await
        .map_err(|e| (ErrorCode::InternalError, format!("Vector reindex failed: {e}")))?;

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
    let app_json = serde_json::to_value(&ctx.config.app)
        .map_err(|e| (ErrorCode::InternalError, format!("Failed to serialize config: {e}")))?;

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
            Ok(json!({ "reset": true, "message": "Configuration reloaded from disk" }))
        }
        Err(e) => Err((ErrorCode::InternalError, format!("Failed to reload config: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CommandContext;
    use shore_config::models::ModelCatalog;
    use crate::engine::ConversationEngine;
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
        let (autonomy, _compaction_rx) = crate::autonomy::manager::AutonomyManager::new(Default::default(), Default::default(), data_dir.clone(), rx);

        let ctx = CommandContext {
            config,
            push_tx,
            data_dir: data_dir.clone(),
            active_model: None,
            session_tokens: std::sync::Arc::new(std::sync::Mutex::new(crate::commands::SessionTokens::default())),
            autonomy,
            llm_client: shore_llm_client::LlmClient::new(),
            diagnostics: std::sync::Arc::new(std::sync::Mutex::new(shore_diagnostics::Diagnostics::default())),
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
            canonical: false,
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

        let result = memory(&engine, &ctx, &json!({"query": null})).await.unwrap();
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
        assert!(warnings.iter().any(|w| w.as_str().unwrap().contains("No chat models")));
        assert_eq!(result["chat_models"], 0);
    }

    #[test]
    fn config_check_with_models() {
        let tmp = TempDir::new().unwrap();
        let (_engine, ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

        let result = config_check(&ctx).unwrap();
        assert_eq!(result["chat_models"], 2);
        let info = result["info"].as_array().unwrap();
        assert!(info.iter().any(|i| i.as_str().unwrap().contains("2 chat model")));
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
}
