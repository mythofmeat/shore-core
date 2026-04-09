//! Message processing handler.
//!
//! Consumes routed messages from the SWP server and orchestrates the
//! engine → prompt → LLM → tool loop → persist pipeline.
//!
//! Generation (Message/Regen) runs in spawned tokio tasks so the handler loop
//! never blocks on LLM streaming. Commands (status, log, etc.) are processed
//! inline and always return immediately.

mod generation;
mod images;
mod persistence;

pub(crate) use images::{build_content, embed_image_data, media_type_for_path};
use generation::{run_tool_phase, stream_with_retry};
use images::ingest_images;
use persistence::persist_and_notify;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use base64::Engine as _;
use serde_json::{json, Value};
use shore_protocol::client_msg::{ClientMessage, ClientMessageBody};
use shore_protocol::error::ErrorCode;
use shore_protocol::server_msg::{Error as SwpError, ServerMessage};
use shore_protocol::types::{ContentBlock, Message, Role};
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, error, info, instrument, warn};

use crate::autonomy::manager::AutonomyManager;
use crate::autonomy::parse_cache_ttl_secs;
use crate::characters::CharacterRegistry;
use crate::commands::{self, CommandContext, SessionTokens};
use crate::engine::prompt::{self, CapabilitiesConfig, PromptParams};
use crate::memory::agent::{AgentSearchContext, MemoryAgent};
use crate::memory::agent_llm::AgentLlm;
use crate::memory::compaction_impls::ImageGenConfig;
use crate::memory::db::MemoryDB;
use crate::memory::researcher::MemoryResearcher;
use crate::notifications::{NotificationEvent, NotificationService};
use crate::server::RoutedMessage;
use crate::tools::context::SharedToolContext;
use crate::tools::{self as tool_system, ToolContext};
use shore_config::app::SearchConfig;
use shore_config::models::Sdk;
use shore_config::LoadedConfig;
use shore_ledger::LedgerClient;
use shore_llm_client::stream::CacheContext;

// ── Per-request tool context (wraps SharedToolContext + autonomy) ────

pub(super) struct HandlerToolContext {
    inner: SharedToolContext,
    autonomy_val: AutonomyManager,
}

impl ToolContext for HandlerToolContext {
    fn memory_db(&self) -> &MemoryDB {
        self.inner.memory_db()
    }
    fn memory_agent(&self) -> &MemoryAgent {
        self.inner.memory_agent()
    }
    fn agent_llm(&self) -> &dyn AgentLlm {
        self.inner.agent_llm()
    }
    fn agent_model(&self) -> &shore_config::models::ResolvedModel {
        self.inner.agent_model()
    }
    fn researcher_llm(&self) -> Option<&dyn AgentLlm> {
        self.inner.researcher_llm()
    }
    fn researcher_model(&self) -> Option<&shore_config::models::ResolvedModel> {
        self.inner.researcher_model()
    }
    fn memory_researcher(&self) -> Option<&MemoryResearcher> {
        self.inner.memory_researcher()
    }
    fn indexer(&self) -> Option<&dyn crate::memory::agent::types::AgentIndexer> {
        self.inner.indexer()
    }
    fn search_context(&self) -> Option<&AgentSearchContext> {
        self.inner.search_context()
    }
    fn rag(&self) -> &dyn crate::memory::agent::AgentRag {
        self.inner.rag()
    }
    fn image_dir(&self) -> &str {
        self.inner.image_dir()
    }
    fn llm_client(&self) -> Option<&shore_llm_client::LlmClient> {
        self.inner.llm_client()
    }
    fn image_gen_config(&self) -> Option<&ImageGenConfig> {
        self.inner.image_gen_config()
    }
    fn search_config(&self) -> &SearchConfig {
        self.inner.search_config()
    }
    fn autonomy_manager(&self) -> Option<&AutonomyManager> {
        Some(&self.autonomy_val)
    }
    fn character_name(&self) -> &str {
        self.inner.character_name()
    }
    fn scratchpad_dir(&self) -> &str {
        self.inner.scratchpad_dir()
    }
}

// ── Shared context for spawned generation tasks ───────────────────────

/// All state needed by a generation task. Cheap to clone (all Arc-backed).
#[derive(Clone)]
struct GenContext {
    registry: Arc<Mutex<CharacterRegistry>>,
    llm_client: LedgerClient,
    push_tx: broadcast::Sender<ServerMessage>,
    autonomy: AutonomyManager,
    /// Set to false after the first successful generation since daemon start.
    is_first_after_restart: Arc<AtomicBool>,
    /// Set to true after the first cache-read hit is observed.
    has_seen_cache_read: Arc<AtomicBool>,
    /// Set by the compaction task after a successful compaction.
    compaction_occurred: Arc<std::sync::atomic::AtomicBool>,
    /// Accumulated token counts (shared with CommandContext for status display).
    session_tokens: Arc<std::sync::Mutex<SessionTokens>>,
    /// In-memory diagnostics ring buffers.
    diagnostics: Arc<std::sync::Mutex<shore_diagnostics::Diagnostics>>,
    /// Push notification service.
    notifier: NotificationService,
}

