//! Message processing handler.
//!
//! Consumes routed messages from the SWP server and orchestrates the
//! engine -> prompt -> LLM -> tool loop -> persist pipeline.
//!
//! Generation (Message/Regen) runs in spawned tokio tasks so the handler loop
//! never blocks on LLM streaming. Commands (status, log, etc.) are processed
//! inline and always return immediately.

mod command_dispatch;
mod generation;
mod images;
mod key_fallback;
mod persistence;
mod resize;
mod task;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub(crate) use images::{build_content, embed_image_data};
pub(crate) use task::build_llm_messages;
use task::handle_generation;

use shore_protocol::client_msg::{ClientMessage, ClientMessageBody};
use shore_protocol::error::ErrorCode;
use shore_protocol::server_msg::{
    AudioError as SwpAudioError, CommandOutput as SwpCommandOutput, Error as SwpError,
    ServerMessage,
};
use tokio::sync::{broadcast, mpsc, Mutex};
use tracing::{debug, error, info};

use crate::autonomy::manager::AutonomyManager;
use crate::characters::CharacterRegistry;
use crate::commands::{CommandContext, SessionTokens};
use crate::memory::compaction_impls::ImageGenConfig;
use crate::notifications::{NotificationEvent, NotificationService};
use crate::tools::context::SharedToolContext;
use crate::tools::ToolContext;
use shore_config::app::SearchConfig;
use shore_config::LoadedConfig;
use shore_ledger::LedgerClient;
use shore_swp_server::{RequestMeta, RoutedMessage, SessionId, SessionRouter};

use crate::tts::TtsClient;

pub(super) struct HandlerToolContext {
    inner: SharedToolContext,
    autonomy_val: AutonomyManager,
}

