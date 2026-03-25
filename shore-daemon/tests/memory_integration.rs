//! US-026: Full memory system milestone — end-to-end integration test.
//!
//! Exercises the complete memory pipeline with real SQLite, LanceDB, BM25,
//! and RAG components, using mock LLM traits for compaction and collation.
//!
//! Coverage:
//! - Multi-turn conversation simulation (5+ messages)
//! - Compaction → entries in SQLite + LanceDB vector store
//! - BM25 indexing + hybrid RAG retrieval
//! - Memory tool dispatch via ToolContext
//! - Collation (tidy, collate, normalize, decay)
//! - Privacy toggle suppresses memory tools and RAG
//! - Memory command returns entry counts and search results
//! - Memory agent create_entry persists entries

use chrono::Utc;
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use tempfile::TempDir;

use shore_daemon::commands::{self, CommandContext};
use shore_daemon::memory::agent::{
    AgentError, AgentIndexer, AgentRag, CallerIdentity, MemoryAgent, RagHit,
};
use shore_daemon::memory::collation::{
    CollateMerge, CollationConfig, CollationError, CollationLlm, CollationManager,
    EntityNormalization, TidyReplacement, TidySplit, DEFAULT_COLLATE_PROMPT,
    DEFAULT_NORMALIZE_PROMPT, DEFAULT_TIDY_PROMPT,
};
use shore_daemon::memory::compaction::{
    CompactedEntry, CompactionConfig, CompactionError, CompactionLlm, CompactionManager,
    CompactionOutcome, ConversationManager, ConversationMessage, VectorIndexer,
    DEFAULT_COMPACT_PROMPT,
};
use shore_daemon::memory::db::{Entry, MemoryDB};
use shore_daemon::memory::rag::{EntryMeta, RagPipeline, SourceResult};
use shore_daemon::memory::search::Bm25Index;
use shore_daemon::memory::vectorstore::VectorStore;
use shore_daemon::tools::{self, ToolContext};

// ---------------------------------------------------------------------------
// Simple deterministic embedding — 8-dimensional bag-of-words hash
// ---------------------------------------------------------------------------

fn simple_embed(text: &str) -> Vec<f32> {
    let dim = 8;
    let mut vec = vec![0.0f32; dim];
    for word in text.to_lowercase().split_whitespace() {
        let hash = word.bytes().fold(0u64, |acc, b| {
            acc.wrapping_mul(31).wrapping_add(b as u64)
        });
        let idx = (hash % dim as u64) as usize;
        vec[idx] += 1.0;
    }
    // L2 normalize
    let norm: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in &mut vec {
            *v /= norm;
        }
    }
    vec
}

// ---------------------------------------------------------------------------
// Mock LLM for compaction
// ---------------------------------------------------------------------------

struct MockCompactionLlm {
    response: Vec<CompactedEntry>,
}

impl CompactionLlm for MockCompactionLlm {
    fn summarize(
        &self,
        _prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CompactedEntry>, CompactionError>> + Send + '_>>
    {
        let result = Ok(self.response.clone());
        Box::pin(async move { result })
    }
}

// ---------------------------------------------------------------------------
// Mock LLM for collation
// ---------------------------------------------------------------------------

struct MockCollationLlm {
    tidy_response: Vec<TidySplit>,
    collate_response: Vec<CollateMerge>,
    normalize_response: Vec<EntityNormalization>,
}

impl MockCollationLlm {
    #[allow(dead_code)]
    fn empty() -> Self {
        Self {
            tidy_response: vec![],
            collate_response: vec![],
            normalize_response: vec![],
        }
    }
}

impl CollationLlm for MockCollationLlm {
    fn tidy(
        &self,
        _prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<TidySplit>, CollationError>> + Send + '_>> {
        let result = Ok(self.tidy_response.clone());
        Box::pin(async move { result })
    }

    fn collate(
        &self,
        _prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CollateMerge>, CollationError>> + Send + '_>> {
        let result = Ok(self.collate_response.clone());
        Box::pin(async move { result })
    }

