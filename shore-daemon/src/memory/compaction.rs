use crate::memory::db::{Entry, MemoryDB};
use chrono::Utc;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::time::Duration;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

const DEFAULT_IDLE_TRIGGER_MINUTES: u64 = 15;
const DEFAULT_MESSAGE_COUNT_THRESHOLD: usize = 50;

/// Configuration for compaction triggers.
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// Minutes of idle time before proactive compaction fires.
    pub idle_trigger_minutes: u64,
    /// Compact when message count reaches this threshold.
    pub message_count_threshold: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            idle_trigger_minutes: DEFAULT_IDLE_TRIGGER_MINUTES,
            message_count_threshold: DEFAULT_MESSAGE_COUNT_THRESHOLD,
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
}

/// Result of a dry-run compaction.
#[derive(Debug)]
pub struct DryRunResult {
    pub would_create_entries: usize,
    pub entries_preview: Vec<CompactedEntry>,
    pub message_count: usize,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum CompactionError {
    Llm(String),
    Db(String),
    PrivateConversation,
    InsufficientMessages,
    Indexing(String),
    ConversationManager(String),
}

impl std::fmt::Display for CompactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompactionError::Llm(e) => write!(f, "llm: {e}"),
            CompactionError::Db(e) => write!(f, "db: {e}"),
            CompactionError::PrivateConversation => write!(f, "private conversation: skipped"),
            CompactionError::InsufficientMessages => write!(f, "insufficient messages"),
            CompactionError::Indexing(e) => write!(f, "indexing: {e}"),
            CompactionError::ConversationManager(e) => write!(f, "conversation: {e}"),
        }
    }
}

impl std::error::Error for CompactionError {}

// ---------------------------------------------------------------------------
// Traits for external dependencies
// ---------------------------------------------------------------------------

/// LLM client for compaction. Takes a rendered prompt, returns extracted entries.
pub trait CompactionLlm: Send + Sync {
    fn summarize(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CompactedEntry>, CompactionError>> + Send + '_>>;
}

/// Vector indexer for newly created entries.
pub trait VectorIndexer: Send + Sync {
    fn index_entry(
        &self,
        entry_id: &str,
        text: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), CompactionError>> + Send + '_>>;
}

/// Conversation lifecycle management (archive old, create new).
pub trait ConversationManager: Send + Sync {
    fn archive_conversation(&self, conversation_id: &str) -> Result<(), CompactionError>;
    fn create_conversation(&self) -> Result<String, CompactionError>;
}

// ---------------------------------------------------------------------------
// Default prompt template
// ---------------------------------------------------------------------------

/// Default compaction prompt template. In production, loaded from `compact.md`.
/// The `{{conversation}}` placeholder is replaced with formatted messages.
pub const DEFAULT_COMPACT_PROMPT: &str = r#"Analyze the following conversation and extract key memories that should be preserved.

For each memory, determine:
- Whether it is episodic (an event or experience) or semantic (a fact or piece of knowledge)
- A concise but complete summary
- Relevant topic tags (comma-separated)
- A primary topic key
- Your confidence (0.0-1.0) that this memory is worth preserving

Respond with a JSON object:
{"entries":[{"memory_type":"episodic","summary_text":"...","topic_tags":"tag1,tag2","topic_key":"topic","confidence":0.9}]}

Conversation:
{{conversation}}"#;

// ---------------------------------------------------------------------------
// CompactionManager
// ---------------------------------------------------------------------------

pub struct CompactionManager {
    config: CompactionConfig,
    activity_notify: Arc<Notify>,
}

impl CompactionManager {
    pub fn new(config: CompactionConfig) -> Self {
        Self {
            config,
            activity_notify: Arc::new(Notify::new()),
        }
    }

    /// Signal that a new message was received, resetting the idle timer.
    pub fn notify_activity(&self) {
        self.activity_notify.notify_one();
    }