impl ToolContext for HandlerToolContext {
    fn image_dir(&self) -> &str {
        self.inner.image_dir()
    }
    fn llm_client(&self) -> Option<&shore_llm::LlmClient> {
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
    fn workspace_dir(&self) -> &str {
        self.inner.workspace_dir()
    }
    fn character_data_dir(&self) -> &str {
        self.inner.character_data_dir()
    }
    fn markdown_store(&self) -> Option<&crate::memory::markdown_store::MarkdownMemoryStore> {
        self.inner.markdown_store()
    }
    fn memory_retrieval_config(&self) -> &shore_config::app::RetrievalConfig {
        self.inner.memory_retrieval_config()
    }
    fn embedding_config(&self) -> Option<&crate::memory::retrieval::EmbeddingConfig> {
        self.inner.embedding_config()
    }
    fn memory_index_path(&self) -> Option<&std::path::Path> {
        self.inner.memory_index_path()
    }
    fn memory_access_allowed(&self) -> bool {
        self.inner.memory_access_allowed()
    }
    fn memory_read_allowed(&self) -> bool {
        self.inner.memory_read_allowed()
    }
    fn memory_write_allowed(&self) -> bool {
        self.inner.memory_write_allowed()
    }
    fn config_dir(&self) -> &str {
        self.inner.config_dir()
    }
    fn defer_edit(&self, path: &str) {
        self.inner.defer_edit(path);
    }
}

#[derive(Clone)]
struct GenContext {
    registry: Arc<Mutex<CharacterRegistry>>,
    llm_client: LedgerClient,
    event_tx: broadcast::Sender<ServerMessage>,
    direct_tx: mpsc::Sender<ServerMessage>,
    autonomy: AutonomyManager,
    session_tokens: Arc<std::sync::Mutex<SessionTokens>>,
    diagnostics: Arc<std::sync::Mutex<shore_diagnostics::Diagnostics>>,
    notifier: NotificationService,
    /// Daemon-wide live TTS flag.
    live_speak: Arc<AtomicBool>,
    /// TTS client (None if TTS is not configured).
    tts_client: Option<TtsClient>,
}

struct GenerationParams {
    request: RequestMeta,
    body: ClientMessageBody,
    regen: bool,
    char_name: String,
    rid: Option<String>,
    effective_config: LoadedConfig,
    data_dir: PathBuf,
    active_model: Option<String>,
    reasoning_effort_override: Option<Option<String>>,
    /// Phase 3+: per-model sampler overlay derived from the merged
    /// global+character preferences for the active `(provider, model_id)`.
    /// Empty `SamplerSettings` means "no preference overrides apply".
    sampler_overlay: crate::preferences::SamplerSettings,
}

#[derive(Default)]
struct SessionState {
    active_model: Option<String>,
    reasoning_effort_override: Option<Option<String>>,
    session_tokens: Arc<std::sync::Mutex<SessionTokens>>,
    generation_handle: Option<tokio::task::JoinHandle<()>>,
}

/// The message processing handler.
///
/// Routes commands inline (fast path) and spawns tokio tasks for generation
/// (Message/Regen), so the handler loop is never blocked by LLM streaming.
pub struct MessageHandler {
    pub registry: Arc<Mutex<CharacterRegistry>>,
    pub cmd_ctx: CommandContext,
    pub llm_client: LedgerClient,
    pub push_tx: broadcast::Sender<ServerMessage>,
    pub session_router: SessionRouter,
    pub autonomy: AutonomyManager,
    pub notifier: NotificationService,
    /// Daemon-wide live TTS flag.
    pub live_speak: Arc<AtomicBool>,
    /// TTS client (None if TTS is not configured).
    pub tts_client: Option<TtsClient>,
    sessions: HashMap<SessionId, SessionState>,
}

pub struct MessageHandlerDeps {
    pub registry: Arc<Mutex<CharacterRegistry>>,
    pub cmd_ctx: CommandContext,
    pub llm_client: LedgerClient,
    pub push_tx: broadcast::Sender<ServerMessage>,
    pub session_router: SessionRouter,
    pub autonomy: AutonomyManager,
    pub notifier: NotificationService,
    pub live_speak: Arc<AtomicBool>,
    pub tts_client: Option<TtsClient>,
}

impl MessageHandler {
    pub fn new(deps: MessageHandlerDeps) -> Self {
        Self {
            registry: deps.registry,
            cmd_ctx: deps.cmd_ctx,
            llm_client: deps.llm_client,
            push_tx: deps.push_tx,
            session_router: deps.session_router,
            autonomy: deps.autonomy,
            notifier: deps.notifier,
            live_speak: deps.live_speak,
            tts_client: deps.tts_client,
            sessions: HashMap::new(),
        }
    }

    fn session_state_mut(&mut self, session_id: SessionId) -> &mut SessionState {
        self.sessions.entry(session_id).or_default()
    }

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
                RoutedMessage::Command { cmd, meta } => {
                    debug!(
                        client_id = meta.session.client_id.0,
                        session_id = meta.session.session_id.0,
                        client_type = %meta.session.client_type,
                        rid = meta.rid.as_deref().unwrap_or("-"),
                        character = ?meta.session.selected_character,
                        "handling command"
                    );
                    let result = self.dispatch_command(&cmd, &meta).await;
                    let _ = self
                        .session_router
                        .send_to_session(meta.session.session_id, result)
                        .await;
                }
                RoutedMessage::Engine { msg, meta } => {
                    let msg_kind = match &msg {
                        ClientMessage::Message(_) => "message",
                        ClientMessage::Regen(_) => "regen",
                        ClientMessage::Cancel(_) => "cancel",
                        ClientMessage::Speak(_) => "speak",
                        ClientMessage::SetLiveSpeak(_) => "set_live_speak",
                        _ => "other",
                    };
                    debug!(
                        client_id = meta.session.client_id.0,
                        session_id = meta.session.session_id.0,
                        client_type = %meta.session.client_type,
                        rid = meta.rid.as_deref().unwrap_or("-"),
                        character = ?meta.session.selected_character,
                        kind = msg_kind,
                        "handling engine message"
                    );
                    if let ClientMessage::SetLiveSpeak(ref toggle) = msg {
                        let prev = self.live_speak.swap(toggle.enabled, Ordering::Relaxed);
                        info!(enabled = toggle.enabled, prev, "Live TTS toggled");
                        let _ = self
                            .session_router
                            .send_to_session(
                                meta.session.session_id,
                                ServerMessage::CommandOutput(SwpCommandOutput {
                                    rid: meta.rid.clone(),
                                    name: "set_live_speak".into(),
                                    data: serde_json::json!({ "enabled": toggle.enabled }),
                                }),
                            )
                            .await;
                        continue;
                    }

                    if let ClientMessage::Speak(ref speak) = msg {
                        let Some(tts_client) = self.tts_client.clone() else {
                            let _ = self.push_tx.send(ServerMessage::AudioError(SwpAudioError {
                                rid: speak.rid.clone(),
                                message: "TTS not configured".into(),
                            }));
                            continue;
                        };
                        let push_tx = self.push_tx.clone();
                        let registry = self.registry.clone();
                        let rid = speak.rid.clone();
                        let msg_id = speak.msg_id.clone();
                        let character = meta.session.selected_character.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_speak_request(
                                &tts_client,
                                &push_tx,
                                &registry,
                                rid.clone(),
                                msg_id,
                                character.as_deref(),
                            )
                            .await
                            {
                                error!(error = %e, "TTS speak failed");
                                let _ = push_tx.send(ServerMessage::AudioError(SwpAudioError {
                                    rid,
                                    message: e.to_string(),
                                }));
                            }
                        });
                        continue;
                    }