    fn normalize_entities(
        &self,
        _prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<EntityNormalization>, CollationError>> + Send + '_>>
    {
        let result = Ok(self.normalize_response.clone());
        Box::pin(async move { result })
    }
}

// ---------------------------------------------------------------------------
// Real VectorStore-backed indexer (wraps VectorStore + simple_embed)
// ---------------------------------------------------------------------------

struct RealVectorIndexer<'a> {
    store: &'a VectorStore,
}

impl<'a> VectorIndexer for RealVectorIndexer<'a> {
    fn index_entry(
        &self,
        entry_id: &str,
        text: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), CompactionError>> + Send + '_>> {
        let embedding = simple_embed(text);
        let id = entry_id.to_string();
        Box::pin(async move {
            self.store
                .index_entry(&id, &embedding)
                .await
                .map_err(|e| CompactionError::Indexing(e.to_string()))
        })
    }
}

// ---------------------------------------------------------------------------
// Mock conversation manager
// ---------------------------------------------------------------------------

struct MockConversationMgr {
    next_id: String,
}

impl ConversationManager for MockConversationMgr {
    fn archive_conversation(&self, _conversation_id: &str) -> Result<(), CompactionError> {
        Ok(())
    }
    fn create_conversation(&self) -> Result<String, CompactionError> {
        Ok(self.next_id.clone())
    }
}

// ---------------------------------------------------------------------------
// Pre-computed RAG: holds results from a prior RAG pipeline run.
// Implements AgentRag (Send + Sync) without holding &MemoryDB.
// ---------------------------------------------------------------------------

struct PrecomputedRag {
    results: Vec<RagHit>,
}

impl AgentRag for PrecomputedRag {
    fn query(
        &self,
        _query: &str,
        _top_k: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RagHit>, AgentError>> + Send + '_>> {
        let result = Ok(self.results.clone());
        Box::pin(async move { result })
    }
}

// ---------------------------------------------------------------------------
// VectorStore-backed AgentIndexer
// ---------------------------------------------------------------------------

struct VectorIndexerAdapter<'a> {
    store: &'a VectorStore,
}

impl<'a> AgentIndexer for VectorIndexerAdapter<'a> {
    fn index_entry(
        &self,
        entry_id: &str,
        text: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), AgentError>> + Send + '_>> {
        let embedding = simple_embed(text);
        let id = entry_id.to_string();
        Box::pin(async move {
            self.store
                .index_entry(&id, &embedding)
                .await
                .map_err(|e| AgentError::Indexing(e.to_string()))
        })
    }
}

// ---------------------------------------------------------------------------
// ToolContext implementation
// ---------------------------------------------------------------------------

struct IntegrationToolCtx<'a> {
    db: &'a MemoryDB,
    agent: MemoryAgent,
    rag: PrecomputedRag,
    indexer: VectorIndexerAdapter<'a>,
}

impl<'a> ToolContext for IntegrationToolCtx<'a> {
    fn memory_db(&self) -> &MemoryDB {
        self.db
    }
    fn memory_agent(&self) -> &MemoryAgent {
        &self.agent
    }
    fn rag(&self) -> &dyn AgentRag {
        &self.rag
    }
    fn indexer(&self) -> &dyn AgentIndexer {
        &self.indexer
    }
    fn image_dir(&self) -> &str {
        "/tmp/test_images"
    }
}

// ---------------------------------------------------------------------------
// Helper: run the full RAG pipeline (vector + BM25 → RRF fusion)
// ---------------------------------------------------------------------------

