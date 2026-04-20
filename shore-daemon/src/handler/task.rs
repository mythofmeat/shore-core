use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Instant;

use serde_json::{json, Value};
use shore_config::models::Sdk;
use shore_protocol::types::{ContentBlock, Message, Role};
use tracing::{debug, info, instrument};

use crate::autonomy::parse_cache_ttl_secs;
use crate::engine::prompt::{self, CapabilitiesConfig, PromptParams};
use crate::handler::generation::{run_tool_phase, stream_with_retry, thinking_enabled_from_request};
use crate::handler::images::{embed_image_data, ingest_images};
use crate::handler::persistence::persist_and_notify;
use crate::handler::resize::warm_image_cache;

use super::{GenContext, GenerationParams};

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
        reasoning_effort_override,
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

    {
        let mut engine = engine_arc.lock().await;
        if regen {
            engine.truncate_after_last_user_turn()?;
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
                        message: wire_msg,
                    },
                ));
        }
    }

    let model_name = active_model
        .as_deref()
        .or(effective_config.app.defaults.model.as_deref());
    let resolved_base = match model_name {
        Some(name) => effective_config
            .models
            .find_model(name)
            .map_err(|e| e.to_string())?,
        None => effective_config
            .models
            .first_chat_model()
            .ok_or("No model configured")?,
    };
    // Apply the per-session `reasoning_effort` override (if any) by
    // cloning the resolved model and patching the field. The override
    // deliberately does NOT touch agent_model / researcher_model — those
    // are separate roles with their own configured defaults.
    let resolved_owned;
    let resolved: &shore_config::models::ResolvedModel =
        if let Some(new_effort) = reasoning_effort_override.as_ref() {
            let mut patched = resolved_base.clone();
            patched.reasoning_effort = new_effort.clone();
            resolved_owned = patched;
            &resolved_owned
        } else {
            resolved_base
        };
    debug!(
        model = %resolved.qualified_name,
        provider = %resolved.provider_key,
        reasoning_effort = ?resolved.reasoning_effort,
        override_active = reasoning_effort_override.is_some(),
        "model resolved"
    );

    let agent_model = effective_config
        .app
        .defaults
        .memory_agent
        .as_deref()
        .and_then(|name| effective_config.models.find_model(name).ok())
        .unwrap_or(resolved)
        .clone();

    let researcher_model = effective_config
        .app
        .defaults
        .tool_model
        .as_deref()
        .and_then(|name| effective_config.models.find_model(name).ok())
        .cloned();

    let cache_ttl_secs = resolved.cache_ttl.as_deref().and_then(parse_cache_ttl_secs);
    let is_new_autonomy_state =
        ctx.autonomy
            .ensure_state_with_config(&char_name, cache_ttl_secs, Some(&effective_config));

    if is_new_autonomy_state {
        let engine = engine_arc.lock().await;
        let cutoff = chrono::Local::now().naive_local() - chrono::Duration::days(90);
        let mut timestamps: Vec<chrono::NaiveDateTime> = Vec::new();

        for msg in engine.messages() {
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
                for msg in &segment_msgs {
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

    if !regen && (!body.text.is_empty() || !body.images.is_empty()) {
        let turn_count = engine_arc.lock().await.turn_count();
        ctx.autonomy.notify_user_message(&char_name, turn_count);
    }

    let (character_definition, user_definition) = {
        let registry = ctx.registry.lock().await;
        (
            registry.character_definition(&char_name),
            registry.user_definition(&char_name),
        )
    };

    let messages = engine_arc.lock().await.messages().to_vec();

    let character_data_dir = data_dir.join(&char_name);
    let display_name = effective_config.app.defaults.resolve_display_name();
    let tool_toggles = &effective_config.app.behavior.tool_use.tools;
    let capabilities = CapabilitiesConfig {
        interiority_enabled: effective_config.app.behavior.autonomy.interiority.enabled,
        scratchpad_enabled: tool_toggles.scratchpad_read() || tool_toggles.scratchpad_write(),
        memory_enabled: tool_toggles.memory(),
        image_memory_enabled: tool_toggles.recall_image(),
        send_image_enabled: tool_toggles.send_image(),
        generate_image_enabled: tool_toggles.generate_image(),
        web_search_enabled: tool_toggles.web_search(),
    };

    let prompt_result = prompt::assemble_prompt(&PromptParams {
        config_dir: &effective_config.dirs.config,
        character_name: &char_name,
        display_name: &display_name,
        character_definition: character_definition.as_deref(),
        user_definition: user_definition.as_deref(),
        is_private: false,
        character_data_dir: &character_data_dir,
        messages: &messages,
        max_context_tokens: resolved.max_context_tokens,
        max_output_tokens: resolved.max_tokens,
        capabilities: Some(&capabilities),
    });

    let cache_dir = &effective_config.dirs.cache;
    warm_image_cache(
        &prompt_result.messages,
        effective_config.app.advanced.max_image_size,
        cache_dir,
    )
    .await;
    let include_unsigned_thinking = matches!(resolved.sdk, Sdk::Zai);
    let (mut llm_messages, system) = build_llm_messages(
        &prompt_result,
        include_unsigned_thinking,
        effective_config.app.advanced.max_image_size,
        cache_dir,
    );
    // Strip thinking / redacted_thinking blocks from prior-turn assistant
    // messages unless the user has opted to preserve them. The in-progress
    // tool-use loop (messages appended by `run_tool_phase`) is built via a
    // different path and is not affected by this call.
    if !effective_config.app.memory.thinking.preserve_prior_turns {
        crate::content_util::strip_thinking_from_assistant_history(&mut llm_messages);
    }

    let tool_defs = if effective_config.app.behavior.tool_use.enabled {
        let toggles = &effective_config.app.behavior.tool_use.tools;
        Some(crate::tools::render_tool_defs(
            false,
            toggles,
            &char_name,
            &display_name,
        ))
    } else {
        None
    };

    let mut request =
        shore_ledger::LedgerClient::build_request(resolved, llm_messages, system, tool_defs, None)?;
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

    info!(
        model = %resolved.model_id,
        messages = request.messages.len(),
        "Sending streaming request to LLM"
    );

    let thinking_enabled = thinking_enabled_from_request(&request);

    let mut result = stream_with_retry(
        &ctx,
        &request,
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
                &agent_model,
                &researcher_model,
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
    )
    .await?;

    // Emit StreamEnd ONLY after persistence completes — clients that issue
    // an immediate follow-up command (e.g. `memory_compact` via shore-mcp)
    // would otherwise race the persist write and snapshot stale engine
    // state. See QUIRKS.md (StreamEnd / persistence ordering).
    shore_llm_client::stream::emit_stream_end(&ctx.direct_tx, request.rid.clone(), &result, true)
        .await;

    if ctx.live_speak.load(Ordering::Relaxed) {
        if let Some(ref tts_client) = ctx.tts_client {
            let text = result.content.clone();
            if !text.is_empty() {
                let msg_id = engine_arc
                    .lock()
                    .await
                    .messages()
                    .last()
                    .map(|m| m.msg_id.clone())
                    .unwrap_or_default();
                let voice = effective_config
                    .app
                    .tts
                    .voice
                    .clone()
                    .unwrap_or_else(|| char_name.clone());
                crate::tts::relay_speech(
                    tts_client,
                    &text,
                    &voice,
                    &msg_id,
                    request.rid.clone(),
                    &ctx.event_tx,
                )
                .await;
            }
        }
    }

    {
        let mut engine = engine_arc.lock().await;
        let turn_count = engine.turn_count();
        let context_tokens = result.usage.input_tokens as usize
            + result.usage.cache_read_tokens as usize
            + result.usage.cache_creation_tokens as usize;
        if ctx
            .autonomy
            .should_compact_now(&char_name, turn_count, context_tokens)
        {
            info!(
                character = %char_name,
                turn_count,
                context_tokens,
                "Running inline compaction"
            );
            let _ = ctx
                .direct_tx
                .send(
                    shore_protocol::server_msg::ServerMessage::Phase(
                        shore_protocol::server_msg::Phase {
                            rid: None,
                            phase: "compacting".into(),
                            model: None,
                        },
                    )
                    .with_rid(request.rid.clone()),
                )
                .await;

            match crate::memory::compaction::run_compaction(
                &char_name,
                &effective_config,
                &ctx.llm_client,
                &data_dir,
                &ctx.notifier,
            )
            .await
            {
                Ok(retained_count) => {
                    engine.reload().map_err(|e| e.to_string())?;
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
    }

    Ok(())
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
