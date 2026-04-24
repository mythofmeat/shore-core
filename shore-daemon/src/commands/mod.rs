pub mod conversation;
pub mod navigation;
pub mod state;
pub mod usage;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use shore_protocol::client_msg::Command;
use shore_protocol::error::ErrorCode;
use shore_protocol::server_msg::{CommandOutput, Error, ServerMessage};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::autonomy::manager::AutonomyManager;
use crate::engine::{ConversationEngine, EngineError};
use shore_config::LoadedConfig;
use shore_diagnostics::Diagnostics;
use shore_ledger::LedgerClient;

/// Cumulative token usage tracked across the daemon session.
#[derive(Debug, Default)]
pub struct SessionTokens {
    pub input: u32,
    pub output: u32,
    pub cache_read: u32,
    pub cache_write: u32,
}

/// Shared state for command handlers (does not own the engine).
pub struct CommandContext {
    /// Loaded configuration.
    pub config: LoadedConfig,
    /// Broadcast sender for push messages.
    pub push_tx: broadcast::Sender<ServerMessage>,
    /// Data directory (`$XDG_DATA_HOME/shore/`).
    pub data_dir: PathBuf,
    /// Currently active model name.
    pub active_model: Option<String>,
    /// Per-session override for the resolved model's `reasoning_effort`.
    /// `None` means "use the value from config"; `Some(v)` patches the
    /// resolved chat model for every subsequent generation on this session.
    pub reasoning_effort_override: Option<Option<String>>,
    /// Cumulative token usage for the session (shared with generation tasks).
    pub session_tokens: Arc<Mutex<SessionTokens>>,
    /// Shared autonomy manager for scheduler state.
    pub autonomy: AutonomyManager,
    /// LLM client for commands that need model access.
    pub llm_client: LedgerClient,
    /// In-memory diagnostics ring buffers.
    pub diagnostics: Arc<Mutex<Diagnostics>>,
}

/// Convenience type for command handler results.
pub type CommandResult = Result<serde_json::Value, (ErrorCode, String)>;

/// Build the active prompt directory path for a character.
pub fn memory_dir(ctx: &CommandContext, char_name: &str) -> PathBuf {
    crate::memory::deferred_edits::active_prompt_dir(&ctx.data_dir.join(char_name))
}

/// Dispatch a command to the appropriate handler.
pub async fn dispatch(
    engine: Arc<tokio::sync::Mutex<ConversationEngine>>,
    ctx: &mut CommandContext,
    cmd: &Command,
) -> ServerMessage {
    info!(command = %cmd.name, "Dispatching command");

    let mut guard = engine.lock().await;
    let engine = &mut *guard;

    let result = match cmd.name.as_str() {
        // Navigation
        "list_characters" => navigation::list_characters(engine, ctx),
        "switch_character" => navigation::switch_character(engine, ctx, &cmd.args),
        "character_info" => navigation::character_info(engine, ctx, &cmd.args),

        // Conversation
        "log" => conversation::log(engine, ctx, &cmd.args),
        "get" => conversation::get(engine, ctx, &cmd.args),
        "edit" => conversation::edit(engine, ctx, &cmd.args),
        "delete" => conversation::delete(engine, ctx, &cmd.args),
        "inject_system" => conversation::inject_system(engine, ctx, &cmd.args),

        // State
        "status" => state::status(engine, ctx),
        "list_models" => state::list_models(ctx),
        "model_info" => state::model_info(ctx, &cmd.args),
        "switch_model" => state::switch_model(ctx, &cmd.args),
        "reset_model" => state::reset_model(ctx),
        "set_reasoning_effort" => state::set_reasoning_effort(ctx, &cmd.args),
        "memory_changelog" => state::memory_changelog(engine, ctx, &cmd.args),
        "memory_dream" => state::memory_dream(engine, ctx, &cmd.args).await,
        "memory" => state::memory(engine, ctx, &cmd.args).await,
        "compact" => state::compact(engine, ctx, &cmd.args).await,
        "config" => state::config(ctx, &cmd.args),
        "config_check" => state::config_check(ctx),
        "config_reset" => state::config_reset(ctx),
        "diagnostics" => state::diagnostics(ctx, &cmd.args),
        "heartbeat_log" => state::heartbeat_log(engine, ctx, &cmd.args),
        "heartbeat_tick_now" => state::heartbeat_tick_now(engine, ctx),
        "heartbeat_set_dormant" => state::heartbeat_set_dormant(engine, ctx),
        "heartbeat_set_active" => state::heartbeat_set_active(engine, ctx),
        "usage" => usage::usage(ctx, &cmd.args).await,

        _ => Err((
            ErrorCode::InvalidRequest,
            format!("Unknown command: {}", cmd.name),
        )),
    };

    match result {
        Ok(data) => ServerMessage::CommandOutput(CommandOutput {
            rid: None,
            name: cmd.name.clone(),
            data,
        }),
        Err((code, message)) => {
            warn!(command = %cmd.name, ?code, %message, "Command failed");
            ServerMessage::Error(Error {
                rid: None,
                code,
                message,
            })
        }
    }
}