/// Per-generation parameters that vary with each request.
struct GenerationParams {
    body: ClientMessageBody,
    regen: bool,
    char_name: String,
    rid: Option<String>,
    effective_config: LoadedConfig,
    data_dir: PathBuf,
    active_model: Option<String>,
}

// ── MessageHandler ────────────────────────────────────────────────────

/// The message processing handler.
///
/// Routes commands inline (fast path) and spawns tokio tasks for generation
/// (Message/Regen), so the handler loop is never blocked by LLM streaming.
pub struct MessageHandler {
    pub registry: Arc<Mutex<CharacterRegistry>>,
    pub cmd_ctx: CommandContext,
    pub llm_client: LedgerClient,
    pub push_tx: broadcast::Sender<ServerMessage>,
    pub is_first_after_restart: Arc<AtomicBool>,
    pub has_seen_cache_read: Arc<AtomicBool>,
    pub compaction_occurred: Arc<std::sync::atomic::AtomicBool>,
    pub autonomy: AutonomyManager,
    pub notifier: NotificationService,
    /// Handle to the active generation task, if any. Used for cancellation.
    pub generation_handle: Option<tokio::task::JoinHandle<()>>,
}

impl MessageHandler {
    /// Run the message processing loop. Blocks until the route channel closes.
    ///
    /// Commands are processed inline (no LLM I/O, always fast).
    /// Engine messages (Message/Regen) are spawned as independent tokio tasks,
    /// so this loop never blocks on LLM streaming.
    pub async fn run(&mut self, route_rx: Arc<Mutex<tokio::sync::mpsc::Receiver<RoutedMessage>>>) {
        info!("message handler started");
        let mut rx = route_rx.lock().await;
        while let Some(msg) = rx.recv().await {
            match msg {
                RoutedMessage::Command { cmd, character } => {
                    debug!(character = ?character, "handling command");
                    let result = self.dispatch_command(&cmd, character.as_deref()).await;
                    let _ = self.push_tx.send(result);
                }
                RoutedMessage::Engine { msg, character } => {
                    let msg_kind = match &msg {
                        ClientMessage::Message(_) => "message",
                        ClientMessage::Regen(_) => "regen",
                        ClientMessage::Cancel(_) => "cancel",
                        _ => "other",
                    };
                    debug!(character = ?character, kind = msg_kind, "handling engine message");
                    // Handle cancellation: abort the active generation task.
                    if matches!(msg, ClientMessage::Cancel(_)) {
                        self.cancel_generation("user cancelled");
                        continue;
                    }

                    // Resolve char_name and effective config with a brief registry lock.
                    // Done here (before spawning) so the handler can report resolution
                    // errors synchronously and the task has an owned config snapshot.
                    let (char_name, effective_config) = {
                        let mut registry = self.registry.lock().await;
                        let char_name = match registry.resolve_character(character.as_deref()) {
                            Ok(name) => name,
                            Err(e) => {
                                let _ = self.push_tx.send(ServerMessage::Error(SwpError {
                                    code: ErrorCode::InvalidRequest,
                                    message: e.to_string(),
                                }));
                                continue;
                            }
                        };
                        let effective_config = registry.effective_config(&char_name).clone();
                        (char_name, effective_config)
                    };

                    let (body, regen) = match msg {
                        ClientMessage::Message(body) => (body, false),
                        ClientMessage::Regen(regen) => {
                            let body = ClientMessageBody {
                                rid: regen.rid,
                                text: String::new(),
                                stream: regen.stream,
                                images: vec![],
                                image_data: vec![],
                                absence_seconds: None,
                                overrides: None,
                            };
                            (body, true)
                        }
                        _ => continue,
                    };

                    // Validate rid is safe for HTTP headers before propagating.
                    let rid = body.rid.clone().filter(|r| r.is_ascii() && !r.contains('\0'));
                    let gen = self.gen_context();
                    let push_tx = self.push_tx.clone();
                    let notifier = self.notifier.clone();
                    let params = GenerationParams {
                        body,
                        regen,
                        char_name,
                        rid,
                        effective_config,
                        data_dir: self.cmd_ctx.data_dir.clone(),
                        active_model: self.cmd_ctx.active_model.clone(),
                    };

                    if let Some(prev) = self.generation_handle.take() {
                        info!("Aborting previous generation (superseded by new request)");
                        prev.abort();
                    }
                    self.generation_handle = Some(tokio::spawn(async move {
                        let notify_name = params.char_name.clone();
                        if let Err(e) = handle_generation(gen, params).await {
                            error!(error = %e, "Error processing engine message");
                            let err_msg = e.to_string();
                            let _ = push_tx.send(ServerMessage::Error(SwpError {
                                code: ErrorCode::InternalError,
                                message: err_msg.clone(),
                            }));
                            notifier.notify(
                                NotificationEvent::Error,
                                &format!("Shore — {notify_name}"),
                                &err_msg,
                            );
                        }
                    }));
                }
                RoutedMessage::AllClientsDisconnected => {
                    self.cancel_generation("all clients disconnected");
                }
            }
        }
        info!("Message handler shutting down (route channel closed)");
    }

