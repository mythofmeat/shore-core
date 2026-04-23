//! Shared test mocks and builders for shore-daemon tests.
//!
//! This module consolidates duplicated test infrastructure
//! (test_model, TestToolContext) so each test file can import from a
//! single source.

use crate::autonomy::manager::AutonomyManager;
use crate::memory::compaction_impls::ImageGenConfig;
use crate::memory::markdown_store::MarkdownMemoryStore;
use crate::memory::memory_llm::{MemoryLlm, MockMemoryLlm};
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

// ── TestToolContext ─────────────────────────────────────────────────────

/// Shared `ToolContext` implementation for unit tests.
pub struct TestToolContext {
    pub memory_llm: MockMemoryLlm,
    pub model: ResolvedModel,
    pub image_dir_val: String,
    pub search_config_val: SearchConfig,
    pub autonomy_mgr: Option<AutonomyManager>,
    pub character_name_val: String,
    pub markdown_store_val: Option<MarkdownMemoryStore>,
    pub memory_access_allowed_val: bool,
    pub memory_read_allowed_val: bool,
    pub memory_write_allowed_val: bool,
    pub workspace_dir_val: String,
}

impl TestToolContext {
    /// Create a default test context.
    pub fn new() -> Self {
        Self {
            memory_llm: MockMemoryLlm::new(vec![]),
            model: test_model(),
            image_dir_val: "/tmp/test_images".to_string(),
            search_config_val: SearchConfig::default(),
            autonomy_mgr: None,
            character_name_val: String::new(),
            markdown_store_val: None,
            memory_access_allowed_val: true,
            memory_read_allowed_val: true,
            memory_write_allowed_val: true,
            workspace_dir_val: String::new(),
        }
    }

    /// Create with a custom memory LLM (for canned responses).
    pub fn with_memory_llm(mut self, llm: MockMemoryLlm) -> Self {
        self.memory_llm = llm;
        self
    }

    /// Set a custom image directory.
    pub fn with_image_dir(mut self, dir: &str) -> Self {
        self.image_dir_val = dir.to_string();
        self
    }

    /// Set an autonomy manager and character name for activity tests.
    pub fn with_autonomy(mut self, mgr: AutonomyManager, character: &str) -> Self {
        self.autonomy_mgr = Some(mgr);
        self.character_name_val = character.to_string();
        self
    }

    /// Set a markdown memory store.
    pub fn with_markdown_store(mut self, store: MarkdownMemoryStore) -> Self {
        self.markdown_store_val = Some(store);
        self
    }

    /// Allow or deny memory access for dispatch-layer tests.
    pub fn with_memory_access_allowed(mut self, allowed: bool) -> Self {
        self.memory_access_allowed_val = allowed;
        self.memory_read_allowed_val = allowed;
        self.memory_write_allowed_val = allowed;
        self
    }

    /// Allow or deny memory read access for dispatch-layer tests.
    pub fn with_memory_read_allowed(mut self, allowed: bool) -> Self {
        self.memory_read_allowed_val = allowed;
        self
    }

    /// Allow or deny memory write access for dispatch-layer tests.
    pub fn with_memory_write_allowed(mut self, allowed: bool) -> Self {
        self.memory_write_allowed_val = allowed;
        self
    }

    /// Set a workspace directory for workspace dispatch tests.
    pub fn with_workspace_dir(mut self, dir: &str) -> Self {
        self.workspace_dir_val = dir.to_string();
        self
    }
}

impl ToolContext for TestToolContext {
    fn memory_llm(&self) -> &dyn MemoryLlm {
        &self.memory_llm
    }
    fn memory_model(&self) -> &ResolvedModel {
        &self.model
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
    fn memory_access_allowed(&self) -> bool {
        self.memory_access_allowed_val
    }
    fn memory_read_allowed(&self) -> bool {
        self.memory_read_allowed_val
    }
    fn memory_write_allowed(&self) -> bool {
        self.memory_write_allowed_val
    }
    fn workspace_dir(&self) -> &str {
        &self.workspace_dir_val
    }
    fn config_dir(&self) -> &str {
        ""
    }
    fn defer_edit(&self, _path: &str) {}
}
