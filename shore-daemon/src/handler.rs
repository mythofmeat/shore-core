//! Message processing handler.
//!
//! Consumes routed messages from the SWP server and orchestrates the
//! engine → prompt → LLM → tool loop → persist pipeline.
//!
//! Generation (Message/Regen) runs in spawned tokio tasks so the handler loop
//! never blocks on LLM streaming. Commands (status, log, etc.) are processed
//! inline and always return immediately.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine as _;
use serde_json::{json, Value};
use shore_protocol::client_msg::{ClientMessage, ClientMessageBody};
use shore_protocol::error::ErrorCode;
use shore_protocol::server_msg::{Error as SwpError, ServerMessage};
use shore_protocol::types::{derive_content_from_blocks, ContentBlock, ImageRef, Message, Role};
use tokio::sync::{broadcast, Mutex};
use tracing::{error, info, instrument, warn};

use crate::autonomy::cache_keepalive::CacheKeepaliveConfig;
use crate::autonomy::manager::AutonomyManager;
use crate::characters::CharacterRegistry;
use crate::commands::{self, CommandContext, SessionTokens};
use crate::engine::prompt::{self, CapabilitiesConfig, PromptParams};
use crate::engine::tools;
use crate::memory::agent::{AgentError, AgentIndexer, AgentRag, AgentSearchContext, CallerIdentity, MemoryAgent, RagHit};
use crate::memory::agent_llm::{AgentLlm, RealAgentLlm};
use crate::memory::compaction_impls::resolve_embed_config;
use crate::memory::db::MemoryDB;
use crate::memory::researcher::MemoryResearcher;
use crate::memory::vectorstore::VectorStore;
use crate::tools::{self as tool_system, ToolContext};
use shore_llm_client::retry::{self, RetryDecision, RetryPolicy};
use shore_llm_client::stream::{CacheContext, StreamConsumer};
use shore_llm_client::LlmClient;
use crate::notifications::{NotificationEvent, NotificationService};
use shore_config::app::SearchConfig;
use shore_config::LoadedConfig;
use crate::memory::compaction_impls::ImageGenConfig;
use crate::server::RoutedMessage;

// ── NoopRag stub (legacy, needed by image tools via ToolContext) ──────

struct NoopRag;

