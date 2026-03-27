pub mod conversation;
pub mod navigation;
pub mod state;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use shore_protocol::client_msg::Command;
use shore_protocol::error::ErrorCode;
use shore_protocol::server_msg::{CommandOutput, Error, ServerMessage};
use tokio::sync::broadcast;
use tracing::info;

use crate::autonomy::manager::AutonomyManager;
use shore_config::models::ResolvedModel;
use shore_config::LoadedConfig;
use shore_diagnostics::Diagnostics;
use crate::engine::{ConversationEngine, EngineError};
use shore_llm_client::LlmClient;
use crate::memory::agent::MemoryAgent;

/// Cumulative token usage tracked across the daemon session.
#[derive(Debug, Clone, Default)]
pub struct SessionTokens {
    pub input: u32,
    pub output: u32,
    pub cache_read: u32,
    pub cache_write: u32,
}

/// An active memory shell session.
pub struct MemoryShellSession {
    pub agent: MemoryAgent,
    pub history: Vec<serde_json::Value>,
    pub character: String,
    pub model: ResolvedModel,
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
    /// Cumulative token usage for the session.
    pub session_tokens: SessionTokens,
    /// Shared autonomy manager for scheduler state.
    pub autonomy: AutonomyManager,
    /// LLM client for commands that need model access (e.g. memory query).
    pub llm_client: LlmClient,
    /// In-memory diagnostics ring buffers.
    pub diagnostics: Arc<Mutex<Diagnostics>>,
    /// Active memory shell sessions, keyed by session ID.
    pub memory_shell_sessions: HashMap<String, MemoryShellSession>,
}

/// Convenience type for command handler results.
pub type CommandResult = Result<serde_json::Value, (ErrorCode, String)>;

/// Dispatch a command to the appropriate handler.
pub async fn dispatch(
    engine: &mut ConversationEngine,
    ctx: &mut CommandContext,
    cmd: &Command,
) -> ServerMessage {
    info!(command = %cmd.name, "Dispatching command");

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

        // State
        "status" => state::status(engine, ctx),
        "list_models" => state::list_models(ctx),
        "model_info" => state::model_info(ctx, &cmd.args),
        "switch_model" => state::switch_model(ctx, &cmd.args),
        "reset_model" => state::reset_model(ctx),
        "memory_changelog" => state::memory_changelog(engine, ctx, &cmd.args),
        "memory" => state::memory(engine, ctx, &cmd.args).await,
        "memory_shell_start" => state::memory_shell_start(engine, ctx, &cmd.args).await,
        "memory_shell_query" => state::memory_shell_query(ctx, &cmd.args).await,
        "memory_shell_end" => state::memory_shell_end(ctx, &cmd.args),
        "compact" => state::compact(engine, ctx, &cmd.args).await,
        "collate" => state::collate(engine, ctx, &cmd.args).await,
        "config" => state::config(ctx, &cmd.args),
        "config_check" => state::config_check(ctx),
        "memory_reindex" => state::memory_reindex(engine, ctx).await,
        "config_reset" => state::config_reset(ctx),
        "diagnostics" => state::diagnostics(ctx, &cmd.args),
        "heartbeat_log" => state::heartbeat_log(engine, ctx, &cmd.args),

        _ => Err((
            ErrorCode::InvalidRequest,
            format!("Unknown command: {}", cmd.name),
        )),
    };

    match result {
        Ok(data) => ServerMessage::CommandOutput(CommandOutput {
            name: cmd.name.clone(),
            data,
        }),
        Err((code, message)) => ServerMessage::Error(Error { code, message }),
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
        let engine = ConversationEngine::new(
            "TestChar".to_string(),
            data_dir.clone(),
            push_tx.clone(),
        )
        .unwrap();

        let config = shore_config::LoadedConfig::new_for_test(
            shore_config::app::AppConfig::default(),
            shore_config::models::ModelCatalog::default(),
            shore_config::ShoreDirs {
                config: tmp.path().join("config"),
                data: data_dir.clone(),
                runtime: tmp.path().join("runtime"),
            },
        );

        let (_tx, rx) = tokio::sync::watch::channel(());
        let (autonomy, _compaction_rx) = AutonomyManager::new(Default::default(), data_dir.clone(), rx);

        let ctx = CommandContext {
            config,
            push_tx,
            data_dir: data_dir.clone(),
            active_model: None,
            session_tokens: Default::default(),
            autonomy,
            llm_client: LlmClient::new(data_dir.join("dummy.sock")),
            diagnostics: Arc::new(Mutex::new(Diagnostics::default())),
            memory_shell_sessions: HashMap::new(),
        };
        (engine, ctx, push_rx)
    }

    #[tokio::test]
    async fn unknown_command_returns_invalid_request() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);

        let cmd = Command {
            rid: None,
            name: "bogus_command".into(),
            args: serde_json::json!({}),
        };

        let result = dispatch(&mut engine, &mut ctx, &cmd).await;
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
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);

        let cmd = Command {
            rid: None,
            name: "status".into(),
            args: serde_json::json!({}),
        };

        let result = dispatch(&mut engine, &mut ctx, &cmd).await;
        match result {
            ServerMessage::CommandOutput(output) => {
                assert_eq!(output.name, "status");
                assert!(output.data.is_object());
            }
            other => panic!("Expected CommandOutput, got {:?}", other),
        }
    }
}
