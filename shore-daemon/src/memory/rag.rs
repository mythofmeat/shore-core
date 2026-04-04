use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// RRF constant (standard default is 60).
const DEFAULT_RRF_K: f64 = 60.0;

/// Status weight multipliers for lifecycle scoring.
const STATUS_WEIGHT_ACTIVE: f64 = 1.0;
const STATUS_WEIGHT_PROTECTED: f64 = 0.9;
const STATUS_WEIGHT_SUPERSEDED: f64 = 0.3;

/// Maximum recency boost (applied to entries created within the last hour).
const RECENCY_BOOST_MAX: f64 = 0.15;

/// Half-life for recency decay in seconds (7 days).
const RECENCY_HALF_LIFE_SECS: f64 = 7.0 * 24.0 * 3600.0;

/// Confidence floor — entries below this get penalized.
const CONFIDENCE_FLOOR: f64 = 0.5;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single scored search result from one retrieval source.
#[derive(Debug, Clone)]
pub struct SourceResult {
    pub entry_id: String,
    pub score: f64,
}

/// Metadata about an entry used for lifecycle scoring.
#[derive(Debug, Clone)]
pub struct EntryMeta {
    pub entry_id: String,
    pub status: String,
    pub confidence: f64,
    /// ISO-8601 timestamp (RFC 3339).
    pub created_at: String,
}

/// A ranked result from the RAG pipeline with combined score.
#[derive(Debug, Clone)]
pub struct RagResult {
    pub entry_id: String,
    pub score: f64,
}

// ---------------------------------------------------------------------------
// RagPipeline
// ---------------------------------------------------------------------------

pub struct RagPipeline {
    /// Maximum number of results to return.
    pub top_k: usize,
    /// RRF constant (higher = less emphasis on top ranks).
    pub rrf_k: f64,
}

impl Default for RagPipeline {
    fn default() -> Self {
        Self {
            top_k: 32,
            rrf_k: DEFAULT_RRF_K,
        }
    }
}

impl RagPipeline {
    pub fn new(top_k: usize) -> Self {
        Self {
            top_k,
            rrf_k: DEFAULT_RRF_K,
        }
    }

    /// Retrieve and rank memory entries by fusing vector + BM25 results with
    /// reciprocal rank fusion, then applying lifecycle scoring.
    ///
    /// Returns an empty list when `is_private` is true.
    pub fn retrieve(
        &self,
        vector_results: &[SourceResult],
        bm25_results: &[SourceResult],
        metadata: &[EntryMeta],
        is_private: bool,
    ) -> Vec<RagResult> {
        // Suppressed entirely when conversation is private.
        if is_private {
            return vec![];
        }

        // Step 1: Reciprocal rank fusion.
        let mut rrf_scores = self.reciprocal_rank_fusion(vector_results, bm25_results);

        // Step 2: Apply lifecycle scoring.
        let meta_map: HashMap<&str, &EntryMeta> =
            metadata.iter().map(|m| (m.entry_id.as_str(), m)).collect();

        let now = chrono::Utc::now();

        for result in &mut rrf_scores {
            if let Some(meta) = meta_map.get(result.entry_id.as_str()) {
                let lifecycle = lifecycle_score(meta, now);
                result.score *= lifecycle;
            }
        }

        // Step 3: Re-sort by adjusted score descending, cap at top_k.
        rrf_scores.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        rrf_scores.truncate(self.top_k);
        rrf_scores
    }

    /// Compute reciprocal rank fusion scores across two ranked lists.
    ///
    /// RRF score for entry e = Σ_source 1 / (k + rank_in_source(e))
    /// where rank is 1-based.
    fn reciprocal_rank_fusion(
        &self,
        vector_results: &[SourceResult],
        bm25_results: &[SourceResult],
    ) -> Vec<RagResult> {
        let mut scores: HashMap<String, f64> = HashMap::new();

        // Vector results are assumed to be sorted by score descending.
        for (rank, result) in vector_results.iter().enumerate() {
            let rrf = 1.0 / (self.rrf_k + (rank + 1) as f64);
            *scores.entry(result.entry_id.clone()).or_insert(0.0) += rrf;
        }

        // BM25 results are assumed to be sorted by score descending.
        for (rank, result) in bm25_results.iter().enumerate() {
            let rrf = 1.0 / (self.rrf_k + (rank + 1) as f64);
            *scores.entry(result.entry_id.clone()).or_insert(0.0) += rrf;
        }

        let mut results: Vec<RagResult> = scores
            .into_iter()
            .map(|(entry_id, score)| RagResult { entry_id, score })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        results
    }
}

// ---------------------------------------------------------------------------
// Lifecycle scoring
// ---------------------------------------------------------------------------