async fn run_rag_pipeline(
    query: &str,
    bm25: &Bm25Index,
    vector_store: &VectorStore,
    rag_pipeline: &RagPipeline,
    db: &MemoryDB,
    is_private: bool,
) -> Vec<RagHit> {
    let top_k = 32;

    // BM25 search
    let bm25_results: Vec<SourceResult> = bm25
        .search(query, top_k)
        .into_iter()
        .map(|r| SourceResult {
            entry_id: r.entry_id,
            score: r.score,
        })
        .collect();

    // Vector search
    let query_embedding = simple_embed(query);
    let vector_results: Vec<SourceResult> = vector_store
        .search(&query_embedding, top_k)
        .await
        .unwrap()
        .into_iter()
        .map(|r| SourceResult {
            entry_id: r.entry_id,
            score: r.score as f64,
        })
        .collect();

    // Gather metadata
    let mut seen_ids = std::collections::HashSet::new();
    let all_ids: Vec<String> = bm25_results
        .iter()
        .chain(vector_results.iter())
        .filter_map(|r| {
            if seen_ids.insert(r.entry_id.clone()) {
                Some(r.entry_id.clone())
            } else {
                None
            }
        })
        .collect();

    let mut metadata = Vec::new();
    for id in &all_ids {
        if let Some(entry) = db.get_entry(id).unwrap() {
            metadata.push(EntryMeta {
                entry_id: entry.id,
                status: entry.status,
                confidence: entry.confidence,
                created_at: entry.created_at,
            });
        }
    }

    // RAG fusion
    let rag_results =
        rag_pipeline.retrieve(&vector_results, &bm25_results, &metadata, is_private);

    rag_results
        .into_iter()
        .take(top_k)
        .map(|r| RagHit {
            entry_id: r.entry_id,
            score: r.score,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// CommandContext implementation
// ---------------------------------------------------------------------------

struct IntegrationCommandCtx<'a> {
    db: &'a MemoryDB,
    private: AtomicBool,
    autonomy_paused: AtomicBool,
}

impl<'a> CommandContext for IntegrationCommandCtx<'a> {
    fn memory_db(&self) -> &MemoryDB {
        self.db
    }
    fn is_private(&self) -> bool {
        self.private.load(Ordering::SeqCst)
    }
    fn set_private(&self, private: bool) {
        self.private.store(private, Ordering::SeqCst);
    }
    fn is_autonomy_paused(&self) -> bool {
        self.autonomy_paused.load(Ordering::SeqCst)
    }
    fn set_autonomy_paused(&self, paused: bool) {
        self.autonomy_paused.store(paused, Ordering::SeqCst);
    }
    fn effective_config(&self) -> Value {
        json!({
            "model": "claude-haiku-4-5-20251001",
            "memory": { "enabled": true },
        })
    }
}

// ---------------------------------------------------------------------------
// Helper: build conversation messages
// ---------------------------------------------------------------------------

fn make_conversation() -> Vec<ConversationMessage> {
    let base = Utc::now();
    vec![
        ConversationMessage {
            role: "user".to_string(),
            content: "Hi! I just got back from Tokyo. The ramen there was incredible."
                .to_string(),
            timestamp: (base - chrono::Duration::minutes(10)).to_rfc3339(),
        },
        ConversationMessage {
            role: "assistant".to_string(),
            content: "Welcome back! Tokyo ramen is legendary. Did you have a favorite spot?"
                .to_string(),
            timestamp: (base - chrono::Duration::minutes(9)).to_rfc3339(),
        },
        ConversationMessage {
            role: "user".to_string(),
            content: "Ichiran in Shibuya was the best. I prefer tonkotsu broth over miso."
                .to_string(),
            timestamp: (base - chrono::Duration::minutes(8)).to_rfc3339(),
        },
        ConversationMessage {
            role: "assistant".to_string(),
            content: "Ichiran is a classic choice! Tonkotsu is rich and creamy. Did you try any other food?"
                .to_string(),
            timestamp: (base - chrono::Duration::minutes(7)).to_rfc3339(),
        },
        ConversationMessage {
            role: "user".to_string(),
            content: "Yes, I had amazing sushi at Tsukiji market. Also tried takoyaki in Osaka on the way back."
                .to_string(),
            timestamp: (base - chrono::Duration::minutes(6)).to_rfc3339(),
        },
        ConversationMessage {
            role: "user".to_string(),
            content: "By the way, I'm working on a Rust project for my company ACME Corp."
                .to_string(),
            timestamp: (base - chrono::Duration::minutes(5)).to_rfc3339(),
        },
        ConversationMessage {
            role: "assistant".to_string(),
            content: "That sounds great! What kind of Rust project is it?"
                .to_string(),
            timestamp: (base - chrono::Duration::minutes(4)).to_rfc3339(),
        },
    ]
}

fn make_entry(id: &str, summary: &str, confidence: f64) -> Entry {
    let now = Utc::now().to_rfc3339();
    Entry {
        id: id.to_string(),
        memory_type: "semantic".to_string(),
        source: "summary".to_string(),
        reason: "compaction".to_string(),
        status: "active".to_string(),
        canonical: false,
        confidence,
        summary_text: summary.to_string(),
        topic_tags: "test".to_string(),
        topic_key: "test".to_string(),
        start_timestamp: now.clone(),
        end_timestamp: now.clone(),
        message_count: 7,
        source_entry_ids: String::new(),
        related_entry_ids: String::new(),
        superseded_by: String::new(),
        created_at: now.clone(),
        updated_at: now,
        entry_type: String::new(),
        image_path: String::new(),
    }
}

// ===========================================================================
// Integration test: full memory system end-to-end
// ===========================================================================

#[tokio::test]
async fn test_full_memory_system_e2e() {
    // -----------------------------------------------------------------------
    // Phase 1: Setup — real SQLite, LanceDB, BM25
    // -----------------------------------------------------------------------

    let tmp_dir = TempDir::new().unwrap();
    let db_path = tmp_dir.path().join("memory.db");
    let vs_path = tmp_dir.path().join("vectorstore");

    let db = MemoryDB::open(&db_path).unwrap();
    let vector_store = VectorStore::open(&vs_path, 8).await.unwrap();
    let mut bm25 = Bm25Index::new();
    let rag_pipeline = RagPipeline::new(32);

    // -----------------------------------------------------------------------
    // Phase 2: Simulate multi-turn conversation (7 messages, >5 required)
    // -----------------------------------------------------------------------

    let messages = make_conversation();
    assert!(messages.len() >= 5, "Need at least 5 messages");

    // -----------------------------------------------------------------------
    // Phase 3: Compaction — mock LLM extracts memories → SQLite + LanceDB
    // -----------------------------------------------------------------------

    let compaction_llm = MockCompactionLlm {
        response: vec![
            CompactedEntry {
                memory_type: "episodic".to_string(),
                summary_text: "User recently traveled to Tokyo and visited Ichiran ramen in Shibuya"
                    .to_string(),
                topic_tags: "travel,food,tokyo,ramen".to_string(),
                topic_key: "travel_tokyo".to_string(),
                confidence: 0.9,
            },
            CompactedEntry {
                memory_type: "semantic".to_string(),
                summary_text: "User prefers tonkotsu ramen broth over miso".to_string(),
                topic_tags: "preference,food,ramen".to_string(),
                topic_key: "food_preferences".to_string(),
                confidence: 0.95,
            },
            CompactedEntry {
                memory_type: "episodic".to_string(),
                summary_text: "User had sushi at Tsukiji market and takoyaki in Osaka".to_string(),
                topic_tags: "travel,food,sushi,osaka".to_string(),
                topic_key: "travel_japan_food".to_string(),
                confidence: 0.85,
            },
            CompactedEntry {
                memory_type: "semantic".to_string(),
                summary_text: "User works at ACME Corp on a Rust project".to_string(),
                topic_tags: "work,rust,acme".to_string(),
                topic_key: "employment".to_string(),
                confidence: 0.9,
            },
        ],
    };

    let real_indexer = RealVectorIndexer {
        store: &vector_store,
    };
    let conv_mgr = MockConversationMgr {
        next_id: "conv-2".to_string(),
    };
    let compaction_mgr = CompactionManager::new(CompactionConfig::default());

    let compaction_result = compaction_mgr
        .compact(
            "conv-1",
            &messages,
            false,
            DEFAULT_COMPACT_PROMPT,
            &compaction_llm,
            &db,
            &real_indexer,
            &conv_mgr,
            false,
        )
        .await
        .unwrap();

    // Verify: entries created in SQLite
    let created_ids = match &compaction_result {
        CompactionOutcome::Compacted(r) => {
            assert_eq!(r.entries_created.len(), 4, "Should create 4 entries");
            assert_eq!(r.message_count, 7);
            assert_eq!(r.conversation_id, "conv-1");
            assert_eq!(r.new_conversation_id, "conv-2");
            r.entries_created.clone()
        }
        _ => panic!("Expected Compacted outcome"),
    };

    // Verify each entry exists in SQLite with correct content
    let active_entries = db.get_entries_by_status("active").unwrap();
    assert_eq!(active_entries.len(), 4);

    for id in &created_ids {
        let entry = db.get_entry(id).unwrap().expect("entry should exist");
        assert_eq!(entry.status, "active");
        assert_eq!(entry.reason, "compaction");
        assert_eq!(entry.source, "summary");
    }

    // Verify changelog recorded compaction
    let changelog = db.get_recent_changelog(10).unwrap();
    assert_eq!(changelog.len(), 4);
    assert!(changelog.iter().all(|l| l.operation == "compaction"));

    // -----------------------------------------------------------------------
    // Phase 4: Verify entries indexed in LanceDB vector store
    // -----------------------------------------------------------------------

    // Search for "ramen" — should find the ramen-related entries
    let ramen_embedding = simple_embed("ramen tonkotsu broth");
    let vs_results = vector_store.search(&ramen_embedding, 4).await.unwrap();
    assert!(
        !vs_results.is_empty(),
        "Vector store should have indexed entries"
    );

    // -----------------------------------------------------------------------
    // Phase 5: BM25 indexing + hybrid RAG retrieval
    // -----------------------------------------------------------------------

    // Index all active entries in BM25
    for entry in &active_entries {
        bm25.add_document(&entry.id, &entry.summary_text);
    }
    assert_eq!(bm25.len(), 4);

    // BM25 search for "ramen"
    let bm25_results = bm25.search("ramen tonkotsu", 10);
    assert!(
        !bm25_results.is_empty(),
        "BM25 should find ramen-related entries"
    );

    // Full RAG pipeline: vector + BM25 → RRF fusion
    let rag_hits = run_rag_pipeline(
        "ramen tonkotsu broth",
        &bm25,
        &vector_store,
        &rag_pipeline,
        &db,
        false,
    )
    .await;
    assert!(!rag_hits.is_empty(), "RAG should return fused results");

    // The top result should be ramen-related (appears in both sources)
    let top_entry = db.get_entry(&rag_hits[0].entry_id).unwrap().unwrap();
    assert!(
        top_entry.summary_text.to_lowercase().contains("ramen")
            || top_entry.summary_text.to_lowercase().contains("tonkotsu"),
        "Top RAG result should be ramen-related, got: {}",
        top_entry.summary_text
    );

    // Verify: RAG suppressed in private mode returns empty
    let private_hits = run_rag_pipeline(
        "ramen tonkotsu broth",
        &bm25,
        &vector_store,
        &rag_pipeline,
        &db,
        true,
    )
    .await;
    assert!(
        private_hits.is_empty(),
        "RAG should return empty in private mode"
    );

    // -----------------------------------------------------------------------
    // Phase 6: Memory agent query via ToolContext + tool dispatch
    // -----------------------------------------------------------------------

    // Pre-compute RAG results for the tool's query
    let food_rag_hits = run_rag_pipeline(
        "What food does Shore like?",
        &bm25,
        &vector_store,
        &rag_pipeline,
        &db,
        false,
    )
    .await;

    let tool_ctx = IntegrationToolCtx {
        db: &db,
        agent: MemoryAgent::one_shot(CallerIdentity::Char, "Shore"),
        rag: PrecomputedRag {
            results: food_rag_hits,
        },
        indexer: VectorIndexerAdapter {
            store: &vector_store,
        },
    };

    // Dispatch memory tool — search for ramen
    let tool_result = tools::dispatch_tool(
        "memory",
        json!({"request": "What food does the user like?"}),
        &tool_ctx,
    )
    .await
    .unwrap();

    let entries_array = tool_result["entries"].as_array().unwrap();
    assert!(
        !entries_array.is_empty(),
        "Memory tool should return results for food query"
    );

    // Verify the tool returned structured data
    let first = &entries_array[0];
    assert!(first.get("entry_id").is_some());
    assert!(first.get("summary").is_some());
    assert!(first.get("relevance").is_some());

    // -----------------------------------------------------------------------
    // Phase 7: Memory agent create_entry persists and indexes
    // -----------------------------------------------------------------------

    let agent = MemoryAgent::one_shot(CallerIdentity::Char, "Shore");
    let new_entry = make_entry(
        "agent_created_001",
        "User mentioned their cat is named Mochi",
        0.85,
    );

    let agent_indexer = VectorIndexerAdapter {
        store: &vector_store,
    };
    let write_result = agent
        .create_entry(&new_entry, &db, &agent_indexer)
        .await
        .unwrap();

    assert_eq!(write_result.entry_id, "agent_created_001");
    assert_eq!(write_result.operation, "create");
    assert!(write_result.indexed);

    // Verify persisted in DB
    let persisted = db.get_entry("agent_created_001").unwrap().unwrap();
    assert_eq!(persisted.summary_text, "User mentioned their cat is named Mochi");

    // Verify indexed in vector store
    let cat_embedding = simple_embed("cat named Mochi");
    let cat_results = vector_store.search(&cat_embedding, 5).await.unwrap();
    assert!(
        cat_results.iter().any(|r| r.entry_id == "agent_created_001"),
        "Agent-created entry should be searchable in vector store"
    );

    // Verify changelog
    let agent_logs = db.get_recent_changelog(20).unwrap();
    assert!(
        agent_logs.iter().any(|l| l.operation == "agent_create"),
        "Changelog should record agent_create"
    );

    // -----------------------------------------------------------------------
    // Phase 8: Collation — tidy, collate, normalize, decay
    // -----------------------------------------------------------------------

    // Add some duplicate-ish entries and entities for collation to work on
    let thirty_days_ago = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
    let now_str = Utc::now().to_rfc3339();

    // Entry that will be split by tidy
    let broad_entry = Entry {
        id: "broad_001".to_string(),
        memory_type: "semantic".to_string(),
        source: "summary".to_string(),
        reason: "compaction".to_string(),
        status: "active".to_string(),
        canonical: false,
        confidence: 0.9,
        summary_text: "User likes hiking in mountains and also codes in Python".to_string(),
        topic_tags: "hobby,coding".to_string(),
        topic_key: "mixed".to_string(),
        start_timestamp: now_str.clone(),
        end_timestamp: now_str.clone(),
        message_count: 5,
        source_entry_ids: String::new(),
        related_entry_ids: String::new(),
        superseded_by: String::new(),
        created_at: now_str.clone(),
        updated_at: now_str.clone(),
        entry_type: String::new(),
        image_path: String::new(),
    };
    db.create_entry(&broad_entry).unwrap();

    // Entries that will be merged by collate
    let sim1 = Entry {
        id: "sim_001".to_string(),
        summary_text: "User enjoys green tea".to_string(),
        updated_at: now_str.clone(),
        created_at: now_str.clone(),
        ..broad_entry.clone()
    };
    let sim2 = Entry {
        id: "sim_002".to_string(),
        summary_text: "User drinks green tea daily".to_string(),
        updated_at: now_str.clone(),
        created_at: now_str.clone(),
        ..broad_entry.clone()
    };
    db.create_entry(&sim1).unwrap();
    db.create_entry(&sim2).unwrap();

    // Old entry for decay
    let stale_entry = Entry {
        id: "stale_001".to_string(),
        summary_text: "User used to play chess".to_string(),
        confidence: 0.8,
        created_at: thirty_days_ago.clone(),
        updated_at: thirty_days_ago.clone(),
        ..broad_entry.clone()
    };
    db.create_entry(&stale_entry).unwrap();

    // Entities for normalization
    db.upsert_entity("Tokyo", "city", "Capital of Japan").unwrap();
    db.upsert_entity("Tokyo, Japan", "city", "Also Tokyo").unwrap();

    let collation_llm = MockCollationLlm {
        tidy_response: vec![TidySplit {
            original_entry_id: "broad_001".to_string(),
            replacements: vec![
                TidyReplacement {
                    summary_text: "User likes hiking in mountains".to_string(),
                    topic_tags: "hobby,hiking".to_string(),
                    topic_key: "hobbies".to_string(),
                    confidence: 0.9,
                },
                TidyReplacement {
                    summary_text: "User codes in Python".to_string(),
                    topic_tags: "coding,python".to_string(),
                    topic_key: "skills".to_string(),
                    confidence: 0.85,
                },
            ],
        }],
        collate_response: vec![CollateMerge {
            source_entry_ids: vec!["sim_001".to_string(), "sim_002".to_string()],
            merged_summary: "User regularly enjoys and drinks green tea".to_string(),
            merged_topic_tags: "preference,beverage,tea".to_string(),
            merged_topic_key: "preferences".to_string(),
            merged_confidence: 0.9,
        }],
        normalize_response: vec![EntityNormalization {
            canonical_name: "Tokyo".to_string(),
            duplicate_names: vec!["Tokyo, Japan".to_string()],
        }],
    };

    // Sleep 1.1s to ensure collation generates different timestamp-based entry IDs
    // than compaction (both use YYYYMMDD_HHMMSS_N format).
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    let collation_mgr = CollationManager::new(CollationConfig::default());
    let collation_outcome = collation_mgr
        .run(
            &db,
            &collation_llm,
            DEFAULT_TIDY_PROMPT,
            DEFAULT_COLLATE_PROMPT,
            DEFAULT_NORMALIZE_PROMPT,
        )
        .await
        .unwrap();

    // Phase 1: tidy split
    assert_eq!(collation_outcome.tidy_splits, 1, "Should have 1 tidy split");
    assert_eq!(
        collation_outcome.tidy_new_entries, 2,
        "Tidy should create 2 new entries"
    );
    let broad = db.get_entry("broad_001").unwrap().unwrap();
    assert_eq!(broad.status, "superseded", "Broad entry should be superseded");

    // Phase 2: collate merge
    assert_eq!(collation_outcome.collate_merges, 1, "Should have 1 merge");
    let sim1 = db.get_entry("sim_001").unwrap().unwrap();
    let sim2 = db.get_entry("sim_002").unwrap().unwrap();
    assert_eq!(sim1.status, "superseded");
    assert_eq!(sim2.status, "superseded");

    // Phase 3: entity normalization
    assert_eq!(
        collation_outcome.entities_normalized, 1,
        "Should normalize 1 entity"
    );
    assert!(
        db.get_entity_by_name("Tokyo, Japan").unwrap().is_none(),
        "Duplicate entity should be removed"
    );
    assert!(
        db.get_entity_by_name("Tokyo").unwrap().is_some(),
        "Canonical entity should remain"
    );

    // Phase 4: confidence decay (stale_001 is 30 days old = one half-life)
    assert!(
        collation_outcome.entries_decayed >= 1,
        "At least stale_001 should be decayed"
    );
    let stale = db.get_entry("stale_001").unwrap().unwrap();
    assert!(
        stale.confidence < 0.8,
        "Decayed confidence {:.3} should be < 0.8",
        stale.confidence
    );
    assert!(
        stale.confidence >= 0.1,
        "Confidence should respect floor"
    );

    // Verify collation changelog entries
    let all_logs = db.get_recent_changelog(50).unwrap();
    assert!(all_logs.iter().any(|l| l.operation == "collation_tidy"));
    assert!(all_logs.iter().any(|l| l.operation == "collation_collate"));
    assert!(all_logs.iter().any(|l| l.operation == "collation_normalize"));
    assert!(all_logs.iter().any(|l| l.operation == "collation_decay"));

    // -----------------------------------------------------------------------
    // Phase 9: Privacy toggle — tools hidden, RAG suppressed
    // -----------------------------------------------------------------------

    // Verify available tools in non-private mode includes "memory"
    let public_tools = tools::available_tools(false);
    assert!(
        public_tools.iter().any(|t| t.name == "memory"),
        "Memory tool should be available in non-private mode"
    );

    // Toggle to private — memory tools should be excluded
    let private_tools = tools::available_tools(true);
    assert!(
        !private_tools.iter().any(|t| t.name == "memory"),
        "Memory tool should NOT be available in private mode"
    );

    // Verify specific excluded tools
    let private_names: Vec<&str> = private_tools.iter().map(|t| t.name).collect();
    assert!(!private_names.contains(&"memory"));
    assert!(!private_names.contains(&"send_image"));
    assert!(!private_names.contains(&"recall_image"));

    // Web tools should still be available
    assert!(private_names.contains(&"web_search"));
    assert!(private_names.contains(&"activity_heatmap"));

    // -----------------------------------------------------------------------
    // Phase 10: Memory command — entry counts and search
    // -----------------------------------------------------------------------

    let cmd_ctx = IntegrationCommandCtx {
        db: &db,
        private: AtomicBool::new(false),
        autonomy_paused: AtomicBool::new(false),
    };

    // Status mode (no query)
    let status_result = commands::dispatch("memory", json!({}), &cmd_ctx)
        .await
        .unwrap();
    let status_data = &status_result.data;

    let total = status_data["entries"]["total"].as_i64().unwrap();
    assert!(total > 0, "Should have entries in DB");

    let active_count = status_data["entries"]["active"].as_i64().unwrap();
    assert!(active_count > 0, "Should have active entries");

    let superseded_count = status_data["entries"]["superseded"].as_i64().unwrap();
    assert!(superseded_count > 0, "Should have superseded entries from collation");

    // Search mode
    let search_result = commands::dispatch("memory", json!({"query": "ramen"}), &cmd_ctx)
        .await
        .unwrap();
    let search_data = &search_result.data;
    let result_count = search_data["count"].as_i64().unwrap();
    assert!(result_count > 0, "Search for 'ramen' should find entries");

    // Search for something from agent-created entry
    let cat_result = commands::dispatch("memory", json!({"query": "Mochi"}), &cmd_ctx)
        .await
        .unwrap();
    assert!(
        cat_result.data["count"].as_i64().unwrap() > 0,
        "Search for 'Mochi' should find agent-created entry"
    );

    // -----------------------------------------------------------------------
    // Phase 11: Toggle private via command, verify state
    // -----------------------------------------------------------------------

    assert!(!cmd_ctx.is_private());

    let toggle_result = commands::dispatch("toggle_private", json!({}), &cmd_ctx)
        .await
        .unwrap();
    assert!(toggle_result.push_history, "toggle_private should push history");
    assert_eq!(toggle_result.data["private"], true);
    assert!(cmd_ctx.is_private());

    // Compact command should be skipped in private mode
    let compact_result = commands::dispatch("compact", json!({}), &cmd_ctx)
        .await
        .unwrap();
    assert!(
        compact_result.data["error"]
            .as_str()
            .unwrap()
            .contains("private"),
        "Compact should report private skip"
    );

    // Toggle back
    commands::dispatch("toggle_private", json!({}), &cmd_ctx)
        .await
        .unwrap();
    assert!(!cmd_ctx.is_private());

    // Config command returns expected shape
    let config_result = commands::dispatch("config", json!({}), &cmd_ctx)
        .await
        .unwrap();
    assert_eq!(
        config_result.data["config"]["model"],
        "claude-haiku-4-5-20251001"
    );
}

// ===========================================================================
// Focused test: compaction into private conversation is rejected
// ===========================================================================

#[tokio::test]
async fn test_compaction_rejects_private_conversation() {
    let tmp = TempDir::new().unwrap();
    let db = MemoryDB::open(&tmp.path().join("test.db")).unwrap();
    let vs = VectorStore::open(&tmp.path().join("vs"), 8).await.unwrap();

    let llm = MockCompactionLlm {
        response: vec![CompactedEntry {
            memory_type: "semantic".to_string(),
            summary_text: "Should not be created".to_string(),
            topic_tags: "test".to_string(),
            topic_key: "test".to_string(),
            confidence: 0.9,
        }],
    };
    let indexer = RealVectorIndexer { store: &vs };
    let conv_mgr = MockConversationMgr {
        next_id: "new".to_string(),
    };
    let mgr = CompactionManager::new(CompactionConfig::default());

    let result = mgr
        .compact(
            "private-conv",
            &make_conversation(),
            true, // private
            DEFAULT_COMPACT_PROMPT,
            &llm,
            &db,
            &indexer,
            &conv_mgr,
            false,
        )
        .await;

    assert!(
        matches!(result, Err(CompactionError::PrivateConversation)),
        "Should reject private conversation"
    );
    assert_eq!(db.count_entries().unwrap(), 0, "No entries should be created");
}

// ===========================================================================
// Focused test: vector store round-trip with real embeddings
// ===========================================================================

#[tokio::test]
async fn test_vector_store_roundtrip_with_simple_embeddings() {
    let tmp = TempDir::new().unwrap();
    let store = VectorStore::open(tmp.path(), 8).await.unwrap();

    let texts = [
        ("e1", "User loves ramen and Japanese food"),
        ("e2", "User works at ACME Corp as a software engineer"),
        ("e3", "User has a cat named Mochi"),
    ];

    for (id, text) in &texts {
        let emb = simple_embed(text);
        store.index_entry(id, &emb).await.unwrap();
    }

    // Query for food-related content
    let query_emb = simple_embed("ramen Japanese food");
    let results = store.search(&query_emb, 3).await.unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(
        results[0].entry_id, "e1",
        "Food query should rank food entry first"
    );

    // Query for work-related content
    let work_emb = simple_embed("software engineer ACME");
    let work_results = store.search(&work_emb, 3).await.unwrap();
    assert_eq!(
        work_results[0].entry_id, "e2",
        "Work query should rank work entry first"
    );
}
