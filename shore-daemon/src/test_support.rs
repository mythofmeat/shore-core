//! Shared test mocks and builders for shore-daemon tests.
//!
//! This module consolidates duplicated test infrastructure (MockRag,
//! test_model, TestToolContext) so each test file can import from a
//! single source.

use std::future::Future;
use std::pin::Pin;

use crate::autonomy::manager::AutonomyManager;
use crate::memory::agent::types::{AgentError, AgentIndexer, AgentRag, RagHit};
use crate::memory::agent::{CallerIdentity, MemoryAgent};
use crate::memory::agent_llm::{AgentLlm, MockAgentLlm};
use crate::memory::compaction_impls::ImageGenConfig;
use crate::memory::db::MemoryDB;
use crate::memory::markdown_store::MarkdownMemoryStore;
use crate::memory::researcher::MemoryResearcher;
use crate::tools::ToolContext;
use shore_config::app::SearchConfig;
use shore_config::models::{ResolvedModel, Sdk};
use shore_llm_client::LlmClient;

// ── test_model ──────────────────────────────────────────────────────────

/// Canonical test `ResolvedModel` used across all daemon tests.
pub fn test_model() -> ResolvedModel {
    ResolvedModel {
        name: "test".into(),
        qualified_name: "chat.test".into(),
        category: "chat".into(),
        provider_key: "anthropic".into(),
        sdk: Sdk::Anthropic,
        model_id: "claude-test".into(),
        api_key_env: Some("TEST_KEY".into()),
        base_url: None,
        max_context_tokens: None,
        max_tokens: Some(4096),
        temperature: Some(0.7),
        top_p: None,
        reasoning_effort: None,
        budget_tokens: None,
        cache_ttl: None,
        keepalive_enabled: None,
        keepalive_ttl: None,
        keepalive_max_pings: None,
        openrouter_provider: None,
        vertex_project: None,
        vertex_location: None,
        gemini_generation: None,
        gemini_web_search: None,
        zai_clear_thinking: None,
        zai_subscription: None,
    }
}

// ── MockRag ─────────────────────────────────────────────────────────────

/// Mock RAG implementation with configurable results.
pub struct MockRag {
    pub results: Vec<RagHit>,
}

impl MockRag {
    pub fn empty() -> Self {
        Self { results: vec![] }
    }

    pub fn with_results(results: Vec<RagHit>) -> Self {
        Self { results }
    }
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

// ── TestToolContext ─────────────────────────────────────────────────────

/// Shared `ToolContext` implementation for unit tests.
///
/// Supports optional researcher configuration via `with_researcher()`.
pub struct TestToolContext {
    pub db: MemoryDB,
    pub agent: MemoryAgent,
    pub agent_llm: MockAgentLlm,
    pub model: ResolvedModel,
    pub rag: MockRag,
    pub image_dir_val: String,
    pub researcher: Option<MemoryResearcher>,
    pub researcher_llm_val: Option<MockAgentLlm>,
    pub researcher_model_val: Option<ResolvedModel>,
    pub search_config_val: SearchConfig,
    pub autonomy_mgr: Option<AutonomyManager>,
    pub character_name_val: String,
    pub markdown_store_val: Option<MarkdownMemoryStore>,
}

impl TestToolContext {
    /// Create a default test context with empty rag and no researcher.
    pub fn new() -> Self {
        Self {
            db: MemoryDB::open_in_memory().unwrap(),
            agent: MemoryAgent::one_shot(CallerIdentity::Char, "Test", "User"),
            agent_llm: MockAgentLlm::new(vec![]),
            model: test_model(),
            rag: MockRag::empty(),
            image_dir_val: "/tmp/test_images".to_string(),
            researcher: None,
            researcher_llm_val: None,
            researcher_model_val: None,
            search_config_val: SearchConfig::default(),
            autonomy_mgr: None,
            character_name_val: String::new(),
            markdown_store_val: None,
        }
    }

