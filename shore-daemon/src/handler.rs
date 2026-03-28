//! Message processing handler.
//!
//! Consumes routed messages from the SWP server and orchestrates the
//! engine → prompt → LLM → tool loop → persist pipeline.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use shore_protocol::client_msg::{ClientMessage, ClientMessageBody};
use shore_protocol::error::ErrorCode;
use shore_protocol::server_msg::{Error as SwpError, ServerMessage};
use shore_protocol::types::{ContentBlock, ImageRef, Message, Role};
use tokio::sync::{broadcast, Mutex};
use tracing::{error, info, instrument, warn};

use crate::autonomy::cache_keepalive::CacheKeepaliveConfig;
use crate::autonomy::manager::AutonomyManager;
use crate::characters::CharacterRegistry;
use crate::commands::{self, CommandContext};
use crate::engine::prompt::{self, CapabilitiesConfig, PromptParams};
use crate::engine::tools;
use crate::memory::agent::{AgentError, AgentIndexer, AgentRag, CallerIdentity, MemoryAgent, RagHit};
use crate::memory::agent_llm::{AgentLlm, RealAgentLlm};
use crate::memory::db::MemoryDB;
use crate::memory::researcher::MemoryResearcher;
use crate::tools::{self as tool_system, ToolContext};
use shore_llm_client::retry::{self, RetryDecision, RetryPolicy};
use shore_llm_client::stream::{CacheContext, StreamConsumer};
use shore_llm_client::LlmClient;
use crate::notifications::{NotificationEvent, NotificationService};
use shore_config::app::SearchConfig;
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
    image_dir_val: String,
    llm_client_val: LlmClient,
    image_gen_config_val: Option<ImageGenConfig>,
    search_config_val: SearchConfig,
    autonomy_val: AutonomyManager,
    character_name_val: String,
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
    fn rag(&self) -> &dyn AgentRag { &self.rag }
    fn image_dir(&self) -> &str { &self.image_dir_val }
    fn llm_client(&self) -> Option<&LlmClient> { Some(&self.llm_client_val) }
    fn image_gen_config(&self) -> Option<&ImageGenConfig> { self.image_gen_config_val.as_ref() }
    fn search_config(&self) -> &SearchConfig { &self.search_config_val }
    fn autonomy_manager(&self) -> Option<&AutonomyManager> { Some(&self.autonomy_val) }
    fn character_name(&self) -> &str { &self.character_name_val }
}