/// Compute a lifecycle multiplier in (0, ~1.15] based on entry metadata.
///
/// Components:
/// - **Status weight**: active=1.0, protected=0.9, superseded=0.3
/// - **Recency boost**: exponential decay from creation time (max +0.15)
/// - **Confidence penalty**: entries below the confidence floor are penalized
fn lifecycle_score(meta: &EntryMeta, now: chrono::DateTime<chrono::Utc>) -> f64 {
    let status_w = match meta.status.as_str() {
        "active" => STATUS_WEIGHT_ACTIVE,
        "protected" => STATUS_WEIGHT_PROTECTED,
        "superseded" => STATUS_WEIGHT_SUPERSEDED,
        _ => STATUS_WEIGHT_ACTIVE, // unknown status treated as active
    };

    // Recency boost: exponential decay based on age.
    let recency = if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&meta.created_at) {
        let age_secs = (now - created.with_timezone(&chrono::Utc))
            .num_seconds()
            .max(0) as f64;
        RECENCY_BOOST_MAX * (-age_secs * (2.0_f64.ln()) / RECENCY_HALF_LIFE_SECS).exp()
    } else {
        0.0 // unparseable timestamp → no boost
    };

    // Confidence factor: linear penalty below floor, neutral above.
    let confidence_factor = if meta.confidence < CONFIDENCE_FLOOR {
        // Scale from 0.5 (at confidence=0) to 1.0 (at confidence=floor)
        0.5 + 0.5 * (meta.confidence / CONFIDENCE_FLOOR)
    } else {
        1.0
    };

    (status_w + recency) * confidence_factor
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn source(id: &str, score: f64) -> SourceResult {
        SourceResult {
            entry_id: id.to_string(),
            score,
        }
    }

    fn meta(id: &str, status: &str, confidence: f64, created_at: &str) -> EntryMeta {
        EntryMeta {
            entry_id: id.to_string(),
            status: status.to_string(),
            confidence,
            created_at: created_at.to_string(),
        }
    }

    fn recent_timestamp() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    fn old_timestamp() -> String {
        (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339()
    }

    // -- RRF fusion -------------------------------------------------------

    #[test]
    fn test_rrf_fusion_basic_ranking() {
        let pipeline = RagPipeline::new(10);

        // e1 is rank 1 in both sources → highest RRF
        // e2 is rank 2 in vector only
        // e3 is rank 2 in BM25 only
        let vector = vec![source("e1", 0.9), source("e2", 0.7)];
        let bm25 = vec![source("e1", 5.0), source("e3", 3.0)];
        let now = recent_timestamp();
        let metas = vec![
            meta("e1", "active", 0.9, &now),
            meta("e2", "active", 0.9, &now),
            meta("e3", "active", 0.9, &now),
        ];

        let results = pipeline.retrieve(&vector, &bm25, &metas, false);

        // e1 appears in both → should be ranked first.
        assert!(!results.is_empty());
        assert_eq!(results[0].entry_id, "e1");
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn test_rrf_fusion_disjoint_sources() {
        let pipeline = RagPipeline::new(10);

        // Completely disjoint results.
        let vector = vec![source("v1", 0.9), source("v2", 0.8)];
        let bm25 = vec![source("b1", 5.0), source("b2", 3.0)];
        let now = recent_timestamp();
        let metas = vec![
            meta("v1", "active", 0.9, &now),
            meta("v2", "active", 0.9, &now),
            meta("b1", "active", 0.9, &now),
            meta("b2", "active", 0.9, &now),
        ];

        let results = pipeline.retrieve(&vector, &bm25, &metas, false);
        assert_eq!(results.len(), 4);

        // Rank-1 items from each source should tie (same RRF contribution).
        let ids: Vec<&str> = results.iter().map(|r| r.entry_id.as_str()).collect();
        assert!(ids.contains(&"v1"));
        assert!(ids.contains(&"b1"));
    }

    #[test]
    fn test_rrf_fusion_single_source_only() {
        let pipeline = RagPipeline::new(10);

        let vector = vec![source("e1", 0.9), source("e2", 0.7)];
        let bm25: Vec<SourceResult> = vec![];
        let now = recent_timestamp();
        let metas = vec![
            meta("e1", "active", 0.9, &now),
            meta("e2", "active", 0.9, &now),
        ];

        let results = pipeline.retrieve(&vector, &bm25, &metas, false);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].entry_id, "e1");
    }

    // -- Lifecycle scoring ------------------------------------------------

    #[test]
    fn test_lifecycle_status_weighting() {
        let pipeline = RagPipeline::new(10);
        let now = recent_timestamp();

        // Same rank, same confidence — status should differentiate.
        let vector = vec![
            source("active_e", 0.9),
            source("superseded_e", 0.8),
            source("protected_e", 0.7),
        ];
        let bm25 = vec![
            source("active_e", 5.0),
            source("superseded_e", 4.0),
            source("protected_e", 3.0),
        ];
        let metas = vec![
            meta("active_e", "active", 0.9, &now),
            meta("superseded_e", "superseded", 0.9, &now),
            meta("protected_e", "protected", 0.9, &now),
        ];

        let results = pipeline.retrieve(&vector, &bm25, &metas, false);

        // active_e should still be first (highest RRF + highest status weight)
        assert_eq!(results[0].entry_id, "active_e");
        // superseded should be last despite same RRF rank pattern
        assert_eq!(results[2].entry_id, "superseded_e");
    }

    #[test]
    fn test_lifecycle_recency_boost() {
        let pipeline = RagPipeline::new(10);

        // Two entries at same RRF rank (only in vector results).
        // One is recent, one is old.
        let vector = vec![source("recent_e", 0.9)];
        let bm25 = vec![source("old_e", 5.0)];

        let now_ts = recent_timestamp();
        let old_ts = old_timestamp();
        let metas = vec![
            meta("recent_e", "active", 0.9, &now_ts),
            meta("old_e", "active", 0.9, &old_ts),
        ];

        let results = pipeline.retrieve(&vector, &bm25, &metas, false);
        assert_eq!(results.len(), 2);

        // Both are rank 1 in their respective source → same base RRF.
        // Recent entry should score higher due to recency boost.
        let recent_score = results
            .iter()
            .find(|r| r.entry_id == "recent_e")
            .unwrap()
            .score;
        let old_score = results
            .iter()
            .find(|r| r.entry_id == "old_e")
            .unwrap()
            .score;
        assert!(
            recent_score > old_score,
            "Recent ({recent_score}) should score higher than old ({old_score})"
        );
    }

    #[test]
    fn test_lifecycle_low_confidence_penalty() {
        let pipeline = RagPipeline::new(10);
        let now = recent_timestamp();

        // Same rank, same status — confidence should differentiate.
        let vector = vec![source("high_conf", 0.9), source("low_conf", 0.8)];
        let bm25 = vec![source("high_conf", 5.0), source("low_conf", 4.0)];
        let metas = vec![
            meta("high_conf", "active", 0.95, &now),
            meta("low_conf", "active", 0.2, &now), // below CONFIDENCE_FLOOR
        ];

        let results = pipeline.retrieve(&vector, &bm25, &metas, false);
        assert_eq!(results[0].entry_id, "high_conf");

        // Low-confidence entry should have a significantly lower score.
        assert!(results[0].score > results[1].score * 1.2);
    }

    // -- Private conversation suppression ---------------------------------

    #[test]
    fn test_private_conversation_returns_empty() {
        let pipeline = RagPipeline::new(10);

        let vector = vec![source("e1", 0.9), source("e2", 0.7)];
        let bm25 = vec![source("e1", 5.0)];
        let now = recent_timestamp();
        let metas = vec![
            meta("e1", "active", 0.9, &now),
            meta("e2", "active", 0.9, &now),
        ];

        let results = pipeline.retrieve(&vector, &bm25, &metas, true);
        assert!(results.is_empty());
    }

    // -- Top-K capping ----------------------------------------------------

    #[test]
    fn test_top_k_caps_results() {
        let pipeline = RagPipeline::new(3);
        let now = recent_timestamp();

        let vector: Vec<SourceResult> = (0..10)
            .map(|i| source(&format!("e{i}"), 1.0 - i as f64 * 0.05))
            .collect();
        let bm25: Vec<SourceResult> = vec![];
        let metas: Vec<EntryMeta> = (0..10)
            .map(|i| meta(&format!("e{i}"), "active", 0.9, &now))
            .collect();

        let results = pipeline.retrieve(&vector, &bm25, &metas, false);
        assert_eq!(results.len(), 3);
    }

    // -- Edge cases -------------------------------------------------------

    #[test]
    fn test_empty_inputs_returns_empty() {
        let pipeline = RagPipeline::new(10);
        let results = pipeline.retrieve(&[], &[], &[], false);
        assert!(results.is_empty());
    }

    #[test]
    fn test_missing_metadata_uses_rrf_only() {
        let pipeline = RagPipeline::new(10);

        let vector = vec![source("e1", 0.9)];
        let bm25 = vec![source("e1", 5.0)];
        // No metadata provided for e1.
        let results = pipeline.retrieve(&vector, &bm25, &[], false);

        // Should still return results — just no lifecycle adjustment.
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry_id, "e1");
    }
}