    /// Check whether compaction should trigger based on message count.
    pub fn should_compact_by_count(&self, message_count: usize) -> bool {
        message_count >= self.config.message_count_threshold
    }

    /// Build a compaction prompt from a template and conversation messages.
    pub fn build_prompt(template: &str, messages: &[ConversationMessage]) -> String {
        let mut conversation_text = String::new();
        for msg in messages {
            conversation_text.push_str(&format!(
                "[{}] {}: {}\n",
                msg.timestamp, msg.role, msg.content
            ));
        }
        template.replace("{{conversation}}", &conversation_text)
    }

    /// Generate an entry ID in the standard format: YYYYMMDD_HHMMSS_N
    fn generate_entry_id(index: usize) -> String {
        let now = Utc::now();
        format!("{}_{}", now.format("%Y%m%d_%H%M%S"), index)
    }

    /// Run compaction on a conversation.
    ///
    /// If `dry_run` is true, returns what entries would be created without
    /// writing to the database, indexing, or archiving the conversation.
    #[allow(clippy::too_many_arguments)]
    pub async fn compact(
        &self,
        conversation_id: &str,
        messages: &[ConversationMessage],
        is_private: bool,
        prompt_template: &str,
        llm: &dyn CompactionLlm,
        db: &MemoryDB,
        indexer: &dyn VectorIndexer,
        conversation_mgr: &dyn ConversationManager,
        dry_run: bool,
    ) -> Result<CompactionOutcome, CompactionError> {
        // Skip private conversations entirely.
        if is_private {
            return Err(CompactionError::PrivateConversation);
        }

        if messages.is_empty() {
            return Err(CompactionError::InsufficientMessages);
        }

        // Build and send prompt to LLM.
        let prompt = Self::build_prompt(prompt_template, messages);
        let compacted = llm.summarize(&prompt).await?;

        // Dry run: return preview without side effects.
        if dry_run {
            return Ok(CompactionOutcome::DryRun(DryRunResult {
                would_create_entries: compacted.len(),
                entries_preview: compacted,
                message_count: messages.len(),
            }));
        }

        // Determine time range from messages.
        let start_timestamp = messages
            .first()
            .map(|m| m.timestamp.clone())
            .unwrap_or_default();
        let end_timestamp = messages
            .last()
            .map(|m| m.timestamp.clone())
            .unwrap_or_default();

        let now_str = Utc::now().to_rfc3339();
        let mut entry_ids = Vec::new();

        for (i, ce) in compacted.iter().enumerate() {
            let entry_id = Self::generate_entry_id(i);

            let entry = Entry {
                id: entry_id.clone(),
                memory_type: ce.memory_type.clone(),
                source: "summary".to_string(),
                reason: "compaction".to_string(),
                status: "active".to_string(),
                canonical: false,
                confidence: ce.confidence,
                summary_text: ce.summary_text.clone(),
                topic_tags: ce.topic_tags.clone(),
                topic_key: ce.topic_key.clone(),
                start_timestamp: start_timestamp.clone(),
                end_timestamp: end_timestamp.clone(),
                message_count: messages.len() as i64,
                source_entry_ids: String::new(),
                related_entry_ids: String::new(),
                superseded_by: String::new(),
                created_at: now_str.clone(),
                updated_at: now_str.clone(),
                entry_type: String::new(),
                image_path: String::new(),
            };

            db.create_entry(&entry)
                .map_err(|e| CompactionError::Db(e.to_string()))?;

            // Index to vector store.
            indexer.index_entry(&entry_id, &ce.summary_text).await?;

            // Record changelog.
            let cl_id = db
                .append_changelog(
                    "compaction",
                    &format!(
                        "Compacted conversation {} into entry {}",
                        conversation_id, entry_id
                    ),
                )
                .map_err(|e| CompactionError::Db(e.to_string()))?;

            db.link_changelog_entry(cl_id, &entry_id)
                .map_err(|e| CompactionError::Db(e.to_string()))?;

            entry_ids.push(entry_id);
        }

        // Archive current conversation, create new one.
        conversation_mgr.archive_conversation(conversation_id)?;
        let new_conversation_id = conversation_mgr.create_conversation()?;

        Ok(CompactionOutcome::Compacted(CompactionResult {
            entries_created: entry_ids,
            conversation_id: conversation_id.to_string(),
            new_conversation_id,
            message_count: messages.len(),
        }))
    }