    /// Cancel any active generation task and send a minimal StreamEnd.
    fn cancel_generation(&mut self, reason: &str) {
        if let Some(handle) = self.generation_handle.take() {
            info!(reason, "Cancelling active generation");
            handle.abort();
            let _ = self.push_tx.send(ServerMessage::StreamEnd(
                shore_protocol::server_msg::StreamEnd {
                    content: String::new(),
                    metadata: shore_protocol::types::StreamMetadata {
                        tokens: shore_protocol::types::TokenCounts {
                            input: 0,
                            output: 0,
                            cache_read: 0,
                            cache_write: 0,
                        },
                        timing: shore_protocol::types::TimingInfo {
                            total_ms: 0,
                            ttft_ms: 0,
                        },
                        model: String::new(),
                    },
                    finish_reason: "cancelled".into(),
                },
            ));
        }
    }

    /// Build a GenContext from the current handler state.
    fn gen_context(&self) -> GenContext {
        GenContext {
            registry: self.registry.clone(),
            llm_client: self.llm_client.clone(),
            push_tx: self.push_tx.clone(),
            autonomy: self.autonomy.clone(),
            is_first_after_restart: self.is_first_after_restart.clone(),
            has_seen_cache_read: self.has_seen_cache_read.clone(),
            compaction_occurred: self.compaction_occurred.clone(),
            session_tokens: self.cmd_ctx.session_tokens.clone(),
            diagnostics: self.cmd_ctx.diagnostics.clone(),
            notifier: self.notifier.clone(),
        }
    }

    /// Resolve the engine for a character and dispatch a command.
    async fn dispatch_command(
        &mut self,
        cmd: &shore_protocol::client_msg::Command,
        character: Option<&str>,
    ) -> ServerMessage {
        debug!(command = %cmd.name, character = ?character, "dispatching command");
        // list_characters doesn't need a resolved character — handle it
        // before character resolution so it works when multiple characters
        // are available and none is explicitly selected.
        if cmd.name == "list_characters" {
            return match commands::dispatch_characterless(&self.cmd_ctx, cmd) {
                Ok(data) => {
                    ServerMessage::CommandOutput(shore_protocol::server_msg::CommandOutput {
                        name: cmd.name.clone(),
                        data,
                    })
                }
                Err((code, msg)) => ServerMessage::Error(SwpError { code, message: msg }),
            };
        }

        // Resolve character, get effective config and engine Arc (brief registry lock).
        let (engine_arc, effective_config) = {
            let mut registry = self.registry.lock().await;

            let char_name = match registry.resolve_character(character) {
                Ok(name) => name,
                Err(e) => {
                    return ServerMessage::Error(SwpError {
                        code: ErrorCode::InvalidRequest,
                        message: e.to_string(),
                    });
                }
            };

            let effective_config = registry.effective_config(&char_name).clone();

            let engine_arc = match registry.get_or_create(&char_name) {
                Ok(arc) => arc,
                Err(e) => {
                    return ServerMessage::Error(SwpError {
                        code: ErrorCode::InternalError,
                        message: e.to_string(),
                    });
                }
            };

            (engine_arc, effective_config)
        };

        // Swap in per-character effective config for the duration of this dispatch.
        let original = std::mem::replace(&mut self.cmd_ctx.config, effective_config);

        let result = commands::dispatch(engine_arc, &mut self.cmd_ctx, cmd).await;

        // config_reset reloads the global config — keep the new value and
        // invalidate the per-character cache so future lookups re-merge.
        if cmd.name == "config_reset" {
            let mut registry = self.registry.lock().await;
            registry.set_global_config(self.cmd_ctx.config.clone());
        } else {
            self.cmd_ctx.config = original;
        }

        result
    }
}

// ---------------------------------------------------------------------------
// Generation task (free async fn, runs in spawned tokio task)
// ---------------------------------------------------------------------------