                    if matches!(msg, ClientMessage::Cancel(_)) {
                        info!(
                            client_id = meta.session.client_id.0,
                            session_id = meta.session.session_id.0,
                            rid = meta.rid.as_deref().unwrap_or("-"),
                            "cancelling generation from routed cancel request"
                        );
                        self.cancel_generation(
                            meta.session.session_id,
                            meta.rid.clone(),
                            "user cancelled",
                        )
                        .await;
                        continue;
                    }

                    let (char_name, effective_config) = {
                        let mut registry = self.registry.lock().await;
                        let char_name = match registry
                            .resolve_character(meta.session.selected_character.as_deref())
                        {
                            Ok(name) => name,
                            Err(e) => {
                                let _ = self
                                    .session_router
                                    .send_to_session(
                                        meta.session.session_id,
                                        ServerMessage::Error(SwpError {
                                            rid: None,
                                            code: ErrorCode::InvalidRequest,
                                            message: e.to_string(),
                                        })
                                        .with_rid(meta.rid.clone()),
                                    )
                                    .await;
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

                    let rid = body
                        .rid
                        .clone()
                        .filter(|r| r.is_ascii() && !r.contains('\0'));
                    let direct_tx = match self
                        .session_router
                        .sender_for(meta.session.session_id)
                        .await
                    {
                        Some(tx) => tx,
                        None => continue,
                    };
                    // Phase 3: preferences are authoritative.
                    // Legacy `runtime_state.json` remains as a migration
                    // fallback for one release; it is read but never
                    // written by Phase 3+ code paths.
                    let (active_model, sampler_overlay) = {
                        let character_data_dir = self.cmd_ctx.data_dir.join(&char_name);
                        let (global_prefs, char_prefs) =
                            crate::preferences::load_for_character(&self.cmd_ctx.data_dir, &char_name)
                                .unwrap_or_else(|e| {
                                    tracing::warn!(error = %e, character = %char_name, "Failed to load preferences; using empty defaults");
                                    (
                                        crate::preferences::ModelPreferences::default(),
                                        crate::preferences::ModelPreferences::default(),
                                    )
                                });
                        let legacy = crate::runtime_state::load_active_model(&character_data_dir);
                        let resolved = crate::preferences::resolve_active_for_character(
                            &effective_config,
                            &self.cmd_ctx.data_dir,
                            &global_prefs,
                            &char_prefs,
                            legacy.as_deref(),
                            effective_config.app.defaults.model.as_deref(),
                        );
                        let overlay = match resolved.as_ref() {
                            Some(m) => crate::preferences::resolve_sampler_settings(
                                &global_prefs,
                                Some(&char_prefs),
                                &m.provider_key,
                                &m.model_id,
                            ),
                            None => crate::preferences::SamplerSettings::default(),
                        };
                        (resolved.map(|m| m.qualified_name.clone()), overlay)
                    };
                    let reasoning_effort_override = {
                        let session = self.session_state_mut(meta.session.session_id);
                        session.reasoning_effort_override.clone()
                    };
                    let gen = self.gen_context(meta.session.session_id, direct_tx.clone());
                    let notifier = self.notifier.clone();
                    let params = GenerationParams {
                        request: meta.clone(),
                        body,
                        regen,
                        char_name,
                        rid,
                        effective_config,
                        data_dir: self.cmd_ctx.data_dir.clone(),
                        active_model,
                        reasoning_effort_override,
                        sampler_overlay,
                    };

                    let session = self.session_state_mut(meta.session.session_id);
                    if let Some(prev) = session.generation_handle.take() {
                        info!("Aborting previous generation (superseded by new request)");
                        prev.abort();
                    }
                    session.generation_handle = Some(tokio::spawn(async move {
                        let notify_name = params.char_name.clone();
                        let request_rid = params.rid.clone();
                        if let Err(e) = handle_generation(gen, params).await {
                            error!(error = %e, "Error processing engine message");
                            let err_msg = e.to_string();
                            let _ = direct_tx
                                .send(
                                    ServerMessage::Error(SwpError {
                                        rid: None,
                                        code: ErrorCode::InternalError,
                                        message: err_msg.clone(),
                                    })
                                    .with_rid(request_rid),
                                )
                                .await;
                            notifier.notify(
                                NotificationEvent::Error,
                                &format!("Shore - {notify_name}"),
                                &err_msg,
                            );
                        }
                    }));
                }
                RoutedMessage::AllClientsDisconnected => {
                    let session_ids: Vec<SessionId> = self.sessions.keys().copied().collect();
                    for session_id in session_ids {
                        self.cancel_generation(session_id, None, "all clients disconnected")
                            .await;
                    }
                }
            }
        }
        info!("Message handler shutting down (route channel closed)");
    }
}