impl AgentRag for NoopRag {
    fn query(
        &self,
        _query: &str,
        _top_k: usize,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<RagHit>, AgentError>> + Send + '_>,
    > {
        Box::pin(async { Ok(vec![]) })
    }
}

// ── Per-request tool context (owns all memory dependencies) ──────────

struct HandlerToolContext {
    db: MemoryDB,
    agent: MemoryAgent,
    agent_llm: RealAgentLlm,
    agent_model_val: shore_config::models::ResolvedModel,
    researcher: Option<MemoryResearcher>,
    researcher_llm_val: Option<RealAgentLlm>,
    researcher_model_val: Option<shore_config::models::ResolvedModel>,
    rag: NoopRag,
    search_ctx: Option<AgentSearchContext>,
    image_dir_val: String,
    llm_client_val: LlmClient,
    image_gen_config_val: Option<ImageGenConfig>,
    search_config_val: SearchConfig,
    autonomy_val: AutonomyManager,
    character_name_val: String,
    scratchpad_dir_val: String,
}

impl ToolContext for HandlerToolContext {
    fn memory_db(&self) -> &MemoryDB { &self.db }
    fn memory_agent(&self) -> &MemoryAgent { &self.agent }
    fn agent_llm(&self) -> &dyn AgentLlm { &self.agent_llm }
    fn agent_model(&self) -> &shore_config::models::ResolvedModel { &self.agent_model_val }
    fn researcher_llm(&self) -> Option<&dyn AgentLlm> {
        self.researcher_llm_val.as_ref().map(|l| l as &dyn AgentLlm)
    }
    fn researcher_model(&self) -> Option<&shore_config::models::ResolvedModel> {
        self.researcher_model_val.as_ref()
    }
    fn memory_researcher(&self) -> Option<&MemoryResearcher> {
        self.researcher.as_ref()
    }
    fn indexer(&self) -> Option<&dyn AgentIndexer> { None }
    fn search_context(&self) -> Option<&AgentSearchContext> { self.search_ctx.as_ref() }
    fn rag(&self) -> &dyn AgentRag { &self.rag }
    fn image_dir(&self) -> &str { &self.image_dir_val }
    fn llm_client(&self) -> Option<&LlmClient> { Some(&self.llm_client_val) }
    fn image_gen_config(&self) -> Option<&ImageGenConfig> { self.image_gen_config_val.as_ref() }
    fn search_config(&self) -> &SearchConfig { &self.search_config_val }
    fn autonomy_manager(&self) -> Option<&AutonomyManager> { Some(&self.autonomy_val) }
    fn character_name(&self) -> &str { &self.character_name_val }
    fn scratchpad_dir(&self) -> &str { &self.scratchpad_dir_val }
}

// ── Shared context for spawned generation tasks ───────────────────────

/// All state needed by a generation task. Cheap to clone (all Arc-backed).
#[derive(Clone)]
struct GenContext {
    registry: Arc<Mutex<CharacterRegistry>>,
    llm_client: LlmClient,
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

// ── MessageHandler ────────────────────────────────────────────────────

/// The message processing handler.
///
/// Routes commands inline (fast path) and spawns tokio tasks for generation
/// (Message/Regen), so the handler loop is never blocked by LLM streaming.
pub struct MessageHandler {
    pub registry: Arc<Mutex<CharacterRegistry>>,
    pub cmd_ctx: CommandContext,
    pub llm_client: LlmClient,
    pub push_tx: broadcast::Sender<ServerMessage>,
    pub is_first_after_restart: Arc<AtomicBool>,
    pub has_seen_cache_read: Arc<AtomicBool>,
    pub compaction_occurred: Arc<std::sync::atomic::AtomicBool>,
    pub autonomy: AutonomyManager,
    pub notifier: NotificationService,
}

// ---------------------------------------------------------------------------
// Image → LLM content helpers
// ---------------------------------------------------------------------------

/// Detect MIME type from file extension.
fn media_type_for_path(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Build a `content` value for an LLM message.
///
/// If `images` is non-empty, returns a JSON array containing image blocks
/// (base64-encoded) followed by a text block. Otherwise returns a plain string.
fn build_content(text: &str, images: &[ImageRef]) -> Value {
    if images.is_empty() {
        return json!(text);
    }

    let mut blocks: Vec<Value> = Vec::with_capacity(images.len() + 1);

    for img in images {
        let media_type = match media_type_for_path(&img.path) {
            Some(mt) => mt,
            None => {
                warn!(path = %img.path, "Skipping image with unsupported extension");
                continue;
            }
        };
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
                warn!(path = %img.path, error = %e, "Failed to read image file");
            }
        }
    }

