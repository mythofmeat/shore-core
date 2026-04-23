//! Shared tool context — canonical `ToolContext` implementation.
//!
//! Both the message handler and interiority ticks construct a `SharedToolContext`
//! with their caller-specific wiring. Adding a new `ToolContext` method requires
//! updating this struct + impl (one place) instead of two separate copies.

use std::path::Path;

use shore_config::app::SearchConfig;
use shore_config::models::ResolvedModel;
use shore_llm_client::LlmClient;

use crate::memory::agent_llm::{AgentLlm, RealAgentLlm};
use crate::memory::compaction_impls::ImageGenConfig;
use crate::memory::markdown_store::MarkdownMemoryStore;

use super::ToolContext;

// ---------------------------------------------------------------------------
// SharedToolContext
// ---------------------------------------------------------------------------

/// Shared tool context holding all dependencies needed by tool handlers.
///
/// Used directly by interiority ticks. Wrapped by `HandlerToolContext` in the
/// message handler (which adds `AutonomyManager` access).
pub(crate) struct SharedToolContext {
    pub(crate) agent_llm: RealAgentLlm,
    pub(crate) agent_model_val: ResolvedModel,
    pub(crate) image_dir_val: String,
    pub(crate) llm_client_val: LlmClient,
    pub(crate) image_gen_config_val: Option<ImageGenConfig>,
    pub(crate) search_config_val: SearchConfig,
    pub(crate) character_name_val: String,
    pub(crate) scratchpad_dir_val: String,
    pub(crate) workspace_dir_val: String,
    pub(crate) markdown_store_val: Option<MarkdownMemoryStore>,
    pub(crate) config_dir_val: String,
    pub(crate) character_data_dir_val: String,
}

impl ToolContext for SharedToolContext {
    fn agent_llm(&self) -> &dyn AgentLlm {
        &self.agent_llm
    }
    fn agent_model(&self) -> &ResolvedModel {
        &self.agent_model_val
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
    fn workspace_dir(&self) -> &str {
        &self.workspace_dir_val
    }
    fn markdown_store(&self) -> Option<&MarkdownMemoryStore> {
        self.markdown_store_val.as_ref()
    }
    fn config_dir(&self) -> &str {
        &self.config_dir_val
    }
    fn defer_edit(&self, path: &str) {
        if !crate::memory::deferred_edits::is_protected_path(path) {
            return;
        }
        let data_dir = Path::new(&self.character_data_dir_val);
        if let Err(e) = crate::memory::deferred_edits::queue_deferred_edit(data_dir, path) {
            tracing::warn!(path = %path, error = %e, "Failed to queue deferred edit");
        }
    }
}