/// Dispatch commands that don't require a character/engine (e.g. list_characters).
pub fn dispatch_characterless(ctx: &CommandContext, cmd: &Command) -> CommandResult {
    debug!(command = %cmd.name, "Dispatching characterless command");
    match cmd.name.as_str() {
        "list_characters" => navigation::list_characters_standalone(ctx),
        "list_models" => state::list_models(ctx),
        _ => Err((
            ErrorCode::InvalidRequest,
            format!("Command '{}' requires a character", cmd.name),
        )),
    }
}

/// Convert an EngineError to a command error tuple.
pub fn engine_err(e: EngineError) -> (ErrorCode, String) {
    match &e {
        EngineError::MessageNotFound(_) | EngineError::CharacterNotFound(_) => {
            (ErrorCode::NotFound, e.to_string())
        }
        _ => (ErrorCode::InternalError, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shore_protocol::client_msg::Command;

    fn make_ctx(
        tmp: &tempfile::TempDir,
    ) -> (
        ConversationEngine,
        CommandContext,
        broadcast::Receiver<ServerMessage>,
    ) {
        let (push_tx, push_rx) = broadcast::channel(16);
        let data_dir = tmp.path().to_path_buf();
        let engine =
            ConversationEngine::new("TestChar".to_string(), data_dir.clone(), push_tx.clone())
                .unwrap();

        let config = shore_config::LoadedConfig::new_for_test(
            shore_config::app::AppConfig::default(),
            shore_config::models::ModelCatalog::default(),
            shore_config::ShoreDirs {
                config: tmp.path().join("config"),
                data: data_dir.clone(),
                runtime: tmp.path().join("runtime"),
                cache: tmp.path().join("cache"),
            },
        );

        let (_tx, rx) = tokio::sync::watch::channel(());
        let autonomy =
            AutonomyManager::new(Default::default(), Default::default(), data_dir.clone(), rx);

        let ctx = CommandContext {
            config,
            push_tx,
            data_dir: data_dir.clone(),
            active_model: None,
            reasoning_effort_override: None,
            session_tokens: Arc::new(Mutex::new(SessionTokens::default())),
            autonomy,
            llm_client: LedgerClient::new(
                shore_llm_client::LlmClient::new(),
                &data_dir.join("ledger.db"),
            )
            .unwrap(),
            diagnostics: Arc::new(Mutex::new(Diagnostics::default())),
        };
        (engine, ctx, push_rx)
    }

    #[tokio::test]
    async fn unknown_command_returns_invalid_request() {
        let tmp = tempfile::tempdir().unwrap();
        let (engine, mut ctx, _rx) = make_ctx(&tmp);
        let engine_arc = Arc::new(tokio::sync::Mutex::new(engine));

        let cmd = Command {
            rid: None,
            name: "bogus_command".into(),
            args: serde_json::json!({}),
        };

        let result = dispatch(engine_arc, &mut ctx, &cmd).await;
        match result {
            ServerMessage::Error(e) => {
                assert_eq!(e.code, ErrorCode::InvalidRequest);
                assert!(e.message.contains("bogus_command"));
            }
            other => panic!("Expected Error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn dispatch_returns_command_output_with_name() {
        let tmp = tempfile::tempdir().unwrap();
        let (engine, mut ctx, _rx) = make_ctx(&tmp);
        let engine_arc = Arc::new(tokio::sync::Mutex::new(engine));

        let cmd = Command {
            rid: None,
            name: "status".into(),
            args: serde_json::json!({}),
        };

        let result = dispatch(engine_arc, &mut ctx, &cmd).await;
        match result {
            ServerMessage::CommandOutput(output) => {
                assert_eq!(output.name, "status");
                assert!(output.data.is_object());
            }
            other => panic!("Expected CommandOutput, got {:?}", other),
        }
    }

    // ── dispatch_characterless ───────────────────────────────────────────

    #[test]
    fn dispatch_characterless_list_characters() {
        let tmp = tempfile::tempdir().unwrap();

        // Create a character directory so list_characters finds something.
        let char_dir = tmp.path().join("config").join("characters").join("alice");
        std::fs::create_dir_all(&char_dir).unwrap();
        std::fs::write(
            char_dir.join("definition.toml"),
            "[character]\nname = \"Alice\"\n",
        )
        .unwrap();

        let (_engine, ctx, _rx) = make_ctx(&tmp);

        let cmd = Command {
            rid: None,
            name: "list_characters".into(),
            args: serde_json::json!({}),
        };

        let result = dispatch_characterless(&ctx, &cmd);
        assert!(result.is_ok());
        let data = result.unwrap();
        assert!(data["characters"].is_array());
    }

    #[test]
    fn dispatch_characterless_rejects_unknown_command() {
        let tmp = tempfile::tempdir().unwrap();
        let (_engine, ctx, _rx) = make_ctx(&tmp);

        let cmd = Command {
            rid: None,
            name: "status".into(),
            args: serde_json::json!({}),
        };

        let result = dispatch_characterless(&ctx, &cmd);
        assert!(result.is_err());
        let (code, msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
        assert!(msg.contains("requires a character"));
    }
}