    /// Create an idle timer bound to this manager's activity signal.
    pub fn idle_timer(&self) -> IdleTimer {
        IdleTimer {
            idle_duration: Duration::from_secs(self.config.idle_trigger_minutes * 60),
            activity_notify: Arc::clone(&self.activity_notify),
        }
    }
}

// ---------------------------------------------------------------------------
// IdleTimer
// ---------------------------------------------------------------------------

/// A timer that waits for an idle period to elapse without activity.
/// Activity notifications (via `CompactionManager::notify_activity`) reset it.
pub struct IdleTimer {
    idle_duration: Duration,
    activity_notify: Arc<Notify>,
}

impl IdleTimer {
    /// Wait until the full idle period elapses without any activity.
    /// Returns when compaction should be triggered.
    pub async fn wait_for_idle(&self) {
        loop {
            tokio::select! {
                () = tokio::time::sleep(self.idle_duration) => {
                    return;
                }
                () = self.activity_notify.notified() => {
                    // Activity detected — reset timer by restarting loop.
                    continue;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex as StdMutex;

    // -- Mock implementations ------------------------------------------------

    struct MockLlm {
        response: Vec<CompactedEntry>,
    }

    impl CompactionLlm for MockLlm {
        fn summarize(
            &self,
            _prompt: &str,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<CompactedEntry>, CompactionError>> + Send + '_>>
        {
            let result = Ok(self.response.clone());
            Box::pin(async move { result })
        }
    }

    struct MockIndexer {
        indexed: StdMutex<Vec<(String, String)>>,
    }

    impl MockIndexer {
        fn new() -> Self {
            Self {
                indexed: StdMutex::new(Vec::new()),
            }
        }

        fn indexed_entries(&self) -> Vec<(String, String)> {
            self.indexed.lock().unwrap().clone()
        }
    }

    impl VectorIndexer for MockIndexer {
        fn index_entry(
            &self,
            entry_id: &str,
            text: &str,
        ) -> Pin<Box<dyn Future<Output = Result<(), CompactionError>> + Send + '_>> {
            self.indexed
                .lock()
                .unwrap()
                .push((entry_id.to_string(), text.to_string()));
            Box::pin(async { Ok(()) })
        }
    }

    struct MockConversationMgr {
        archived: StdMutex<Vec<String>>,
        next_id: String,
    }

    impl MockConversationMgr {
        fn new(next_id: &str) -> Self {
            Self {
                archived: StdMutex::new(Vec::new()),
                next_id: next_id.to_string(),
            }
        }

        fn archived_conversations(&self) -> Vec<String> {
            self.archived.lock().unwrap().clone()
        }
    }

    impl ConversationManager for MockConversationMgr {
        fn archive_conversation(&self, conversation_id: &str) -> Result<(), CompactionError> {
            self.archived
                .lock()
                .unwrap()
                .push(conversation_id.to_string());
            Ok(())
        }

        fn create_conversation(&self) -> Result<String, CompactionError> {
            Ok(self.next_id.clone())
        }
    }

    // -- Helpers --------------------------------------------------------------

    fn make_messages(count: usize) -> Vec<ConversationMessage> {
        (0..count)
            .map(|i| ConversationMessage {
                role: if i % 2 == 0 {
                    "user".to_string()
                } else {
                    "assistant".to_string()
                },
                content: format!("Message {i}"),
                timestamp: Utc::now().to_rfc3339(),
            })
            .collect()
    }

    fn make_compacted_entries() -> Vec<CompactedEntry> {
        vec![
            CompactedEntry {
                memory_type: "episodic".to_string(),
                summary_text: "User discussed their day".to_string(),
                topic_tags: "daily,personal".to_string(),
                topic_key: "daily_life".to_string(),
                confidence: 0.85,
            },
            CompactedEntry {
                memory_type: "semantic".to_string(),
                summary_text: "User prefers tea over coffee".to_string(),
                topic_tags: "preference,food".to_string(),
                topic_key: "preferences".to_string(),
                confidence: 0.95,
            },
        ]
    }

    // -- Tests: mock LLM compaction, verify entries created -------------------

    #[tokio::test]
    async fn test_compact_creates_entries() {
        let db = MemoryDB::open_in_memory().unwrap();
        let llm = MockLlm {
            response: make_compacted_entries(),
        };
        let indexer = MockIndexer::new();
        let conv_mgr = MockConversationMgr::new("new-conv-1");
        let mgr = CompactionManager::new(CompactionConfig::default());
        let messages = make_messages(10);

        let result = mgr
            .compact(
                "conv-1",
                &messages,
                false,
                DEFAULT_COMPACT_PROMPT,
                &llm,
                &db,
                &indexer,
                &conv_mgr,
                false,
            )
            .await
            .unwrap();

        match result {
            CompactionOutcome::Compacted(r) => {
                assert_eq!(r.entries_created.len(), 2);
                assert_eq!(r.conversation_id, "conv-1");
                assert_eq!(r.new_conversation_id, "new-conv-1");
                assert_eq!(r.message_count, 10);

                // Verify entries exist in DB with correct fields.
                for id in &r.entries_created {
                    let entry = db.get_entry(id).unwrap().unwrap();
                    assert_eq!(entry.reason, "compaction");
                    assert_eq!(entry.source, "summary");
                    assert_eq!(entry.status, "active");
                }

                let e1 = db.get_entry(&r.entries_created[0]).unwrap().unwrap();
                assert_eq!(e1.summary_text, "User discussed their day");
                assert_eq!(e1.memory_type, "episodic");
                assert_eq!(e1.confidence, 0.85);

                let e2 = db.get_entry(&r.entries_created[1]).unwrap().unwrap();
                assert_eq!(e2.summary_text, "User prefers tea over coffee");
                assert_eq!(e2.memory_type, "semantic");
                assert_eq!(e2.confidence, 0.95);
            }
            _ => panic!("Expected Compacted outcome"),
        }
    }

    #[tokio::test]
    async fn test_compact_indexes_to_vector_store() {
        let db = MemoryDB::open_in_memory().unwrap();
        let llm = MockLlm {
            response: make_compacted_entries(),
        };
        let indexer = MockIndexer::new();
        let conv_mgr = MockConversationMgr::new("new-conv-1");
        let mgr = CompactionManager::new(CompactionConfig::default());

        mgr.compact(
            "conv-1",
            &make_messages(10),
            false,
            DEFAULT_COMPACT_PROMPT,
            &llm,
            &db,
            &indexer,
            &conv_mgr,
            false,
        )
        .await
        .unwrap();

        let indexed = indexer.indexed_entries();
        assert_eq!(indexed.len(), 2);
        assert_eq!(indexed[0].1, "User discussed their day");
        assert_eq!(indexed[1].1, "User prefers tea over coffee");
    }

    #[tokio::test]
    async fn test_compact_records_changelog() {
        let db = MemoryDB::open_in_memory().unwrap();
        let llm = MockLlm {
            response: make_compacted_entries(),
        };
        let indexer = MockIndexer::new();
        let conv_mgr = MockConversationMgr::new("new-conv-1");
        let mgr = CompactionManager::new(CompactionConfig::default());

        mgr.compact(
            "conv-1",
            &make_messages(10),
            false,
            DEFAULT_COMPACT_PROMPT,
            &llm,
            &db,
            &indexer,
            &conv_mgr,
            false,
        )
        .await
        .unwrap();

        let logs = db.get_recent_changelog(10).unwrap();
        assert_eq!(logs.len(), 2);
        assert!(logs.iter().all(|l| l.operation == "compaction"));
        assert!(logs.iter().all(|l| l.description.contains("conv-1")));
    }

    #[tokio::test]
    async fn test_compact_archives_and_creates_conversation() {
        let db = MemoryDB::open_in_memory().unwrap();
        let llm = MockLlm {
            response: make_compacted_entries(),
        };
        let indexer = MockIndexer::new();
        let conv_mgr = MockConversationMgr::new("new-conv-2");
        let mgr = CompactionManager::new(CompactionConfig::default());

        let result = mgr
            .compact(
                "old-conv",
                &make_messages(5),
                false,
                DEFAULT_COMPACT_PROMPT,
                &llm,
                &db,
                &indexer,
                &conv_mgr,
                false,
            )
            .await
            .unwrap();

        match result {
            CompactionOutcome::Compacted(r) => {
                assert_eq!(r.new_conversation_id, "new-conv-2");
            }
            _ => panic!("Expected Compacted outcome"),
        }

        assert_eq!(conv_mgr.archived_conversations(), vec!["old-conv"]);
    }

    // -- Tests: private conversation skips compaction -------------------------

    #[tokio::test]
    async fn test_private_conversation_skips_compaction() {
        let db = MemoryDB::open_in_memory().unwrap();
        let llm = MockLlm {
            response: make_compacted_entries(),
        };
        let indexer = MockIndexer::new();
        let conv_mgr = MockConversationMgr::new("new-conv-1");
        let mgr = CompactionManager::new(CompactionConfig::default());

        let result = mgr
            .compact(
                "private-conv",
                &make_messages(10),
                true,
                DEFAULT_COMPACT_PROMPT,
                &llm,
                &db,
                &indexer,
                &conv_mgr,
                false,
            )
            .await;

        assert!(matches!(result, Err(CompactionError::PrivateConversation)));

        // No side effects.
        assert!(db.get_entries_by_status("active").unwrap().is_empty());
        assert!(indexer.indexed_entries().is_empty());
        assert!(conv_mgr.archived_conversations().is_empty());
    }

    // -- Tests: dry run -------------------------------------------------------

    #[tokio::test]
    async fn test_compact_dry_run() {
        let db = MemoryDB::open_in_memory().unwrap();
        let llm = MockLlm {
            response: make_compacted_entries(),
        };
        let indexer = MockIndexer::new();
        let conv_mgr = MockConversationMgr::new("new-conv-1");
        let mgr = CompactionManager::new(CompactionConfig::default());

        let result = mgr
            .compact(
                "conv-1",
                &make_messages(10),
                false,
                DEFAULT_COMPACT_PROMPT,
                &llm,
                &db,
                &indexer,
                &conv_mgr,
                true,
            )
            .await
            .unwrap();

        match result {
            CompactionOutcome::DryRun(r) => {
                assert_eq!(r.would_create_entries, 2);
                assert_eq!(r.message_count, 10);
                assert_eq!(r.entries_preview.len(), 2);
                assert_eq!(r.entries_preview[0].summary_text, "User discussed their day");
            }
            _ => panic!("Expected DryRun outcome"),
        }

        // No side effects.
        assert!(db.get_entries_by_status("active").unwrap().is_empty());
        assert!(indexer.indexed_entries().is_empty());
        assert!(conv_mgr.archived_conversations().is_empty());
    }

    // -- Tests: message count trigger -----------------------------------------

    #[test]
    fn test_should_compact_by_count() {
        let mgr = CompactionManager::new(CompactionConfig {
            message_count_threshold: 50,
            ..Default::default()
        });

        assert!(!mgr.should_compact_by_count(0));
        assert!(!mgr.should_compact_by_count(49));
        assert!(mgr.should_compact_by_count(50));
        assert!(mgr.should_compact_by_count(100));
    }

    // -- Tests: prompt building -----------------------------------------------

    #[test]
    fn test_build_prompt() {
        let messages = vec![
            ConversationMessage {
                role: "user".to_string(),
                content: "Hello!".to_string(),
                timestamp: "2026-03-25T10:00:00Z".to_string(),
            },
            ConversationMessage {
                role: "assistant".to_string(),
                content: "Hi there!".to_string(),
                timestamp: "2026-03-25T10:00:01Z".to_string(),
            },
        ];

        let prompt =
            CompactionManager::build_prompt("Template:\n{{conversation}}", &messages);
        assert!(prompt.contains("[2026-03-25T10:00:00Z] user: Hello!"));
        assert!(prompt.contains("[2026-03-25T10:00:01Z] assistant: Hi there!"));
        assert!(!prompt.contains("{{conversation}}"));
    }

    // -- Tests: idle timer scheduling logic -----------------------------------

    #[tokio::test]
    async fn test_idle_timer_fires_after_duration() {
        tokio::time::pause();

        let mgr = CompactionManager::new(CompactionConfig {
            idle_trigger_minutes: 5,
            ..Default::default()
        });

        let timer = mgr.idle_timer();
        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = Arc::clone(&fired);

        let handle = tokio::spawn(async move {
            timer.wait_for_idle().await;
            fired_clone.store(true, Ordering::SeqCst);
        });

        // 4 minutes — should NOT have fired.
        tokio::time::advance(Duration::from_secs(4 * 60)).await;
        tokio::task::yield_now().await;
        assert!(!fired.load(Ordering::SeqCst));

        // 1 more minute (total 5) — should fire.
        tokio::time::advance(Duration::from_secs(60)).await;
        handle.await.unwrap();
        assert!(fired.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_idle_timer_resets_on_activity() {
        tokio::time::pause();

        let mgr = CompactionManager::new(CompactionConfig {
            idle_trigger_minutes: 5,
            ..Default::default()
        });

        let timer = mgr.idle_timer();
        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = Arc::clone(&fired);

        let handle = tokio::spawn(async move {
            timer.wait_for_idle().await;
            fired_clone.store(true, Ordering::SeqCst);
        });

        // Advance 4 minutes — should NOT have fired.
        tokio::time::advance(Duration::from_secs(4 * 60)).await;
        tokio::task::yield_now().await;
        assert!(!fired.load(Ordering::SeqCst));

        // Notify activity — resets timer.
        mgr.notify_activity();
        tokio::task::yield_now().await;

        // 4 more minutes since reset — still should NOT have fired.
        tokio::time::advance(Duration::from_secs(4 * 60)).await;
        tokio::task::yield_now().await;
        assert!(!fired.load(Ordering::SeqCst));

        // 1 more minute (5 since reset) — should fire.
        tokio::time::advance(Duration::from_secs(60)).await;
        handle.await.unwrap();
        assert!(fired.load(Ordering::SeqCst));
    }

    // -- Tests: edge cases ----------------------------------------------------

    #[tokio::test]
    async fn test_compact_empty_messages() {
        let db = MemoryDB::open_in_memory().unwrap();
        let llm = MockLlm { response: vec![] };
        let indexer = MockIndexer::new();
        let conv_mgr = MockConversationMgr::new("new-conv-1");
        let mgr = CompactionManager::new(CompactionConfig::default());

        let result = mgr
            .compact(
                "conv-1",
                &[],
                false,
                DEFAULT_COMPACT_PROMPT,
                &llm,
                &db,
                &indexer,
                &conv_mgr,
                false,
            )
            .await;

        assert!(matches!(result, Err(CompactionError::InsufficientMessages)));
    }
}
