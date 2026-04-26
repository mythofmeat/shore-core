use serde_json::json;
use tokio::sync::mpsc;

use shore_protocol::error::ErrorCode;
use shore_protocol::server_msg::{Error as SwpError, ServerMessage};
use shore_swp_server::{RequestMeta, SessionId};
use tracing::{debug, info};

use crate::commands::{self, CommandContext};
use crate::handshake::build_session_history_snapshot;
use crate::runtime_state::{load_active_model, save_active_model};

use super::{GenContext, MessageHandler};

impl MessageHandler {
    /// Cancel any active generation task and send a minimal StreamEnd.
    pub(super) async fn cancel_generation(
        &mut self,
        session_id: SessionId,
        rid: Option<String>,
        reason: &str,
    ) {
        if let Some(handle) = self.session_state_mut(session_id).generation_handle.take() {
            info!(reason, "Cancelling active generation");
            handle.abort();
            let _ = self
                .session_router
                .send_to_session(
                    session_id,
                    ServerMessage::StreamEnd(shore_protocol::server_msg::StreamEnd {
                        rid: None,
                        msg_id: None,
                        revision: None,
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
                        is_final: true,
                    })
                    .with_rid(rid),
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
            live_speak: self.live_speak.clone(),
            tts_client: self.tts_client.clone(),
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
        // `list_characters` and `list_models` both read from the global
        // config and never touch a character engine, so they bypass the
        // character-resolution step below. This lets `shore complete
        // models` succeed even on a multi-character config where no
        // character has been selected yet.
        if cmd.name == "list_characters" || cmd.name == "list_models" {
            let (active_model, reasoning_effort_override, session_tokens) = {
                let session = self.session_state_mut(session_id);
                (
                    session.active_model.clone(),
                    session.reasoning_effort_override.clone(),
                    session.session_tokens.clone(),
                )
            };
            let ctx = CommandContext {
                config: self.cmd_ctx.config.clone(),
                push_tx: self.push_tx.clone(),
                data_dir: self.cmd_ctx.data_dir.clone(),
                active_model,
                reasoning_effort_override,
                session_tokens,
                autonomy: self.cmd_ctx.autonomy.clone(),
                llm_client: self.cmd_ctx.llm_client.clone(),
                diagnostics: self.cmd_ctx.diagnostics.clone(),
            };
            let result = commands::dispatch_characterless(&ctx, cmd);
            {
                let session = self.session_state_mut(session_id);
                session.active_model = ctx.active_model.clone();
                session.reasoning_effort_override = ctx.reasoning_effort_override.clone();
            }
            return match result {
                Ok(data) => {
                    ServerMessage::CommandOutput(shore_protocol::server_msg::CommandOutput {
                        rid: None,
                        name: cmd.name.clone(),
                        data,
                    })
                }
                Err((code, msg)) => ServerMessage::Error(SwpError {
                    rid: None,
                    code,
                    message: msg,
                }),
            };
        }

        let (char_name, engine_arc, effective_config) = {
            let mut registry = self.registry.lock().await;

            let char_name =
                match registry.resolve_character(meta.session.selected_character.as_deref()) {
                    Ok(name) => name,
                    Err(e) => {
                        return ServerMessage::Error(SwpError {
                            rid: None,
                            code: ErrorCode::InvalidRequest,
                            message: e.to_string(),
                        })
                        .with_rid(meta.rid.clone());
                    }
                };

            let effective_config = registry.effective_config(&char_name).clone();

            let engine_arc = match registry.get_or_create(&char_name) {
                Ok(arc) => arc,
                Err(e) => {
                    return ServerMessage::Error(SwpError {
                        rid: None,
                        code: ErrorCode::InternalError,
                        message: e.to_string(),
                    })
                    .with_rid(meta.rid.clone());
                }
            };

            (char_name, engine_arc, effective_config)
        };

        let character_data_dir = self.cmd_ctx.data_dir.join(&char_name);
        let persisted_active_model = load_active_model(&character_data_dir);

        let (reasoning_effort_override, session_tokens) = {
            let session = self.session_state_mut(session_id);
            (
                session.reasoning_effort_override.clone(),
                session.session_tokens.clone(),
            )
        };

        let mut cmd_ctx = CommandContext {
            config: effective_config,
            push_tx: self.push_tx.clone(),
            data_dir: self.cmd_ctx.data_dir.clone(),
            active_model: persisted_active_model,
            reasoning_effort_override,
            session_tokens,
            autonomy: self.cmd_ctx.autonomy.clone(),
            llm_client: self.cmd_ctx.llm_client.clone(),
            diagnostics: self.cmd_ctx.diagnostics.clone(),
        };

        let mut result = commands::dispatch(engine_arc.clone(), &mut cmd_ctx, cmd)
            .await
            .with_rid(meta.rid.clone());
        let runtime_config_set = cmd.name == "config"
            && cmd
                .args
                .get("value")
                .and_then(serde_json::Value::as_str)
                .is_some();
        let active_model_after_command = cmd_ctx.active_model.clone();
        let reasoning_effort_after_command = cmd_ctx.reasoning_effort_override.clone();
        if let Err(e) = save_active_model(&character_data_dir, active_model_after_command.clone()) {
            tracing::warn!(
                character = %char_name,
                error = %e,
                "Failed to persist active model"
            );
        }

        {
            let session = self.session_state_mut(session_id);
            session.active_model = active_model_after_command.clone();
            session.reasoning_effort_override = reasoning_effort_after_command;
        }

        if runtime_config_set {
            if let ServerMessage::CommandOutput(output) = &mut result {
                let runtime_config = cmd_ctx.config.clone();
                {
                    let mut registry = self.registry.lock().await;
                    registry.set_runtime_effective_config(&char_name, runtime_config.clone());
                }
                self.autonomy.reload_runtime_config(runtime_config.clone());
                self.cmd_ctx
                    .autonomy
                    .reload_runtime_config(runtime_config.clone());

                if output
                    .data
                    .get("invalidated")
                    .and_then(serde_json::Value::as_object)
                    .is_none()
                {
                    output.data["invalidated"] = json!({});
                }
                output.data["invalidated"]["merged_character_configs"] = json!(true);
            }
        }

        if cmd.name == "config_reset" {
            if let ServerMessage::CommandOutput(output) = &mut result {
                let reloaded_config = cmd_ctx.config.clone();
                self.cmd_ctx.config = reloaded_config.clone();
                self.cmd_ctx
                    .autonomy
                    .reload_runtime_config(reloaded_config.clone());
                self.autonomy.reload_runtime_config(reloaded_config.clone());

                let summary = {
                    let mut registry = self.registry.lock().await;
                    registry.reload_runtime_state(reloaded_config)
                };

                if output
                    .data
                    .get("invalidated")
                    .and_then(serde_json::Value::as_object)
                    .is_none()
                {
                    output.data["invalidated"] = json!({});
                }
                output.data["invalidated"]["character_discovery"] =
                    json!(summary.character_discovery_changed);
                output.data["invalidated"]["merged_character_configs"] = json!(true);
                output.data["invalidated"]["removed_character_engines"] =
                    json!(summary.dropped_engines);
            }
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
                        None,
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
                                rid: None,
                                messages: snapshot.messages,
                                config: snapshot.config,
                                selected_character: snapshot.selected_character,
                                revision: snapshot.revision,
                            })
                            .with_rid(meta.rid.clone()),
                        )
                        .await;
                }
            }
        }

        result
    }
}
