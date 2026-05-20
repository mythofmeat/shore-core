use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};
use shore_config::character_data_dir;
use shore_config::models::Sdk;
use shore_protocol::types::{ContentBlock, Message, Role};
use tracing::{debug, info, instrument, warn};

use crate::autonomy::parse_cache_ttl_secs;
use crate::engine::messages::PendingAlt;
use crate::engine::prompt;
use crate::handler::generation::{run_tool_phase, thinking_enabled_from_request};
use crate::handler::images::{embed_image_data, ingest_images};
use crate::handler::key_fallback::stream_with_credential_fallback;
use crate::handler::persistence::persist_and_notify;
use crate::handler::resize::warm_image_cache;
use crate::memory::compaction_impls::resolve_image_gen_config;
use crate::memory::markdown_store::MarkdownMemoryStore;
use crate::memory::retrieval::resolve_embedder;
use crate::tools::context::SharedToolContext;
use crate::tools::ToolContext;

use super::{
    GenContext, GenerationParams, HandlerToolContext, PrepareChatContextParams, PreparedChatContext,
};

#[instrument(
    skip(ctx, params),
    fields(
        client_id = params.request.session.client_id.0,
        session_id = params.request.session.session_id.0,
        client_type = %params.request.session.client_type,
        char = %params.char_name,
        rid = params.rid.as_deref().unwrap_or("-")
    )
)]
pub(super) async fn handle_generation(
    ctx: GenContext,
    params: GenerationParams,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let GenerationParams {
        request: _request,
        body,
        regen,
        char_name,
        rid,
        effective_config,
        data_dir,
        active_model,
        sampler_overlay,
    } = params;
    info!(
        character = %char_name,
        regen,
        text_len = body.text.len(),
        image_count = body.images.len() + body.image_data.len(),
        "handle_generation starting"
    );
    let wall_clock_start = Instant::now();

    let engine_arc = {
        let mut registry = ctx.registry.lock().await;
        registry
            .get_or_create(&char_name)
            .map_err(|e| e.to_string())?
    };

    let mut regen_alt: Option<PendingAlt> = None;
    {
        let mut engine = engine_arc.lock().await;
        if regen {
            regen_alt = Some(engine.pending_regen_alt().unwrap_or(PendingAlt {
                alternatives: Vec::new(),
            }));
        } else if !body.text.is_empty() || !body.images.is_empty() || !body.image_data.is_empty() {
            let (images, mut content_blocks) =
                ingest_images(&data_dir, &char_name, &body.images, &body.image_data);

            content_blocks.push(ContentBlock::Text {
                text: body.text.clone(),
            });

            let user_msg = Message {
                msg_id: format!("m_{}", uuid::Uuid::new_v4()),
                role: Role::User,
                content: body.text.clone(),
                images,
                content_blocks,
                alt_index: None,
                alt_count: None,
                alternatives: vec![],
                timestamp: chrono::Local::now().to_rfc3339(),
            };
            engine.append_message(user_msg.clone())?;
            let revision = engine.current_revision();
            let mut wire_msg = user_msg;
            embed_image_data(&mut wire_msg.images);
            let _ = ctx
                .event_tx
                .send(shore_protocol::server_msg::ServerMessage::NewMessage(
                    shore_protocol::server_msg::NewMessage {
                        revision,
                        character: Some(char_name.clone()),
                        origin: Some(shore_protocol::server_msg::MessageOrigin::UserInput),
                        message: wire_msg,
                    },
                ));
        }
    }

    // The handler resolves the active model (via preferences +
    // discovery) and passes the `ResolvedModel` through directly, so we
    // do not re-run `find_effective_model` here — discovered-only models
    // have a synthetic `qualified_name` that the resolver does not
    // accept as input. If nothing was passed, fall back to the
    // configured app default, then the first static chat model.
    let resolved_base_owned: shore_config::models::ResolvedModel = match active_model {
        Some(m) => m,
        None => match effective_config.app.defaults.model.as_deref() {
            Some(name) => crate::effective_catalog::find_effective_model(
                &effective_config,
                &effective_config.dirs.cache,
                name,
                // App-level defaults are user configuration, not a
                // discovery-cache selection — `discovery.ignore` still
                // applies for safety, but a misspelled default should surface.
                true,
            )
            .map_err(|e| e.to_string())?,
            None => effective_config
                .models
                .first_chat_model()
                .cloned()
                .ok_or("No model configured")?,
        },
    };
    let resolved_base = &resolved_base_owned;
    let resolved_owned;
    let resolved: &shore_config::models::ResolvedModel = if !sampler_overlay.is_empty() {
        resolved_owned = crate::preferences::apply_sampler_overlay(resolved_base, &sampler_overlay);
        &resolved_owned
    } else {
        resolved_base
    };
    debug!(
        model = %resolved.qualified_name,
        provider = %resolved.provider_key,
        reasoning_effort = ?resolved.reasoning_effort,
        sampler_overlay_active = !sampler_overlay.is_empty(),
        "model resolved"
    );

    let cache_ttl_secs = resolved.cache_ttl.as_deref().and_then(parse_cache_ttl_secs);
    let is_new_autonomy_state =
        ctx.autonomy
            .ensure_state_with_config(&char_name, cache_ttl_secs, Some(&effective_config));

    if is_new_autonomy_state {
        let engine = engine_arc.lock().await;
        let cutoff = chrono::Local::now().naive_local() - chrono::Duration::days(90);
        let mut timestamps: Vec<chrono::NaiveDateTime> = Vec::new();

        for msg in engine.messages().iter().filter(|msg| {
            msg.role == shore_protocol::types::Role::User && !msg.is_tool_result_only()
        }) {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&msg.timestamp) {
                let naive = dt.with_timezone(&chrono::Local).naive_local();
                if naive >= cutoff {
                    timestamps.push(naive);
                }
            }
        }

        let segments = engine.segments();
        for i in 0..segments.segment_count() {
            if let Ok(segment_msgs) = segments.read_segment(i) {
                for msg in segment_msgs.iter().filter(|msg| {
                    msg.role == shore_protocol::types::Role::User && !msg.is_tool_result_only()
                }) {
                    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&msg.timestamp) {
                        let naive = dt.with_timezone(&chrono::Local).naive_local();
                        if naive >= cutoff {
                            timestamps.push(naive);
                        }
                    }
                }
            }
        }

        drop(engine);

        if !timestamps.is_empty() {
            info!(
                character = %char_name,
                count = timestamps.len(),
                "Backfilling activity tracker from chat history"
            );
            ctx.autonomy.backfill_activity(&char_name, timestamps);
        }
    }

    if !regen && (!body.text.is_empty() || !body.images.is_empty() || !body.image_data.is_empty()) {
        let turn_count = engine_arc.lock().await.turn_count();
        ctx.autonomy.notify_user_message(&char_name, turn_count);
    }

    let (messages, has_prior_context, history_rewrite_generation) = {
        let engine = engine_arc.lock().await;
        let has_prior = engine.segments().segment_count() > 0;
        let base_history_rewrite_generation = engine.history_rewrite_generation();
        let history_rewrite_generation = if regen {
            base_history_rewrite_generation.saturating_add(1)
        } else {
            base_history_rewrite_generation
        };
        (
            if regen {
                engine.messages_through_last_user_turn()
            } else {
                engine.messages().to_vec()
            },
            has_prior,
            history_rewrite_generation,
        )
    };

    let character_data_dir = character_data_dir(&data_dir, &char_name);
    let include_unsigned_thinking = resolved.sdk.echoes_unsigned_thinking();
    let PreparedChatContext {
        llm_messages,
        system,
        tool_defs,
        prompt: prompt_result,
        character_definition,
        user_definition,
    } = super::prepare_chat_context(PrepareChatContextParams {
        character: &char_name,
        character_data_dir: &character_data_dir,
        config: &effective_config,
        resolved,
        messages: &messages,
        has_prior_context,
        is_private: false,
        include_unsigned_thinking,
    });

    let cache_dir = &effective_config.dirs.cache;
    warm_image_cache(
        &prompt_result.messages,
        effective_config.app.advanced.max_image_size,
        cache_dir,
    )
    .await;

    // Phase 4: build the request without baking in a specific API key.
    // The credential-fallback wrapper resolves the candidate key list
    // from the provider registry just-in-time and rewrites
    // `request.api_key` per attempt during rotation, so we can leave
    // the placeholder empty here.
    let mut request = shore_llm::LlmClient::build_request_with_resolved_key(
        resolved,
        String::new(),
        llm_messages,
        system,
        tool_defs,
        None,
    );
    request.rid = rid;
    request.forensic_character = Some(char_name.to_owned());

    if let Some(ref ov) = body.overrides {
        if let Some(t) = ov.temperature {
            request.temperature = Some(t);
        }
        if let Some(p) = ov.top_p {
            request.top_p = Some(p);
        }
        if let Some(budget) = ov.thinking_budget {
            let opts = request
                .provider_options
                .get_or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            if let Some(map) = opts.as_object_mut() {
                map.insert("budget_tokens".into(), serde_json::json!(budget));
            }
        }
    }

    let mut claude_code_session = None;
    if matches!(resolved.sdk, Sdk::ClaudeCode) {
        let subprocess_key =
            claude_code_subprocess_key(&data_dir, &char_name, history_rewrite_generation);
        let tool_ctx =
            build_claude_code_tool_context(&ctx, &data_dir, &char_name, &effective_config)?;
        claude_code_session = crate::claude_code::prepare_request(
            &mut request,
            ctx.http.as_ref(),
            Some(subprocess_key),
            tool_ctx,
        )
        .await
        .map_err(|e| e.to_string())?;
    }

    info!(
        model = %resolved.model_id,
        messages = request.messages.len(),
        "Sending streaming request to LLM"
    );

    let thinking_enabled = thinking_enabled_from_request(&request);

    let mut result = stream_with_credential_fallback(
        &ctx,
        &mut request,
        resolved,
        &effective_config,
        regen,
        &char_name,
        thinking_enabled,
    )
    .await?;

    let tool_intermediate_messages = if result.finish_reason == "tool_use"
        && effective_config.app.behavior.tool_use.enabled
        && !matches!(resolved.sdk, Sdk::ClaudeCode)
    {
        let tool_loop_result = run_tool_phase(
            &ctx,
            &data_dir,
            &char_name,
            &effective_config,
            &character_definition,
            &user_definition,
            &mut request,
            result,
        )
        .await?;
        result = tool_loop_result.result;
        tool_loop_result.intermediate_messages
    } else {
        Vec::new()
    };

    if let Some(session) = claude_code_session.take() {
        let ledger = session.drain().await;
        splice_claude_code_tool_ledger(&mut result, ledger);
    }

    persist_and_notify(
        &ctx,
        &engine_arc,
        &char_name,
        resolved,
        &result,
        &request,
        tool_intermediate_messages,
        wall_clock_start,
        effective_config.app.memory.thinking.preserve_prior_turns,
        regen_alt,
    )
    .await?;

    let (stream_msg_id, stream_revision) = {
        let engine = engine_arc.lock().await;
        (
            engine.messages().last().map(|m| m.msg_id.clone()),
            Some(engine.current_revision()),
        )
    };

    // Emit StreamEnd ONLY after persistence completes — clients that issue
    // an immediate follow-up command (e.g. `memory_compact` via shore-mcp)
    // would otherwise race the persist write and snapshot stale engine
    // state. See ARCHITECTURE.md (runtime flow).
    shore_llm::stream::emit_stream_end(
        &ctx.direct_tx,
        request.rid.clone(),
        &result,
        true,
        stream_msg_id.clone(),
        stream_revision,
    )
    .await;

    let (turn_count, context_tokens, should_compact) = {
        let engine = engine_arc.lock().await;
        let turn_count = engine.turn_count();
        let context_tokens = result.usage.input_tokens as usize
            + result.usage.cache_read_tokens as usize
            + result.usage.cache_creation_tokens as usize;
        let should_compact =
            ctx.autonomy
                .should_compact_now(&char_name, turn_count, context_tokens);
        (turn_count, context_tokens, should_compact)
    };
    if should_compact {
        info!(
            character = %char_name,
            turn_count,
            context_tokens,
            "Scheduling inline compaction"
        );
        spawn_inline_compaction(
            ctx.clone(),
            engine_arc.clone(),
            char_name.clone(),
            effective_config.clone(),
            data_dir.clone(),
            request.rid.clone(),
            ctx.autonomy.cached_last_request(&char_name),
        );
    }

    Ok(())
}