#[instrument(skip(ctx, params), fields(char = %params.char_name, rid = params.rid.as_deref().unwrap_or("-")))]
async fn handle_generation(
    ctx: GenContext,
    params: GenerationParams,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let GenerationParams {
        body,
        regen,
        char_name,
        rid,
        effective_config,
        data_dir,
        active_model,
    } = params;
    info!(
        character = %char_name,
        regen,
        text_len = body.text.len(),
        image_count = body.images.len() + body.image_data.len(),
        "handle_generation starting"
    );
    let wall_clock_start = Instant::now();

    // Get engine Arc from registry (brief lock — registry released immediately after).
    let engine_arc = {
        let mut registry = ctx.registry.lock().await;
        registry
            .get_or_create(&char_name)
            .map_err(|e| e.to_string())?
    };

    // 1. Reload engine if compaction ran since the last message.
    //    MUST happen before appending the user message — append calls persist()
    //    which rewrites active.jsonl from the in-memory state; without a reload
    //    first, that would overwrite the compacted file with stale messages.
    if ctx.autonomy.take_needs_reload(&char_name) {
        let mut engine = engine_arc.lock().await;
        info!(character = %char_name, "Reloading engine after compaction");
        engine.reload().map_err(|e| e.to_string())?;
    }

    // 2. Append user message or truncate after last user turn for regen.
    {
        let mut engine = engine_arc.lock().await;
        if regen {
            engine.truncate_after_last_user_turn()?;
        } else if !body.text.is_empty() || !body.images.is_empty() || !body.image_data.is_empty() {
            let (images, mut content_blocks) =
                ingest_images(&data_dir, &char_name, &body.images, &body.image_data);

            // User text comes after the image annotations.
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
            // Embed image data before broadcasting so clients can display
            // without filesystem access to the server's paths.
            let mut wire_msg = user_msg;
            embed_image_data(&mut wire_msg.images);
            let _ = ctx.push_tx.send(ServerMessage::NewMessage(
                shore_protocol::server_msg::NewMessage { message: wire_msg },
            ));
        }
    } // engine lock released

    // 3. Resolve model.
    let model_name = active_model
        .as_deref()
        .or(effective_config.app.defaults.model.as_deref());
    let resolved = match model_name {
        Some(name) => effective_config
            .models
            .find_model(name)
            .map_err(|e| e.to_string())?,
        None => effective_config
            .models
            .first_chat_model()
            .ok_or("No model configured")?,
    };
    debug!(
        model = %resolved.qualified_name,
        provider = %resolved.provider_key,
        "model resolved"
    );

    // 4. Resolve memory agent and researcher models.
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

    // 5. Ensure autonomy state with cache TTL for unified interiority timer.
    // Must happen before notify_user_message so session_start is set on first message.
    let cache_ttl_secs = resolved.cache_ttl.as_deref().and_then(parse_cache_ttl_secs);
    let is_new_autonomy_state = ctx.autonomy.ensure_state_with_config(
        &char_name,
        cache_ttl_secs,
        Some(&effective_config),
    );

    // Backfill activity tracker from existing chat history on first creation.
    // Only include the last 90 days — older data would pollute availability signals.
    if is_new_autonomy_state {
        let engine = engine_arc.lock().await;
        let cutoff = chrono::Local::now().naive_local() - chrono::Duration::days(90);
        let mut timestamps: Vec<chrono::NaiveDateTime> = Vec::new();

        // Active window messages.
        for msg in engine.messages() {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&msg.timestamp) {
                let naive = dt.with_timezone(&chrono::Local).naive_local();
                if naive >= cutoff {
                    timestamps.push(naive);
                }
            }
        }

        // Archived segments.
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

    // 5. Load character and user definitions (brief registry lock).
    let (character_definition, user_definition) = {
        let registry = ctx.registry.lock().await;
        (
            registry.character_definition(&char_name),
            registry.user_definition(&char_name),
        )
    };

    // 6. Read messages for prompt assembly (brief engine lock, then clone).
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
        remember_image_enabled: tool_toggles.remember_image(),
        generate_image_enabled: tool_toggles.generate_image(),
        web_search_enabled: tool_toggles.web_search(),
        activity_heatmap_enabled: tool_toggles.activity_heatmap(),
        roll_dice_enabled: tool_toggles.roll_dice(),
        check_time_enabled: tool_toggles.check_time(),
    };

    let recap_path = character_data_dir.join("recaps.jsonl");
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
        recap_store_path: Some(&recap_path),
    });

    // 7. Build LLM messages from assembled prompt.
    // Z.AI thinking blocks have no signature, so we include unsigned
    // thinking blocks for providers that handle them natively.
    let include_unsigned_thinking = matches!(resolved.sdk, Sdk::Zai);
    let (llm_messages, system) = build_llm_messages(&prompt_result, include_unsigned_thinking);

    // 8. Build tool definitions from unified tool system.
    let tool_defs = if effective_config.app.behavior.tool_use.enabled {
        let toggles = &effective_config.app.behavior.tool_use.tools;
        let defs: Vec<Value> = tool_system::available_tools(false, toggles)
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters.clone(),
                })
            })
            .collect();
        Some(defs)
    } else {
        None
    };

    // 9. Build LLM request.
    let mut request = LedgerClient::build_request(resolved, llm_messages, system, tool_defs, None)?;
    request.rid = rid;
    request.forensic_character = Some(char_name.to_owned());

    // Apply per-message parameter overrides from the client.
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

    // Determine if thinking is enabled for this request.
    let thinking_enabled = request
        .provider_options
        .as_ref()
        .and_then(|opts| opts.get("budget_tokens"))
        .and_then(|v| v.as_u64())
        .map_or(false, |b| b > 0);

    // 10. Stream response from shore-llm (with retry on transient errors).
    let mut result = stream_with_retry(
        &ctx,
        &request,
        &engine_arc,
        resolved,
        &effective_config,
        regen,
        &char_name,
        thinking_enabled,
    )
    .await?;

    // Build cache context for tool loop.
    let tool_cache_warnings = matches!(resolved.sdk, Sdk::Anthropic)
        && effective_config.app.advanced.cache_invalidation_warnings;
    let cache_ctx = CacheContext {
        conversation_turn_count: engine_arc.lock().await.messages().len(),
        is_first_after_restart: ctx.is_first_after_restart.load(Ordering::Acquire),
        is_first_after_compaction: false,
        cache_invalidation_warnings: tool_cache_warnings,
        has_seen_cache_read: ctx.has_seen_cache_read.load(Ordering::Acquire),
    };

    ctx.is_first_after_restart.store(false, Ordering::Release);

    // 11. Run tool loop if the LLM requested tool use.
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
                &cache_ctx,
            )
            .await?;
            result = tool_loop_result.result;
            tool_loop_result.intermediate_messages
        } else {
            Vec::new()
        };

    // 12. Persist intermediate tool messages and final assistant message.
    persist_and_notify(
        &ctx,
        &engine_arc,
        &char_name,
        resolved,
        &result,
        &request,
        tool_intermediate_messages,
        wall_clock_start,
    )
    .await
}

