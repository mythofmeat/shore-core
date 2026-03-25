pub mod state;

use serde_json::Value;

// ---------------------------------------------------------------------------
// Command error
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum CommandError {
    UnknownCommand(String),
    InvalidArgs(String),
    Db(String),
    Internal(String),
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandError::UnknownCommand(name) => write!(f, "unknown command: {name}"),
            CommandError::InvalidArgs(msg) => write!(f, "invalid args: {msg}"),
            CommandError::Db(e) => write!(f, "db: {e}"),
            CommandError::Internal(e) => write!(f, "internal: {e}"),
        }
    }
}

impl std::error::Error for CommandError {}

// ---------------------------------------------------------------------------
// Command result
// ---------------------------------------------------------------------------

/// The result of a command execution. Maps directly to `CommandOutput` in the
/// protocol: the `name` field comes from the command dispatch and `data` is
/// whatever the handler returns.
#[derive(Debug)]
pub struct CommandResult {
    pub data: Value,
    /// Whether this command triggers a History push (e.g. toggle_private).
    pub push_history: bool,
}

impl CommandResult {
    pub fn data(data: Value) -> Self {
        Self {
            data,
            push_history: false,
        }
    }

    pub fn with_history_push(data: Value) -> Self {
        Self {
            data,
            push_history: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Command context trait — dependency injection
// ---------------------------------------------------------------------------

/// Provides access to shared state needed by command handlers.
/// Not `Send + Sync` because `MemoryDB` (rusqlite) is not `Sync`.
/// Command handlers run on a per-connection task.
pub trait CommandContext {
    fn memory_db(&self) -> &crate::memory::db::MemoryDB;
    fn is_private(&self) -> bool;
    fn set_private(&self, private: bool);
    fn effective_config(&self) -> Value;
}

// ---------------------------------------------------------------------------
// Command dispatch
// ---------------------------------------------------------------------------

/// Dispatch a command by name to its handler.
///
/// Only handles the state commands (memory, compact, toggle_private, config)
/// that are part of US-025. Navigation and conversation commands are
/// deferred to their respective user stories.
pub async fn dispatch(
    name: &str,
    args: Value,
    ctx: &dyn CommandContext,
) -> Result<CommandResult, CommandError> {
    match name {
        "memory" => state::handle_memory(args, ctx).await,
        "compact" => state::handle_compact(args, ctx).await,
        "toggle_private" => state::handle_toggle_private(ctx).await,
        "config" => state::handle_config(ctx).await,
        _ => Err(CommandError::UnknownCommand(name.to_string())),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::db::MemoryDB;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct TestCommandCtx {
        db: MemoryDB,
        private: AtomicBool,
    }

    impl TestCommandCtx {
        fn new() -> Self {
            Self {
                db: MemoryDB::open_in_memory().unwrap(),
                private: AtomicBool::new(false),
            }
        }
    }

    impl CommandContext for TestCommandCtx {
        fn memory_db(&self) -> &MemoryDB {
            &self.db
        }
        fn is_private(&self) -> bool {
            self.private.load(Ordering::SeqCst)
        }
        fn set_private(&self, private: bool) {
            self.private.store(private, Ordering::SeqCst);
        }
        fn effective_config(&self) -> Value {
            serde_json::json!({
                "model": "claude-sonnet-4-20250514",
                "memory": { "enabled": true },
            })
        }
    }

    #[tokio::test]
    async fn test_dispatch_unknown_command() {
        let ctx = TestCommandCtx::new();
        let result = dispatch("nonexistent", serde_json::json!({}), &ctx).await;
        assert!(matches!(result, Err(CommandError::UnknownCommand(_))));
    }

    #[tokio::test]
    async fn test_dispatch_memory_command() {
        let ctx = TestCommandCtx::new();
        let result = dispatch("memory", serde_json::json!({}), &ctx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_dispatch_toggle_private() {
        let ctx = TestCommandCtx::new();
        let result = dispatch("toggle_private", serde_json::json!({}), &ctx).await;
        assert!(result.is_ok());
        assert!(result.unwrap().push_history);
    }

    #[tokio::test]
    async fn test_dispatch_config() {
        let ctx = TestCommandCtx::new();
        let result = dispatch("config", serde_json::json!({}), &ctx).await;
        assert!(result.is_ok());
    }
}
