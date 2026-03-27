use serde_json::json;
use shore_protocol::error::ErrorCode;
use shore_protocol::types::Role;

use crate::config::resolve_prompt_template;
use crate::engine::ConversationEngine;
use crate::memory::agent::{CallerIdentity, MemoryAgent};
use crate::memory::agent_llm::RealAgentLlm;
use crate::memory::collation::{
    CollationConfig as LibCollationConfig, CollationError, CollationManager,
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

use super::{CommandContext, CommandResult};

/// Return system status: character, message count, model, token counts.
pub fn status(engine: &ConversationEngine, ctx: &CommandContext) -> CommandResult {
    Ok(json!({
        "character": engine.character_name(),
        "message_count": engine.message_count(),
        "active_model": ctx.active_model,
        "tokens": {
            "input": ctx.session_tokens.input,
            "output": ctx.session_tokens.output,
            "cache_read": ctx.session_tokens.cache_read,
            "cache_write": ctx.session_tokens.cache_write,
        },
        "autonomy": ctx.autonomy.status(engine.character_name()),
    }))
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

    let agent = MemoryAgent::one_shot(CallerIdentity::User, "User", char_name);
    let agent_llm = RealAgentLlm::new(ctx.llm_client.clone());

    let result = agent
        .ask(query, &agent_llm, &db, None, &agent_model)
        .await
        .map_err(|e| (ErrorCode::InternalError, format!("Memory query failed: {e}")))?;

    Ok(json!({
        "character": char_name,
        "query": query,
        "result": result,
    }))
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

    // Resolve model: memory_agent -> active_model -> first_chat_model.
    let model = ctx
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
    let indexer = RealVectorIndexer::new(store, ctx.llm_client.clone(), embed_config);
    let conv_mgr = RealConversationManager::new(engine.character_dir());

    // Create compaction manager with config.
    let app_compaction = &ctx.config.app.behavior.autonomy.compaction;
    let config = CompactionConfig {
        idle_trigger_minutes: app_compaction.idle_trigger_minutes as u64,
        min_messages: app_compaction.min_messages,
        max_messages: app_compaction.max_messages,
        keep_recent: app_compaction.keep_recent,
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
    let outcome = mgr
        .compact(
            &char_name,
            &messages,
            false, // is_private: V2 has no per-conversation privacy flag yet
            &prompt_template,
            existing_recap.as_deref(),
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

            Ok(json!({
                "status": "compacted",
                "character": char_name,
                "entries_created": result.entries_created.len(),
                "entry_ids": result.entries_created,
                "message_count": result.message_count,
                "retained_count": result.retained_count,
                "recap_generated": result.recap_generated,
                "new_conversation_id": result.new_conversation_id,
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

/// Run the 4-phase collation pipeline on the current character's memory.
///
/// Phases: tidy (split broad entries), collate (merge similar), normalize
/// entities, and confidence decay.
pub async fn collate(
    engine: &mut ConversationEngine,
    ctx: &CommandContext,
    _args: &serde_json::Value,
) -> CommandResult {
    let char_name = engine.character_name().to_string();

    // Open the memory database.
    let db_path = ctx
        .data_dir
        .join(&char_name)
        .join("memory")
        .join("memory.db");
    let db = MemoryDB::open(&db_path)
        .map_err(|e| (ErrorCode::InternalError, format!("Failed to open memory DB: {e}")))?;

    // Resolve model: memory_agent -> active_model -> first_chat_model.
    let model = ctx
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

    let outcome = mgr
        .run(&db, &llm, &tidy_template, &collate_template, &normalize_template)
        .await
        .map_err(collation_err)?;

    Ok(json!({
        "status": "collated",
        "character": char_name,
        "tidy_splits": outcome.tidy_splits,
        "tidy_new_entries": outcome.tidy_new_entries,
        "collate_merges": outcome.collate_merges,
        "collate_new_entries": outcome.collate_new_entries,
        "entities_normalized": outcome.entities_normalized,
        "entries_decayed": outcome.entries_decayed,
        "entries_skipped": outcome.entries_skipped,
    }))
}

/// Map `CollationError` to a command error tuple.
fn collation_err(e: CollationError) -> (ErrorCode, String) {
    (ErrorCode::InternalError, e.to_string())
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

    // Embed all entries and rebuild the vector index.
    let texts: Vec<&str> = entries.iter().map(|e| e.summary_text.as_str()).collect();

    // Batch embed (the embed endpoint handles batching internally).
    let embeddings = ctx.llm_client
        .embed(
            &embed_config.provider,
            &embed_config.model_id,
            &embed_config.api_key,
            embed_config.base_url.as_deref(),
            &texts,
        )
        .await
        .map_err(|e| (ErrorCode::InternalError, format!("Embedding failed: {e}")))?;

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
        "cache_keepalive.enabled" | "behavior.autonomy.cache_keepalive.enabled" => {
            let v: bool = value.parse()
                .map_err(|_| (ErrorCode::InvalidRequest, "expected true or false".into()))?;
            ctx.config.app.behavior.autonomy.cache_keepalive.enabled = v;
            Ok(json!({ "set": "cache_keepalive.enabled", "value": v }))
        }
        _ => Err((
            ErrorCode::InvalidRequest,
            format!("Config key not settable at runtime: {key}. Supported: defaults.model, defaults.stream, autonomy.enabled, cache_keepalive.enabled"),
        )),
    }
}

/// Reset all runtime config overrides by reloading from disk.
pub fn config_reset(ctx: &mut CommandContext) -> CommandResult {
    match crate::config::load_config(None) {
        Ok(fresh) => {
            ctx.active_model = fresh.app.defaults.model.clone();
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
    use crate::config::models::ModelCatalog;
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

        let config = crate::config::LoadedConfig::new_for_test(
            crate::config::app::AppConfig::default(),
            models,
            crate::config::ShoreDirs {
                config: tmp.path().join("config"),
                data: data_dir.clone(),
                runtime: tmp.path().join("runtime"),
            },
        );

        let (_tx, rx) = tokio::sync::watch::channel(());
        let (autonomy, _compaction_rx) = crate::autonomy::manager::AutonomyManager::new(Default::default(), data_dir.clone(), rx);

        let ctx = CommandContext {
            config,
            push_tx,
            data_dir: data_dir.clone(),
            active_model: None,
            session_tokens: Default::default(),
            autonomy,
            llm_client: crate::llm_client::LlmClient::new(data_dir.join("dummy.sock")),
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