    blocks.push(json!({ "type": "text", "text": text }));
    json!(blocks)
}

impl MessageHandler {
    /// Run the message processing loop. Blocks until the route channel closes.
    ///
    /// Commands are processed inline (no LLM I/O, always fast).
    /// Engine messages (Message/Regen) are spawned as independent tokio tasks,
    /// so this loop never blocks on LLM streaming.
    pub async fn run(
        &mut self,
        route_rx: Arc<Mutex<tokio::sync::mpsc::Receiver<RoutedMessage>>>,
    ) {
        let mut rx = route_rx.lock().await;
        while let Some(msg) = rx.recv().await {
            match msg {
                RoutedMessage::Command { cmd, character } => {
                    let result = self.dispatch_command(&cmd, character.as_deref()).await;
                    let _ = self.push_tx.send(result);
                }
                RoutedMessage::Engine { msg, character } => {
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
                                absence_seconds: None,
                                overrides: None,
                            };
                            (body, true)
                        }
                        _ => continue,
                    };

                    let rid = body.rid.clone();
                    let gen = self.gen_context();
                    let data_dir = self.cmd_ctx.data_dir.clone();
                    let active_model = self.cmd_ctx.active_model.clone();
                    let push_tx = self.push_tx.clone();
                    let notifier = self.notifier.clone();

                    tokio::spawn(async move {
                        if let Err(e) = handle_generation(
                            gen, body, regen, char_name, rid,
                            effective_config, data_dir, active_model,
                        ).await {
                            error!(error = %e, "Error processing engine message");
                            let err_msg = e.to_string();
                            let _ = push_tx.send(ServerMessage::Error(SwpError {
                                code: ErrorCode::InternalError,
                                message: err_msg.clone(),
                            }));
                            notifier.notify(NotificationEvent::Error, "Shore — Error", &err_msg);
                        }
                    });
                }
            }
        }
        info!("Message handler shutting down (route channel closed)");
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
        // list_characters doesn't need a resolved character — handle it
        // before character resolution so it works when multiple characters
        // are available and none is explicitly selected.
        if cmd.name == "list_characters" {
            return match commands::dispatch_characterless(&self.cmd_ctx, cmd) {
                Ok(data) => ServerMessage::CommandOutput(shore_protocol::server_msg::CommandOutput {
                    name: cmd.name.clone(),
                    data,
                }),
                Err((code, msg)) => ServerMessage::Error(SwpError {
                    code,
                    message: msg,
                }),
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

#[instrument(skip(ctx, body), fields(char = %char_name, rid = rid.as_deref().unwrap_or("-")))]
async fn handle_generation(
    ctx: GenContext,
    body: ClientMessageBody,
    regen: bool,
    char_name: String,
    rid: Option<String>,
    effective_config: LoadedConfig,
    data_dir: PathBuf,
    active_model: Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Get engine Arc from registry (brief lock — registry released immediately after).
    let engine_arc = {
        let mut registry = ctx.registry.lock().await;
        registry.get_or_create(&char_name).map_err(|e| e.to_string())?
    };

    // 1. Append user message or truncate after last user turn for regen.
    {
        let mut engine = engine_arc.lock().await;
        if regen {
            engine.truncate_after_last_user_turn()?;
        } else if !body.text.is_empty() || !body.images.is_empty() {
            let images: Vec<ImageRef> = body.images.iter()
                .map(|p| ImageRef { path: p.clone(), caption: None })
                .collect();
            let user_msg = Message {
                msg_id: format!("m_{}", uuid::Uuid::new_v4()),
                role: Role::User,
                content: body.text.clone(),
                images,
                content_blocks: vec![ContentBlock::Text { text: body.text.clone() }],
                alt_index: None,
                alt_count: None,
                timestamp: chrono::Utc::now().to_rfc3339(),
            };
            engine.append_message(user_msg.clone())?;
            let _ = ctx.push_tx.send(ServerMessage::NewMessage(
                shore_protocol::server_msg::NewMessage { message: user_msg },
            ));
        }
    } // engine lock released

    // 2. Resolve model.
    let model_name = active_model.as_deref()
        .or(effective_config.app.defaults.model.as_deref());
    let resolved = match model_name {
        Some(name) => effective_config.models.find_model(name).map_err(|e| e.to_string())?,
        None => effective_config.models.first_chat_model().ok_or("No model configured")?,
    };

    // 3. Resolve memory agent and researcher models.
    let agent_model = effective_config.app.defaults.memory_agent.as_deref()
        .and_then(|name| effective_config.models.find_model(name).ok())
        .unwrap_or(resolved)
        .clone();

    let researcher_model = effective_config.app.defaults.tool_model.as_deref()
        .and_then(|name| effective_config.models.find_model(name).ok())
        .cloned();

    // 4. Ensure autonomy state with model-specific keepalive config.
    // Must happen before notify_user_message so session_start is set on first message.
    let keepalive_cfg = CacheKeepaliveConfig::from_resolved_model(
        &resolved.provider_key,
        resolved.cache_ttl.is_some(),
        resolved.keepalive_enabled,
        resolved.keepalive_ttl_minutes,
        resolved.cache_ttl.as_deref(),
        resolved.keepalive_max_pings,
    );
    ctx.autonomy.ensure_state_with_config(&char_name, keepalive_cfg, Some(&effective_config));

    if !regen && (!body.text.is_empty() || !body.images.is_empty()) {
        let turn_count = engine_arc.lock().await.turn_count();
        ctx.autonomy.notify_user_message(&char_name, turn_count);
    }

    // 5. Load character and user definitions (brief registry lock).
    let (character_definition, user_definition) = {
        let registry = ctx.registry.lock().await;
        (registry.character_definition(&char_name), registry.user_definition(&char_name))
    };

    // 6. Read messages for prompt assembly (brief engine lock, then clone).
    let messages = engine_arc.lock().await.messages().to_vec();

    let character_data_dir = data_dir.join(&char_name);
    let display_name = effective_config.app.defaults.resolve_display_name();
    let tool_toggles = &effective_config.app.behavior.tool_use.tools;
    let capabilities = CapabilitiesConfig {
        interiority_enabled: effective_config.app.behavior.autonomy.interiority.enabled,
        scratchpad_enabled: tool_toggles.scratchpad_read || tool_toggles.scratchpad_write,
        memory_enabled: tool_toggles.memory,
        image_memory_enabled: effective_config.app.memory.image_enabled,
        send_image_enabled: tool_toggles.send_image,
        generate_image_enabled: tool_toggles.generate_image,
        web_search_enabled: tool_toggles.web_search,
        activity_heatmap_enabled: tool_toggles.activity_heatmap,
        roll_dice_enabled: tool_toggles.roll_dice,
        check_time_enabled: tool_toggles.check_time,
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

    // 7. Build LLM messages from assembled prompt.
    //
    // All content blocks are sent intact — the Anthropic API handles
    // thinking block stripping for prior turns internally.
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
                let blocks: Vec<Value> = m.content_blocks.iter().filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(json!({ "type": "text", "text": text })),
                    ContentBlock::Thinking { thinking, signature } => {
                        signature.as_ref().map(|sig| {
                            json!({ "type": "thinking", "thinking": thinking, "signature": sig })
                        })
                    }
                    ContentBlock::RedactedThinking { data } => {
                        Some(json!({ "type": "redacted_thinking", "data": data }))
                    }
                    ContentBlock::ToolUse { id, name, input } => Some(json!({
                        "type": "tool_use", "id": id, "name": name, "input": input,
                    })),
                    ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                        let mut v = json!({
                            "type": "tool_result", "tool_use_id": tool_use_id, "content": content,
                        });
                        if *is_error {
                            v["is_error"] = json!(true);
                        }
                        Some(v)
                    }
                }).collect();
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
        Some(json!(prompt_result.system.iter().map(|b| {
            json!({"type": "text", "text": b.content})
        }).collect::<Vec<_>>()))
    };

    // 8. Build tool definitions from unified tool system.
    let tool_defs = if effective_config.app.behavior.tool_use.enabled {
        let toggles = &effective_config.app.behavior.tool_use.tools;
        let defs: Vec<Value> = tool_system::available_tools(false, toggles)
            .iter()
            .map(|t| json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters.clone(),
            }))
            .collect();
        Some(defs)
    } else {
        None
    };

    // 9. Build LLM request.
    let mut request =
        LlmClient::build_request(&resolved, llm_messages, system, tool_defs, None)?;

    // Apply per-message parameter overrides from the client.
    if let Some(ref ov) = body.overrides {
        if let Some(t) = ov.temperature {
            request.temperature = Some(t);
        }
        if let Some(p) = ov.top_p {
            request.top_p = Some(p);
        }
        if let Some(budget) = ov.thinking_budget {
            let opts = request.provider_options.get_or_insert_with(|| {
                serde_json::Value::Object(serde_json::Map::new())
            });
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

    // 10. Stream response from shore-llm (with retry on transient errors).
    let retry_policy = RetryPolicy {
        max_retries: effective_config.app.advanced.max_retries
            .unwrap_or(RetryPolicy::default().max_retries),
        ..RetryPolicy::default()
    };
    let mut attempt: u32 = 0;
    let mut result;

    loop {
        let consumer = StreamConsumer::new(ctx.push_tx.clone());

        let stream_result = async {
            let mut reader = ctx.llm_client.stream_raw(&request, rid.as_deref()).await?;

            let turn_count = engine_arc.lock().await.messages().len();
            let cache_warnings = resolved.provider_key == "anthropic"
                && effective_config.app.advanced.cache_invalidation_warnings;
            let is_first_after_compaction = ctx.compaction_occurred.swap(false, Ordering::AcqRel);
            let cache_ctx = CacheContext {
                conversation_turn_count: turn_count,
                is_first_after_restart: ctx.is_first_after_restart.load(Ordering::Acquire),
                is_first_after_compaction,
                cache_invalidation_warnings: cache_warnings,
                has_seen_cache_read: ctx.has_seen_cache_read.load(Ordering::Acquire),
            };

            consumer.consume(&mut reader, regen, &cache_ctx).await
        }
        .await;

        match stream_result {
            Ok(r) => {
                if r.usage.cache_read_tokens > 0 {
                    ctx.has_seen_cache_read.store(true, Ordering::Release);
                }
                result = r;
                break;
            }
            Err(e) => {
                match retry::should_retry_error(&e, attempt, &retry_policy) {
                    RetryDecision::Retry => {
                        let base_ms = effective_config.app.advanced.retry_backoff_seconds
                            .map(|s| (s * 1000.0) as u64)
                            .unwrap_or(500);
                        let delay = std::time::Duration::from_millis(base_ms * 2u64.pow(attempt));
                        warn!(attempt, delay_ms = delay.as_millis() as u64, "Retrying after transient LLM error");
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                    }
                    RetryDecision::FallbackModel(_model) => {
                        return Err(e.into());
                    }
                    RetryDecision::Fail => return Err(e.into()),
                }
            }
        }
    }

    // Build cache context for tool loop.
    let tool_cache_warnings = resolved.provider_key == "anthropic"
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
    let mut tool_intermediate_messages: Vec<Message> = Vec::new();

    if result.finish_reason == "tool_use"
        && effective_config.app.behavior.tool_use.enabled
    {
        let db_path = data_dir
            .join(&char_name)
            .join("memory")
            .join("memory.db");
        let memory_db = MemoryDB::open(&db_path)
            .map_err(|e| format!("failed to open memory DB: {e}"))?;

        let char_def = character_definition.clone().unwrap_or_default();
        let user_def = user_definition.clone().unwrap_or_default();

        let image_gen_config = crate::memory::compaction_impls::resolve_image_gen_config(
            effective_config.app.defaults.image_generation.as_deref(),
            &effective_config.models.image_generation,
        ).ok();

        // Build semantic search context (graceful: None if no embedding model configured).
        let search_ctx = match resolve_embed_config(
            effective_config.app.defaults.embedding.as_deref(),
            &effective_config.models.embedding,
        ) {
            Ok(embed_config) => {
                let vs_path = data_dir
                    .join(&char_name)
                    .join("memory")
                    .join("vectorstore");
                match VectorStore::open(&vs_path, embed_config.dimensions).await {
                    Ok(vs) => Some(AgentSearchContext::new(vs, ctx.llm_client.clone(), embed_config)),
                    Err(e) => {
                        warn!("Failed to open vector store for semantic search: {e}");
                        None
                    }
                }
            }
            Err(_) => None, // No embedding model configured — semantic search unavailable.
        };

        let tool_ctx = HandlerToolContext {
            db: memory_db,
            agent: MemoryAgent::one_shot(
                CallerIdentity::Char,
                &char_name,
                &effective_config.app.defaults.resolve_display_name(),
            ),
            agent_llm: RealAgentLlm::new(ctx.llm_client.clone()),
            agent_model_val: agent_model.clone(),
            researcher: researcher_model.as_ref().map(|_| {
                MemoryResearcher::new(char_def, user_def)
            }),
            researcher_llm_val: researcher_model.as_ref().map(|_| {
                RealAgentLlm::new(ctx.llm_client.clone())
            }),
            researcher_model_val: researcher_model.clone(),
            rag: NoopRag,
            search_ctx,
            image_dir_val: data_dir
                .join(&char_name)
                .join("images")
                .to_string_lossy()
                .into_owned(),
            llm_client_val: ctx.llm_client.clone(),
            image_gen_config_val: image_gen_config,
            search_config_val: effective_config.app.behavior.tool_use.search.clone(),
            autonomy_val: ctx.autonomy.clone(),
            character_name_val: char_name.clone(),
            scratchpad_dir_val: data_dir
                .join(&char_name)
                .join("scratchpad")
                .to_string_lossy()
                .into_owned(),
        };

        let tool_loop_result = tools::run_tool_loop(
            &ctx.llm_client,
            &ctx.push_tx,
            &mut request,
            result,
            &tool_ctx,
            effective_config.app.behavior.tool_use.max_iterations,
            &cache_ctx,
            &ctx.diagnostics,
        )
        .await?;

        result = tool_loop_result.result;
        tool_intermediate_messages = tool_loop_result.intermediate_messages;
    }

    // 12. Persist intermediate tool messages and final assistant message.
    {
        let mut engine = engine_arc.lock().await;

        for msg in tool_intermediate_messages {
            engine.append_message(msg)?;
        }

        // Track cumulative token usage.
        {
            let mut tokens = ctx.session_tokens.lock().unwrap();
            tokens.input += result.usage.input_tokens;
            tokens.output += result.usage.output_tokens;
            tokens.cache_read += result.usage.cache_read_tokens;
            tokens.cache_write += result.usage.cache_creation_tokens;
        }

        // Record API call in diagnostics ring buffer.
        {
            let entry = shore_diagnostics::ApiCallEntry {
                timestamp: chrono::Utc::now().to_rfc3339(),
                model: result.model.clone(),
                provider: resolved.provider_key.clone(),
                input_tokens: result.usage.input_tokens,
                output_tokens: result.usage.output_tokens,
                cache_read_tokens: result.usage.cache_read_tokens,
                cache_write_tokens: result.usage.cache_creation_tokens,
                ttft_ms: result.timing.time_to_first_token_ms,
                total_ms: result.timing.total_ms,
                finish_reason: result.finish_reason.clone(),
                error: None,
            };
            ctx.diagnostics.lock().unwrap().api_calls.push(entry);
        }

        // Notify cache keepalive of API response.
        ctx.autonomy.notify_api_response(
            &char_name,
            result.usage.cache_read_tokens,
            result.usage.input_tokens,
        );
        ctx.autonomy.notify_last_request(&char_name, request.clone());

        info!(
            input_tokens = result.usage.input_tokens,
            output_tokens = result.usage.output_tokens,
            model = %result.model,
            "Response complete"
        );

        let content_blocks = if result.content_blocks.is_empty() && !result.content.is_empty() {
            vec![ContentBlock::Text { text: result.content.clone() }]
        } else {
            result.content_blocks.clone()
        };
        let content = derive_content_from_blocks(&content_blocks);
        let assistant_msg = Message {
            msg_id: format!("m_{}", uuid::Uuid::new_v4()),
            role: Role::Assistant,
            content,
            images: vec![],
            content_blocks,
            alt_index: None,
            alt_count: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        engine.append_message(assistant_msg)?;
        ctx.autonomy.notify_assistant_message(&char_name, engine.turn_count());
    } // engine lock released

    ctx.notifier.notify_message_complete(
        &format!("Shore — {char_name}"),
        &format!("Response complete ({:.1}s)", result.timing.total_ms as f64 / 1000.0),
        result.timing.total_ms,
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use shore_protocol::client_msg::{Command, Regen};
    use shore_protocol::error::ErrorCode;
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

        let registry = CharacterRegistry::new(config_dir, data_dir.clone(), push_tx.clone(), loaded_config.clone());

        let cmd_ctx = CommandContext {
            config: loaded_config.clone(),
            push_tx: push_tx.clone(),
            data_dir: data_dir.clone(),
            active_model: None,
            session_tokens: Arc::new(std::sync::Mutex::new(SessionTokens::default())),
            autonomy: autonomy.clone(),
            llm_client: LlmClient::new(),
            diagnostics: Arc::new(std::sync::Mutex::new(shore_diagnostics::Diagnostics::default())),
            memory_shell_sessions: std::collections::HashMap::new(),
        };

        let handler = MessageHandler {
            registry: Arc::new(Mutex::new(registry)),
            cmd_ctx,
            llm_client: LlmClient::new(),
            push_tx: push_tx.clone(),
            is_first_after_restart: Arc::new(AtomicBool::new(false)),
            has_seen_cache_read: Arc::new(AtomicBool::new(false)),
            compaction_occurred: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            autonomy,
            notifier: NotificationService::new(Default::default()),
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
        let (mut handler, _rx) = make_handler(&tmp, &["Alice"]);

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
            gen, body, is_regen, char_name, None, effective_config, data_dir, None,
        ).await;

        assert!(result.is_err(), "Expected error due to no model configured");
    }
}
