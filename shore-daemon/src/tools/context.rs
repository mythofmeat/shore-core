//! Shared tool context — canonical `ToolContext` implementation.
//!
//! Both the message handler and interiority ticks construct a `SharedToolContext`
//! with their caller-specific wiring. Adding a new `ToolContext` method requires
//! updating this struct + impl (one place) instead of two separate copies.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use shore_config::app::SearchConfig;
use shore_config::models::ResolvedModel;
use shore_llm_client::LlmClient;

use crate::memory::agent::types::{AgentIndexer, AgentSearchContext};
use crate::memory::agent::{AgentError, AgentRag, MemoryAgent, RagHit};
use crate::memory::agent_llm::{AgentLlm, RealAgentLlm};
use crate::memory::compaction_impls::ImageGenConfig;
use crate::memory::db::MemoryDB;
use crate::memory::researcher::MemoryResearcher;

use super::ToolContext;

// ---------------------------------------------------------------------------
// NoopRag — legacy stub, single canonical definition
// ---------------------------------------------------------------------------

pub(crate) struct NoopRag;

impl AgentRag for NoopRag {
    fn query(
        &self,
        _query: &str,
        _top_k: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RagHit>, AgentError>> + Send + '_>> {
        Box::pin(async { Ok(vec![]) })
    }
}

// ---------------------------------------------------------------------------
// SharedToolContext
// ---------------------------------------------------------------------------

/// Shared tool context holding all dependencies needed by tool handlers.
///
/// Used directly by interiority ticks. Wrapped by `HandlerToolContext` in the
/// message handler (which adds `AutonomyManager` access).
pub(crate) struct SharedToolContext {
    pub(crate) db: Arc<MemoryDB>,
    pub(crate) agent: MemoryAgent,
    pub(crate) agent_llm: RealAgentLlm,
    pub(crate) agent_model_val: ResolvedModel,
    pub(crate) researcher: Option<MemoryResearcher>,
    pub(crate) researcher_llm_val: Option<RealAgentLlm>,
    pub(crate) researcher_model_val: Option<ResolvedModel>,
    pub(crate) rag: NoopRag,
    pub(crate) search_ctx: Option<AgentSearchContext>,
    pub(crate) image_dir_val: String,
    pub(crate) llm_client_val: LlmClient,
    pub(crate) image_gen_config_val: Option<ImageGenConfig>,
    pub(crate) search_config_val: SearchConfig,
    pub(crate) character_name_val: String,
    pub(crate) scratchpad_dir_val: String,
}

impl ToolContext for SharedToolContext {
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
        &self.agent_model_val
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
    fn search_context(&self) -> Option<&AgentSearchContext> {
        self.search_ctx.as_ref()
    }
    fn rag(&self) -> &dyn AgentRag {
        &self.rag
    }
    fn image_dir(&self) -> &str {
        &self.image_dir_val
    }
    fn llm_client(&self) -> Option<&LlmClient> {
        Some(&self.llm_client_val)
    }
    fn image_gen_config(&self) -> Option<&ImageGenConfig> {
        self.image_gen_config_val.as_ref()
    }
    fn search_config(&self) -> &SearchConfig {
        &self.search_config_val
    }
    fn character_name(&self) -> &str {
        &self.character_name_val
    }
    fn scratchpad_dir(&self) -> &str {
        &self.scratchpad_dir_val
    }
}
