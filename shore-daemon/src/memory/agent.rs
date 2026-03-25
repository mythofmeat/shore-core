use crate::memory::db::{Entry, MemoryDB};
use chrono::Utc;
use std::future::Future;
use std::pin::Pin;

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
    /// Interactive memory shell session (stub — deferred per §3.8).
    Interactive,
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A structured result from a one-shot memory query.
#[derive(Debug, Clone)]
pub struct MemoryQueryResult {
    pub entries: Vec<RetrievedEntry>,
    pub query_text: String,
    pub resolved_query: String,
}

/// A single entry returned from a memory query.
#[derive(Debug, Clone)]
pub struct RetrievedEntry {
    pub entry_id: String,
    pub summary_text: String,
    pub memory_type: String,
    pub confidence: f64,
    pub relevance_score: f64,
}

/// Result of a memory write operation (create/update/supersede).
#[derive(Debug, Clone)]
pub struct MemoryWriteResult {
    pub entry_id: String,
    pub operation: String,
    pub indexed: bool,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum AgentError {
    Db(String),
    Rag(String),
    Indexing(String),
    InteractiveNotImplemented,
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::Db(e) => write!(f, "db: {e}"),
            AgentError::Rag(e) => write!(f, "rag: {e}"),
            AgentError::Indexing(e) => write!(f, "indexing: {e}"),
            AgentError::InteractiveNotImplemented => {
                write!(f, "interactive mode not yet implemented")
            }
        }
    }
}

impl std::error::Error for AgentError {}

// ---------------------------------------------------------------------------
// Traits for external dependencies
// ---------------------------------------------------------------------------

/// RAG retrieval: takes a query string, returns scored entry IDs.
pub trait AgentRag: Send + Sync {
    fn query(
        &self,
        query: &str,
        top_k: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RagHit>, AgentError>> + Send + '_>>;
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
    ) -> Pin<Box<dyn Future<Output = Result<(), AgentError>> + Send + '_>>;
}

// ---------------------------------------------------------------------------
// Pronoun resolution
// ---------------------------------------------------------------------------

/// Resolve first-person pronouns in a query based on caller identity.
///
/// When the caller is `Char`, "I"/"me"/"my" refer to the character.
/// When the caller is `User`, "I"/"me"/"my" refer to the user.
pub fn resolve_pronouns(query: &str, caller: CallerIdentity, name: &str) -> String {
    let mut result = query.to_string();

    // Replace whole-word first-person pronouns with the caller's name.
    // Order matters: replace longer patterns first to avoid partial matches.
    // Both caller variants resolve the same way — we just use the provided name.
    let _ = caller;
    let replacements: &[(&str, &str)] = &[
        ("my ", &format!("{name}'s ")),
        ("My ", &format!("{name}'s ")),
        ("I ", &format!("{name} ")),
        (" me ", &format!(" {name} ")),
        (" me.", &format!(" {name}.")),
        (" me?", &format!(" {name}?")),
        (" me!", &format!(" {name}!")),
        ("myself", name),
    ];

    for &(pattern, replacement) in replacements {
        result = result.replace(pattern, replacement);
    }

    result
}

// ---------------------------------------------------------------------------
// MemoryAgent
// ---------------------------------------------------------------------------

pub struct MemoryAgent {
    /// Who is calling the agent.
    caller: CallerIdentity,
    /// The name to substitute for first-person pronouns.
    caller_name: String,
    /// Operating mode.
    mode: AgentMode,
    /// Maximum results for RAG queries.
    top_k: usize,
}

impl MemoryAgent {
    /// Create a new memory agent for a one-shot tool call.
    pub fn one_shot(caller: CallerIdentity, caller_name: &str) -> Self {
        Self {
            caller,
            caller_name: caller_name.to_string(),
            mode: AgentMode::OneShot,
            top_k: 32,
        }
    }

    /// Create a new memory agent for an interactive session (stub).
    pub fn interactive(caller: CallerIdentity, caller_name: &str) -> Self {
        Self {
            caller,
            caller_name: caller_name.to_string(),
            mode: AgentMode::Interactive,
            top_k: 32,
        }
    }

    pub fn caller(&self) -> CallerIdentity {
        self.caller
    }

