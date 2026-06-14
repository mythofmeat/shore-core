use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};
use shore_config::character_data_dir;
use shore_config::LoadedConfig;
use shore_protocol::client_msg::ClientMessageBody;
use shore_protocol::types::{ContentBlock, Message, Role};
use tokio::sync::Mutex;
use tracing::{debug, info, instrument};

use crate::convert::u64_to_usize;
use crate::engine::messages::PendingAlt;
use crate::engine::prompt;
use crate::engine::ConversationEngine;
use crate::handler::generation::{run_tool_phase, thinking_enabled_from_request};
use crate::handler::images::{embed_image_data, ingest_images};
use crate::handler::key_fallback::stream_with_credential_fallback;
use crate::handler::persistence::persist_and_notify;
use crate::handler::resize::warm_image_cache;

use super::{GenContext, GenerationParams, PrepareChatContextParams, PreparedChatContext};

/// Set up the generation engine: get or create the character engine,
/// record the incoming user turn (or capture regen alternatives), resolve
/// the active model, backfill autonomy state, and notify on new user
/// messages.
async fn setup_generation(
    ctx: &GenContext,
    params: &GenerationParams,
) -> Result<
    (
        Arc<Mutex<ConversationEngine>>,
        Option<PendingAlt>,
        shore_config::models::ResolvedModel,
    ),
    Box<dyn std::error::Error + Send + Sync>,
> {
    let engine_arc = {
        let mut registry = ctx.registry.lock().await;
        registry
            .get_or_create(&params.char_name)
            .map_err(|e| e.to_string())?
    };

    // Recovery generates a reply to a user turn already on disk, so it appends
    // nothing and captures no regen alternatives.
    let regen_alt = if params.recovery {
        None
    } else {
        append_user_turn(
            ctx,
            &engine_arc,
            &params.data_dir,
            &params.char_name,
            &params.body,
            params.regen,
        )
        .await?
    };

    // The handler resolves the active model (via preferences +
    // discovery) and passes the `ResolvedModel` through directly, so we
    // do not re-run `find_effective_model` here — discovered-only models
    // have a synthetic `qualified_name` that the resolver does not
    // accept as input. If nothing was passed, fall back to the
    // configured app default, then the first static chat model.
    let resolved_owned = resolve_generation_model(
        params.active_model.clone(),
        &params.effective_config,
        &params.sampler_overlay,
    )?;
    debug!(
        model = %resolved_owned.qualified_name,
        provider = %resolved_owned.provider_key,
        reasoning_effort = ?resolved_owned.reasoning_effort,
        sampler_overlay_active = !params.sampler_overlay.is_empty(),
        "model resolved"
    );

    ensure_and_backfill_autonomy(
        ctx,
        &engine_arc,
        &params.char_name,
        &params.effective_config,
    )
    .await;

    if !params.regen
        && !params.recovery
        && (!params.body.text.is_empty()
            || !params.body.images.is_empty()
            || !params.body.image_data.is_empty())
    {
        let turn_count = engine_arc.lock().await.turn_count();
        ctx.autonomy
            .notify_user_message(&params.char_name, turn_count);
    }

    Ok((engine_arc, regen_alt, resolved_owned))
}

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
    info!(
        character = %params.char_name,
        regen = params.regen,
        text_len = params.body.text.len(),
        image_count = params.body.images.len().saturating_add(params.body.image_data.len()),
        "handle_generation starting"
    );
    let wall_clock_start = Instant::now();

    let (engine_arc, regen_alt, resolved_owned) = setup_generation(&ctx, &params).await?;
    let resolved = &resolved_owned;

    // Optional human-like reply delay: hold before generating so the reply
    // doesn't arrive the instant the user hits enter. The wait scales with how
    // long they were silent and is jittered (see `response_delay`). Skipped for
    // regen (no new user turn). Because the generation runs as an abortable
    // spawned task, a superseding user message cancels this sleep mid-wait —
    // rapid follow-ups collapse into a single reply. The deadline is persisted
    // by `begin_response_delay`, so a restart mid-hold recovers via the tick.
    let response_delay_held = hold_response_delay(&ctx, &params).await;

    let mut request = build_generation_request(
        &engine_arc,
        &params.data_dir,
        &params.char_name,
        &params.effective_config,
        resolved,
        &params.body,
        params.regen,
    )
    .await;
    request.rid = params.rid.clone();
    request.forensic_character = Some(params.char_name.clone());

    maybe_inject_delay_note(&mut request, &params, &engine_arc, response_delay_held).await;

    let (result, tool_intermediate_messages) = run_generation_stream(
        &ctx,
        &params.data_dir,
        &params.char_name,
        &params.effective_config,
        &mut request,
        resolved,
        params.regen,
    )
    .await?;

    persist_and_notify(
        &ctx,
        &engine_arc,
        &params.char_name,
        resolved,
        &result,
        &request,
        tool_intermediate_messages,
        wall_clock_start,
        // Per-model override (preferences overlay) falls back to the global
        // `[memory.thinking]` default. The effect is model-dependent — see #129.
        resolved.replay_prior_thinking.unwrap_or(
            params
                .effective_config
                .app
                .memory
                .thinking
                .replay_prior_thinking,
        ),
        regen_alt,
    )
    .await?;

    // Clear the held-reply deadline only now that the assistant turn is durably
    // persisted — clearing after the sleep instead would drop the reply on a
    // crash mid-generation (the deadline would be gone, so recovery couldn't
    // re-fire it).
    if response_delay_held.is_some() || params.recovery {
        ctx.autonomy.clear_pending_reply(&params.char_name);
    }

    emit_post_persist_stream_end(&ctx, &engine_arc, request.rid.clone(), &result).await;

    maybe_schedule_compaction(
        &ctx,
        &engine_arc,
        &params.char_name,
        &params.effective_config,
        &params.data_dir,
        &result,
        request.rid.clone(),
    )
    .await;

    Ok(())
}

