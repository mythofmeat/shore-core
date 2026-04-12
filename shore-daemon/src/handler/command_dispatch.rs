use tokio::sync::mpsc;

use shore_daemon_server::{RequestMeta, SessionId};
use shore_protocol::error::ErrorCode;
use shore_protocol::server_msg::{Error as SwpError, ServerMessage};
use tracing::{debug, info};

use crate::commands::{self, CommandContext};
use crate::handshake::build_session_history_snapshot;

use super::{GenContext, MessageHandler};

impl MessageHandler {
    /// Cancel any active generation task and send a minimal StreamEnd.
    pub(super) async fn cancel_generation(&mut self, session_id: SessionId, reason: &str) {
        if let Some(handle) = self.session_state_mut(session_id).generation_handle.take() {
            info!(reason, "Cancelling active generation");
            handle.abort();
            let _ = self
                .session_router
                .send_to_session(
                    session_id,
                    ServerMessage::StreamEnd(shore_protocol::server_msg::StreamEnd {
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
                    }),
                )
                .await;
        }
    }

    /// Build a GenContext from the current handler state.
    pub(super) fn gen_context(
        &mut self,
        session_id: SessionId,
        direct_tx: mpsc::Sender<ServerMessage>,
    ) -> GenContext {
        let session_tokens = self.session_state_mut(session_id).session_tokens.clone();
        GenContext {
            registry: self.registry.clone(),
            llm_client: self.llm_client.clone(),
            event_tx: self.push_tx.clone(),
            direct_tx,
            autonomy: self.autonomy.clone(),
            session_tokens,
            diagnostics: self.cmd_ctx.diagnostics.clone(),
            notifier: self.notifier.clone(),
        }
    }

    /// Resolve the engine for a character and dispatch a command.
    pub(super) async fn dispatch_command(
        &mut self,
        cmd: &shore_protocol::client_msg::Command,
        meta: &RequestMeta,
    ) -> ServerMessage {
        debug!(
            command = %cmd.name,
            client_id = meta.session.client_id.0,
            session_id = meta.session.session_id.0,
            client_type = %meta.session.client_type,
            rid = meta.rid.as_deref().unwrap_or("-"),
            character = ?meta.session.selected_character,
            "dispatching command"
        );
        let session_id = meta.session.session_id;
        if cmd.name == "list_characters" {
            let (active_model, session_tokens, memory_shell_sessions) = {
                let session = self.session_state_mut(session_id);
                (
                    session.active_model.clone(),
                    session.session_tokens.clone(),
                    std::mem::take(&mut session.memory_shell_sessions),
                )
            };
            let ctx = CommandContext {
                config: self.cmd_ctx.config.clone(),
                push_tx: self.push_tx.clone(),
                data_dir: self.cmd_ctx.data_dir.clone(),
                active_model,
                session_tokens,
                autonomy: self.cmd_ctx.autonomy.clone(),
                llm_client: self.cmd_ctx.llm_client.clone(),
                diagnostics: self.cmd_ctx.diagnostics.clone(),
                memory_shell_sessions,
            };
            let result = commands::dispatch_characterless(&ctx, cmd);
            {
                let session = self.session_state_mut(session_id);
                session.memory_shell_sessions = ctx.memory_shell_sessions;
                session.active_model = ctx.active_model.clone();
            }
            return match result {
                Ok(data) => {
                    ServerMessage::CommandOutput(shore_protocol::server_msg::CommandOutput {
                        name: cmd.name.clone(),
                        data,
                    })
                }
                Err((code, msg)) => ServerMessage::Error(SwpError { code, message: msg }),
            };
        }

        let (engine_arc, effective_config) = {
            let mut registry = self.registry.lock().await;

            let char_name =
                match registry.resolve_character(meta.session.selected_character.as_deref()) {
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

        let (active_model, session_tokens, memory_shell_sessions) = {
            let session = self.session_state_mut(session_id);
            (
                session.active_model.clone(),
                session.session_tokens.clone(),
                std::mem::take(&mut session.memory_shell_sessions),
            )
        };

        let mut cmd_ctx = CommandContext {
            config: effective_config,
            push_tx: self.push_tx.clone(),
            data_dir: self.cmd_ctx.data_dir.clone(),
            active_model,
            session_tokens,
            autonomy: self.cmd_ctx.autonomy.clone(),
            llm_client: self.cmd_ctx.llm_client.clone(),
            diagnostics: self.cmd_ctx.diagnostics.clone(),
            memory_shell_sessions,
        };

        let mut result = commands::dispatch(engine_arc.clone(), &mut cmd_ctx, cmd).await;
        let active_model_after_command = cmd_ctx.active_model.clone();

        {
            let session = self.session_state_mut(session_id);
            session.active_model = active_model_after_command.clone();
            session.memory_shell_sessions = cmd_ctx.memory_shell_sessions;
        }

        if cmd.name == "config_reset" {
            self.cmd_ctx.config = cmd_ctx.config.clone();
            let mut registry = self.registry.lock().await;
            registry.set_global_config(cmd_ctx.config.clone());
        }

        if cmd.name == "switch_character" {
            if let ServerMessage::CommandOutput(output) = &mut result {
                let selected = output
                    .data
                    .get("character")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                if let Some(selected) = selected {
                    let _ = self
                        .session_router
                        .set_selected_character(session_id, Some(selected.clone()))
                        .await;

                    let snapshot = build_session_history_snapshot(
                        self.registry.clone(),
                        Some(selected.clone()),
                        active_model_after_command.clone(),
                    )
                    .await;

                    output.data["selected_character"] = serde_json::Value::String(selected.clone());
                    output.data["active_model"] = snapshot
                        .config
                        .get("active_model")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    output.data["private"] = snapshot
                        .config
                        .get("private")
                        .cloned()
                        .unwrap_or(serde_json::Value::Bool(false));

                    let _ = self
                        .session_router
                        .send_to_session(
                            session_id,
                            ServerMessage::History(shore_protocol::server_msg::History {
                                messages: snapshot.messages,
                                config: snapshot.config,
                                selected_character: snapshot.selected_character,
                                revision: snapshot.revision,
                            }),
                        )
                        .await;
                }
            }
        }

        result
    }
}
