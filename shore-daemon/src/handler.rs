//! Message processing handler.
//!
//! Consumes routed messages from the SWP server and orchestrates the
//! engine → prompt → LLM → tool loop → persist pipeline.

use std::sync::Arc;

use serde_json::{json, Value};
use shore_protocol::client_msg::{ClientMessage, ClientMessageBody};
use shore_protocol::error::ErrorCode;
use shore_protocol::server_msg::{Error as SwpError, ServerMessage};
use shore_protocol::types::{Message, Role};
use tokio::sync::{broadcast, Mutex};
use tracing::{error, info, instrument};

use crate::commands::{self, CommandContext};
use crate::engine::prompt::{self, PromptParams};
use crate::engine::tools::{self, ToolRegistry};
use crate::llm_client::stream::{CacheContext, StreamConsumer};
use crate::llm_client::LlmClient;
use crate::server::RoutedMessage;

/// The message processing handler.
///
/// Consumes routed messages from the server and orchestrates the full
/// message → LLM → response pipeline.
pub struct MessageHandler {
    pub cmd_ctx: CommandContext,
    pub llm_client: LlmClient,
    pub push_tx: broadcast::Sender<ServerMessage>,
    pub is_first_after_restart: bool,
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
                RoutedMessage::Command(cmd) => {
                    let result = commands::dispatch(&mut self.cmd_ctx, &cmd);
                    let _ = self.push_tx.send(result);
                }
                RoutedMessage::Engine(client_msg) => {
                    if let Err(e) = self.handle_engine_message(client_msg).await {
                        error!(error = %e, "Error processing engine message");
                        let _ = self.push_tx.send(ServerMessage::Error(SwpError {
                            code: ErrorCode::InternalError,
                            message: e.to_string(),
                        }));
                    }
                }
            }
        }
        info!("Message handler shutting down (route channel closed)");
    }

    async fn handle_engine_message(
        &mut self,
        msg: ClientMessage,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match msg {
            ClientMessage::Message(body) => self.handle_user_message(body, false).await,
            ClientMessage::Regen(regen) => {
                // Regen re-generates the last assistant response.
                let body = ClientMessageBody {
                    rid: regen.rid,
                    text: String::new(),
                    stream: regen.stream,
                    images: vec![],
                    absence_seconds: None,
                };
                self.handle_user_message(body, true).await
            }
            _ => Ok(()),
        }
    }

    #[instrument(skip(self, body), fields(rid = body.rid.as_deref().unwrap_or("-")))]
    async fn handle_user_message(
        &mut self,
        body: ClientMessageBody,
        regen: bool,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let rid = body.rid.clone();

        // 1. Ensure conversation exists.
        if self.cmd_ctx.engine.active_conversation_id().is_none() {
            self.cmd_ctx.engine.new_conversation("New Chat")?;
        }

        // 2. Append user message (unless regen).
        if !regen && !body.text.is_empty() {
            let user_msg = Message {
                msg_id: format!("m_{}", uuid::Uuid::new_v4()),
                role: Role::User,
                content: body.text.clone(),
                images: vec![],
                alt_index: None,
                alt_count: None,
                timestamp: chrono::Utc::now().to_rfc3339(),
            };
            self.cmd_ctx.engine.append_message(user_msg)?;
        }

        // 3. Resolve model.
        let model_name = self
            .cmd_ctx
            .active_model
            .as_deref()
            .or(self.cmd_ctx.config.app.defaults.model.as_deref());

        let model_name = match model_name {
            Some(name) => name.to_string(),
            None => self
                .cmd_ctx
                .config
                .models
                .models
                .first()
                .map(|m| m.name.clone())
                .ok_or("No model configured in models.toml")?,
        };

        let resolved = self
            .cmd_ctx
            .config
            .models
            .resolve_model(&model_name)
            .ok_or_else(|| format!("Model not found: {model_name}"))?;

        // 4. Assemble prompt.
        let messages = self.cmd_ctx.engine.messages()?;
        let character_data_dir = self
            .cmd_ctx
            .data_dir
            .join(self.cmd_ctx.engine.character_name());

        let prompt_result = prompt::assemble_prompt(&PromptParams {
            config_dir: &self.cmd_ctx.config.dirs.config,
            character_name: self.cmd_ctx.engine.character_name(),
            character_definition: self.cmd_ctx.config.character_definition.as_deref(),
            user_definition: self.cmd_ctx.config.user_definition.as_deref(),
            is_private: self.cmd_ctx.engine.is_active_private(),
            character_data_dir: &character_data_dir,
            messages,
            max_context_tokens: resolved.max_context_tokens,
            max_output_tokens: resolved.max_tokens,
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
                json!({ "role": role, "content": m.content })
            })
            .collect();

        let system = if prompt_result.system.is_empty() {
            None
        } else {
            Some(json!(prompt_result.system[0].content))
        };

        // 6. Build tool definitions.
        let is_private = self.cmd_ctx.engine.is_active_private();
        let registry = ToolRegistry::new(is_private);
        let tool_defs = if self.cmd_ctx.config.app.behavior.tool_use.enabled {
            Some(registry.definitions().to_vec())
        } else {
            None
        };

        // 7. Build LLM request.
        let mut request =
            LlmClient::build_request(&resolved, llm_messages, system, tool_defs, None)?;

        info!(
            model = %resolved.model_id,
            messages = request.messages.len(),
            "Sending streaming request to LLM"
        );

        // 8. Stream response from shore-llm.
        let consumer = StreamConsumer::new(self.push_tx.clone());
        let mut reader = self
            .llm_client
            .stream_raw(&request, rid.as_deref())
            .await?;

        let turn_count = self.cmd_ctx.engine.messages()?.len();
        let cache_ctx = CacheContext {
            conversation_turn_count: turn_count,
            is_first_after_restart: self.is_first_after_restart,
            is_first_after_compaction: false,
            cache_invalidation_warnings: self
                .cmd_ctx
                .config
                .app
                .advanced
                .cache_invalidation_warnings,
        };

        let mut result = consumer.consume(&mut reader, regen, &cache_ctx).await?;
        self.is_first_after_restart = false;

        // 9. Run tool loop if the LLM requested tool use.
        if result.finish_reason == "tool_use"
            && self.cmd_ctx.config.app.behavior.tool_use.enabled
        {
            if !result.content.is_empty() {
                self.cmd_ctx
                    .engine
                    .accumulate_pre_tool_text(&result.content);
            }

            result = tools::run_tool_loop(
                &self.llm_client,
                &self.push_tx,
                &mut request,
                result,
                &registry,
                self.cmd_ctx.config.app.behavior.tool_use.max_iterations,
                &cache_ctx,
            )
            .await?;
        }

        // 10. Finalize response content (prepend any pre-tool text).
        let content = self.cmd_ctx.engine.finalize_response(&result.content);

        // 11. Track cumulative token usage.
        self.cmd_ctx.session_tokens.input += result.usage.input_tokens;
        self.cmd_ctx.session_tokens.output += result.usage.output_tokens;
        self.cmd_ctx.session_tokens.cache_read += result.usage.cache_read_tokens;
        self.cmd_ctx.session_tokens.cache_write += result.usage.cache_creation_tokens;

        info!(
            input_tokens = result.usage.input_tokens,
            output_tokens = result.usage.output_tokens,
            model = %result.model,
            "Response complete"
        );

        // 12. Append assistant message to conversation.
        let assistant_msg = Message {
            msg_id: format!("m_{}", uuid::Uuid::new_v4()),
            role: Role::Assistant,
            content,
            images: vec![],
            alt_index: None,
            alt_count: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        self.cmd_ctx.engine.append_message(assistant_msg)?;

        Ok(())
    }
}
