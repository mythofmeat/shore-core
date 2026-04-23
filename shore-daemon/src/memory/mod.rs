//! Memory subsystem.
//!
//! Active memory for characters is markdown-first:
//! - runtime memory tools use [`markdown_store`] and [`markdown_query`]
//! - compaction writes markdown files directly
//! - protected character/user/system edits are staged by [`deferred_edits`]

pub mod compaction;
pub mod compaction_impls;
pub mod deferred_edits;
pub mod markdown_query;
pub mod markdown_store;
pub mod memory_llm;