    /// Create with a custom agent LLM (for canned responses).
    pub fn with_agent_llm(mut self, llm: MockAgentLlm) -> Self {
        self.agent_llm = llm;
        self
    }

    /// Set a custom image directory.
    pub fn with_image_dir(mut self, dir: &str) -> Self {
        self.image_dir_val = dir.to_string();
        self
    }

    /// Set custom RAG results.
    pub fn with_rag(mut self, results: Vec<RagHit>) -> Self {
        self.rag = MockRag::with_results(results);
        self
    }

    /// Set an autonomy manager and character name for activity tests.
    pub fn with_autonomy(mut self, mgr: AutonomyManager, character: &str) -> Self {
        self.autonomy_mgr = Some(mgr);
        self.character_name_val = character.to_string();
        self
    }

    /// Configure researcher with its own LLM and model.
    pub fn with_researcher(
        mut self,
        researcher: MemoryResearcher,
        llm: MockAgentLlm,
        model: ResolvedModel,
    ) -> Self {
        self.researcher = Some(researcher);
        self.researcher_llm_val = Some(llm);
        self.researcher_model_val = Some(model);
        self
    }

    /// Set a markdown memory store.
    pub fn with_markdown_store(mut self, store: MarkdownMemoryStore) -> Self {
        self.markdown_store_val = Some(store);
        self
    }
}

impl ToolContext for TestToolContext {
    fn memory_db(&self) -> &MemoryDB {
        &self.db
    }
    fn memory_agent(&self) -> &MemoryAgent {
        &self.agent
    }
    fn agent_llm(&self) -> &dyn AgentLlm {
        &self.agent_llm
    }
    fn agent_model(&self) -> &ResolvedModel {
        &self.model
    }
    fn researcher_llm(&self) -> Option<&dyn AgentLlm> {
        self.researcher_llm_val.as_ref().map(|l| l as &dyn AgentLlm)
    }
    fn researcher_model(&self) -> Option<&ResolvedModel> {
        self.researcher_model_val.as_ref()
    }
    fn memory_researcher(&self) -> Option<&MemoryResearcher> {
        self.researcher.as_ref()
    }
    fn indexer(&self) -> Option<&dyn AgentIndexer> {
        None
    }
    fn rag(&self) -> &dyn AgentRag {
        &self.rag
    }
    fn image_dir(&self) -> &str {
        &self.image_dir_val
    }
    fn llm_client(&self) -> Option<&LlmClient> {
        None
    }
    fn image_gen_config(&self) -> Option<&ImageGenConfig> {
        None
    }
    fn search_config(&self) -> &SearchConfig {
        &self.search_config_val
    }
    fn autonomy_manager(&self) -> Option<&AutonomyManager> {
        self.autonomy_mgr.as_ref()
    }
    fn character_name(&self) -> &str {
        &self.character_name_val
    }
    fn markdown_store(&self) -> Option<&MarkdownMemoryStore> {
        self.markdown_store_val.as_ref()
    }
}

// ── make_image_entry ────────────────────────────────────────────────────

/// Build a memory `Entry` with memory_type "image" for testing image tools.
pub fn make_image_entry(id: &str, summary: &str, image_path: &str) -> crate::memory::db::Entry {
    let now = chrono::Local::now().to_rfc3339();
    crate::memory::db::Entry {
        id: id.to_string(),
        memory_type: "image".to_string(),
        source: "user".to_string(),
        reason: "upload".to_string(),
        status: "active".to_string(),
        confidence: 1.0,
        summary_text: summary.to_string(),
        topic_tags: "image".to_string(),
        topic_key: "images".to_string(),
        start_timestamp: now.clone(),
        end_timestamp: now.clone(),
        message_count: 0,
        source_entry_ids: String::new(),
        related_entry_ids: String::new(),
        superseded_by: String::new(),
        created_at: now.clone(),
        updated_at: now,
        entry_type: String::new(),
        image_path: image_path.to_string(),
        collated_at: String::new(),
    }
}
