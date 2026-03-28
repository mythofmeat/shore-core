//! Types shared across the memory agent module.

use serde_json::Value;

// ---------------------------------------------------------------------------
// Caller identity
// ---------------------------------------------------------------------------

/// Who is invoking the memory agent.
///
/// V1 bug: the agent couldn't resolve first-person pronouns because it didn't
/// know whether "I" referred to the character or the user. This enum fixes
/// that by explicitly tracking caller identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerIdentity {
    /// The character is calling via an agentic tool call during generation.
    /// "I" / "me" / "my" → the character's name.
    Char,
    /// The user is calling via an interactive memory shell session.
    /// "I" / "me" / "my" → the user's name.
    User,
}

// ---------------------------------------------------------------------------
// Agent mode
// ---------------------------------------------------------------------------

/// Operating mode for the memory agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMode {
    /// One-shot tool call: accept a natural language request, return result.
    OneShot,
    /// Interactive memory shell session.
    Interactive,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum AgentError {
    Db(String),
    Rag(String),
    Indexing(String),
    Llm(String),
    MaxIterations,
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::Db(e) => write!(f, "db: {e}"),
            AgentError::Rag(e) => write!(f, "rag: {e}"),
            AgentError::Indexing(e) => write!(f, "indexing: {e}"),
            AgentError::Llm(e) => write!(f, "llm: {e}"),
            AgentError::MaxIterations => write!(f, "agent loop reached maximum iterations"),
        }
    }
}

impl std::error::Error for AgentError {}

// ---------------------------------------------------------------------------
// Tool result
// ---------------------------------------------------------------------------

/// Result from executing a single tool call.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

// ---------------------------------------------------------------------------
// Proposed operation (for confirmation flow)
// ---------------------------------------------------------------------------

/// A proposed write operation, shown to the user for confirmation.
#[derive(Debug, Clone)]
pub struct ProposedOperation {
    pub tool_use_id: String,
    pub tool_name: String,
    pub args: Value,
    pub description: String,
}

// ---------------------------------------------------------------------------
// Confirm callback
// ---------------------------------------------------------------------------

/// Trait for confirming proposed write operations.
///
/// In interactive mode, this prompts the user. In non-interactive mode
/// (one-shot / researcher), all writes are auto-accepted (no callback).
///
/// Returns the set of tool_use_ids that were **denied**.
pub trait ConfirmCallback: Send + Sync {
    fn confirm(
        &self,
        operations: &[ProposedOperation],
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::collections::HashSet<String>> + Send + '_>,
    >;
}

// ---------------------------------------------------------------------------
// RAG / Indexer traits (carried forward from old agent.rs)
// ---------------------------------------------------------------------------

/// RAG retrieval: takes a query string, returns scored entry IDs.
pub trait AgentRag: Send + Sync {
    fn query(
        &self,
        query: &str,
        top_k: usize,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<RagHit>, AgentError>> + Send + '_>,
    >;
}

/// A single RAG hit with entry ID and relevance score.
#[derive(Debug, Clone)]
pub struct RagHit {
    pub entry_id: String,
    pub score: f64,
}

/// Vector indexer for entries after create/update/supersede.
pub trait AgentIndexer: Send + Sync {
    fn index_entry(
        &self,
        entry_id: &str,
        text: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), AgentError>> + Send + '_>,
    >;
}
