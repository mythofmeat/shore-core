use std::future::Future;
use std::pin::Pin;

// ---------------------------------------------------------------------------
// Configuration — re-exported from shore-config (single source of truth)
// ---------------------------------------------------------------------------

pub use shore_config::app::CompactionConfig;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A message from a conversation, used as input to compaction.
#[derive(Debug, Clone)]
pub struct ConversationMessage {
    pub role: String,
    pub content: String,
    pub timestamp: String,
    /// True when a user message's content_blocks are ALL ToolResult
    /// (i.e. a tool-loop intermediate, not a real user turn).
    pub is_tool_result_only: bool,
}

/// Outcome of a compaction operation.
#[derive(Debug)]
pub enum CompactionOutcome {
    Compacted(CompactionResult),
    DryRun(DryRunResult),
}

/// Result of an actual compaction.
#[derive(Debug)]
pub struct CompactionResult {
    pub memory_files_written: Vec<String>,
    pub conversation_id: String,
    pub new_conversation_id: String,
    pub message_count: usize,
    pub retained_count: usize,
    pub retained_turns: usize,
    /// Paths of markdown files written during compaction.
    pub markdown_paths: Vec<String>,
}

/// Result of a dry-run compaction.
#[derive(Debug)]
pub struct DryRunResult {
    pub would_write_files: usize,
    pub file_ops_preview: Vec<crate::memory::compaction::parser::MemoryFileOp>,
    pub message_count: usize,
    pub retained_count: usize,
    pub retained_turns: usize,
    /// Paths of markdown files that would be written.
    pub markdown_preview: Vec<String>,
}

/// Parameters for archiving with message retention.
#[derive(Debug)]
pub struct RetentionParams {
    /// Number of messages to keep from the end of active.jsonl.
    pub keep_last_n: usize,
    /// Pre-read content of active.jsonl at the time messages were parsed.
    /// Eliminates the TOCTOU race where the file could change between
    /// message analysis and the archive-and-retain write.
    pub active_content: String,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    #[error("llm: {0}")]
    Llm(String),
    #[error("parse: {0}")]
    Parse(String),
    #[error("private conversation: skipped")]
    PrivateConversation,
    #[error("insufficient messages")]
    InsufficientMessages,
    #[error("conversation: {0}")]
    ConversationManager(String),
    #[error("markdown store: {0}")]
    MarkdownStore(String),
}

// ---------------------------------------------------------------------------
// Traits for external dependencies
// ---------------------------------------------------------------------------

/// LLM client for compaction.
///
/// Takes a system prompt and structured messages (conversation history with the
/// compaction instruction appended), returns raw LLM text. Splitting system from
/// messages enables prompt-prefix caching of the stable system instructions.
pub trait CompactionLlm: Send + Sync {
    fn summarize(
        &self,
        system: &str,
        messages: Vec<serde_json::Value>,
    ) -> Pin<Box<dyn Future<Output = Result<String, CompactionError>> + Send + '_>>;
}

/// Conversation lifecycle management — archive old messages and retain recent ones.
pub trait ConversationManager: Send + Sync {
    fn archive_and_retain(
        &self,
        conversation_id: &str,
        params: RetentionParams,
    ) -> Pin<Box<dyn Future<Output = Result<String, CompactionError>> + Send + '_>>;
}
