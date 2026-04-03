use std::future::Future;
use std::pin::Pin;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Half-life in days for confidence decay.
pub(super) const DEFAULT_DECAY_HALF_LIFE_DAYS: f64 = 30.0;

/// Floor below which confidence is not further decayed.
pub(super) const DEFAULT_DECAY_FLOOR: f64 = 0.1;

/// Days before a previously-collated entry becomes eligible for reconsideration.
pub(super) const DEFAULT_RECONSIDER_TTL_DAYS: f64 = 30.0;

/// Decay/reconsideration parameters for the collation pipeline.
///
/// Named `DecayConfig` (not `CollationConfig`) to avoid collision with the
/// user-facing `shore_config::app::CollationConfig` which controls enable/auto_run/batch_limit.
#[derive(Debug, Clone)]
pub struct DecayConfig {
    /// Half-life in days for Phase 4 confidence decay.
    pub decay_half_life_days: f64,
    /// Minimum confidence floor — entries at or below this are not decayed further.
    pub decay_floor: f64,
    /// Days before a previously-collated entry becomes eligible for reconsideration.
    pub reconsider_ttl_days: f64,
}

impl Default for DecayConfig {
    fn default() -> Self {
        Self {
            decay_half_life_days: DEFAULT_DECAY_HALF_LIFE_DAYS,
            decay_floor: DEFAULT_DECAY_FLOOR,
            reconsider_ttl_days: DEFAULT_RECONSIDER_TTL_DAYS,
        }
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum CollationError {
    #[error("llm: {0}")]
    Llm(String),
    #[error("db: {0}")]
    Db(String),
}

// ---------------------------------------------------------------------------
// LLM response types
// ---------------------------------------------------------------------------

/// Fields for an entry produced by the refine phase.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RefineEntryFields {
    pub summary_text: String,
    pub topic_tags: String,
    pub topic_key: String,
    pub confidence: f64,
}

/// A single action from the unified refine phase.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "action")]
pub enum RefineAction {
    #[serde(rename = "merge")]
    Merge {
        source_entry_ids: Vec<String>,
        result: RefineEntryFields,
        reason: String,
    },
    #[serde(rename = "split")]
    Split {
        source_entry_id: String,
        results: Vec<RefineEntryFields>,
        reason: String,
    },
    #[serde(rename = "update")]
    Update {
        entry_id: String,
        result: RefineEntryFields,
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// Trait for LLM dependency
// ---------------------------------------------------------------------------

/// LLM client for the collation refine phase.
pub trait CollationLlm: Send + Sync {
    /// Given a prompt with candidate and context entries, return refine actions.
    fn refine(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RefineAction>, CollationError>> + Send + '_>>;
}

// ---------------------------------------------------------------------------
// Collation outcome
// ---------------------------------------------------------------------------

/// Summary of what the collation pipeline did.
#[derive(Debug, Default)]
pub struct CollationOutcome {
    pub refine_merges: usize,
    pub refine_splits: usize,
    pub refine_updates: usize,
    pub refine_new_entries: usize,
    pub refine_kept: usize,
    pub entries_decayed: usize,
    pub entries_skipped: usize,
    pub timestamps_backfilled: usize,
}
