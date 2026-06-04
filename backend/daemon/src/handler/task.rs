use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};
use shore_config::character_data_dir;
use shore_protocol::types::{ContentBlock, Message, Role};
use tracing::{debug, info, instrument};

use crate::convert::u64_to_usize;
use crate::engine::messages::PendingAlt;
use crate::engine::prompt;
use crate::handler::generation::{run_tool_phase, thinking_enabled_from_request};
use crate::handler::images::{embed_image_data, ingest_images};
use crate::handler::key_fallback::stream_with_credential_fallback;
use crate::handler::persistence::persist_and_notify;
use crate::handler::resize::warm_image_cache;

use super::{GenContext, GenerationParams, PrepareChatContextParams, PreparedChatContext};

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
#[expect(
    clippy::too_many_lines,
    reason = "generation orchestration phase split is tracked in #109"
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
        image_count = body.images.len().saturating_add(body.image_data.len()),
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
                provider_key: None,
                timestamp: chrono::Local::now().to_rfc3339(),
            };
            engine.append_message(user_msg.clone())?;
            let revision = engine.current_revision();
            let mut wire_msg = user_msg;
            embed_image_data(&mut wire_msg.images);
            let _ignored =
                ctx.event_tx
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
    let resolved: &shore_config::models::ResolvedModel = if sampler_overlay.is_empty() {
        resolved_base
    } else {
        resolved_owned = crate::preferences::apply_sampler_overlay(resolved_base, &sampler_overlay);
        &resolved_owned
    };
    debug!(
        model = %resolved.qualified_name,
        provider = %resolved.provider_key,
        reasoning_effort = ?resolved.reasoning_effort,
        sampler_overlay_active = !sampler_overlay.is_empty(),
        "model resolved"
    );

    let is_new_autonomy_state = ctx
        .autonomy
        .ensure_state_with_config(&char_name, Some(&effective_config));

    if is_new_autonomy_state {
        let engine = engine_arc.lock().await;
        let now = chrono::Local::now().naive_local();
        let cutoff = now
            .checked_sub_signed(chrono::Duration::days(90))
            .unwrap_or(now);
        let mut timestamps: Vec<chrono::NaiveDateTime> = Vec::new();

        for msg in engine
            .messages()
            .iter()
            .filter(|msg| msg.role == Role::User && !msg.is_tool_result_only())
        {
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
                for msg in segment_msgs
                    .iter()
                    .filter(|msg| msg.role == Role::User && !msg.is_tool_result_only())
                {
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
            ctx.autonomy.backfill_activity(&char_name, &timestamps);
        }
    }

    if !regen && (!body.text.is_empty() || !body.images.is_empty() || !body.image_data.is_empty()) {
        let turn_count = engine_arc.lock().await.turn_count();
        ctx.autonomy.notify_user_message(&char_name, turn_count);
    }

    let (messages, has_prior_context) = {
        let engine = engine_arc.lock().await;
        let has_prior = engine.segments().segment_count() > 0;
        (
            if regen {
                engine.messages_through_last_user_turn()
            } else {
                engine.messages().to_vec()
            },
            has_prior,
        )
    };

    let character_data_dir = character_data_dir(&data_dir, &char_name);
    let include_unsigned_thinking = resolved.sdk.echoes_unsigned_thinking();
    let PreparedChatContext {
        llm_messages,
        system,
        tool_defs,
        prompt: prompt_result,
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
    request.forensic_character = Some(char_name.clone());

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
                .get_or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Some(map) = opts.as_object_mut() {
                let _ignored = map.insert("budget_tokens".into(), serde_json::json!(budget));
            }
        }
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

    let tool_intermediate_messages =
        if result.finish_reason == "tool_use" && effective_config.app.behavior.tool_use.enabled {
            let tool_loop_result = run_tool_phase(
                &ctx,
                &data_dir,
                &char_name,
                &effective_config,
                &mut request,
                result,
            )
            .await?;
            result = tool_loop_result.result;
            tool_loop_result.intermediate_messages
        } else {
            Vec::new()
        };

    persist_and_notify(
        &ctx,
        &engine_arc,
        &char_name,
        resolved,
        &result,
        &request,
        tool_intermediate_messages,
        wall_clock_start,
        // Per-model override (preferences overlay) falls back to the global
        // `[memory.thinking]` default. The effect is model-dependent — see #129.
        resolved
            .replay_prior_thinking
            .unwrap_or(effective_config.app.memory.thinking.replay_prior_thinking),
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
        let context_tokens = u64_to_usize(result.usage.input_tokens)
            .saturating_add(u64_to_usize(result.usage.cache_read_tokens))
            .saturating_add(u64_to_usize(result.usage.cache_creation_tokens));
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
            Arc::clone(&engine_arc),
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
    let _ignored = tokio::spawn(async move {
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
    let _ignored = ctx
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
        None,
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

/// Convert assembled prompt messages into LLM API JSON format.
pub(crate) fn build_llm_messages(
    prompt_result: &prompt::AssembledPrompt,
    include_unsigned_thinking: bool,
    max_image_size: u64,
    cache_dir: &Path,
    active_provider_key: &str,
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
            let content = if m.content_blocks.is_empty() {
                super::build_content(&m.content, &m.images, max_image_size, cache_dir)
            } else {
                let mut blocks: Vec<Value> = Vec::new();

                for img in &m.images {
                    if let Some(block) =
                        super::images::encode_image_block(img, max_image_size, cache_dir)
                    {
                        blocks.push(block);
                    }
                }

                // Drop opaque thinking data (signatures, redacted blobs) that
                // a different provider minted — replaying it to the active
                // provider hard-fails the request.
                let minting = m.provider_key.as_deref();
                let portable = m.content_blocks.iter().filter(|b| {
                    crate::content_util::thinking_block_portable_to(b, minting, active_provider_key)
                });
                if include_unsigned_thinking {
                    blocks.extend(portable.map(crate::content_util::content_block_to_json));
                } else {
                    blocks.extend(
                        portable.filter_map(crate::content_util::content_block_to_api_json),
                    );
                }
                json!(blocks)
            };
            json!({ "role": role, "content": content })
        })
        .collect();

    let system = if prompt_result.system.is_empty() {
        None
    } else if prompt_result.system.len() == 1 {
        prompt_result
            .system
            .first()
            .map(|block| json!(block.content))
    } else {
        Some(json!(prompt_result
            .system
            .iter()
            .map(|b| { json!({"type": "text", "text": b.content, "_label": b.label}) })
            .collect::<Vec<_>>()))
    };

    (llm_messages, system)
}
