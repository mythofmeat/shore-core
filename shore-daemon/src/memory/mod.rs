//! Memory subsystem.
//!
//! Active memory for characters is markdown-first:
//! - runtime memory tools use [`markdown_store`] and [`markdown_query`]
//! - compaction writes markdown files directly
//! - protected character/user/system edits are staged by [`deferred_edits`]
//!
//! The SQLite/vector/agent modules below are retained as legacy compatibility,
//! migration, benchmarks, and tests. They should not be used for new runtime
//! memory features.

// Legacy SQLite/tool-loop memory agent surface. Kept public for compatibility
// tests and historical benchmarks; not part of the active markdown memory path.
pub mod agent;
pub mod agent_llm;
pub mod compaction;
pub mod compaction_impls;
// Legacy SQLite entry store retained for migration and compatibility code.
pub mod db;
pub mod deferred_edits;
// Legacy markdown-to-SQLite index retained for compatibility tooling.
pub mod markdown_index;
pub mod markdown_query;
pub mod markdown_store;
// Legacy retrieval stack retained for compatibility tests/benchmarks.
pub mod rag;
pub mod researcher;
pub mod search;
pub mod vectorstore;