// ---------------------------------------------------------------------------
// Extracted helpers for handle_generation phases
// ---------------------------------------------------------------------------

/// Phase 7: Convert assembled prompt messages into LLM API JSON format.
///
/// Returns `(messages, system)` — the system parameter is `None` if empty,
/// a plain string for a single block, or an array of `{type, text}` objects.
pub(crate) fn build_llm_messages(
    prompt_result: &prompt::AssembledPrompt,
    include_unsigned_thinking: bool,
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

                // Prepend base64-encoded image blocks from m.images.
                for img in &m.images {
                    if let Some(media_type) = media_type_for_path(&img.path) {
                        match std::fs::read(&img.path) {
                            Ok(bytes) => {
                                let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
                                blocks.push(json!({
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": media_type,
                                        "data": encoded,
                                    }
                                }));
                            }
                            Err(e) => {
                                warn!(path = %img.path, error = %e, "Failed to read image file for LLM");
                            }
                        }
                    }
                }

                if include_unsigned_thinking {
                    blocks.extend(m.content_blocks.iter()
                        .map(crate::content_util::content_block_to_json));
                } else {
                    blocks.extend(m.content_blocks.iter()
                        .filter_map(crate::content_util::content_block_to_api_json));
                }
                json!(blocks)
            } else {
                build_content(&m.content, &m.images)
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use shore_protocol::client_msg::{Command, Regen};
    use shore_protocol::error::ErrorCode;
    use shore_protocol::types::ImageRef;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    /// Build a `MessageHandler` backed by a tempdir with the given characters.
    fn make_handler(
        tmp: &TempDir,
        chars: &[&str],
    ) -> (MessageHandler, broadcast::Receiver<ServerMessage>) {
        let config_dir = tmp.path().join("config");
        let data_dir = tmp.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();

        for name in chars {
            let char_dir = config_dir.join("characters").join(name);
            std::fs::create_dir_all(&char_dir).unwrap();
            std::fs::write(
                char_dir.join("character.md"),
                format!("{name} system prompt"),
            )
            .unwrap();
        }

        let (push_tx, push_rx) = broadcast::channel(16);

        let loaded_config = shore_config::LoadedConfig::new_for_test(
            shore_config::app::AppConfig::default(),
            shore_config::models::ModelCatalog::default(),
            shore_config::ShoreDirs {
                config: config_dir.clone(),
                data: data_dir.clone(),
                runtime: tmp.path().join("runtime"),
            },
        );

        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(());
        let (autonomy, _compaction_rx) = AutonomyManager::new(
            Default::default(),
            Default::default(),
            data_dir.clone(),
            shutdown_rx,
        );

        let registry = CharacterRegistry::new(
            config_dir,
            data_dir.clone(),
            push_tx.clone(),
            loaded_config.clone(),
        );

        let ledger_client = shore_ledger::LedgerClient::new(
            shore_llm_client::LlmClient::new(),
            &data_dir.join("ledger.db"),
        )
        .unwrap();

        let cmd_ctx = CommandContext {
            config: loaded_config.clone(),
            push_tx: push_tx.clone(),
            data_dir: data_dir.clone(),
            active_model: None,
            session_tokens: Arc::new(std::sync::Mutex::new(SessionTokens::default())),
            autonomy: autonomy.clone(),
            llm_client: ledger_client.clone(),
            diagnostics: Arc::new(std::sync::Mutex::new(
                shore_diagnostics::Diagnostics::default(),
            )),
            memory_shell_sessions: std::collections::HashMap::new(),
        };

        let handler = MessageHandler {
            registry: Arc::new(Mutex::new(registry)),
            cmd_ctx,
            llm_client: ledger_client,
            push_tx: push_tx.clone(),
            is_first_after_restart: Arc::new(AtomicBool::new(false)),
            has_seen_cache_read: Arc::new(AtomicBool::new(false)),
            compaction_occurred: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            autonomy,
            notifier: NotificationService::new(Default::default()),
            generation_handle: None,
        };

        (handler, push_rx)
    }

    // ── dispatch_command ────────────────────────────────────────────

    #[tokio::test]
    async fn dispatch_command_valid_character() {
        let tmp = TempDir::new().unwrap();
        let (mut handler, _rx) = make_handler(&tmp, &["Alice"]);

        let cmd = Command {
            rid: None,
            forensic_character: None,
            name: "status".into(),
            args: serde_json::json!({}),
        };

        let result = handler.dispatch_command(&cmd, Some("Alice")).await;
        assert!(
            matches!(result, ServerMessage::CommandOutput(_)),
            "Expected CommandOutput, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn dispatch_command_invalid_character() {
        let tmp = TempDir::new().unwrap();
        let (mut handler, _rx) = make_handler(&tmp, &["Alice"]);

        let cmd = Command {
            rid: None,
            forensic_character: None,
            name: "status".into(),
            args: serde_json::json!({}),
        };

        let result = handler.dispatch_command(&cmd, Some("Bob")).await;
        match result {
            ServerMessage::Error(e) => {
                assert_eq!(e.code, ErrorCode::InvalidRequest);
                assert!(e.message.contains("Bob"));
            }
            other => panic!("Expected Error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn dispatch_command_auto_select() {
        let tmp = TempDir::new().unwrap();
        let (mut handler, _rx) = make_handler(&tmp, &["Alice"]);

        let cmd = Command {
            rid: None,
            forensic_character: None,
            name: "status".into(),
            args: serde_json::json!({}),
        };

        let result = handler.dispatch_command(&cmd, None).await;
        assert!(
            matches!(result, ServerMessage::CommandOutput(_)),
            "Expected auto-select to succeed, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn dispatch_command_ambiguous_character() {
        let tmp = TempDir::new().unwrap();
        let (mut handler, _rx) = make_handler(&tmp, &["Alice", "Bob"]);

        let cmd = Command {
            rid: None,
            forensic_character: None,
            name: "status".into(),
            args: serde_json::json!({}),
        };

        let result = handler.dispatch_command(&cmd, None).await;
        match result {
            ServerMessage::Error(e) => {
                assert_eq!(e.code, ErrorCode::InvalidRequest);
            }
            other => panic!("Expected Error, got {:?}", other),
        }
    }

    // ── handle_engine_message ──────────────────────────────────────

    #[tokio::test]
    async fn handle_engine_message_regen_builds_empty_body() {
        let tmp = TempDir::new().unwrap();
        let (handler, _rx) = make_handler(&tmp, &["Alice"]);

        // Regen without a model configured will fail at model resolution,
        // but the important thing is it doesn't fail at message routing.
        let regen = ClientMessage::Regen(Regen {
            rid: Some("r1".into()),
            stream: false,
            guidance: None,
        });

        // Spawn is non-blocking; just verify the handler doesn't panic when
        // routing a Regen to a character with no model configured.
        let (char_name, effective_config) = {
            let mut registry = handler.registry.lock().await;
            let char_name = registry.resolve_character(Some("Alice")).unwrap();
            let effective_config = registry.effective_config(&char_name).clone();
            (char_name, effective_config)
        };

        let (body, is_regen) = match regen {
            ClientMessage::Regen(r) => {
                let body = ClientMessageBody {
                    rid: r.rid,
                    text: String::new(),
                    stream: r.stream,
                    images: vec![],
                    image_data: vec![],
                    absence_seconds: None,
                    overrides: None,
                };
                (body, true)
            }
            _ => unreachable!(),
        };

        let gen = handler.gen_context();
        let data_dir = handler.cmd_ctx.data_dir.clone();

        // This will return an Err (no model configured) — that's expected.
        let result = handle_generation(
            gen,
            GenerationParams {
                body,
                regen: is_regen,
                char_name,
                rid: None,
            forensic_character: None,
                effective_config,
                data_dir,
                active_model: None,
            },
        )
        .await;

        assert!(result.is_err(), "Expected error due to no model configured");
    }

    // ── image helpers ────────────────────────────────────────────────────

    #[test]
    fn media_type_for_path_supported() {
        assert_eq!(media_type_for_path("photo.jpg"), Some("image/jpeg"));
        assert_eq!(media_type_for_path("photo.jpeg"), Some("image/jpeg"));
        assert_eq!(media_type_for_path("photo.JPG"), Some("image/jpeg"));
        assert_eq!(media_type_for_path("photo.png"), Some("image/png"));
        assert_eq!(media_type_for_path("photo.gif"), Some("image/gif"));
        assert_eq!(media_type_for_path("photo.webp"), Some("image/webp"));
    }

    #[test]
    fn media_type_for_path_unsupported() {
        assert_eq!(media_type_for_path("photo.bmp"), None);
        assert_eq!(media_type_for_path("photo.tiff"), None);
        assert_eq!(media_type_for_path("file.txt"), None);
        assert_eq!(media_type_for_path("noext"), None);
    }

    #[test]
    fn build_content_text_only() {
        let result = build_content("hello", &[]);
        assert_eq!(result, serde_json::json!("hello"));
    }

    #[test]
    fn build_content_with_image() {
        let tmp = TempDir::new().unwrap();
        let img_path = tmp.path().join("test.png");
        // Minimal valid PNG: 8-byte header.
        std::fs::write(&img_path, b"\x89PNG\r\n\x1a\n").unwrap();

        let images = vec![ImageRef {
            path: img_path.to_str().unwrap().to_string(),
            caption: None,
            data: None,
        }];

        let result = build_content("describe this", &images);
        let blocks = result.as_array().expect("Should be a JSON array");
        assert_eq!(blocks.len(), 2); // image block + text block

        // Image block.
        assert_eq!(blocks[0]["type"], "image");
        assert_eq!(blocks[0]["source"]["type"], "base64");
        assert_eq!(blocks[0]["source"]["media_type"], "image/png");
        assert!(!blocks[0]["source"]["data"].as_str().unwrap().is_empty());

        // Text block.
        assert_eq!(blocks[1]["type"], "text");
        assert_eq!(blocks[1]["text"], "describe this");
    }

    #[test]
    fn build_content_skips_unsupported_and_missing() {
        let tmp = TempDir::new().unwrap();
        let images = vec![
            // Unsupported extension → skipped.
            ImageRef {
                path: tmp.path().join("file.bmp").to_str().unwrap().to_string(),
                caption: None,
                data: None,
            },
            // Missing file → skipped.
            ImageRef {
                path: tmp.path().join("ghost.png").to_str().unwrap().to_string(),
                caption: None,
                data: None,
            },
        ];

        let result = build_content("text", &images);
        let blocks = result.as_array().expect("Should be a JSON array");
        // Only the text block remains (both images were skipped).
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
    }

    // ── Pipeline integration ────────────────────────────────────────

    /// Build a mock Anthropic SSE stream for a simple text response.
    fn sse_text_response(text: &str) -> String {
        format!(
            "event: message_start\n\
             data: {{\"type\":\"message_start\",\"message\":{{\"model\":\"test\",\"usage\":{{\"input_tokens\":20}}}}}}\n\n\
             event: content_block_start\n\
             data: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n\
             event: content_block_delta\n\
             data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{text}\"}}}}\n\n\
             event: content_block_stop\n\
             data: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
             event: message_delta\n\
             data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":10}}}}\n\n\
             event: message_stop\n\
             data: {{\"type\":\"message_stop\"}}\n\n"
        )
    }

    /// Spawn a mock HTTP server that returns canned SSE on each connection.
    async fn mock_sse_server(sse_body: String) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base_url = format!("http://127.0.0.1:{port}");

        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.split();
            // Drain the HTTP request.
            let mut buf = vec![0u8; 16384];
            let _ = tokio::io::AsyncReadExt::read(&mut reader, &mut buf).await;

            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: text/event-stream\r\n\
                 \r\n\
                 {sse_body}"
            );
            writer.write_all(response.as_bytes()).await.unwrap();
            writer.shutdown().await.unwrap();
        });

        (base_url, handle)
    }

    /// Build a model catalog containing a single test model pointing at a mock server.
    fn mock_model_catalog(base_url: &str) -> shore_config::models::ModelCatalog {
        use shore_config::models::{ModelCatalog, ResolvedModel, Sdk};

        let model = ResolvedModel {
            name: "test".into(),
            qualified_name: "chat.anthropic.test".into(),
            category: "chat".into(),
            provider_key: "anthropic".into(),
            sdk: Sdk::Anthropic,
            model_id: "claude-test".into(),
            api_key_env: None,
            base_url: Some(base_url.to_string()),
            max_context_tokens: None,
            max_tokens: Some(4096),
            temperature: Some(0.7),
            top_p: None,
            reasoning_effort: None,
            budget_tokens: None,
            cache_ttl: None,
            keepalive_enabled: None,
            keepalive_ttl: None,
            keepalive_max_pings: None,
            openrouter_provider: None,
            vertex_project: None,
            vertex_location: None,
            gemini_generation: None,
            gemini_web_search: None,
            zai_clear_thinking: None,
            zai_subscription: None,
        };

        let mut chat = BTreeMap::new();
        chat.insert("test".into(), model);
        ModelCatalog {
            chat,
            ..Default::default()
        }
    }

    /// Build a `MessageHandler` with a model catalog pointing at a mock server.
    fn make_handler_with_models(
        tmp: &TempDir,
        chars: &[&str],
        models: shore_config::models::ModelCatalog,
    ) -> (MessageHandler, broadcast::Receiver<ServerMessage>) {
        let config_dir = tmp.path().join("config");
        let data_dir = tmp.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();

        for name in chars {
            let char_dir = config_dir.join("characters").join(name);
            std::fs::create_dir_all(&char_dir).unwrap();
            std::fs::write(
                char_dir.join("character.md"),
                format!("You are {name}. Keep responses very short."),
            )
            .unwrap();
        }

        let (push_tx, push_rx) = broadcast::channel(64);

        let mut app_config = shore_config::app::AppConfig::default();
        app_config.defaults.model = Some("test".into());
        // Disable tool_use to keep the pipeline simple (no tool loop).
        app_config.behavior.tool_use.enabled = false;

        let loaded_config = shore_config::LoadedConfig::new_for_test(
            app_config,
            models,
            shore_config::ShoreDirs {
                config: config_dir.clone(),
                data: data_dir.clone(),
                runtime: tmp.path().join("runtime"),
            },
        );

        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(());
        let (autonomy, _compaction_rx) = AutonomyManager::new(
            Default::default(),
            Default::default(),
            data_dir.clone(),
            shutdown_rx,
        );

        let registry = CharacterRegistry::new(
            config_dir,
            data_dir.clone(),
            push_tx.clone(),
            loaded_config.clone(),
        );

        let ledger_client = shore_ledger::LedgerClient::new(
            shore_llm_client::LlmClient::new(),
            &data_dir.join("ledger.db"),
        )
        .unwrap();

        let cmd_ctx = CommandContext {
            config: loaded_config.clone(),
            push_tx: push_tx.clone(),
            data_dir: data_dir.clone(),
            active_model: None,
            session_tokens: Arc::new(std::sync::Mutex::new(SessionTokens::default())),
            autonomy: autonomy.clone(),
            llm_client: ledger_client.clone(),
            diagnostics: Arc::new(std::sync::Mutex::new(
                shore_diagnostics::Diagnostics::default(),
            )),
            memory_shell_sessions: std::collections::HashMap::new(),
        };

        let handler = MessageHandler {
            registry: Arc::new(Mutex::new(registry)),
            cmd_ctx,
            llm_client: ledger_client,
            push_tx: push_tx.clone(),
            is_first_after_restart: Arc::new(AtomicBool::new(false)),
            has_seen_cache_read: Arc::new(AtomicBool::new(false)),
            compaction_occurred: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            autonomy,
            notifier: NotificationService::new(Default::default()),
            generation_handle: None,
        };

        (handler, push_rx)
    }

    /// End-to-end pipeline: user message → prompt → LLM stream → persist.
    ///
    /// Uses a real ConversationEngine, real prompt assembly, and a mock HTTP
    /// server returning canned Anthropic SSE. Verifies that both the user
    /// message and the assistant response are persisted to the engine.
    ///
    /// Requires ANTHROPIC_API_KEY in env (LlmClient validates on construction).
    /// Run with: `cargo test --lib -- --ignored pipeline_user_message`
    #[tokio::test]
    #[ignore]
    async fn pipeline_user_message_to_persisted_response() {
        let (base_url, _server) =
            mock_sse_server(sse_text_response("Hello from the mock LLM!")).await;
        let models = mock_model_catalog(&base_url);

        let tmp = TempDir::new().unwrap();
        let (handler, mut push_rx) = make_handler_with_models(&tmp, &["Alice"], models);

        // Resolve character and config (same steps the handler loop takes).
        let (char_name, effective_config) = {
            let mut registry = handler.registry.lock().await;
            let char_name = registry.resolve_character(Some("Alice")).unwrap();
            let effective_config = registry.effective_config(&char_name).clone();
            (char_name, effective_config)
        };

        let body = ClientMessageBody {
            rid: Some("test-rid".into()),
            text: "Hello, Alice!".into(),
            stream: true,
            images: vec![],
            image_data: vec![],
            absence_seconds: None,
            overrides: None,
        };

        let gen = handler.gen_context();
        let data_dir = handler.cmd_ctx.data_dir.clone();

        // Run the full pipeline.
        let result = handle_generation(
            gen,
            GenerationParams {
                body,
                regen: false,
                char_name: char_name.clone(),
                rid: Some("test-rid".into()),
                effective_config,
                data_dir: data_dir.clone(),
                active_model: None,
            },
        )
        .await;

        assert!(
            result.is_ok(),
            "Pipeline should succeed: {:?}",
            result.err()
        );

        // Verify: messages are persisted in the engine.
        let engine_arc = {
            let mut registry = handler.registry.lock().await;
            registry.get_or_create(&char_name).unwrap()
        };
        let engine = engine_arc.lock().await;
        let messages = engine.messages();
        assert_eq!(
            messages.len(),
            2,
            "Should have user + assistant messages, got {}",
            messages.len()
        );
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].content, "Hello, Alice!");
        assert_eq!(messages[1].role, Role::Assistant);
        assert!(
            messages[1].content.contains("Hello from the mock LLM!"),
            "Assistant content should contain mock response, got: {}",
            messages[1].content
        );

        // Verify: active.jsonl was written to disk.
        let active_path = data_dir.join(&char_name).join("active.jsonl");
        assert!(active_path.exists(), "active.jsonl should exist");
        let line_count = std::fs::read_to_string(&active_path)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .count();
        assert_eq!(
            line_count, 2,
            "active.jsonl should have 2 lines (user + assistant)"
        );

        // Verify: broadcast events were sent (NewMessage for user message at minimum).
        let mut saw_new_message = false;
        while let Ok(msg) = push_rx.try_recv() {
            if matches!(msg, ServerMessage::NewMessage(_)) {
                saw_new_message = true;
            }
        }
        assert!(
            saw_new_message,
            "Should have broadcast at least one NewMessage"
        );
    }
}