    pub fn caller_name(&self) -> &str {
        &self.caller_name
    }

    pub fn mode(&self) -> AgentMode {
        self.mode
    }

    /// Execute a natural language memory query (one-shot mode).
    ///
    /// 1. Resolves first-person pronouns based on caller identity.
    /// 2. Queries RAG for relevant entries.
    /// 3. Fetches full entries from DB.
    /// 4. Returns structured results.
    pub async fn query(
        &self,
        request: &str,
        rag: &dyn AgentRag,
        db: &MemoryDB,
    ) -> Result<MemoryQueryResult, AgentError> {
        if self.mode == AgentMode::Interactive {
            return Err(AgentError::InteractiveNotImplemented);
        }

        // Step 1: Resolve pronouns.
        let resolved = resolve_pronouns(request, self.caller, &self.caller_name);

        // Step 2: Query RAG.
        let hits = rag.query(&resolved, self.top_k).await?;

        // Step 3: Fetch full entries and build results.
        let mut entries = Vec::new();
        for hit in &hits {
            if let Some(entry) = db
                .get_entry(&hit.entry_id)
                .map_err(|e| AgentError::Db(e.to_string()))?
            {
                entries.push(RetrievedEntry {
                    entry_id: entry.id,
                    summary_text: entry.summary_text,
                    memory_type: entry.memory_type,
                    confidence: entry.confidence,
                    relevance_score: hit.score,
                });
            }
        }

        Ok(MemoryQueryResult {
            entries,
            query_text: request.to_string(),
            resolved_query: resolved,
        })
    }

    /// Create a new memory entry and index it to the vector store.
    pub async fn create_entry(
        &self,
        entry: &Entry,
        db: &MemoryDB,
        indexer: &dyn AgentIndexer,
    ) -> Result<MemoryWriteResult, AgentError> {
        db.create_entry(entry)
            .map_err(|e| AgentError::Db(e.to_string()))?;

        let indexed = indexer
            .index_entry(&entry.id, &entry.summary_text)
            .await
            .is_ok();

        let cl_id = db
            .append_changelog(
                "agent_create",
                &format!("Memory agent created entry {}", entry.id),
            )
            .map_err(|e| AgentError::Db(e.to_string()))?;
        db.link_changelog_entry(cl_id, &entry.id)
            .map_err(|e| AgentError::Db(e.to_string()))?;

        Ok(MemoryWriteResult {
            entry_id: entry.id.clone(),
            operation: "create".to_string(),
            indexed,
        })
    }

    /// Update an existing entry and re-index it.
    pub async fn update_entry(
        &self,
        entry: &Entry,
        db: &MemoryDB,
        indexer: &dyn AgentIndexer,
    ) -> Result<MemoryWriteResult, AgentError> {
        db.update_entry(entry)
            .map_err(|e| AgentError::Db(e.to_string()))?;

        let indexed = indexer
            .index_entry(&entry.id, &entry.summary_text)
            .await
            .is_ok();

        let cl_id = db
            .append_changelog(
                "agent_update",
                &format!("Memory agent updated entry {}", entry.id),
            )
            .map_err(|e| AgentError::Db(e.to_string()))?;
        db.link_changelog_entry(cl_id, &entry.id)
            .map_err(|e| AgentError::Db(e.to_string()))?;

        Ok(MemoryWriteResult {
            entry_id: entry.id.clone(),
            operation: "update".to_string(),
            indexed,
        })
    }

    /// Supersede an old entry with a new one. Creates the new entry, marks
    /// the old one as superseded, and indexes the new entry.
    pub async fn supersede_entry(
        &self,
        old_id: &str,
        new_entry: &Entry,
        db: &MemoryDB,
        indexer: &dyn AgentIndexer,
    ) -> Result<MemoryWriteResult, AgentError> {
        db.create_entry(new_entry)
            .map_err(|e| AgentError::Db(e.to_string()))?;

        db.supersede_entry(old_id, &new_entry.id)
            .map_err(|e| AgentError::Db(e.to_string()))?;

        let indexed = indexer
            .index_entry(&new_entry.id, &new_entry.summary_text)
            .await
            .is_ok();

        let cl_id = db
            .append_changelog(
                "agent_supersede",
                &format!(
                    "Memory agent superseded entry {} with {}",
                    old_id, new_entry.id
                ),
            )
            .map_err(|e| AgentError::Db(e.to_string()))?;
        db.link_changelog_entry(cl_id, &new_entry.id)
            .map_err(|e| AgentError::Db(e.to_string()))?;

        Ok(MemoryWriteResult {
            entry_id: new_entry.id.clone(),
            operation: "supersede".to_string(),
            indexed,
        })
    }