fn spawn_inline_compaction(
    ctx: GenContext,
    engine_arc: Arc<tokio::sync::Mutex<crate::engine::ConversationEngine>>,
    char_name: String,
    effective_config: shore_config::LoadedConfig,
    data_dir: PathBuf,
    rid: Option<String>,
    cached_request: Option<shore_llm::types::LlmRequest>,
) {
    tokio::spawn(async move {
        run_inline_compaction(
            ctx,
            engine_arc,
            char_name,
            effective_config,
            data_dir,
            rid,
            cached_request,
        )
        .await;
    });
}

async fn run_inline_compaction(
    ctx: GenContext,
    engine_arc: Arc<tokio::sync::Mutex<crate::engine::ConversationEngine>>,
    char_name: String,
    effective_config: shore_config::LoadedConfig,
    data_dir: PathBuf,
    rid: Option<String>,
    cached_request: Option<shore_llm::types::LlmRequest>,
) {
    let _ = ctx
        .direct_tx
        .send(
            shore_protocol::server_msg::ServerMessage::Phase(shore_protocol::server_msg::Phase {
                rid: None,
                phase: "compacting".into(),
                model: None,
            })
            .with_rid(rid),
        )
        .await;

    match crate::memory::compaction::run_compaction(
        &char_name,
        &effective_config,
        &ctx.llm_client,
        &ctx.notifier,
        cached_request,
        ctx.http.clone(),
    )
    .await
    {
        Ok(retained_count) => {
            {
                let mut engine = engine_arc.lock().await;
                if let Err(e) = engine.reload() {
                    tracing::warn!(
                        character = %char_name,
                        error = %e,
                        "Inline compaction: engine reload failed"
                    );
                    ctx.autonomy.notify_compaction_failed(&char_name);
                    return;
                }
            }

            // Apply deferred character self-edits now that the cache
            // has been busted by the engine reload.
            let character_data_dir = character_data_dir(&data_dir, &char_name);
            if let Err(e) = crate::memory::deferred_edits::apply_deferred_edits(
                &character_data_dir,
                &effective_config.dirs.config,
                &char_name,
            ) {
                tracing::warn!(
                    character = %char_name,
                    error = %e,
                    "Failed to apply deferred edits after inline compaction"
                );
            }

            ctx.autonomy
                .notify_compaction_complete(&char_name, retained_count);
            info!(
                character = %char_name,
                retained_count,
                "Inline compaction complete, engine reloaded"
            );
        }
        Err(e) => {
            tracing::warn!(
                character = %char_name,
                error = %e,
                "Inline compaction failed"
            );
            ctx.autonomy.notify_compaction_failed(&char_name);
        }
    }
}