/// The message processing handler.
///
/// Consumes routed messages from the server and orchestrates the full
/// message → LLM → response pipeline.
pub struct MessageHandler {
    pub registry: CharacterRegistry,
    pub cmd_ctx: CommandContext,
    pub llm_client: LlmClient,
    pub push_tx: broadcast::Sender<ServerMessage>,
    pub is_first_after_restart: bool,
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
                    if let Err(e) = self.handle_engine_message(msg, character.as_deref()).await {
                        error!(error = %e, "Error processing engine message");
                        let err_msg = e.to_string();
                        let _ = self.push_tx.send(ServerMessage::Error(SwpError {
                            code: ErrorCode::InternalError,
                            message: err_msg.clone(),
                        }));
                        self.notifier.notify(
                            NotificationEvent::Error,
                            "Shore — Error",
                            &err_msg,
                        );
                    }
                }
            }
        }
        info!("Message handler shutting down (route channel closed)");
    }

    /// Resolve the engine for a character and dispatch a command.
    async fn dispatch_command(
        &mut self,
        cmd: &shore_protocol::client_msg::Command,
        character: Option<&str>,
    ) -> ServerMessage {
        // Resolve character and get engine.
        let char_name = match self.registry.resolve_character(character) {
            Ok(name) => name,
            Err(e) => {
                return ServerMessage::Error(SwpError {
                    code: ErrorCode::InvalidRequest,
                    message: e.to_string(),
                });
            }
        };

        // Swap in per-character effective config for the duration of this dispatch.
        let effective = self.registry.effective_config(&char_name).clone();
        let original = std::mem::replace(&mut self.cmd_ctx.config, effective);

        let engine = match self.registry.get_or_create(&char_name) {
            Ok(engine) => engine,
            Err(e) => {
                self.cmd_ctx.config = original;
                return ServerMessage::Error(SwpError {
                    code: ErrorCode::InternalError,
                    message: e.to_string(),
                });
            }
        };

        let result = commands::dispatch(engine, &mut self.cmd_ctx, cmd).await;

        // config_reset reloads the global config — keep the new value and
        // invalidate the per-character cache so future lookups re-merge.
        if cmd.name == "config_reset" {
            self.registry.set_global_config(self.cmd_ctx.config.clone());
        } else {
            self.cmd_ctx.config = original;
        }

        result
    }

    async fn handle_engine_message(
        &mut self,
        msg: ClientMessage,
        character: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match msg {
            ClientMessage::Message(body) => {
                self.handle_user_message(body, false, character).await
            }
            ClientMessage::Regen(regen) => {
                // Regen re-generates the last assistant response.
                let body = ClientMessageBody {
                    rid: regen.rid,
                    text: String::new(),
                    stream: regen.stream,
                    images: vec![],
                    absence_seconds: None,
                    overrides: None,
                };
                self.handle_user_message(body, true, character).await
            }
            _ => Ok(()),
        }
    }

    #[instrument(skip(self, body), fields(rid = body.rid.as_deref().unwrap_or("-")))]
    async fn handle_user_message(
        &mut self,
        body: ClientMessageBody,
        regen: bool,
        character: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let rid = body.rid.clone();

        // Resolve character.
        let char_name = self.registry.resolve_character(character)
            .map_err(|e| e.to_string())?;

        // Swap in per-character effective config for the duration of this message.
        let effective = self.registry.effective_config(&char_name).clone();
        let original = std::mem::replace(&mut self.cmd_ctx.config, effective);
        // Use a closure-like pattern: restore on all exit paths.
        let result = self.handle_user_message_inner(body, regen, &char_name, rid).await;
        self.cmd_ctx.config = original;
        result
    }

    async fn handle_user_message_inner(
        &mut self,
        body: ClientMessageBody,
        regen: bool,
        char_name: &str,
        rid: Option<String>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        {
            // Get engine for this character (scoped borrow).
            let engine = self.registry.get_or_create(&char_name)
                .map_err(|e| e.to_string())?;

            // 1. Append user message (unless regen).
            if regen {
                // Remove the last assistant message so the LLM regenerates it.
                let msgs = engine.messages();
                if let Some(last) = msgs.last() {
                    if last.role == Role::Assistant {
                        let id = last.msg_id.clone();
                        engine.delete_message(&id)?;
                    }
                }
            } else if !body.text.is_empty() || !body.images.is_empty() {
                let images: Vec<ImageRef> = body
                    .images
                    .iter()
                    .map(|p| ImageRef { path: p.clone(), caption: None })
                    .collect();
                let user_msg = Message {
                    msg_id: format!("m_{}", uuid::Uuid::new_v4()),
                    role: Role::User,
                    content: body.text.clone(),
                    images,
                    content_blocks: vec![],
                    alt_index: None,
                    alt_count: None,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                };
                engine.append_message(user_msg.clone())?;

                // Broadcast user message so follow-mode clients see it.
                let _ = self.push_tx.send(ServerMessage::NewMessage(
                    shore_protocol::server_msg::NewMessage { message: user_msg },
                ));
            }
        }

        // 2. Ensure autonomy state and resolve model.
        let model_name = self
            .cmd_ctx
            .active_model
            .as_deref()
            .or(self.cmd_ctx.config.app.defaults.model.as_deref());

        let resolved = match model_name {
            Some(name) => self
                .cmd_ctx
                .config
                .models
                .find_model(name)
                .map_err(|e| e.to_string())?,
            None => self
                .cmd_ctx
                .config
                .models
                .first_chat_model()
                .ok_or("No model configured")?,
        };

        // 3. Resolve memory agent and researcher models.
        let agent_model = self.cmd_ctx.config.app.defaults.memory_agent.as_deref()
            .and_then(|name| self.cmd_ctx.config.models.find_model(name).ok())
            .unwrap_or(resolved)
            .clone();

        let researcher_model = self.cmd_ctx.config.app.defaults.tool_model.as_deref()
            .and_then(|name| self.cmd_ctx.config.models.find_model(name).ok())
            .cloned();

        // Ensure autonomy state exists with model-specific keepalive config.
        // Must happen before notify_user_message so session_start is set on first message.
        let keepalive_cfg = CacheKeepaliveConfig::from_resolved_model(
            &resolved.provider_key,
            resolved.cache_ttl.is_some(),
            resolved.keepalive_enabled,
            resolved.keepalive_ttl_minutes,
            resolved.keepalive_max_pings,
        );
        self.autonomy.ensure_state_with_config(
            char_name,
            keepalive_cfg,
            Some(&self.cmd_ctx.config),
        );

        // Notify autonomy of the user message (must be after ensure_state).
        if !regen && (!body.text.is_empty() || !body.images.is_empty()) {
            let engine = self.registry.get_or_create(&char_name)
                .map_err(|e| e.to_string())?;
            self.autonomy.notify_user_message(&char_name, engine.message_count());
        }

        // 4. Assemble prompt.
        // Load definitions before borrowing engine (avoids borrow conflicts).
        let character_definition = self.registry.character_definition(&char_name);
        let user_definition = self.registry.user_definition(&char_name);

        let engine = self.registry.get_or_create(&char_name)
            .map_err(|e| e.to_string())?;

        let messages = engine.messages();
        let character_data_dir = self
            .cmd_ctx
            .data_dir
            .join(engine.character_name());

        let display_name = self.cmd_ctx.config.app.defaults.resolve_display_name();
        let tool_toggles = &self.cmd_ctx.config.app.behavior.tool_use.tools;
        let capabilities = CapabilitiesConfig {
            heartbeat_enabled: self.cmd_ctx.config.app.behavior.autonomy.heartbeat.enabled,
            memory_enabled: tool_toggles.memory,
            image_memory_enabled: self.cmd_ctx.config.app.memory.image_enabled,
            send_image_enabled: tool_toggles.send_image,
            generate_image_enabled: tool_toggles.generate_image,
            web_search_enabled: tool_toggles.web_search,
            activity_heatmap_enabled: tool_toggles.activity_heatmap,
            roll_dice_enabled: tool_toggles.roll_dice,
            check_time_enabled: tool_toggles.check_time,
        };

        let prompt_result = prompt::assemble_prompt(&PromptParams {
            config_dir: &self.cmd_ctx.config.dirs.config,
            character_name: engine.character_name(),
            display_name: &display_name,
            character_definition: character_definition.as_deref(),
            user_definition: user_definition.as_deref(),
            is_private: false,
            character_data_dir: &character_data_dir,
            messages,
            max_context_tokens: resolved.max_context_tokens,
            max_output_tokens: resolved.max_tokens,
            capabilities: Some(&capabilities),
        });

        // 5. Build LLM messages from assembled prompt.
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
                    // Build payload from structured content blocks.
                    let blocks: Vec<Value> = m.content_blocks.iter().filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(json!({ "type": "text", "text": text })),
                        ContentBlock::Thinking { thinking, signature } => {
                            // Only include thinking blocks with signatures (required by Anthropic API).
                            // Pre-signature blocks (no signature captured) are still stripped.
                            signature.as_ref().map(|sig| {
                                json!({ "type": "thinking", "thinking": thinking, "signature": sig })
                            })
                        }
                        ContentBlock::RedactedThinking { data } => Some(json!({
                            "type": "redacted_thinking", "data": data,
                        })),
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
                    // Legacy messages without content_blocks — use text + images.
                    build_content(&m.content, &m.images)
                };
                json!({ "role": role, "content": content })
            })
            .collect();

        let system = if prompt_result.system.is_empty() {
            None
        } else if prompt_result.system.len() == 1 {
            // Single block — send as plain string for maximum provider compat.
            Some(json!(prompt_result.system[0].content))
        } else {
            // Multiple blocks — send as TextBlockParam[] for Anthropic API.
            Some(json!(prompt_result.system.iter().map(|b| {
                json!({"type": "text", "text": b.content})
            }).collect::<Vec<_>>()))
        };

        // 6. Build tool definitions from unified tool system.
        let tool_defs = if self.cmd_ctx.config.app.behavior.tool_use.enabled {
            let toggles = &self.cmd_ctx.config.app.behavior.tool_use.tools;
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

        // 7. Build LLM request.
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

        // 8. Stream response from shore-llm (with retry on transient errors).
        let retry_policy = RetryPolicy {
            max_retries: self.cmd_ctx.config.app.advanced.max_retries
                .unwrap_or(RetryPolicy::default().max_retries),
            ..RetryPolicy::default()
        };
        let mut attempt: u32 = 0;
        let mut result;

        loop {
            let consumer = StreamConsumer::new(self.push_tx.clone());

            let stream_result = async {
                let mut reader = self
                    .llm_client
                    .stream_raw(&request, rid.as_deref())
                    .await?;

                let engine = self.registry.get_or_create(&char_name)
                    .map_err(|e| shore_llm_client::LlmError::Provider { message: e.to_string() })?;

                let turn_count = engine.messages().len();
                // Only check for cache invalidation on providers that support
                // prompt caching (currently only Anthropic).
                let cache_warnings = resolved.provider_key == "anthropic"
                    && self.cmd_ctx.config.app.advanced.cache_invalidation_warnings;
                let cache_ctx = CacheContext {
                    conversation_turn_count: turn_count,
                    is_first_after_restart: self.is_first_after_restart,
                    is_first_after_compaction: false,
                    cache_invalidation_warnings: cache_warnings,
                };

                consumer.consume(&mut reader, regen, &cache_ctx).await
            }
            .await;

            match stream_result {
                Ok(r) => {
                    result = r;
                    break;
                }
                Err(e) => {
                    match retry::should_retry_error(&e, attempt, &retry_policy) {
                        RetryDecision::Retry => {
                            let base_ms = self.cmd_ctx.config.app.advanced.retry_backoff_seconds
                                .map(|s| (s * 1000.0) as u64)
                                .unwrap_or(500);
                            let delay = std::time::Duration::from_millis(base_ms * 2u64.pow(attempt));
                            warn!(attempt, delay_ms = delay.as_millis() as u64, "Retrying after transient LLM error");
                            tokio::time::sleep(delay).await;
                            attempt += 1;
                        }
                        RetryDecision::FallbackModel(_model) => {
                            // TODO: fallback model support requires re-resolving the model
                            // and rebuilding the request. For now, treat as failure.
                            return Err(e.into());
                        }
                        RetryDecision::Fail => return Err(e.into()),
                    }
                }
            }
        }

        // Build cache context for tool loop (values don't depend on retry attempt).
        let engine = self.registry.get_or_create(&char_name)
            .map_err(|e| e.to_string())?;
        let turn_count = engine.messages().len();
        // Only check for cache invalidation on providers that support
        // prompt caching (currently only Anthropic).
        let tool_cache_warnings = resolved.provider_key == "anthropic"
            && self.cmd_ctx.config.app.advanced.cache_invalidation_warnings;
        let cache_ctx = CacheContext {
            conversation_turn_count: turn_count,
            is_first_after_restart: self.is_first_after_restart,
            is_first_after_compaction: false,
            cache_invalidation_warnings: tool_cache_warnings,
        };

        self.is_first_after_restart = false;

        // 9. Run tool loop if the LLM requested tool use.
        let mut tool_intermediate_messages: Vec<Message> = Vec::new();

        if result.finish_reason == "tool_use"
            && self.cmd_ctx.config.app.behavior.tool_use.enabled
        {
            // Build per-request tool context with memory dependencies.
            let db_path = self.cmd_ctx.data_dir
                .join(&char_name)
                .join("memory")
                .join("memory.db");
            let memory_db = MemoryDB::open(&db_path)
                .map_err(|e| format!("failed to open memory DB: {e}"))?;

            let char_def = character_definition.clone().unwrap_or_default();
            let user_def = user_definition.clone().unwrap_or_default();

            // Resolve image generation config (best-effort — None if not configured).
            let image_gen_config = crate::memory::compaction_impls::resolve_image_gen_config(
                self.cmd_ctx.config.app.defaults.image_generation.as_deref(),
                &self.cmd_ctx.config.models.image_generation,
            ).ok();

            let tool_ctx = HandlerToolContext {
                db: memory_db,
                agent: MemoryAgent::one_shot(
                    CallerIdentity::Char,
                    &char_name,
                    &self.cmd_ctx.config.app.defaults.resolve_display_name(),
                ),
                agent_llm: RealAgentLlm::new(self.llm_client.clone()),
                agent_model_val: agent_model.clone(),
                researcher: researcher_model.as_ref().map(|_| {
                    MemoryResearcher::new(char_def, user_def)
                }),
                researcher_llm_val: researcher_model.as_ref().map(|_| {
                    RealAgentLlm::new(self.llm_client.clone())
                }),
                researcher_model_val: researcher_model.clone(),
                rag: NoopRag,
                image_dir_val: self.cmd_ctx.data_dir
                    .join(&char_name)
                    .join("images")
                    .to_string_lossy()
                    .into_owned(),
                llm_client_val: self.llm_client.clone(),
                image_gen_config_val: image_gen_config,
                search_config_val: self.cmd_ctx.config.app.behavior.tool_use.search.clone(),
                autonomy_val: self.autonomy.clone(),
                character_name_val: char_name.to_string(),
            };

            let tool_loop_result = tools::run_tool_loop(
                &self.llm_client,
                &self.push_tx,
                &mut request,
                result,
                &tool_ctx,
                self.cmd_ctx.config.app.behavior.tool_use.max_iterations,
                &cache_ctx,
                &self.cmd_ctx.diagnostics,
            )
            .await?;

            result = tool_loop_result.result;
            tool_intermediate_messages = tool_loop_result.intermediate_messages;
        }

        // Re-borrow engine for final operations.
        let engine = self.registry.get_or_create(&char_name)
            .map_err(|e| e.to_string())?;

        // 10. Persist intermediate tool messages (assistant tool_use + user tool_result pairs).
        for msg in tool_intermediate_messages {
            engine.append_message(msg)?;
        }

        // 11. Track cumulative token usage.
        self.cmd_ctx.session_tokens.input += result.usage.input_tokens;
        self.cmd_ctx.session_tokens.output += result.usage.output_tokens;
        self.cmd_ctx.session_tokens.cache_read += result.usage.cache_read_tokens;
        self.cmd_ctx.session_tokens.cache_write += result.usage.cache_creation_tokens;

        // 11b. Record API call in diagnostics ring buffer.
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
            self.cmd_ctx.diagnostics.lock().unwrap().api_calls.push(entry);
        }

        // Notify cache keepalive of API response and cache the request for keepalive pings.
        self.autonomy.notify_api_response(
            &char_name,
            result.usage.cache_read_tokens,
            result.usage.input_tokens,
        );
        self.autonomy.notify_last_request(&char_name, request.clone());

        info!(
            input_tokens = result.usage.input_tokens,
            output_tokens = result.usage.output_tokens,
            model = %result.model,
            "Response complete"
        );

        // 12. Append final assistant message with content_blocks to conversation.
        let content_blocks = result.content_blocks.clone();
        let assistant_msg = Message {
            msg_id: format!("m_{}", uuid::Uuid::new_v4()),
            role: Role::Assistant,
            content: result.content.clone(),
            images: vec![],
            content_blocks,
            alt_index: None,
            alt_count: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        engine.append_message(assistant_msg)?;
        self.autonomy.notify_assistant_message(&char_name, engine.message_count());

        Ok(())
    }
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

        let cmd_ctx = CommandContext {
            config: loaded_config.clone(),
            push_tx: push_tx.clone(),
            data_dir: data_dir.clone(),
            active_model: None,
            session_tokens: Default::default(),
            autonomy: autonomy.clone(),
            llm_client: LlmClient::new(tmp.path().join("dummy.sock")),
            diagnostics: std::sync::Arc::new(std::sync::Mutex::new(shore_diagnostics::Diagnostics::default())),
            memory_shell_sessions: std::collections::HashMap::new(),
        };

        let registry = CharacterRegistry::new(config_dir, data_dir, push_tx.clone(), loaded_config);

        let handler = MessageHandler {
            registry,
            cmd_ctx,
            llm_client: LlmClient::new(tmp.path().join("dummy.sock")),
            push_tx: push_tx.clone(),
            is_first_after_restart: false,
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

        // This will error because no model is configured, but it should
        // get past the routing stage (no panic, no wrong-variant error).
        let result = handler.handle_engine_message(regen, Some("Alice")).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        // Should fail at model resolution, not at message routing.
        assert!(
            err_msg.contains("model") || err_msg.contains("Model"),
            "Expected model-related error, got: {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn handle_engine_message_hello_is_noop() {
        let tmp = TempDir::new().unwrap();
        let (mut handler, _rx) = make_handler(&tmp, &["Alice"]);

        let hello = ClientMessage::Hello(shore_protocol::client_msg::ClientHello {
            client_type: "test".into(),
            client_name: "test".into(),
            capabilities: vec![],
            character: None,
        });

        // Hello variant should be silently ignored (wildcard arm).
        let result = handler.handle_engine_message(hello, Some("Alice")).await;
        assert!(result.is_ok());
    }

    // ── NoopRag ────────────────────────────────────────────────────

    #[tokio::test]
    async fn nooprag_returns_empty() {
        let rag = NoopRag;
        let results = rag.query("anything", 10).await.unwrap();
        assert!(results.is_empty());
    }
}
