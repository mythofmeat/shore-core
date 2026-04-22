//! Types shared across the memory agent module.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use shore_llm_client::LlmClient;

use crate::memory::compaction_impls::EmbedConfig;
use crate::memory::db::MemoryDB;
use crate::memory::search::Bm25Index;
use crate::memory::vectorstore::VectorStore;
use crate::sync::lock_or_recover;

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

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("db: {0}")]
    Db(String),
    #[error("rag: {0}")]
    Rag(String),
    #[error("indexing: {0}")]
    Indexing(String),
    #[error("llm: {0}")]
    Llm(String),
    #[error("agent loop reached maximum iterations")]
    MaxIterations,
}

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
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), AgentError>> + Send + '_>>;
}

// ---------------------------------------------------------------------------
// Semantic search context
// ---------------------------------------------------------------------------

/// Dependencies for semantic search within the agent tool loop.
///
/// When present, enables the `semantic_search` tool (hybrid vector + BM25
/// with reciprocal rank fusion). When absent, the tool returns an error
/// and the agent falls back to FTS5-only `search_entries`.
pub struct AgentSearchContext {
    pub vector_store: Arc<VectorStore>,
    pub bm25: Mutex<Bm25Index>,
    pub llm_client: LlmClient,
    pub embed_config: EmbedConfig,
    bm25_populated: AtomicBool,
}

impl AgentSearchContext {
    pub fn new(
        vector_store: Arc<VectorStore>,
        llm_client: LlmClient,
        embed_config: EmbedConfig,
    ) -> Self {
        Self {
            vector_store,
            bm25: Mutex::new(Bm25Index::new()),
            llm_client,
            embed_config,
            bm25_populated: AtomicBool::new(false),
        }
    }

    /// Lazy-populate the BM25 index from all active entries on first search.
    pub fn populate_bm25_if_needed(&self, db: &MemoryDB) -> Result<(), AgentError> {
        if self.bm25_populated.load(Ordering::Relaxed) {
            return Ok(());
        }

        let entries = db
            .get_entries_by_status("active")
            .map_err(|e| AgentError::Db(e.to_string()))?;

        let mut index = lock_or_recover("memory agent BM25 index", &self.bm25);
        for entry in &entries {
            index.add_document(&entry.id, &entry.summary_text);
        }
        drop(index);

        self.bm25_populated.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Embed a query string via the configured embedding model.
    pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>, AgentError> {
        let result = self
            .llm_client
            .embed(
                &self.embed_config.provider,
                &self.embed_config.model_id,
                &self.embed_config.api_key,
                self.embed_config.base_url.as_deref(),
                &[text],
            )
            .await
            .map_err(|e| AgentError::Rag(format!("embedding failed: {e}")))?;

        result
            .into_iter()
            .next()
            .ok_or_else(|| AgentError::Rag("empty embedding response".to_string()))
    }

    /// Update the BM25 index for a single entry (after create/update).
    pub fn bm25_update(&self, entry_id: &str, text: &str) {
        if let Ok(mut index) = self.bm25.lock() {
            if text.is_empty() {
                index.remove_document(entry_id);
            } else {
                index.add_document(entry_id, text);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Real agent indexer (vector + BM25)
// ---------------------------------------------------------------------------

/// Production `AgentIndexer` that embeds text and indexes into both the
/// vector store and the BM25 index via a shared `AgentSearchContext`.
pub struct RealAgentIndexer<'a> {
    ctx: &'a AgentSearchContext,
}

impl<'a> RealAgentIndexer<'a> {
    pub fn new(ctx: &'a AgentSearchContext) -> Self {
        Self { ctx }
    }
}

impl<'a> AgentIndexer for RealAgentIndexer<'a> {
    fn index_entry(
        &self,
        entry_id: &str,
        text: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), AgentError>> + Send + '_>> {
        let entry_id = entry_id.to_string();
        let text = text.to_string();

        Box::pin(async move {
            // Update BM25 index synchronously.
            self.ctx.bm25_update(&entry_id, &text);

            // Embed and store in vector store (best-effort for empty text).
            if text.is_empty() {
                return Ok(());
            }

            let embedding = self.ctx.embed_query(&text).await?;

            self.ctx
                .vector_store
                .index_entry(&entry_id, &embedding)
                .await
                .map_err(|e| AgentError::Indexing(e.to_string()))?;

            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::db::Entry;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use tempfile::TempDir;

    fn make_entry(id: &str, summary: &str) -> Entry {
        let now = chrono::Utc::now().to_rfc3339();
        Entry {
            id: id.to_string(),
            memory_type: "semantic".to_string(),
            source: "test".to_string(),
            reason: "test".to_string(),
            status: "active".to_string(),
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
            collated_at: String::new(),
            file_path: String::new(),
        }
    }

    #[tokio::test]
    async fn populate_bm25_recovers_from_poisoned_mutex() {
        let tmp = TempDir::new().unwrap();
        let vector_store = Arc::new(VectorStore::open(tmp.path(), 3).await.unwrap());
        let search_ctx = AgentSearchContext::new(
            vector_store,
            LlmClient::new(),
            EmbedConfig {
                provider: "test".into(),
                model_id: "test".into(),
                api_key: "test".into(),
                base_url: None,
                dimensions: 3,
            },
        );
        let db = MemoryDB::open_in_memory().unwrap();
        db.create_entry(&make_entry("entry-1", "Alice likes chocolate"))
            .unwrap();

        let result = catch_unwind(AssertUnwindSafe(|| {
            let _guard = search_ctx.bm25.lock().unwrap();
            panic!("poison memory agent BM25 populate");
        }));
        assert!(result.is_err());

        search_ctx.populate_bm25_if_needed(&db).unwrap();

        let hits = lock_or_recover("memory agent BM25 index", &search_ctx.bm25).search("Alice", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry_id, "entry-1");
    }
}