/// Handle a client `Speak` request: resolve character + voice + message text,
/// then call the TTS relay. All output goes through `push_tx` (broadcast).
async fn handle_speak_request(
    tts_client: &TtsClient,
    push_tx: &broadcast::Sender<ServerMessage>,
    registry: &Arc<Mutex<CharacterRegistry>>,
    rid: Option<String>,
    msg_id: Option<String>,
    character: Option<&str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use shore_protocol::types::Role;

    let (char_name, voice, resolved_id, text) = {
        let mut reg = registry.lock().await;
        let char_name = reg.resolve_character(character)?;
        let engine_arc = reg.get_or_create(&char_name)?;
        let voice = reg
            .effective_config(&char_name)
            .app
            .tts
            .voice
            .clone()
            .unwrap_or_else(|| char_name.clone());
        drop(reg);

        let engine = engine_arc.lock().await;
        let messages = engine.messages();
        let (resolved_id, text) = match msg_id {
            Some(ref id) => {
                let msg = messages
                    .iter()
                    .find(|m| &m.msg_id == id)
                    .ok_or_else(|| format!("message not found: {id}"))?;
                (msg.msg_id.clone(), msg.content.clone())
            }
            None => {
                let msg = messages
                    .iter()
                    .rev()
                    .find(|m| m.role == Role::Assistant)
                    .ok_or("no assistant messages to speak")?;
                (msg.msg_id.clone(), msg.content.clone())
            }
        };

        (char_name, voice, resolved_id, text)
    };

    if text.is_empty() {
        let _ = push_tx.send(ServerMessage::AudioError(SwpAudioError {
            rid,
            message: "message has no text content".into(),
        }));
        return Ok(());
    }

    debug!(character = %char_name, voice = %voice, msg_id = %resolved_id, "handle_speak resolved");
    crate::tts::relay_speech(tts_client, &text, &voice, &resolved_id, rid, push_tx).await;
    Ok(())
}
