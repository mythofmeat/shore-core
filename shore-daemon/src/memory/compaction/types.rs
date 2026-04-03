use std::future::Future;
use std::pin::Pin;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

pub(super) const DEFAULT_IDLE_TRIGGER_MINUTES: u32 = 30;
pub(super) const DEFAULT_MIN_TURNS: usize = 8;
pub(super) const DEFAULT_MAX_TURNS: usize = 16;
pub(super) const DEFAULT_KEEP_RECENT_TURNS: usize = 2;

/// Configuration for compaction triggers.
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// Minutes of idle time before proactive compaction fires.
    pub idle_trigger_minutes: u32,
    /// Minimum user turns before any compaction trigger fires.
    pub min_turns: usize,
    /// Force compaction when this user turn count is reached.
    pub max_turns: usize,
    /// User turns retained in active conversation after compaction.
    pub keep_recent_turns: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            idle_trigger_minutes: DEFAULT_IDLE_TRIGGER_MINUTES,
            min_turns: DEFAULT_MIN_TURNS,
            max_turns: DEFAULT_MAX_TURNS,
            keep_recent_turns: DEFAULT_KEEP_RECENT_TURNS,
        }
    }
}

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

/// A memory entry extracted by the LLM during compaction.
#[derive(Debug, Clone)]
pub struct CompactedEntry {
    pub memory_type: String,
    pub summary_text: String,
    pub topic_tags: String,
    pub topic_key: String,
    pub confidence: f64,
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
    pub entries_created: Vec<String>,
    pub conversation_id: String,
    pub new_conversation_id: String,
    pub message_count: usize,
    pub retained_count: usize,
    pub retained_turns: usize,
    pub recap_generated: bool,
}

/// Result of a dry-run compaction.
#[derive(Debug)]
pub struct DryRunResult {
    pub would_create_entries: usize,
    pub entries_preview: Vec<CompactedEntry>,
    pub message_count: usize,
    pub retained_count: usize,
    pub retained_turns: usize,
    pub recap_preview: Option<String>,
}

/// Parameters for archiving with message retention.
#[derive(Debug)]
pub struct RetentionParams {
    /// Number of messages to keep from the end of active.jsonl.
    pub keep_last_n: usize,
    /// Recap text to write to memory/recap.md (None = leave untouched).
    pub recap: Option<String>,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    #[error("llm: {0}")]
    Llm(String),
    #[error("db: {0}")]
    Db(String),
    #[error("parse: {0}")]
    Parse(String),
    #[error("private conversation: skipped")]
    PrivateConversation,
    #[error("insufficient messages")]
    InsufficientMessages,
    #[error("indexing: {0}")]
    Indexing(String),
    #[error("conversation: {0}")]
    ConversationManager(String),
}

// ---------------------------------------------------------------------------
// Traits for external dependencies
// ---------------------------------------------------------------------------

/// LLM client for compaction. Takes a rendered prompt, returns raw LLM text.
///
/// The library owns the prompt format (XML) and handles parsing the response
/// into recap + entries. The impl just sends text and returns text.
pub trait CompactionLlm: Send + Sync {
    fn summarize(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, CompactionError>> + Send + '_>>;
}

/// Vector indexer for newly created entries.
pub trait VectorIndexer: Send + Sync {
    fn index_entry(
        &self,
        entry_id: &str,
        text: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), CompactionError>> + Send + '_>>;
}

/// Conversation lifecycle management — archive old messages and retain recent ones.
pub trait ConversationManager: Send + Sync {
    fn archive_and_retain(
        &self,
        conversation_id: &str,
        params: RetentionParams,
    ) -> Result<String, CompactionError>;
}