    /// Generate an entry ID in the standard format: YYYYMMDD_HHMMSS_N
    pub fn generate_entry_id(index: usize) -> String {
        let now = Utc::now();
        format!("{}_{}", now.format("%Y%m%d_%H%M%S"), index)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    // -- Mock implementations ------------------------------------------------

    struct MockRag {
        results: Vec<RagHit>,
    }

    impl AgentRag for MockRag {
        fn query(
            &self,
            _query: &str,
            _top_k: usize,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<RagHit>, AgentError>> + Send + '_>> {
            let result = Ok(self.results.clone());
            Box::pin(async move { result })
        }
    }

    /// A mock RAG that captures the resolved query for assertion.
    struct CapturingRag {
        results: Vec<RagHit>,
        captured_query: StdMutex<Option<String>>,
    }

    impl CapturingRag {
        fn new(results: Vec<RagHit>) -> Self {
            Self {
                results,
                captured_query: StdMutex::new(None),
            }
        }

        fn captured_query(&self) -> Option<String> {
            self.captured_query.lock().unwrap().clone()
        }
    }

    impl AgentRag for CapturingRag {
        fn query(
            &self,
            query: &str,
            _top_k: usize,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<RagHit>, AgentError>> + Send + '_>> {
            *self.captured_query.lock().unwrap() = Some(query.to_string());
            let result = Ok(self.results.clone());
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

    impl AgentIndexer for MockIndexer {
        fn index_entry(
            &self,
            entry_id: &str,
            text: &str,
        ) -> Pin<Box<dyn Future<Output = Result<(), AgentError>> + Send + '_>> {
            self.indexed
                .lock()
                .unwrap()
                .push((entry_id.to_string(), text.to_string()));
            Box::pin(async { Ok(()) })
        }
    }

    // -- Helpers --------------------------------------------------------------

    fn make_entry(id: &str, summary: &str) -> Entry {
        let now = Utc::now().to_rfc3339();
        Entry {
            id: id.to_string(),
            memory_type: "semantic".to_string(),
            source: "agent".to_string(),
            reason: "tool_call".to_string(),
            status: "active".to_string(),
            canonical: false,
            confidence: 0.9,
            summary_text: summary.to_string(),
            topic_tags: "test".to_string(),
            topic_key: "test".to_string(),
            start_timestamp: now.clone(),
            end_timestamp: now.clone(),
            message_count: 0,
            source_entry_ids: String::new(),
            related_entry_ids: String::new(),
            superseded_by: String::new(),
            created_at: now.clone(),
            updated_at: now,
            entry_type: String::new(),
            image_path: String::new(),
        }
    }

    // -- Tests: caller identity -----------------------------------------------

    #[test]
    fn test_caller_identity_char_mode() {
        let agent = MemoryAgent::one_shot(CallerIdentity::Char, "Alice");
        assert_eq!(agent.caller(), CallerIdentity::Char);
        assert_eq!(agent.caller_name(), "Alice");
        assert_eq!(agent.mode(), AgentMode::OneShot);
    }

    #[test]
    fn test_caller_identity_user_mode() {
        let agent = MemoryAgent::interactive(CallerIdentity::User, "Bob");
        assert_eq!(agent.caller(), CallerIdentity::User);
        assert_eq!(agent.caller_name(), "Bob");
        assert_eq!(agent.mode(), AgentMode::Interactive);
    }

    // -- Tests: pronoun resolution --------------------------------------------

    #[test]
    fn test_pronoun_resolution_char_caller() {
        let resolved = resolve_pronouns(
            "What do I like to eat?",
            CallerIdentity::Char,
            "Alice",
        );
        assert_eq!(resolved, "What do Alice like to eat?");
    }

    #[test]
    fn test_pronoun_resolution_user_caller() {
        let resolved = resolve_pronouns(
            "What do I like to eat?",
            CallerIdentity::User,
            "Bob",
        );
        assert_eq!(resolved, "What do Bob like to eat?");
    }

    #[test]
    fn test_pronoun_resolution_my() {
        let resolved = resolve_pronouns(
            "my favorite color",
            CallerIdentity::Char,
            "Alice",
        );
        assert_eq!(resolved, "Alice's favorite color");
    }

    #[test]
    fn test_pronoun_resolution_me() {
        let resolved = resolve_pronouns(
            "tell me about me.",
            CallerIdentity::User,
            "Bob",
        );
        assert_eq!(resolved, "tell Bob about Bob.");
    }

    #[test]
    fn test_pronoun_resolution_no_pronouns() {
        let resolved = resolve_pronouns(
            "What does Alice like?",
            CallerIdentity::Char,
            "Alice",
        );
        assert_eq!(resolved, "What does Alice like?");
    }

    #[test]
    fn test_pronoun_resolution_myself() {
        let resolved = resolve_pronouns(
            "things about myself",
            CallerIdentity::User,
            "Bob",
        );
        assert_eq!(resolved, "things about Bob");
    }

    // -- Tests: one-shot query ------------------------------------------------

    #[tokio::test]
    async fn test_one_shot_query_returns_results() {
        let db = MemoryDB::open_in_memory().unwrap();

        // Seed entries in DB.
        let e1 = make_entry("e1", "Alice likes chocolate");
        let e2 = make_entry("e2", "Alice dislikes rain");
        db.create_entry(&e1).unwrap();
        db.create_entry(&e2).unwrap();

        let rag = MockRag {
            results: vec![
                RagHit {
                    entry_id: "e1".to_string(),
                    score: 0.95,
                },
                RagHit {
                    entry_id: "e2".to_string(),
                    score: 0.7,
                },
            ],
        };

        let agent = MemoryAgent::one_shot(CallerIdentity::Char, "Alice");
        let result = agent.query("What do I like?", &rag, &db).await.unwrap();

        assert_eq!(result.entries.len(), 2);
        assert_eq!(result.entries[0].entry_id, "e1");
        assert_eq!(result.entries[0].summary_text, "Alice likes chocolate");
        assert_eq!(result.entries[0].relevance_score, 0.95);
        assert_eq!(result.entries[1].entry_id, "e2");
        assert_eq!(result.query_text, "What do I like?");
        // Resolved query should have "I" replaced with "Alice"
        assert!(result.resolved_query.contains("Alice"));
    }

    #[tokio::test]
    async fn test_one_shot_query_resolves_pronouns_before_rag() {
        let db = MemoryDB::open_in_memory().unwrap();

        let rag = CapturingRag::new(vec![]);
        let agent = MemoryAgent::one_shot(CallerIdentity::Char, "Alice");
        let _result = agent.query("my favorite food", &rag, &db).await.unwrap();

        // The RAG should have received the resolved query.
        let captured = rag.captured_query().unwrap();
        assert_eq!(captured, "Alice's favorite food");
    }

    #[tokio::test]
    async fn test_one_shot_empty_results() {
        let db = MemoryDB::open_in_memory().unwrap();
        let rag = MockRag { results: vec![] };

        let agent = MemoryAgent::one_shot(CallerIdentity::User, "Bob");
        let result = agent
            .query("something obscure", &rag, &db)
            .await
            .unwrap();

        assert!(result.entries.is_empty());
    }

    #[tokio::test]
    async fn test_one_shot_missing_entry_skipped() {
        let db = MemoryDB::open_in_memory().unwrap();

        // RAG returns an entry ID that doesn't exist in DB.
        let rag = MockRag {
            results: vec![RagHit {
                entry_id: "nonexistent".to_string(),
                score: 0.8,
            }],
        };

        let agent = MemoryAgent::one_shot(CallerIdentity::Char, "Alice");
        let result = agent.query("anything", &rag, &db).await.unwrap();

        // Missing entry should be silently skipped.
        assert!(result.entries.is_empty());
    }

    // -- Tests: interactive mode stub -----------------------------------------

    #[tokio::test]
    async fn test_interactive_mode_returns_error() {
        let db = MemoryDB::open_in_memory().unwrap();
        let rag = MockRag { results: vec![] };

        let agent = MemoryAgent::interactive(CallerIdentity::User, "Bob");
        let result = agent.query("anything", &rag, &db).await;

        assert!(matches!(result, Err(AgentError::InteractiveNotImplemented)));
    }

    // -- Tests: create entry with indexing ------------------------------------

    #[tokio::test]
    async fn test_create_entry_indexes_to_vector_store() {
        let db = MemoryDB::open_in_memory().unwrap();
        let indexer = MockIndexer::new();

        let agent = MemoryAgent::one_shot(CallerIdentity::Char, "Alice");
        let entry = make_entry("e1", "Alice likes chocolate");

        let result = agent.create_entry(&entry, &db, &indexer).await.unwrap();

        assert_eq!(result.entry_id, "e1");
        assert_eq!(result.operation, "create");
        assert!(result.indexed);

        // Verify entry exists in DB.
        let stored = db.get_entry("e1").unwrap().unwrap();
        assert_eq!(stored.summary_text, "Alice likes chocolate");

        // Verify indexed.
        let indexed = indexer.indexed_entries();
        assert_eq!(indexed.len(), 1);
        assert_eq!(indexed[0].0, "e1");
        assert_eq!(indexed[0].1, "Alice likes chocolate");

        // Verify changelog.
        let logs = db.get_recent_changelog(10).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].operation, "agent_create");
    }

