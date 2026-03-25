pub mod conversation;
pub mod navigation;
pub mod state;

use std::path::PathBuf;

use shore_protocol::client_msg::Command;
use shore_protocol::error::ErrorCode;
use shore_protocol::server_msg::{CommandOutput, Error, ServerMessage};
use tokio::sync::broadcast;
use tracing::info;

use crate::config::LoadedConfig;
use crate::engine::{ConversationEngine, EngineError};

/// Shared state for command handlers.
pub struct CommandContext {
    /// The active conversation engine.
    pub engine: ConversationEngine,
    /// Loaded configuration.
    pub config: LoadedConfig,
    /// Broadcast sender for push messages.
    pub push_tx: broadcast::Sender<ServerMessage>,
    /// Data directory (`$XDG_DATA_HOME/shore/`).
    pub data_dir: PathBuf,
    /// Currently active model name.
    pub active_model: Option<String>,
    /// Whether autonomy is paused.
    pub autonomy_paused: bool,
}

/// Convenience type for command handler results.
pub type CommandResult = Result<serde_json::Value, (ErrorCode, String)>;

/// Dispatch a command to the appropriate handler.
pub fn dispatch(ctx: &mut CommandContext, cmd: &Command) -> ServerMessage {
    info!(command = %cmd.name, "Dispatching command");

    let result = match cmd.name.as_str() {
        // Navigation
        "list_characters" => navigation::list_characters(ctx),
        "switch_character" => navigation::switch_character(ctx, &cmd.args),
        "list_chats" => navigation::list_chats(ctx),
        "switch_chat" => navigation::switch_chat(ctx, &cmd.args),
        "new_chat" => navigation::new_chat(ctx, &cmd.args),

        // Conversation
        "swipe" => conversation::swipe(ctx, &cmd.args),
        "log" => conversation::log(ctx, &cmd.args),
        "edit" => conversation::edit(ctx, &cmd.args),
        "delete" => conversation::delete(ctx, &cmd.args),

        // State
        "status" => state::status(ctx),
        "list_models" => state::list_models(ctx),
        "switch_model" => state::switch_model(ctx, &cmd.args),
        "memory" => state::memory(&cmd.args),
        "toggle_private" => state::toggle_private(ctx),
        "compact" => state::compact(&cmd.args),
        "toggle_autonomy" => state::toggle_autonomy(ctx),
        "config" => state::config(ctx, &cmd.args),

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
        EngineError::ConversationNotFound(_) | EngineError::MessageNotFound(_) => {
            (ErrorCode::NotFound, e.to_string())
        }
        EngineError::NoActiveConversation => (ErrorCode::InvalidRequest, e.to_string()),
        _ => (ErrorCode::InternalError, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shore_protocol::client_msg::Command;

    fn make_ctx(tmp: &tempfile::TempDir) -> (CommandContext, broadcast::Receiver<ServerMessage>) {
        let (push_tx, push_rx) = broadcast::channel(16);
        let data_dir = tmp.path().to_path_buf();
        let engine = ConversationEngine::new(
            "TestChar".to_string(),
            data_dir.clone(),
            push_tx.clone(),
        )
        .unwrap();

        let config = crate::config::LoadedConfig {
            app: crate::config::app::AppConfig::default(),
            models: crate::config::models::ModelsConfig::default(),
            dirs: crate::config::ShoreDirs {
                config: tmp.path().join("config"),
                data: data_dir.clone(),
                runtime: tmp.path().join("runtime"),
            },
            character_definition: None,
            user_definition: None,
        };

        let ctx = CommandContext {
            engine,
            config,
            push_tx,
            data_dir,
            active_model: None,
            autonomy_paused: false,
        };
        (ctx, push_rx)
    }

    #[test]
    fn unknown_command_returns_invalid_request() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);

        let cmd = Command {
            rid: None,
            name: "bogus_command".into(),
            args: serde_json::json!({}),
        };

        let result = dispatch(&mut ctx, &cmd);
        match result {
            ServerMessage::Error(e) => {
                assert_eq!(e.code, ErrorCode::InvalidRequest);
                assert!(e.message.contains("bogus_command"));
            }
            other => panic!("Expected Error, got {:?}", other),
        }
    }

    #[test]
    fn dispatch_returns_command_output_with_name() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);

        let cmd = Command {
            rid: None,
            name: "status".into(),
            args: serde_json::json!({}),
        };

        let result = dispatch(&mut ctx, &cmd);
        match result {
            ServerMessage::CommandOutput(output) => {
                assert_eq!(output.name, "status");
                assert!(output.data.is_object());
            }
            other => panic!("Expected CommandOutput, got {:?}", other),
        }
    }
}