fn claude_code_subprocess_key(data_dir: &Path, char_name: &str, history_generation: u64) -> String {
    format!(
        "{}:{char_name}:history-{history_generation}",
        data_dir.display()
    )
}

fn build_claude_code_tool_context(
    ctx: &GenContext,
    data_dir: &Path,
    char_name: &str,
    effective_config: &shore_config::LoadedConfig,
) -> Result<Arc<dyn ToolContext + Send + Sync>, Box<dyn std::error::Error + Send + Sync>> {
    let image_gen_config = resolve_image_gen_config(
        effective_config.app.defaults.image_generation.as_deref(),
        &effective_config.models.image_generation,
    )
    .ok();

    let character_data_dir = character_data_dir(data_dir, char_name);
    let config_dir = &effective_config.dirs.config;
    let workspace_dir = shore_config::character_workspace_dir(config_dir, char_name);
    let memory_dir = shore_config::character_memory_dir(config_dir, char_name);
    let embedder = resolve_embedder(
        effective_config.app.defaults.embedding.as_deref(),
        &effective_config.models.embedding,
        ctx.llm_client.inner().http_client(),
    )
    .map_err(|e| {
        tracing::warn!(character = %char_name, error = %e, "embedder unavailable; semantic memory retrieval disabled");
    })
    .ok();

    if let Err(e) = crate::memory::deferred_edits::ensure_active_prompt_snapshot(
        &character_data_dir,
        config_dir,
        char_name,
    ) {
        tracing::warn!(character = %char_name, error = %e, "Failed to prepare active prompt snapshot");
    }

    Ok(Arc::new(HandlerToolContext {
        inner: SharedToolContext {
            image_dir_val: character_data_dir
                .join("images")
                .to_string_lossy()
                .into_owned(),
            llm_client_val: ctx.llm_client.inner().clone(),
            image_gen_config_val: image_gen_config,
            search_config_val: effective_config.app.behavior.tool_use.search.clone(),
            character_name_val: char_name.to_owned(),
            workspace_dir_val: workspace_dir.to_string_lossy().into_owned(),
            markdown_store_val: MarkdownMemoryStore::open_sync(memory_dir).ok(),
            memory_retrieval_config_val: effective_config.app.memory.retrieval.clone(),
            embedder_val: embedder,
            memory_index_path_val: crate::memory::workspace_index::index_path(
                &effective_config.dirs.cache,
                char_name,
            ),
            config_dir_val: config_dir.to_string_lossy().into_owned(),
            character_data_dir_val: character_data_dir.to_string_lossy().into_owned(),
        },
        autonomy_val: ctx.autonomy.clone(),
    }))
}

