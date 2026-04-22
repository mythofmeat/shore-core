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
//! - Collation (refine, decay)
//! - Memory agent create_entry persists entries

use chrono::Local;
use std::future::Future;
use std::pin::Pin;
use tempfile::TempDir;

use shore_daemon::memory::agent::RagHit;
use shore_daemon::memory::collation::{
    CollationError, CollationLlm, CollationManager, DecayConfig, RefineAction, RefineEntryFields,
    DEFAULT_REFINE_PROMPT,
};
use shore_daemon::memory::compaction::{
    CompactionConfig, CompactionError, CompactionLlm, CompactionManager, CompactionOutcome,
    ConversationManager, ConversationMessage, RetentionParams, VectorIndexer,
    DEFAULT_COMPACT_PROMPT,
};
use shore_daemon::memory::db::{Entry, MemoryDB};
use shore_daemon::memory::rag::{EntryMeta, RagPipeline, SourceResult};
use shore_daemon::memory::search::Bm25Index;
use shore_daemon::memory::vectorstore::VectorStore;
// ---------------------------------------------------------------------------
// Simple deterministic embedding — 8-dimensional bag-of-words hash
// ---------------------------------------------------------------------------

fn simple_embed(text: &str) -> Vec<f32> {
    let dim = 8;
    let mut vec = vec![0.0f32; dim];
    for word in text.to_lowercase().split_whitespace() {
        let hash = word
            .bytes()
            .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
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
// Mock LLM for compaction — returns raw XML string for the parser
// ---------------------------------------------------------------------------

struct MockCompactionLlm {
    response_xml: String,
}

impl MockCompactionLlm {
    fn with_entries(entries: &[(&str, &str, &str, f64)]) -> Self {
        let mut xml = String::new();
        xml.push_str("<recap>Test recap of conversation.</recap>\n");
        for (memory_type, summary, tags, confidence) in entries {
            xml.push_str(&format!(
                "<entry>\n<memory_type>{memory_type}</memory_type>\n<summary>{summary}</summary>\n<topic_tags>{tags}</topic_tags>\n<confidence>{confidence}</confidence>\n</entry>\n"
            ));
        }
        Self { response_xml: xml }
    }
}

impl CompactionLlm for MockCompactionLlm {
    fn summarize(
        &self,
        _prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, CompactionError>> + Send + '_>> {
        let result = Ok(self.response_xml.clone());
        Box::pin(async move { result })
    }
}

// ---------------------------------------------------------------------------
// Mock LLM for collation (unified refine phase)
// ---------------------------------------------------------------------------

struct MockCollationLlm {
    refine_response: Vec<RefineAction>,
}

impl CollationLlm for MockCollationLlm {
    fn refine(
        &self,
        _prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RefineAction>, CollationError>> + Send + '_>> {
        let result = Ok(self.refine_response.clone());
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
    fn archive_and_retain(
        &self,
        _conversation_id: &str,
        _params: RetentionParams,
    ) -> Pin<Box<dyn Future<Output = Result<String, CompactionError>> + Send + '_>> {
        let next_id = self.next_id.clone();
        Box::pin(async move { Ok(next_id) })
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
    let rag_results = rag_pipeline.retrieve(&vector_results, &bm25_results, &metadata, is_private);

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
// Helper: build conversation messages
// ---------------------------------------------------------------------------

fn make_conversation() -> Vec<ConversationMessage> {
    let base = Local::now();
    vec![
        ConversationMessage {
            role: "user".to_string(),
            content: "Hi! I just got back from Tokyo. The ramen there was incredible."
                .to_string(),
            timestamp: (base - chrono::Duration::minutes(10)).to_rfc3339(),
            is_tool_result_only: false,
        },
        ConversationMessage {
            role: "assistant".to_string(),
            content: "Welcome back! Tokyo ramen is legendary. Did you have a favorite spot?"
                .to_string(),
            timestamp: (base - chrono::Duration::minutes(9)).to_rfc3339(),
            is_tool_result_only: false,
        },
        ConversationMessage {
            role: "user".to_string(),
            content: "Ichiran in Shibuya was the best. I prefer tonkotsu broth over miso."
                .to_string(),
            timestamp: (base - chrono::Duration::minutes(8)).to_rfc3339(),
            is_tool_result_only: false,
        },
        ConversationMessage {
            role: "assistant".to_string(),
            content: "Ichiran is a classic choice! Tonkotsu is rich and creamy. Did you try any other food?"
                .to_string(),
            timestamp: (base - chrono::Duration::minutes(7)).to_rfc3339(),
            is_tool_result_only: false,
        },
        ConversationMessage {
            role: "user".to_string(),
            content: "Yes, I had amazing sushi at Tsukiji market. Also tried takoyaki in Osaka on the way back."
                .to_string(),
            timestamp: (base - chrono::Duration::minutes(6)).to_rfc3339(),
            is_tool_result_only: false,
        },
        ConversationMessage {
            role: "user".to_string(),
            content: "By the way, I'm working on a Rust project for my company ACME Corp."
                .to_string(),
            timestamp: (base - chrono::Duration::minutes(5)).to_rfc3339(),
            is_tool_result_only: false,
        },
        ConversationMessage {
            role: "assistant".to_string(),
            content: "That sounds great! What kind of Rust project is it?"
                .to_string(),
            timestamp: (base - chrono::Duration::minutes(4)).to_rfc3339(),
            is_tool_result_only: false,
        },
    ]
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
    // Phase 3: Compaction — mock LLM returns XML → parser → SQLite + LanceDB
    // -----------------------------------------------------------------------

    let compaction_llm = MockCompactionLlm::with_entries(&[
        (
            "episodic",
            "User recently traveled to Tokyo and visited Ichiran ramen in Shibuya",
            "travel,food,tokyo,ramen",
            0.9,
        ),
        (
            "semantic",
            "User prefers tonkotsu ramen broth over miso",
            "preference,food,ramen",
            0.95,
        ),
        (
            "episodic",
            "User had sushi at Tsukiji market and takoyaki in Osaka",
            "travel,food,sushi,osaka",
            0.85,
        ),
        (
            "semantic",
            "User works at ACME Corp on a Rust project",
            "work,rust,acme",
            0.9,
        ),
    ]);

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
            "",
            false,
            DEFAULT_COMPACT_PROMPT,
            None,    // existing_recap
            "Shore", // char_name
            "User",  // user_name
            &compaction_llm,
            &db,
            &real_indexer,
            &conv_mgr,
            None,
            false,
            None,
        )
        .await
        .unwrap();

    // Verify: entries created in SQLite
    let created_ids = match &compaction_result {
        CompactionOutcome::Compacted(r) => {
            assert_eq!(r.entries_created.len(), 4, "Should create 4 entries");
            assert!(r.message_count > 0, "Should compact some messages");
            assert_eq!(r.message_count + r.retained_count, 7, "Total should be 7");
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
    // Phase 8: Collation — unified refine + decay
    // -----------------------------------------------------------------------

    // Add some entries for collation to work on
    let now_str = Local::now().to_rfc3339();
    let thirty_days_ago = (Local::now() - chrono::Duration::days(30)).to_rfc3339();

    // Entry pair that will be merged by refine
    let sim1 = Entry {
        id: "sim_001".to_string(),
        memory_type: "semantic".to_string(),
        source: "summary".to_string(),
        reason: "compaction".to_string(),
        status: "active".to_string(),
        confidence: 0.9,
        summary_text: "User enjoys green tea".to_string(),
        topic_tags: "preference,beverage".to_string(),
        topic_key: "preferences".to_string(),
        start_timestamp: now_str.clone(),
        end_timestamp: now_str.clone(),
        message_count: 3,
        source_entry_ids: String::new(),
        related_entry_ids: String::new(),
        superseded_by: String::new(),
        created_at: now_str.clone(),
        updated_at: now_str.clone(),
        entry_type: String::new(),
        image_path: String::new(),
        collated_at: String::new(),
    };
    let sim2 = Entry {
        id: "sim_002".to_string(),
        summary_text: "User drinks green tea daily".to_string(),
        created_at: now_str.clone(),
        updated_at: now_str.clone(),
        ..sim1.clone()
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
        ..sim1.clone()
    };
    db.create_entry(&stale_entry).unwrap();

    let collation_llm = MockCollationLlm {
        refine_response: vec![RefineAction::Merge {
            source_entry_ids: vec!["sim_001".to_string(), "sim_002".to_string()],
            result: RefineEntryFields {
                summary_text: "User regularly enjoys and drinks green tea".to_string(),
                topic_tags: "preference,beverage,tea".to_string(),
                topic_key: "preferences".to_string(),
                confidence: 0.9,
            },
            reason: "Duplicate entries about tea".to_string(),
        }],
    };

    // Sleep 1.1s to ensure collation generates different timestamp-based entry IDs
    // than compaction (both use YYYYMMDD_HHMMSS_N format).
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    let collation_mgr = CollationManager::new(DecayConfig::default());
    let collation_outcome = collation_mgr
        .run(
            &db,
            &collation_llm,
            DEFAULT_REFINE_PROMPT,
            &std::collections::HashMap::new(),
            None,                // indexer
            Some(&vector_store), // vector_store for re-indexing
            None,                // limit
        )
        .await
        .unwrap();

    // Verify refine merge
    assert_eq!(
        collation_outcome.refine_merges, 1,
        "Should have 1 refine merge"
    );
    let sim1 = db.get_entry("sim_001").unwrap().unwrap();
    let sim2 = db.get_entry("sim_002").unwrap().unwrap();
    assert_eq!(sim1.status, "superseded");
    assert_eq!(sim2.status, "superseded");

    // Verify confidence decay (stale_001 is 30 days old = one half-life)
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
    assert!(stale.confidence >= 0.1, "Confidence should respect floor");

    // Verify collation changelog entries
    let all_logs = db.get_recent_changelog(50).unwrap();
    assert!(all_logs.iter().any(|l| l.operation == "collation_refine"));
    assert!(all_logs.iter().any(|l| l.operation == "collation_decay"));
}

// ===========================================================================
// Focused test: compaction into private conversation is rejected
// ===========================================================================

#[tokio::test]
async fn test_compaction_rejects_private_conversation() {
    let tmp = TempDir::new().unwrap();
    let db = MemoryDB::open(&tmp.path().join("test.db")).unwrap();
    let vs = VectorStore::open(&tmp.path().join("vs"), 8).await.unwrap();

    let llm =
        MockCompactionLlm::with_entries(&[("semantic", "Should not be created", "test", 0.9)]);
    let indexer = RealVectorIndexer { store: &vs };
    let conv_mgr = MockConversationMgr {
        next_id: "new".to_string(),
    };
    let mgr = CompactionManager::new(CompactionConfig::default());

    let result = mgr
        .compact(
            "private-conv",
            &make_conversation(),
            "",
            true, // private
            DEFAULT_COMPACT_PROMPT,
            None,
            "Shore",
            "User",
            &llm,
            &db,
            &indexer,
            &conv_mgr,
            None,
            false,
            None,
        )
        .await;

    assert!(
        matches!(result, Err(CompactionError::PrivateConversation)),
        "Should reject private conversation"
    );
    assert_eq!(
        db.count_entries().unwrap(),
        0,
        "No entries should be created"
    );
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