    // -- Tests: update entry with re-indexing ---------------------------------

    #[tokio::test]
    async fn test_update_entry_reindexes() {
        let db = MemoryDB::open_in_memory().unwrap();
        let indexer = MockIndexer::new();

        let agent = MemoryAgent::one_shot(CallerIdentity::Char, "Alice");
        let entry = make_entry("e1", "Alice likes chocolate");
        db.create_entry(&entry).unwrap();

        let mut updated = entry.clone();
        updated.summary_text = "Alice loves dark chocolate".to_string();
        updated.updated_at = Utc::now().to_rfc3339();

        let result = agent.update_entry(&updated, &db, &indexer).await.unwrap();

        assert_eq!(result.operation, "update");
        assert!(result.indexed);

        let indexed = indexer.indexed_entries();
        assert_eq!(indexed.len(), 1);
        assert_eq!(indexed[0].1, "Alice loves dark chocolate");

        let logs = db.get_recent_changelog(10).unwrap();
        assert_eq!(logs[0].operation, "agent_update");
    }

    // -- Tests: supersede entry -----------------------------------------------

    #[tokio::test]
    async fn test_supersede_entry_creates_and_marks_old() {
        let db = MemoryDB::open_in_memory().unwrap();
        let indexer = MockIndexer::new();

        let agent = MemoryAgent::one_shot(CallerIdentity::Char, "Alice");

        // Create original entry.
        let old_entry = make_entry("old_e", "Alice likes milk chocolate");
        db.create_entry(&old_entry).unwrap();

        // Supersede with new entry.
        let new_entry = make_entry("new_e", "Alice prefers dark chocolate");
        let result = agent
            .supersede_entry("old_e", &new_entry, &db, &indexer)
            .await
            .unwrap();

        assert_eq!(result.entry_id, "new_e");
        assert_eq!(result.operation, "supersede");
        assert!(result.indexed);

        // Old entry should be superseded.
        let old = db.get_entry("old_e").unwrap().unwrap();
        assert_eq!(old.status, "superseded");
        assert_eq!(old.superseded_by, "new_e");

        // New entry should exist and be active.
        let new = db.get_entry("new_e").unwrap().unwrap();
        assert_eq!(new.status, "active");
        assert_eq!(new.summary_text, "Alice prefers dark chocolate");

        // Only new entry should be indexed.
        let indexed = indexer.indexed_entries();
        assert_eq!(indexed.len(), 1);
        assert_eq!(indexed[0].0, "new_e");

        // Verify changelog.
        let logs = db.get_recent_changelog(10).unwrap();
        assert_eq!(logs[0].operation, "agent_supersede");
    }
}