fn splice_claude_code_tool_ledger(
    result: &mut shore_llm::types::StreamResult,
    ledger: Vec<crate::engine::mcp_session::LedgerEntry>,
) {
    if ledger.is_empty() {
        result.tool_uses = tool_uses_from_blocks(&result.content_blocks);
        return;
    }

    let existing = std::mem::take(&mut result.content_blocks);
    let mut spliced = Vec::new();
    let mut matched: HashSet<usize> = HashSet::new();
    let mut emitted_tool_use_count = 0usize;
    let mut unmatched_emitted_tool_uses = 0usize;

    if existing.is_empty() && !result.content.is_empty() {
        spliced.push(ContentBlock::Text {
            text: result.content.clone(),
        });
    }

    for block in existing {
        match block {
            ContentBlock::ToolUse { id, name, input } => {
                emitted_tool_use_count += 1;
                let bare_name = strip_mcp_tool_name(&name);
                let match_idx = ledger_match(&ledger, &matched, &id, &bare_name, &input);
                spliced.push(ContentBlock::ToolUse {
                    id: id.clone(),
                    name: bare_name,
                    input: input.clone(),
                });
                if let Some(i) = match_idx {
                    matched.insert(i);
                    spliced.push(ledger_tool_result_block(&ledger[i], &id));
                } else {
                    unmatched_emitted_tool_uses += 1;
                }
            }
            other => spliced.push(other),
        }
    }

    let unmatched_ledger_count = ledger.len().saturating_sub(matched.len());
    if unmatched_emitted_tool_uses > 0 || unmatched_ledger_count > 0 {
        warn!(
            ledger_count = ledger.len(),
            emitted_tool_use_count,
            unmatched_emitted_tool_uses,
            unmatched_ledger_count,
            "claude_code tool ledger mismatch; preserving emitted blocks and appending unmatched ledger entries"
        );
    }

    for (i, entry) in ledger.iter().enumerate() {
        if matched.contains(&i) {
            continue;
        }
        spliced.push(ContentBlock::ToolUse {
            id: entry.tool_use_id.clone(),
            name: entry.name.clone(),
            input: entry.input.clone(),
        });
        spliced.push(ledger_tool_result_block(entry, &entry.tool_use_id));
    }

    result.tool_uses = tool_uses_from_blocks(&spliced);
    result.content_blocks = spliced;
}

