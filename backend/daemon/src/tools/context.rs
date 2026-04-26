//! Shared tool context — canonical `ToolContext` implementation.
//!
//! Both the message handler and heartbeat ticks construct a `SharedToolContext`
//! with their caller-specific wiring. Adding a new `ToolContext` method requires
//! updating this struct + impl (one place) instead of two separate copies.

use std::path::Path;

use shore_config::app::{RetrievalConfig, SearchConfig};
use shore_llm::LlmClient;

use crate::memory::compaction_impls::ImageGenConfig;
use crate::memory::markdown_store::MarkdownMemoryStore;
use crate::memory::retrieval::EmbeddingConfig;

use super::ToolContext;

// ---------------------------------------------------------------------------
// SharedToolContext
// ---------------------------------------------------------------------------

/// Shared tool context holding all dependencies needed by tool handlers.
///
/// Used directly by heartbeat ticks. Wrapped by `HandlerToolContext` in the
/// message handler (which adds `AutonomyManager` access).
pub(crate) struct SharedToolContext {
    pub(crate) image_dir_val: String,
    pub(crate) llm_client_val: LlmClient,
    pub(crate) image_gen_config_val: Option<ImageGenConfig>,
    pub(crate) search_config_val: SearchConfig,
    pub(crate) character_name_val: String,
    pub(crate) workspace_dir_val: String,
    pub(crate) markdown_store_val: Option<MarkdownMemoryStore>,
    pub(crate) memory_retrieval_config_val: RetrievalConfig,
    pub(crate) embedding_config_val: Option<EmbeddingConfig>,
    pub(crate) memory_index_path_val: std::path::PathBuf,
    pub(crate) memory_access_allowed_val: bool,
    pub(crate) memory_read_allowed_val: bool,
    pub(crate) memory_write_allowed_val: bool,
    pub(crate) config_dir_val: String,
    pub(crate) character_data_dir_val: String,
}

impl ToolContext for SharedToolContext {
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
    fn workspace_dir(&self) -> &str {
        &self.workspace_dir_val
    }
    fn character_data_dir(&self) -> &str {
        &self.character_data_dir_val
    }
    fn markdown_store(&self) -> Option<&MarkdownMemoryStore> {
        self.markdown_store_val.as_ref()
    }
    fn memory_retrieval_config(&self) -> &RetrievalConfig {
        &self.memory_retrieval_config_val
    }
    fn embedding_config(&self) -> Option<&EmbeddingConfig> {
        self.embedding_config_val.as_ref()
    }
    fn memory_index_path(&self) -> Option<&std::path::Path> {
        Some(&self.memory_index_path_val)
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