/// Apply the optional human-like reply delay before generating. Holds for the
/// jittered, gap-scaled delay (persisted so a restart mid-hold recovers via the
/// connected-client check), and returns the delay served — `None` when disabled,
/// a regen, or a recovery (the wait was already paid before the restart).
async fn hold_response_delay(
    ctx: &GenContext,
    params: &GenerationParams,
) -> Option<std::time::Duration> {
    if params.regen || params.recovery {
        return None;
    }
    let delay_cfg = &params.effective_config.app.behavior.response_delay;
    let delay = ctx
        .autonomy
        .begin_response_delay(&params.char_name, delay_cfg)?;
    info!(
        delay_secs = delay.as_secs(),
        "holding reply for response delay"
    );
    tokio::time::sleep(delay).await;
    Some(delay)
}

/// Tell the character it kept the user waiting, when the wait crossed
/// `notify_after` — an inline-system note it can acknowledge (same mechanism as
/// the heartbeat guide). For the live path the magnitude is the delay just
/// served; for a restart-recovered reply it is how long the user's message has
/// gone unanswered.
async fn maybe_inject_delay_note(
    request: &mut shore_llm::types::LlmRequest,
    params: &GenerationParams,
    engine_arc: &Arc<Mutex<ConversationEngine>>,
    response_delay_held: Option<std::time::Duration>,
) {
    let waited = if let Some(delay) = response_delay_held {
        Some(delay)
    } else if params.recovery {
        last_message_elapsed(engine_arc).await
    } else {
        None
    };
    let notify_after = params
        .effective_config
        .app
        .behavior
        .response_delay
        .notify_after
        .as_duration();
    if let Some(elapsed) = waited.filter(|w| *w >= notify_after) {
        request.push_inline_system(crate::autonomy::response_delay::format_delay_note(elapsed));
    }
}

/// Wall-clock time since the most recent message — the dangling user turn for a
/// recovered reply — used to tell the character how long it has gone unanswered.
/// `None` if there are no messages or the timestamp can't be parsed.
async fn last_message_elapsed(
    engine_arc: &Arc<Mutex<ConversationEngine>>,
) -> Option<std::time::Duration> {
    let engine = engine_arc.lock().await;
    let last = engine.messages().last()?;
    let ts = chrono::DateTime::parse_from_rfc3339(&last.timestamp).ok()?;
    chrono::Utc::now()
        .signed_duration_since(ts.with_timezone(&chrono::Utc))
        .to_std()
        .ok()
}