fn tool_uses_from_blocks(blocks: &[ContentBlock]) -> Vec<shore_llm::types::ToolUseEvent> {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => Some(shore_llm::types::ToolUseEvent {
                id: id.clone(),
                name: strip_mcp_tool_name(name),
                input: input.clone(),
            }),
            _ => None,
        })
        .collect()
}

fn ledger_match(
    ledger: &[crate::engine::mcp_session::LedgerEntry],
    matched: &HashSet<usize>,
    tool_use_id: &str,
    name: &str,
    input: &Value,
) -> Option<usize> {
    ledger
        .iter()
        .enumerate()
        .find(|(i, entry)| !matched.contains(i) && entry.tool_use_id == tool_use_id)
        .map(|(i, _)| i)
        .or_else(|| {
            ledger
                .iter()
                .enumerate()
                .find(|(i, entry)| {
                    !matched.contains(i) && entry.name == name && entry.input == *input
                })
                .map(|(i, _)| i)
        })
}

fn ledger_tool_result_block(
    entry: &crate::engine::mcp_session::LedgerEntry,
    tool_use_id: &str,
) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: tool_use_id.to_string(),
        content: shore_protocol::types::derive_content_from_blocks(&entry.content),
        is_error: entry.is_error,
    }
}

fn strip_mcp_tool_name(name: &str) -> String {
    if let Some(rest) = name.strip_prefix("mcp__") {
        if let Some((_, tool)) = rest.split_once("__") {
            return tool.to_string();
        }
    }
    name.to_string()
}

