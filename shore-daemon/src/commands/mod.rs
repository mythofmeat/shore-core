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
use crate::engine::{ConversationEngine, EngineError};
use crate::memory::agent::{AgentSearchContext, MemoryAgent};
use crate::memory::compaction_impls::resolve_embed_config;
use crate::memory::vectorstore::VectorStore;
use shore_config::models::ResolvedModel;
use shore_config::{load_character_definition, resolve_user_definition, LoadedConfig};
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
    /// Cumulative token usage for the session (shared with generation tasks).
    pub session_tokens: Arc<Mutex<SessionTokens>>,
    /// Shared autonomy manager for scheduler state.
    pub autonomy: AutonomyManager,
    /// LLM client for commands that need model access (e.g. memory query).
    pub llm_client: LedgerClient,
    /// In-memory diagnostics ring buffers.
    pub diagnostics: Arc<Mutex<Diagnostics>>,
    /// Active memory shell sessions, keyed by session ID.
    pub memory_shell_sessions: HashMap<String, MemoryShellSession>,
}

/// Convenience type for command handler results.
pub type CommandResult = Result<serde_json::Value, (ErrorCode, String)>;

/// Resolve the agent model: configured `memory_agent` → active model → first chat model.
///
/// Used by memory queries, memory shell, and collation setup.
pub fn resolve_agent_model(ctx: &CommandContext) -> Result<ResolvedModel, (ErrorCode, String)> {
    ctx.config
        .app
        .defaults
        .memory_agent
        .as_deref()
        .and_then(|name| ctx.config.models.find_model(name).ok())
        .or_else(|| {
            ctx.active_model
                .as_deref()
                .and_then(|name| ctx.config.models.find_model(name).ok())
        })
        .or_else(|| ctx.config.models.first_chat_model())
        .cloned()
        .ok_or_else(|| (ErrorCode::InternalError, "No model configured".to_string()))
}

/// Build the memory directory path for a character.
pub fn memory_dir(ctx: &CommandContext, char_name: &str) -> PathBuf {
    ctx.data_dir.join(char_name).join("memory")
}

/// Build a semantic search context for a character (graceful: returns None
/// if no embedding model is configured or the vector store can't be opened).
pub async fn setup_search_context(
    ctx: &CommandContext,
    char_name: &str,
) -> Option<AgentSearchContext> {
    let embed_config = resolve_embed_config(
        ctx.config.app.defaults.embedding.as_deref(),
        &ctx.config.models.embedding,
    )
    .ok()?;
    let vs_path = memory_dir(ctx, char_name).join("vectorstore");
    let vs = VectorStore::open(&vs_path, embed_config.dimensions)
        .await
        .ok()?;
    Some(AgentSearchContext::new(
        vs,
        ctx.llm_client.inner().clone(),
        embed_config,
    ))
}

/// Open a vector store with embedding config for a character (error-returning variant).
///
/// Unlike `setup_search_context()` which returns `Option` for graceful degradation,
/// this propagates errors for callers that need diagnostics (e.g. compaction, reindex).
pub async fn open_embed_and_vectorstore(
    ctx: &CommandContext,
    char_name: &str,
) -> Result<(VectorStore, crate::memory::compaction_impls::EmbedConfig), (ErrorCode, String)> {
    let embed_config = resolve_embed_config(
        ctx.config.app.defaults.embedding.as_deref(),
        &ctx.config.models.embedding,
    )
    .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;
    let vs_path = memory_dir(ctx, char_name).join("vectorstore");
    let store = VectorStore::open(&vs_path, embed_config.dimensions)
        .await
        .map_err(|e| {
            (
                ErrorCode::InternalError,
                format!("Failed to open vector store: {e}"),
            )
        })?;
    Ok((store, embed_config))
}

/// Build the template variable map used by collation.
pub fn build_collation_vars(
    ctx: &CommandContext,
    char_name: &str,
    display_name: &str,
) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    vars.insert("char".to_string(), char_name.to_string());
    vars.insert("user".to_string(), display_name.to_string());
    if let Some(cd) = load_character_definition(&ctx.config.dirs.config, char_name) {
        vars.insert("char_description".to_string(), cd);
    }
    if let Some(ud) = resolve_user_definition(&ctx.config.dirs.config, char_name) {
        vars.insert("user_description".to_string(), ud);
    }
    vars
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
        "memory_changelog" => state::memory_changelog(engine, ctx, &cmd.args),
        "memory" => state::memory(engine, ctx, &cmd.args).await,
        "memory_shell_start" => state::memory_shell_start(engine, ctx, &cmd.args).await,
        "memory_shell_query" => state::memory_shell_query(ctx, &cmd.args).await,
        "memory_shell_end" => state::memory_shell_end(ctx, &cmd.args),
        "compact" => state::compact(engine, ctx, &cmd.args).await,
        "collate" => state::collate(engine, ctx, &cmd.args).await,
        "memory_purge" => state::memory_purge(engine, ctx, &cmd.args).await,
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

/// Dispatch commands that don't require a character/engine (e.g. list_characters).
pub fn dispatch_characterless(ctx: &CommandContext, cmd: &Command) -> CommandResult {
    match cmd.name.as_str() {
        "list_characters" => navigation::list_characters_standalone(ctx),
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
            },
        );

        let (_tx, rx) = tokio::sync::watch::channel(());
        let (autonomy, _compaction_rx) =
            AutonomyManager::new(Default::default(), Default::default(), data_dir.clone(), rx);

        let ctx = CommandContext {
            config,
            push_tx,
            data_dir: data_dir.clone(),
            active_model: None,
            session_tokens: Arc::new(Mutex::new(SessionTokens::default())),
            autonomy,
            llm_client: LedgerClient::new(shore_llm_client::LlmClient::new(), &data_dir.join("ledger.db")).unwrap(),
            diagnostics: Arc::new(Mutex::new(Diagnostics::default())),
            memory_shell_sessions: HashMap::new(),
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