/// Record the incoming user turn (or capture the pending regen alternatives).
///
/// Returns the regeneration alternatives to thread into persistence when
/// `regen` is set; `None` when this is a fresh turn. A regen request with no
/// body content appends nothing.
async fn append_user_turn(
    ctx: &GenContext,
    engine_arc: &Arc<Mutex<ConversationEngine>>,
    data_dir: &Path,
    char_name: &str,
    body: &ClientMessageBody,
    regen: bool,
) -> Result<Option<PendingAlt>, Box<dyn std::error::Error + Send + Sync>> {
    let mut engine = engine_arc.lock().await;
    if regen {
        return Ok(Some(engine.pending_regen_alt().unwrap_or(PendingAlt {
            alternatives: Vec::new(),
        })));
    }
    if body.text.is_empty() && body.images.is_empty() && body.image_data.is_empty() {
        // Regen with no body content: nothing to append.
        return Ok(None);
    }

    let (images, mut content_blocks) =
        ingest_images(data_dir, char_name, &body.images, &body.image_data);
    content_blocks.push(ContentBlock::Text {
        text: body.text.clone(),
    });

    let user_msg = Message {
        msg_id: format!("m_{}", uuid::Uuid::new_v4()),
        origin: None,
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
    wire_msg.origin = Some(shore_protocol::server_msg::MessageOrigin::UserInput);
    embed_image_data(&mut wire_msg.images);
    let _ignored = ctx
        .event_tx
        .send(shore_protocol::server_msg::ServerMessage::NewMessage(
            shore_protocol::server_msg::NewMessage {
                revision,
                character: Some(char_name.to_owned()),
                message: wire_msg,
            },
        ));
    Ok(None)
}

/// Resolve the active model for this generation and apply any per-model
/// sampler overlay.
///
/// `active_model` is the pre-resolved model threaded through from preference
/// resolution; when absent we fall back to the configured app default, then the
/// first static chat model. App-level defaults are user configuration, not a
/// discovery-cache selection — `discovery.ignore` still applies for safety, but
/// a misspelled default should surface.
fn resolve_generation_model(
    active_model: Option<shore_config::models::ResolvedModel>,
    effective_config: &LoadedConfig,
    sampler_overlay: &crate::preferences::SamplerSettings,
) -> Result<shore_config::models::ResolvedModel, Box<dyn std::error::Error + Send + Sync>> {
    let resolved_base = match active_model {
        Some(m) => m,
        None => match effective_config.app.defaults.model.as_deref() {
            Some(name) => crate::effective_catalog::find_effective_model(
                effective_config,
                &effective_config.dirs.cache,
                name,
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
    if sampler_overlay.is_empty() {
        Ok(resolved_base)
    } else {
        Ok(crate::preferences::apply_sampler_overlay(
            &resolved_base,
            sampler_overlay,
        ))
    }
}

/// Ensure the per-character autonomy state exists and, when first created,
/// backfill its activity tracker from recent chat history (live + archived
/// segments, user turns within the last 90 days).
async fn ensure_and_backfill_autonomy(
    ctx: &GenContext,
    engine_arc: &Arc<Mutex<ConversationEngine>>,
    char_name: &str,
    effective_config: &LoadedConfig,
) {
    let is_new_autonomy_state = ctx
        .autonomy
        .ensure_state_with_config(char_name, Some(effective_config));
    if !is_new_autonomy_state {
        return;
    }

    let engine = engine_arc.lock().await;
    let now = chrono::Local::now().naive_local();
    let cutoff = now
        .checked_sub_signed(chrono::Duration::days(90))
        .unwrap_or(now);
    let mut timestamps: Vec<chrono::NaiveDateTime> = Vec::new();

    let mut collect = |msgs: &[Message]| {
        for msg in msgs
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
    };

    collect(engine.messages());
    let segments = engine.segments();
    for i in 0..segments.segment_count() {
        if let Ok(segment_msgs) = segments.read_segment(i) {
            collect(&segment_msgs);
        }
    }

    drop(engine);

    if !timestamps.is_empty() {
        info!(
            character = %char_name,
            count = timestamps.len(),
            "Backfilling activity tracker from chat history"
        );
        ctx.autonomy.backfill_activity(char_name, &timestamps);
    }
}

/// Assemble the prompt, warm the image cache, and build the LLM request
/// (sans API key — the credential-fallback wrapper resolves and rewrites the
/// key just-in-time during rotation). Client-supplied overrides are applied
/// last. The caller sets `rid` / `forensic_character` on the returned request.
async fn build_generation_request(
    engine_arc: &Arc<Mutex<ConversationEngine>>,
    data_dir: &Path,
    char_name: &str,
    effective_config: &LoadedConfig,
    resolved: &shore_config::models::ResolvedModel,
    body: &ClientMessageBody,
    regen: bool,
) -> shore_llm::types::LlmRequest {
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

    let character_data_dir = character_data_dir(data_dir, char_name);
    let include_unsigned_thinking = resolved.sdk.echoes_unsigned_thinking();
    let PreparedChatContext {
        llm_messages,
        system,
        tool_defs,
        prompt: prompt_result,
    } = super::prepare_chat_context(PrepareChatContextParams {
        character: char_name,
        character_data_dir: &character_data_dir,
        config: effective_config,
        resolved,
        messages: &messages,
        has_prior_context,
        include_unsigned_thinking,
    });

    warm_image_cache(
        &prompt_result.messages,
        effective_config.app.advanced.max_image_size,
        &effective_config.dirs.cache,
    )
    .await;

    let mut request = shore_llm::LlmClient::build_request_with_resolved_key(
        resolved,
        String::new(),
        llm_messages,
        system,
        tool_defs,
        None,
    );

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

    request
}

/// Stream the LLM response, then run the tool-use phase when the model
/// requested tools and tool use is enabled. Returns the final stream result
/// plus any intermediate (tool-loop) messages to persist.
async fn run_generation_stream(
    ctx: &GenContext,
    data_dir: &Path,
    char_name: &str,
    effective_config: &LoadedConfig,
    request: &mut shore_llm::types::LlmRequest,
    resolved: &shore_config::models::ResolvedModel,
    regen: bool,
) -> Result<(shore_llm::types::StreamResult, Vec<Message>), Box<dyn std::error::Error + Send + Sync>>
{
    info!(
        model = %resolved.model_id,
        messages = request.messages.len(),
        "Sending streaming request to LLM"
    );

    let thinking_enabled = thinking_enabled_from_request(request);

    let mut result = stream_with_credential_fallback(
        ctx,
        request,
        resolved,
        effective_config,
        regen,
        char_name,
        thinking_enabled,
    )
    .await?;

    let tool_intermediate_messages =
        if result.finish_reason == "tool_use" && effective_config.app.tools.any_enabled() {
            let tool_loop_result = run_tool_phase(
                ctx,
                data_dir,
                char_name,
                effective_config,
                request,
                result,
                resolved,
            )
            .await?;
            result = tool_loop_result.result;
            tool_loop_result.intermediate_messages
        } else {
            Vec::new()
        };

    Ok((result, tool_intermediate_messages))
}

/// Emit StreamEnd ONLY after persistence completes — clients that issue an
/// immediate follow-up command (e.g. `memory_compact` via shore-mcp) would
/// otherwise race the persist write and snapshot stale engine state. See
/// ARCHITECTURE.md (runtime flow).
async fn emit_post_persist_stream_end(
    ctx: &GenContext,
    engine_arc: &Arc<Mutex<ConversationEngine>>,
    rid: Option<String>,
    result: &shore_llm::types::StreamResult,
) {
    let (stream_msg_id, stream_revision) = {
        let engine = engine_arc.lock().await;
        (
            engine.messages().last().map(|m| m.msg_id.clone()),
            Some(engine.current_revision()),
        )
    };

    shore_llm::stream::emit_stream_end(
        &ctx.direct_tx,
        rid,
        result,
        true,
        stream_msg_id,
        stream_revision,
    )
    .await;
}

/// Check whether this turn crossed a compaction threshold and, if so, schedule
/// an inline compaction on a detached task.
async fn maybe_schedule_compaction(
    ctx: &GenContext,
    engine_arc: &Arc<Mutex<ConversationEngine>>,
    char_name: &str,
    effective_config: &LoadedConfig,
    data_dir: &Path,
    result: &shore_llm::types::StreamResult,
    rid: Option<String>,
) {
    let (turn_count, context_tokens, should_compact) = {
        let engine = engine_arc.lock().await;
        let turn_count = engine.turn_count();
        let context_tokens = u64_to_usize(result.usage.input_tokens)
            .saturating_add(u64_to_usize(result.usage.cache_read_tokens))
            .saturating_add(u64_to_usize(result.usage.cache_creation_tokens));
        let should_compact = ctx
            .autonomy
            .should_compact_now(char_name, turn_count, context_tokens);
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
            Arc::clone(engine_arc),
            char_name.to_owned(),
            effective_config.clone(),
            data_dir.to_path_buf(),
            rid,
            ctx.autonomy.cached_last_request(char_name),
        );
    }
}

fn spawn_inline_compaction(
    ctx: GenContext,
    engine_arc: Arc<Mutex<ConversationEngine>>,
    char_name: String,
    effective_config: LoadedConfig,
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
    engine_arc: Arc<Mutex<ConversationEngine>>,
    char_name: String,
    effective_config: LoadedConfig,
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
        false,
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