/// Convert assembled prompt messages into LLM API JSON format.
pub(crate) fn build_llm_messages(
    prompt_result: &prompt::AssembledPrompt,
    include_unsigned_thinking: bool,
    max_image_size: u64,
    cache_dir: &Path,
) -> (Vec<Value>, Option<Value>) {
    let llm_messages: Vec<Value> = prompt_result
        .messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
            };
            let content = if !m.content_blocks.is_empty() {
                let mut blocks: Vec<Value> = Vec::new();

                for img in &m.images {
                    if let Some(block) =
                        super::images::encode_image_block(img, max_image_size, cache_dir)
                    {
                        blocks.push(block);
                    }
                }

                if include_unsigned_thinking {
                    blocks.extend(
                        m.content_blocks
                            .iter()
                            .map(crate::content_util::content_block_to_json),
                    );
                } else {
                    blocks.extend(
                        m.content_blocks
                            .iter()
                            .filter_map(crate::content_util::content_block_to_api_json),
                    );
                }
                json!(blocks)
            } else {
                super::build_content(&m.content, &m.images, max_image_size, cache_dir)
            };
            json!({ "role": role, "content": content })
        })
        .collect();

    let system = if prompt_result.system.is_empty() {
        None
    } else if prompt_result.system.len() == 1 {
        Some(json!(prompt_result.system[0].content))
    } else {
        Some(json!(prompt_result
            .system
            .iter()
            .map(|b| { json!({"type": "text", "text": b.content, "_label": b.label}) })
            .collect::<Vec<_>>()))
    };

    (llm_messages, system)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::mcp_session::LedgerEntry;

    fn stream_result(blocks: Vec<ContentBlock>) -> shore_llm::types::StreamResult {
        shore_llm::types::StreamResult {
            content: "final text".into(),
            model: "claude-sonnet-4-5".into(),
            finish_reason: "end_turn".into(),
            usage: shore_llm::types::Usage::default(),
            timing: shore_llm::types::Timing::default(),
            tool_uses: Vec::new(),
            content_blocks: blocks,
        }
    }

    fn ledger(id: &str, name: &str, input: Value, content: &str) -> LedgerEntry {
        LedgerEntry {
            tool_use_id: id.into(),
            name: name.into(),
            input,
            content: vec![ContentBlock::Text {
                text: content.into(),
            }],
            is_error: false,
        }
    }

    #[test]
    fn splice_inserts_tool_result_after_matching_tool_use_and_strips_mcp_name() {
        let input = json!({"message": "hi"});
        let mut result = stream_result(vec![
            ContentBlock::Text {
                text: "calling ".into(),
            },
            ContentBlock::ToolUse {
                id: "toolu_1".into(),
                name: "mcp__shore__check_time".into(),
                input: input.clone(),
            },
            ContentBlock::Text {
                text: "done".into(),
            },
        ]);

        splice_claude_code_tool_ledger(
            &mut result,
            vec![ledger("toolu_1", "check_time", input, "3:22 PM")],
        );

        assert!(matches!(
            &result.content_blocks[1],
            ContentBlock::ToolUse { id, name, .. } if id == "toolu_1" && name == "check_time"
        ));
        assert!(matches!(
            &result.content_blocks[2],
            ContentBlock::ToolResult { tool_use_id, content, is_error } if tool_use_id == "toolu_1" && content == "3:22 PM" && !is_error
        ));
        assert!(matches!(&result.content_blocks[3], ContentBlock::Text { text } if text == "done"));
        assert_eq!(result.tool_uses.len(), 1);
        assert_eq!(result.tool_uses[0].id, "toolu_1");
        assert_eq!(result.tool_uses[0].name, "check_time");
    }

    #[test]
    fn splice_appends_ledger_entries_missing_from_stream_blocks() {
        let input = json!({"path": "notes.txt"});
        let mut result = stream_result(vec![ContentBlock::Text {
            text: "answer".into(),
        }]);

        splice_claude_code_tool_ledger(
            &mut result,
            vec![ledger("rpc-2", "read", input, "file contents")],
        );

        assert!(
            matches!(&result.content_blocks[0], ContentBlock::Text { text } if text == "answer")
        );
        assert!(matches!(
            &result.content_blocks[1],
            ContentBlock::ToolUse { id, name, .. } if id == "rpc-2" && name == "read"
        ));
        assert!(matches!(
            &result.content_blocks[2],
            ContentBlock::ToolResult { tool_use_id, content, .. } if tool_use_id == "rpc-2" && content == "file contents"
        ));
        assert_eq!(result.tool_uses.len(), 1);
        assert_eq!(result.tool_uses[0].id, "rpc-2");
        assert_eq!(result.tool_uses[0].name, "read");
    }

    #[test]
    fn ledger_match_falls_back_to_name_and_input_when_rpc_id_differs() {
        let input = json!({"query": "tea"});
        let mut result = stream_result(vec![ContentBlock::ToolUse {
            id: "toolu_actual".into(),
            name: "mcp__shore__search".into(),
            input: input.clone(),
        }]);

        splice_claude_code_tool_ledger(
            &mut result,
            vec![ledger("rpc-2", "search", input, "matches")],
        );

        assert_eq!(result.content_blocks.len(), 2);
        assert!(matches!(
            &result.content_blocks[1],
            ContentBlock::ToolResult { tool_use_id, content, .. } if tool_use_id == "toolu_actual" && content == "matches"
        ));
    }

    #[test]
    fn claude_code_subprocess_key_rotates_with_history_generation() {
        let data_dir = Path::new("/tmp/shore-data");
        let first = claude_code_subprocess_key(data_dir, "alice", 0);
        let rewritten = claude_code_subprocess_key(data_dir, "alice", 1);

        assert_ne!(first, rewritten);
        assert!(first.ends_with(":alice:history-0"));
        assert!(rewritten.ends_with(":alice:history-1"));
    }
}
