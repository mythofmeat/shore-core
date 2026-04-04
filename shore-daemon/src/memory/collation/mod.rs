pub mod clustering;
pub mod prompt;
pub mod types;

pub use prompt::DEFAULT_REFINE_PROMPT;
pub use types::*;

use crate::memory::agent::AgentIndexer;
use crate::memory::db::{Entry, MemoryDB};
use crate::memory::vectorstore::VectorStore;
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::path::Path;

// ---------------------------------------------------------------------------
// CollationManager
// ---------------------------------------------------------------------------

pub struct CollationManager {
    config: DecayConfig,
}

impl CollationManager {
    pub fn new(config: DecayConfig) -> Self {
        Self { config }
    }

    /// Generate an entry ID in the standard format: YYYYMMDD_HHMMSS_N
    fn generate_entry_id(index: usize) -> String {
        let now = Utc::now();
        format!("{}_{}", now.format("%Y%m%d_%H%M%S"), index)
    }

    /// Unified candidate selection for the refine phase.
    /// An entry is a candidate if:
    /// - It's not an image entry
    /// - It has never been collated (collated_at empty), OR
    /// - It was modified since last collation (updated_at > collated_at), OR
    /// - Its TTL has expired (collated_at older than reconsider_ttl_days)
    fn is_refine_candidate(&self, e: &Entry) -> bool {
        if !e.image_path.is_empty() {
            return false;
        }
        if e.collated_at.is_empty() {
            return true;
        }
        // Modified since last collation
        if e.updated_at.as_str() > e.collated_at.as_str() {
            return true;
        }
        // TTL expired
        let ttl_seconds = self.config.reconsider_ttl_days * 86400.0;
        chrono::DateTime::parse_from_rfc3339(&e.collated_at)
            .map(|ca| {
                let age = (Utc::now() - ca.with_timezone(&Utc)).num_seconds() as f64;
                age > ttl_seconds
            })
            .unwrap_or(true)
    }

    /// Run the collation pipeline: backfill → refine → confidence decay → stamp.
    ///
    /// `vars` provides template variables like `{{char}}` and `{{user}}`.
    pub async fn run(
        &self,
        db: &MemoryDB,
        llm: &dyn CollationLlm,
        refine_template: &str,
        vars: &HashMap<String, String>,
        indexer: Option<&dyn AgentIndexer>,
        vector_store: Option<&VectorStore>,
        limit: Option<usize>,
    ) -> Result<CollationOutcome, CollationError> {
        let mut outcome = CollationOutcome::default();
        let mut id_counter: usize = 0;
        let mut candidates_processed: HashSet<String> = HashSet::new();

        // Phase 0: Backfill missing timestamps (incremental, no LLM)
        self.phase_backfill_timestamps(db, 20, &mut outcome)?;

        // Phase 1: Refine (unified merge/split/update)
        self.phase_refine(
            db,
            llm,
            refine_template,
            vars,
            &mut outcome,
            &mut id_counter,
            indexer,
            vector_store,
            limit,
            &mut candidates_processed,
        )
        .await?;

        // Phase 2: Confidence decay (math only)
        self.phase_confidence_decay(db, &mut outcome, &mut candidates_processed)?;

        // Stamp only entries that were examined as candidates this run.
        let stamp = Utc::now().to_rfc3339();
        for id in &candidates_processed {
            let _ = db.stamp_collated(id, &stamp);
        }

        Ok(outcome)
    }

    // -----------------------------------------------------------------------
    // Phase 0: Backfill missing timestamps (incremental, no LLM)
    // -----------------------------------------------------------------------

    fn phase_backfill_timestamps(
        &self,
        db: &MemoryDB,
        batch_size: usize,
        outcome: &mut CollationOutcome,
    ) -> Result<(), CollationError> {
        let entries = db
            .get_entries_by_status("active")
            .map_err(|e| CollationError::Db(e.to_string()))?;

        let candidates: Vec<&Entry> = entries
            .iter()
            .filter(|e| e.start_timestamp.is_empty())
            .take(batch_size)
            .collect();

        if candidates.is_empty() {
            return Ok(());
        }

        let mut from_ancestors = 0usize;
        let mut from_created_at = 0usize;

        for entry in &candidates {
            let (start, end, source) = self.resolve_timestamps_from_ancestors(db, entry)?;

            let mut updated = (*entry).clone();
            updated.start_timestamp = start.clone();
            updated.end_timestamp = end.clone();
            updated.updated_at = Utc::now().to_rfc3339();
            db.update_entry(&updated)
                .map_err(|e| CollationError::Db(e.to_string()))?;

            if source == "ancestors" {
                from_ancestors += 1;
            } else {
                from_created_at += 1;
            }

            outcome.timestamps_backfilled += 1;
        }

        // Single summary changelog entry for all backfills.
        if outcome.timestamps_backfilled > 0 {
            let _ = db.append_changelog(
                "backfill_timestamp",
                &format!(
                    "Backfilled timestamps for {} entries ({} from ancestors, {} from created_at)",
                    outcome.timestamps_backfilled, from_ancestors, from_created_at,
                ),
            );
        }

        Ok(())
    }

    /// Walk the source_entry_ids chain to find ancestor timestamps.
    /// Returns (start, end, source_description).
    fn resolve_timestamps_from_ancestors(
        &self,
        db: &MemoryDB,
        entry: &Entry,
    ) -> Result<(String, String, String), CollationError> {
        // Try to find timestamps by walking source_entry_ids
        if !entry.source_entry_ids.is_empty() {
            let mut min_start = String::new();
            let mut max_end = String::new();
            let mut found_any = false;

            let mut to_visit: Vec<String> = entry
                .source_entry_ids
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();

            let mut visited = std::collections::HashSet::new();
            // Cap traversal depth to avoid runaway chains
            let mut depth = 0;
            const MAX_DEPTH: usize = 10;

            while !to_visit.is_empty() && depth < MAX_DEPTH {
                depth += 1;
                let mut next_visit = Vec::new();

                for id in &to_visit {
                    if !visited.insert(id.clone()) {
                        continue;
                    }
                    if let Some(ancestor) = db
                        .get_entry(id)
                        .map_err(|e| CollationError::Db(e.to_string()))?
                    {
                        if !ancestor.start_timestamp.is_empty() {
                            if min_start.is_empty()
                                || ancestor.start_timestamp.as_str() < min_start.as_str()
                            {
                                min_start = ancestor.start_timestamp.clone();
                            }
                            found_any = true;
                        }
                        if !ancestor.end_timestamp.is_empty()
                            && (max_end.is_empty()
                                || ancestor.end_timestamp.as_str() > max_end.as_str())
                            {
                                max_end = ancestor.end_timestamp.clone();
                            }
                        // If this ancestor also lacks timestamps, walk its sources
                        if ancestor.start_timestamp.is_empty()
                            && !ancestor.source_entry_ids.is_empty()
                        {
                            for src in ancestor.source_entry_ids.split(',') {
                                let src = src.trim().to_string();
                                if !src.is_empty() && !visited.contains(&src) {
                                    next_visit.push(src);
                                }
                            }
                        }
                    }
                }

                to_visit = next_visit;
            }

            if found_any {
                // If we found start but not end, use start as end too
                if max_end.is_empty() {
                    max_end = min_start.clone();
                }
                return Ok((min_start, max_end, "ancestors".to_string()));
            }
        }

        // Fallback: use created_at
        Ok((
            entry.created_at.clone(),
            entry.created_at.clone(),
            "created_at".to_string(),
        ))
    }

    // -----------------------------------------------------------------------
    // Phase 1: Refine — unified merge/split/update
    // -----------------------------------------------------------------------

    /// Maximum context entries (non-candidates) to include per cluster prompt.
    const MAX_CONTEXT: usize = 10;

    async fn phase_refine(
        &self,
        db: &MemoryDB,
        llm: &dyn CollationLlm,
        template: &str,
        vars: &HashMap<String, String>,
        outcome: &mut CollationOutcome,
        id_counter: &mut usize,
        indexer: Option<&dyn AgentIndexer>,
        vector_store: Option<&VectorStore>,
        limit: Option<usize>,
        candidates_processed: &mut HashSet<String>,
    ) -> Result<(), CollationError> {
        let entries = db
            .get_entries_by_status("active")
            .map_err(|e| CollationError::Db(e.to_string()))?;

        let mut candidates: Vec<&Entry> = entries
            .iter()
            .filter(|e| self.is_refine_candidate(e))
            .collect();

        if let Some(cap) = limit {
            candidates.truncate(cap);
        }

        if candidates.is_empty() {
            outcome.entries_skipped += entries.len();
            return Ok(());
        }

        for c in &candidates {
            candidates_processed.insert(c.id.clone());
        }

        let candidate_ids: HashSet<String> = candidates.iter().map(|e| e.id.clone()).collect();

        // Cluster candidates by semantic similarity.
        let clusters = clustering::cluster_candidates(&candidates, vector_store).await;

        let now_str = Utc::now().to_rfc3339();

        for cluster in &clusters {
            // Fetch context entries (nearby non-candidates).
            let context = self
                .fetch_context_entries(cluster, &entries, &candidate_ids, vector_store)
                .await;

            let prompt = prompt::build_refine_prompt(template, cluster, &context, vars);
            let actions = llm.refine(&prompt).await?;

            let mut acted_on: HashSet<String> = HashSet::new();

            for action in &actions {
                match action {
                    RefineAction::Merge {
                        source_entry_ids,
                        result,
                        reason,
                    } => {
                        if self
                            .apply_merge(
                                db,
                                source_entry_ids,
                                result,
                                reason,
                                id_counter,
                                indexer,
                                &now_str,
                                outcome,
                                candidates_processed,
                                &candidate_ids,
                            )
                            .await?
                        {
                            for id in source_entry_ids {
                                acted_on.insert(id.clone());
                            }
                        }
                    }
                    RefineAction::Split {
                        source_entry_id,
                        results,
                        reason,
                    } => {
                        if self
                            .apply_split(
                                db,
                                source_entry_id,
                                results,
                                reason,
                                id_counter,
                                indexer,
                                &now_str,
                                outcome,
                                candidates_processed,
                                &candidate_ids,
                            )
                            .await?
                        {
                            acted_on.insert(source_entry_id.clone());
                        }
                    }
                    RefineAction::Update {
                        entry_id,
                        result,
                        reason,
                    } => {
                        if self
                            .apply_update(
                                db,
                                entry_id,
                                result,
                                reason,
                                &now_str,
                                indexer,
                                outcome,
                                &candidate_ids,
                            )
                            .await?
                        {
                            acted_on.insert(entry_id.clone());
                        }
                    }
                }
            }

            // Count candidates in this cluster that had no actions.
            let kept = cluster.iter().filter(|e| !acted_on.contains(&e.id)).count();
            outcome.refine_kept += kept;
        }

        Ok(())
    }

    /// Fetch context entries: active entries near the cluster centroid that
    /// are NOT candidates. Provides the LLM with awareness of existing coverage.
    async fn fetch_context_entries(
        &self,
        cluster: &[Entry],
        all_active: &[Entry],
        candidate_ids: &HashSet<String>,
        vector_store: Option<&VectorStore>,
    ) -> Vec<Entry> {
        let vs = match vector_store {
            Some(vs) => vs,
            None => return vec![],
        };

        let ids: Vec<&str> = cluster.iter().map(|e| e.id.as_str()).collect();
        let embeddings = match vs.get_embeddings(&ids).await {
            Ok(embs) if !embs.is_empty() => embs,
            _ => return vec![],
        };

        let centroid = match clustering::compute_centroid(&embeddings) {
            Some(c) => c,
            None => return vec![],
        };

        let search_k = Self::MAX_CONTEXT + cluster.len();
        let results = match vs.search(&centroid, search_k).await {
            Ok(r) => r,
            Err(_) => return vec![],
        };

        let context_ids: HashSet<&str> = results
            .iter()
            .map(|r| r.entry_id.as_str())
            .filter(|id| !candidate_ids.contains(*id))
            .take(Self::MAX_CONTEXT)
            .collect();

        all_active
            .iter()
            .filter(|e| context_ids.contains(e.id.as_str()) && e.status == "active")
            .cloned()
            .collect()
    }

    /// Apply a merge action: combine N entries into 1. Returns true if applied.
    async fn apply_merge(
        &self,
        db: &MemoryDB,
        source_entry_ids: &[String],
        result: &RefineEntryFields,
        reason: &str,
        id_counter: &mut usize,
        indexer: Option<&dyn AgentIndexer>,
        now_str: &str,
        outcome: &mut CollationOutcome,
        candidates_processed: &mut HashSet<String>,
        candidate_ids: &HashSet<String>,
    ) -> Result<bool, CollationError> {
        if source_entry_ids.len() < 2 {
            return Ok(false);
        }

        // All sources must be active candidates.
        for id in source_entry_ids {
            if !candidate_ids.contains(id) {
                return Ok(false);
            }
            match db
                .get_entry(id)
                .map_err(|e| CollationError::Db(e.to_string()))?
            {
                Some(e) if e.status == "active" => {}
                _ => return Ok(false),
            }
        }

        let sources: Vec<Entry> = source_entry_ids
            .iter()
            .filter_map(|id| db.get_entry(id).ok().flatten())
            .collect();
        let first_source = &sources[0];

        // Compute merged timestamps: min(start), max(end).
        let start_timestamp = sources
            .iter()
            .map(|e| e.start_timestamp.as_str())
            .filter(|t| !t.is_empty())
            .min()
            .unwrap_or("")
            .to_string();
        let end_timestamp = sources
            .iter()
            .map(|e| e.end_timestamp.as_str())
            .filter(|t| !t.is_empty())
            .max()
            .unwrap_or("")
            .to_string();

        let message_count: i64 = sources.iter().map(|e| e.message_count).sum();

        let new_id = Self::generate_entry_id(*id_counter);
        *id_counter += 1;

        let confidence = result.confidence.clamp(0.0, 1.0);
        let entry = Entry {
            id: new_id.clone(),
            memory_type: first_source.memory_type.clone(),
            source: "collation_refine".to_string(),
            reason: "refine_merge".to_string(),
            status: "active".to_string(),

            confidence,
            summary_text: result.summary_text.clone(),
            topic_tags: result.topic_tags.clone(),
            topic_key: result.topic_key.clone(),
            start_timestamp,
            end_timestamp,
            message_count,
            source_entry_ids: source_entry_ids.join(","),
            related_entry_ids: String::new(),
            superseded_by: String::new(),
            created_at: now_str.to_string(),
            updated_at: now_str.to_string(),
            entry_type: first_source.entry_type.clone(),
            image_path: String::new(),
            collated_at: String::new(),
        };

        db.create_entry(&entry)
            .map_err(|e| CollationError::Db(e.to_string()))?;

        if let Some(idx) = indexer {
            let _ = idx.index_entry(&new_id, &result.summary_text).await;
        }

        candidates_processed.insert(new_id.clone());

        for source_id in source_entry_ids {
            db.supersede_entry(source_id, &new_id)
                .map_err(|e| CollationError::Db(e.to_string()))?;
        }

        // Changelog with source summaries and reason.
        let source_summaries: Vec<String> = sources
            .iter()
            .map(|s| format!("  - [{}] {}", s.id, s.summary_text))
            .collect();
        let cl_id = db
            .append_changelog(
                "collation_refine",
                &format!(
                    "Merge {} entries -> {}:\n{}\n  => {}\n  Reason: {}",
                    source_entry_ids.len(),
                    new_id,
                    source_summaries.join("\n"),
                    result.summary_text,
                    reason,
                ),
            )
            .map_err(|e| CollationError::Db(e.to_string()))?;
        db.link_changelog_entry(cl_id, &new_id)
            .map_err(|e| CollationError::Db(e.to_string()))?;

        outcome.refine_merges += 1;
        outcome.refine_new_entries += 1;

        Ok(true)
    }

    /// Apply a split action: break 1 entry into N. Returns true if applied.
    async fn apply_split(
        &self,
        db: &MemoryDB,
        source_entry_id: &str,
        results: &[RefineEntryFields],
        reason: &str,
        id_counter: &mut usize,
        indexer: Option<&dyn AgentIndexer>,
        now_str: &str,
        outcome: &mut CollationOutcome,
        candidates_processed: &mut HashSet<String>,
        candidate_ids: &HashSet<String>,
    ) -> Result<bool, CollationError> {
        if results.len() < 2 {
            return Ok(false);
        }

        if !candidate_ids.contains(source_entry_id) {
            return Ok(false);
        }

        let original = match db
            .get_entry(source_entry_id)
            .map_err(|e| CollationError::Db(e.to_string()))?
        {
            Some(e) if e.status == "active" => e,
            _ => return Ok(false),
        };

        let mut new_ids = Vec::new();
        for replacement in results {
            let new_id = Self::generate_entry_id(*id_counter);
            *id_counter += 1;

            let confidence = replacement.confidence.clamp(0.0, 1.0);
            let entry = Entry {
                id: new_id.clone(),
                memory_type: original.memory_type.clone(),
                source: "collation_refine".to_string(),
                reason: "refine_split".to_string(),
                status: "active".to_string(),

                confidence,
                summary_text: replacement.summary_text.clone(),
                topic_tags: replacement.topic_tags.clone(),
                topic_key: replacement.topic_key.clone(),
                start_timestamp: original.start_timestamp.clone(),
                end_timestamp: original.end_timestamp.clone(),
                message_count: original.message_count,
                source_entry_ids: source_entry_id.to_string(),
                related_entry_ids: String::new(),
                superseded_by: String::new(),
                created_at: now_str.to_string(),
                updated_at: now_str.to_string(),
                entry_type: original.entry_type.clone(),
                image_path: String::new(),
                collated_at: String::new(),
            };

            db.create_entry(&entry)
                .map_err(|e| CollationError::Db(e.to_string()))?;
            new_ids.push(new_id.clone());

            if let Some(idx) = indexer {
                let _ = idx.index_entry(&new_id, &replacement.summary_text).await;
            }
        }

        // Supersede original with all new IDs.
        let all_ids = new_ids.join(",");
        db.supersede_entry(source_entry_id, &all_ids)
            .map_err(|e| CollationError::Db(e.to_string()))?;

        for id in &new_ids {
            candidates_processed.insert(id.clone());
        }

        // Changelog.
        let replacement_lines: Vec<String> = results
            .iter()
            .zip(new_ids.iter())
            .map(|(r, id)| format!("  - [{}] {}", id, r.summary_text))
            .collect();
        let cl_id = db
            .append_changelog(
                "collation_refine",
                &format!(
                    "Split [{}] \"{}\" into {} parts:\n{}\n  Reason: {}",
                    source_entry_id,
                    original.summary_text,
                    new_ids.len(),
                    replacement_lines.join("\n"),
                    reason,
                ),
            )
            .map_err(|e| CollationError::Db(e.to_string()))?;
        for id in &new_ids {
            let _ = db.link_changelog_entry(cl_id, id);
        }

        outcome.refine_splits += 1;
        outcome.refine_new_entries += new_ids.len();

        Ok(true)
    }

    /// Apply an update action: rewrite entry in-place. Returns true if applied.
    async fn apply_update(
        &self,
        db: &MemoryDB,
        entry_id: &str,
        result: &RefineEntryFields,
        reason: &str,
        now_str: &str,
        indexer: Option<&dyn AgentIndexer>,
        outcome: &mut CollationOutcome,
        candidate_ids: &HashSet<String>,
    ) -> Result<bool, CollationError> {
        if !candidate_ids.contains(entry_id) {
            return Ok(false);
        }

        let mut entry = match db
            .get_entry(entry_id)
            .map_err(|e| CollationError::Db(e.to_string()))?
        {
            Some(e) if e.status == "active" => e,
            _ => return Ok(false),
        };

        let old_summary = entry.summary_text.clone();
        entry.summary_text = result.summary_text.clone();
        entry.topic_tags = result.topic_tags.clone();
        entry.topic_key = result.topic_key.clone();
        entry.confidence = result.confidence.clamp(0.0, 1.0);
        entry.updated_at = now_str.to_string();

        db.update_entry(&entry)
            .map_err(|e| CollationError::Db(e.to_string()))?;

        if let Some(idx) = indexer {
            let _ = idx.index_entry(entry_id, &result.summary_text).await;
        }

        let _ = db.append_changelog(
            "collation_refine",
            &format!(
                "Update [{}]: \"{}\" -> \"{}\"\n  Reason: {}",
                entry_id, old_summary, result.summary_text, reason,
            ),
        );

        outcome.refine_updates += 1;

        Ok(true)
    }

    // -----------------------------------------------------------------------
    // Phase 2: Confidence decay — reduce confidence on stale entries
    // -----------------------------------------------------------------------

    fn phase_confidence_decay(
        &self,
        db: &MemoryDB,
        outcome: &mut CollationOutcome,
        candidates_processed: &mut HashSet<String>,
    ) -> Result<(), CollationError> {
        let entries = db
            .get_entries_by_status("active")
            .map_err(|e| CollationError::Db(e.to_string()))?;

        let now = Utc::now();

        for entry in &entries {
            // Skip entries already at or below floor.
            if entry.confidence <= self.config.decay_floor {
                outcome.entries_skipped += 1;
                continue;
            }

            // Calculate days since last update.
            let updated_at = chrono::DateTime::parse_from_rfc3339(&entry.updated_at)
                .unwrap_or_else(|_| now.fixed_offset());
            let days_since = (now - updated_at.with_timezone(&Utc)).num_seconds() as f64 / 86400.0;

            if days_since <= 0.0 {
                outcome.entries_skipped += 1;
                continue;
            }

            // Exponential decay: confidence * 0.5^(days / half_life)
            let decay_factor = (0.5_f64).powf(days_since / self.config.decay_half_life_days);
            let new_confidence = (entry.confidence * decay_factor).max(self.config.decay_floor);

            // Only update if confidence actually changed meaningfully.
            if (entry.confidence - new_confidence).abs() < 0.001 {
                outcome.entries_skipped += 1;
                continue;
            }

            let mut updated = entry.clone();
            updated.confidence = new_confidence;
            updated.updated_at = now.to_rfc3339();

            db.update_entry(&updated)
                .map_err(|e| CollationError::Db(e.to_string()))?;

            // Track decayed entries so they get stamped (prevents updated_at
            // bump from making them spurious tidy candidates next run).
            candidates_processed.insert(entry.id.clone());

            outcome.entries_decayed += 1;
        }

        // Single summary changelog entry for all decays (instead of per-entry spam).
        if outcome.entries_decayed > 0 {
            let _ = db.append_changelog(
                "collation_decay",
                &format!(
                    "Confidence decay: {} entries decayed (half-life {:.0}d)",
                    outcome.entries_decayed, self.config.decay_half_life_days
                ),
            );
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Background collation (moved from main.rs)
// ---------------------------------------------------------------------------

/// Run the collation pipeline for a single character.
///
/// Called after compaction (auto-trigger) or could be invoked independently.
pub async fn run_collation(
    character: &str,
    config: &shore_config::LoadedConfig,
    llm_client: &shore_llm_client::LlmClient,
    data_dir: &Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use crate::commands::state::resolve_collation_model;
    use crate::memory::agent::{AgentSearchContext, RealAgentIndexer};
    use crate::memory::collation_impls::RealCollationLlm;
    use crate::memory::compaction_impls::resolve_embed_config;
    use shore_config::{
        load_character_definition, resolve_prompt_template, resolve_user_definition,
    };
    use tracing::info;

    let character_dir = data_dir.join(character);

    // Open memory DB.
    let db_path = character_dir.join("memory").join("memory.db");
    let db = MemoryDB::open(&db_path).map_err(|e| format!("Failed to open memory DB: {e}"))?;

    let model = resolve_collation_model(config).ok_or("No model configured")?;

    let llm = RealCollationLlm::new(llm_client.clone(), model);

    // Resolve prompt template.
    let refine_template = resolve_prompt_template(&config.dirs.config, character, "refine.md")
        .unwrap_or_else(|| DEFAULT_REFINE_PROMPT.to_string());

    let mgr = CollationManager::new(DecayConfig::default());
    let collation_limit = config.app.memory.collation.batch_limit;

    // Construct vector store + indexer for clustering and indexing (optional).
    let search_ctx = match resolve_embed_config(
        config.app.defaults.embedding.as_deref(),
        &config.models.embedding,
    ) {
        Ok(embed_config) => {
            let vs_path = character_dir.join("memory").join("vectorstore");
            match VectorStore::open(&vs_path, embed_config.dimensions).await {
                Ok(vs) => Some(AgentSearchContext::new(
                    vs,
                    llm_client.clone(),
                    embed_config,
                )),
                Err(e) => {
                    tracing::warn!("Vector store unavailable for auto-collation: {e}");
                    None
                }
            }
        }
        Err(_) => None,
    };
    let indexer = search_ctx.as_ref().map(RealAgentIndexer::new);

    let collation_display_name = config.app.defaults.resolve_display_name();
    let mut collation_vars = HashMap::new();
    collation_vars.insert("char".to_string(), character.to_string());
    collation_vars.insert("user".to_string(), collation_display_name);
    if let Some(cd) = load_character_definition(&config.dirs.config, character) {
        collation_vars.insert("char_description".to_string(), cd);
    }
    if let Some(ud) = resolve_user_definition(&config.dirs.config, character) {
        collation_vars.insert("user_description".to_string(), ud);
    }

    let outcome = mgr
        .run(
            &db,
            &llm,
            &refine_template,
            &collation_vars,
            indexer.as_ref().map(|i| i as &dyn AgentIndexer),
            search_ctx.as_ref().map(|ctx| &ctx.vector_store),
            Some(collation_limit),
        )
        .await?;

    info!(
        character = %character,
        refine_merges = outcome.refine_merges,
        refine_splits = outcome.refine_splits,
        refine_updates = outcome.refine_updates,
        entries_decayed = outcome.entries_decayed,
        "Auto-collation completed"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Mock LLM ---------------------------------------------------------

    struct MockCollationLlm {
        refine_response: Vec<RefineAction>,
    }

    impl MockCollationLlm {
        fn empty() -> Self {
            Self {
                refine_response: vec![],
            }
        }
    }

    impl CollationLlm for MockCollationLlm {
        fn refine(
            &self,
            _prompt: &str,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Vec<RefineAction>, CollationError>>
                    + Send
                    + '_,
            >,
        > {
            let result = Ok(self.refine_response.clone());
            Box::pin(async move { result })
        }
    }

    // -- Helpers -----------------------------------------------------------

    fn make_entry(id: &str, summary: &str, confidence: f64, updated_at: &str) -> Entry {
        Entry {
            id: id.to_string(),
            memory_type: "semantic".to_string(),
            source: "summary".to_string(),
            reason: "compaction".to_string(),
            status: "active".to_string(),

            confidence,
            summary_text: summary.to_string(),
            topic_tags: "test".to_string(),
            topic_key: "testing".to_string(),
            start_timestamp: updated_at.to_string(),
            end_timestamp: updated_at.to_string(),
            message_count: 5,
            source_entry_ids: String::new(),
            related_entry_ids: String::new(),
            superseded_by: String::new(),
            created_at: updated_at.to_string(),
            updated_at: updated_at.to_string(),
            entry_type: String::new(),
            image_path: String::new(),
            collated_at: String::new(),
        }
    }

    fn now_str() -> String {
        Utc::now().to_rfc3339()
    }

    /// Helper to run the pipeline with defaults.
    async fn run_pipeline(
        db: &MemoryDB,
        llm: &MockCollationLlm,
        mgr: &CollationManager,
        limit: Option<usize>,
    ) -> CollationOutcome {
        mgr.run(
            db,
            llm,
            DEFAULT_REFINE_PROMPT,
            &HashMap::new(),
            None,
            None,
            limit,
        )
        .await
        .unwrap()
    }

    // -- Refine: merge tests -----------------------------------------------

    #[tokio::test]
    async fn test_refine_merge_two_entries() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        db.create_entry(&make_entry("e1", "User prefers tea", 0.8, &now))
            .unwrap();
        db.create_entry(&make_entry("e2", "User drinks tea daily", 0.85, &now))
            .unwrap();
        db.create_entry(&make_entry("e3", "User works at ACME", 0.9, &now))
            .unwrap();

        let llm = MockCollationLlm {
            refine_response: vec![RefineAction::Merge {
                source_entry_ids: vec!["e1".to_string(), "e2".to_string()],
                result: RefineEntryFields {
                    summary_text: "User prefers and drinks tea daily".to_string(),
                    topic_tags: "preference,beverage".to_string(),
                    topic_key: "preferences".to_string(),
                    confidence: 0.9,
                },
                reason: "Both entries describe tea preferences".to_string(),
            }],
        };

        let mgr = CollationManager::new(DecayConfig::default());
        let outcome = run_pipeline(&db, &llm, &mgr, None).await;

        assert_eq!(outcome.refine_merges, 1);
        assert_eq!(outcome.refine_new_entries, 1);

        // Source entries should be superseded.
        let e1 = db.get_entry("e1").unwrap().unwrap();
        let e2 = db.get_entry("e2").unwrap().unwrap();
        assert_eq!(e1.status, "superseded");
        assert_eq!(e2.status, "superseded");

        // e3 should remain active.
        let e3 = db.get_entry("e3").unwrap().unwrap();
        assert_eq!(e3.status, "active");

        // New merged entry should exist.
        let active = db.get_entries_by_status("active").unwrap();
        let merged: Vec<&Entry> = active
            .iter()
            .filter(|e| e.source == "collation_refine")
            .collect();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].summary_text, "User prefers and drinks tea daily");
        assert_eq!(merged[0].source_entry_ids, "e1,e2");

        // Changelog should include reason.
        let logs = db.get_recent_changelog(10).unwrap();
        assert!(logs.iter().any(
            |l| l.operation == "collation_refine" && l.description.contains("tea preferences")
        ));
    }

    #[tokio::test]
    async fn test_refine_rejects_single_entry_merge() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        db.create_entry(&make_entry("e1", "A fact", 0.8, &now))
            .unwrap();

        let llm = MockCollationLlm {
            refine_response: vec![RefineAction::Merge {
                source_entry_ids: vec!["e1".to_string()],
                result: RefineEntryFields {
                    summary_text: "Same fact".to_string(),
                    topic_tags: "test".to_string(),
                    topic_key: "test".to_string(),
                    confidence: 0.8,
                },
                reason: "bad merge".to_string(),
            }],
        };

        let mgr = CollationManager::new(DecayConfig::default());
        let outcome = run_pipeline(&db, &llm, &mgr, None).await;

        assert_eq!(outcome.refine_merges, 0);
        let e1 = db.get_entry("e1").unwrap().unwrap();
        assert_eq!(e1.status, "active");
    }

    // -- Refine: split tests -----------------------------------------------

    #[tokio::test]
    async fn test_refine_split_broad_entry() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        db.create_entry(&make_entry(
            "entry_001",
            "User likes tea and works at ACME",
            0.9,
            &now,
        ))
        .unwrap();

        let llm = MockCollationLlm {
            refine_response: vec![RefineAction::Split {
                source_entry_id: "entry_001".to_string(),
                results: vec![
                    RefineEntryFields {
                        summary_text: "User likes tea".to_string(),
                        topic_tags: "preference,beverage".to_string(),
                        topic_key: "preferences".to_string(),
                        confidence: 0.9,
                    },
                    RefineEntryFields {
                        summary_text: "User works at ACME".to_string(),
                        topic_tags: "work,employer".to_string(),
                        topic_key: "employment".to_string(),
                        confidence: 0.85,
                    },
                ],
                reason: "Entry covers two distinct topics".to_string(),
            }],
        };

        let mgr = CollationManager::new(DecayConfig::default());
        let outcome = run_pipeline(&db, &llm, &mgr, None).await;

        assert_eq!(outcome.refine_splits, 1);
        assert_eq!(outcome.refine_new_entries, 2);

        // Original should be superseded.
        let original = db.get_entry("entry_001").unwrap().unwrap();
        assert_eq!(original.status, "superseded");

        // Two new active entries should exist.
        let active = db.get_entries_by_status("active").unwrap();
        assert_eq!(active.len(), 2);

        let summaries: Vec<&str> = active.iter().map(|e| e.summary_text.as_str()).collect();
        assert!(summaries.contains(&"User likes tea"));
        assert!(summaries.contains(&"User works at ACME"));

        for e in &active {
            assert_eq!(e.source_entry_ids, "entry_001");
            assert_eq!(e.source, "collation_refine");
        }

        let logs = db.get_recent_changelog(10).unwrap();
        assert!(logs
            .iter()
            .any(|l| l.operation == "collation_refine" && l.description.contains("Split")));
    }

    #[tokio::test]
    async fn test_refine_rejects_split_with_one_result() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        db.create_entry(&make_entry("e1", "A fact", 0.8, &now))
            .unwrap();

        let llm = MockCollationLlm {
            refine_response: vec![RefineAction::Split {
                source_entry_id: "e1".to_string(),
                results: vec![RefineEntryFields {
                    summary_text: "Same fact".to_string(),
                    topic_tags: "test".to_string(),
                    topic_key: "test".to_string(),
                    confidence: 0.8,
                }],
                reason: "pointless split".to_string(),
            }],
        };

        let mgr = CollationManager::new(DecayConfig::default());
        let outcome = run_pipeline(&db, &llm, &mgr, None).await;

        assert_eq!(outcome.refine_splits, 0);
        let e1 = db.get_entry("e1").unwrap().unwrap();
        assert_eq!(e1.status, "active");
    }

    // -- Refine: update tests -----------------------------------------------

    #[tokio::test]
    async fn test_refine_update_entry() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        db.create_entry(&make_entry("e1", "usr likes tea", 0.7, &now))
            .unwrap();

        let llm = MockCollationLlm {
            refine_response: vec![RefineAction::Update {
                entry_id: "e1".to_string(),
                result: RefineEntryFields {
                    summary_text: "User enjoys drinking tea".to_string(),
                    topic_tags: "preference,beverage".to_string(),
                    topic_key: "preferences".to_string(),
                    confidence: 0.85,
                },
                reason: "Improved clarity and specificity".to_string(),
            }],
        };

        let mgr = CollationManager::new(DecayConfig::default());
        let outcome = run_pipeline(&db, &llm, &mgr, None).await;

        assert_eq!(outcome.refine_updates, 1);
        assert_eq!(outcome.refine_new_entries, 0); // no new entries

        let e1 = db.get_entry("e1").unwrap().unwrap();
        assert_eq!(e1.status, "active"); // not superseded
        assert_eq!(e1.summary_text, "User enjoys drinking tea");
        assert_eq!(e1.topic_tags, "preference,beverage");
        assert!((e1.confidence - 0.85).abs() < 0.001);

        let logs = db.get_recent_changelog(10).unwrap();
        assert!(logs
            .iter()
            .any(|l| l.operation == "collation_refine" && l.description.contains("Update")));
    }

    #[tokio::test]
    async fn test_refine_rejects_context_entry_update() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();

        // Entry with collated_at in the future — NOT a candidate.
        let mut entry = make_entry("context_e", "Context fact", 0.9, &now);
        let future = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        entry.collated_at = future;
        db.create_entry(&entry).unwrap();

        // Also need a real candidate so the LLM is actually called.
        db.create_entry(&make_entry("candidate_e", "Real candidate", 0.8, &now))
            .unwrap();

        let llm = MockCollationLlm {
            refine_response: vec![RefineAction::Update {
                entry_id: "context_e".to_string(),
                result: RefineEntryFields {
                    summary_text: "Should not change".to_string(),
                    topic_tags: "test".to_string(),
                    topic_key: "test".to_string(),
                    confidence: 0.9,
                },
                reason: "Attempted context modification".to_string(),
            }],
        };

        let mgr = CollationManager::new(DecayConfig::default());
        let outcome = run_pipeline(&db, &llm, &mgr, None).await;

        assert_eq!(outcome.refine_updates, 0);
        let e = db.get_entry("context_e").unwrap().unwrap();
        assert_eq!(e.summary_text, "Context fact"); // unchanged
    }

    // -- Refine: keep tests -------------------------------------------------

    #[tokio::test]
    async fn test_refine_keep_unchanged() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        db.create_entry(&make_entry("e1", "Good entry", 0.9, &now))
            .unwrap();

        let llm = MockCollationLlm::empty(); // no actions
        let mgr = CollationManager::new(DecayConfig::default());
        let outcome = run_pipeline(&db, &llm, &mgr, None).await;

        assert_eq!(outcome.refine_merges, 0);
        assert_eq!(outcome.refine_splits, 0);
        assert_eq!(outcome.refine_updates, 0);
        assert_eq!(outcome.refine_kept, 1);

        let e1 = db.get_entry("e1").unwrap().unwrap();
        assert_eq!(e1.status, "active");
    }

    // -- Refine: mixed actions test ------------------------------------------

    #[tokio::test]
    async fn test_refine_mixed_actions() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        db.create_entry(&make_entry("m1", "Tea preference A", 0.8, &now))
            .unwrap();
        db.create_entry(&make_entry("m2", "Tea preference B", 0.85, &now))
            .unwrap();
        db.create_entry(&make_entry("s1", "Broad entry: tea and work", 0.9, &now))
            .unwrap();
        db.create_entry(&make_entry("u1", "usr prefs cofee", 0.7, &now))
            .unwrap();

        let llm = MockCollationLlm {
            refine_response: vec![
                RefineAction::Merge {
                    source_entry_ids: vec!["m1".to_string(), "m2".to_string()],
                    result: RefineEntryFields {
                        summary_text: "User prefers tea".to_string(),
                        topic_tags: "preference".to_string(),
                        topic_key: "preferences".to_string(),
                        confidence: 0.9,
                    },
                    reason: "Duplicate tea preferences".to_string(),
                },
                RefineAction::Split {
                    source_entry_id: "s1".to_string(),
                    results: vec![
                        RefineEntryFields {
                            summary_text: "User likes tea".to_string(),
                            topic_tags: "beverage".to_string(),
                            topic_key: "preferences".to_string(),
                            confidence: 0.9,
                        },
                        RefineEntryFields {
                            summary_text: "User works somewhere".to_string(),
                            topic_tags: "work".to_string(),
                            topic_key: "employment".to_string(),
                            confidence: 0.85,
                        },
                    ],
                    reason: "Covers two topics".to_string(),
                },
                RefineAction::Update {
                    entry_id: "u1".to_string(),
                    result: RefineEntryFields {
                        summary_text: "User prefers coffee".to_string(),
                        topic_tags: "preference,beverage".to_string(),
                        topic_key: "preferences".to_string(),
                        confidence: 0.8,
                    },
                    reason: "Fixed typos".to_string(),
                },
            ],
        };

        let mgr = CollationManager::new(DecayConfig::default());
        let outcome = run_pipeline(&db, &llm, &mgr, None).await;

        assert_eq!(outcome.refine_merges, 1);
        assert_eq!(outcome.refine_splits, 1);
        assert_eq!(outcome.refine_updates, 1);
        assert_eq!(outcome.refine_new_entries, 3); // 1 merge + 2 split

        // Merge sources superseded.
        assert_eq!(db.get_entry("m1").unwrap().unwrap().status, "superseded");
        assert_eq!(db.get_entry("m2").unwrap().unwrap().status, "superseded");

        // Split source superseded.
        assert_eq!(db.get_entry("s1").unwrap().unwrap().status, "superseded");

        // Update target still active with new text.
        let u1 = db.get_entry("u1").unwrap().unwrap();
        assert_eq!(u1.status, "active");
        assert_eq!(u1.summary_text, "User prefers coffee");
    }

    // -- Refine: confidence clamping ----------------------------------------

    #[tokio::test]
    async fn test_refine_clamps_confidence() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        db.create_entry(&make_entry("e1", "Fact A", 0.8, &now))
            .unwrap();
        db.create_entry(&make_entry("e2", "Fact B", 0.8, &now))
            .unwrap();

        let llm = MockCollationLlm {
            refine_response: vec![RefineAction::Merge {
                source_entry_ids: vec!["e1".to_string(), "e2".to_string()],
                result: RefineEntryFields {
                    summary_text: "Combined".to_string(),
                    topic_tags: "test".to_string(),
                    topic_key: "test".to_string(),
                    confidence: 1.5, // out of range
                },
                reason: "test".to_string(),
            }],
        };

        let mgr = CollationManager::new(DecayConfig::default());
        let outcome = run_pipeline(&db, &llm, &mgr, None).await;

        assert_eq!(outcome.refine_merges, 1);
        let active = db.get_entries_by_status("active").unwrap();
        let merged = active
            .iter()
            .find(|e| e.source == "collation_refine")
            .unwrap();
        assert!(
            (merged.confidence - 1.0).abs() < 0.001,
            "Should clamp to 1.0"
        );
    }

    // -- Confidence decay tests -------------------------------------------

    #[tokio::test]
    async fn test_confidence_decay_reduces_stale_entries() {
        let db = MemoryDB::open_in_memory().unwrap();
        let thirty_days_ago = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        db.create_entry(&make_entry("old_entry", "Old fact", 0.8, &thirty_days_ago))
            .unwrap();

        let llm = MockCollationLlm::empty();
        let mgr = CollationManager::new(DecayConfig::default());
        let outcome = run_pipeline(&db, &llm, &mgr, None).await;

        assert_eq!(outcome.entries_decayed, 1);
        let entry = db.get_entry("old_entry").unwrap().unwrap();
        assert!(
            entry.confidence > 0.35 && entry.confidence < 0.45,
            "Expected ~0.4, got {}",
            entry.confidence
        );
    }

    #[tokio::test]
    async fn test_confidence_decay_respects_floor() {
        let db = MemoryDB::open_in_memory().unwrap();
        let very_old = (Utc::now() - chrono::Duration::days(365)).to_rfc3339();
        db.create_entry(&make_entry("ancient", "Very old fact", 0.2, &very_old))
            .unwrap();

        let llm = MockCollationLlm::empty();
        let mgr = CollationManager::new(DecayConfig {
            decay_floor: 0.1,
            ..Default::default()
        });
        let outcome = run_pipeline(&db, &llm, &mgr, None).await;

        assert_eq!(outcome.entries_decayed, 1);
        let entry = db.get_entry("ancient").unwrap().unwrap();
        assert!(
            entry.confidence >= 0.1,
            "Confidence {} below floor",
            entry.confidence
        );
    }

    #[tokio::test]
    async fn test_confidence_decay_skips_recent_entries() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        db.create_entry(&make_entry("fresh", "Recent fact", 0.9, &now))
            .unwrap();

        let llm = MockCollationLlm::empty();
        let mgr = CollationManager::new(DecayConfig::default());
        let outcome = run_pipeline(&db, &llm, &mgr, None).await;

        assert_eq!(outcome.entries_decayed, 0);
        let entry = db.get_entry("fresh").unwrap().unwrap();
        assert!((entry.confidence - 0.9).abs() < 0.001);
    }

    #[tokio::test]
    async fn test_confidence_decay_records_changelog() {
        let db = MemoryDB::open_in_memory().unwrap();
        let old = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        db.create_entry(&make_entry("decaying", "Old fact", 0.8, &old))
            .unwrap();

        let llm = MockCollationLlm::empty();
        let mgr = CollationManager::new(DecayConfig::default());
        run_pipeline(&db, &llm, &mgr, None).await;

        let logs = db.get_recent_changelog(10).unwrap();
        assert!(logs.iter().any(|l| l.operation == "collation_decay"));
    }

    #[tokio::test]
    async fn test_decay_runs_every_time_regardless_of_collated_at() {
        let db = MemoryDB::open_in_memory().unwrap();
        let old = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let mut entry = make_entry("old_e", "Old fact", 0.8, &old);
        entry.collated_at = old.clone();
        db.create_entry(&entry).unwrap();

        let llm = MockCollationLlm::empty();
        let mgr = CollationManager::new(DecayConfig::default());
        let outcome = run_pipeline(&db, &llm, &mgr, None).await;

        assert_eq!(outcome.entries_decayed, 1);
        let fetched = db.get_entry("old_e").unwrap().unwrap();
        assert!(
            fetched.confidence > 0.35 && fetched.confidence < 0.45,
            "Expected ~0.4, got {}",
            fetched.confidence
        );
    }

    // -- Candidate selection tests -----------------------------------------

    #[test]
    fn test_new_entries_always_candidates() {
        let mgr = CollationManager::new(DecayConfig::default());
        let now = now_str();
        let entry = make_entry("e1", "New fact", 0.9, &now);
        assert!(mgr.is_refine_candidate(&entry));
    }

    #[test]
    fn test_recently_collated_not_candidates() {
        let mgr = CollationManager::new(DecayConfig::default());
        let now = now_str();
        let mut entry = make_entry("e1", "Old fact", 0.9, &now);
        entry.collated_at = now;
        assert!(!mgr.is_refine_candidate(&entry));
    }

    #[test]
    fn test_ttl_expired_are_candidates() {
        let mgr = CollationManager::new(DecayConfig {
            reconsider_ttl_days: 7.0,
            ..Default::default()
        });
        let now = now_str();
        let mut entry = make_entry("e1", "Old fact", 0.9, &now);
        let ten_days_ago = (Utc::now() - chrono::Duration::days(10)).to_rfc3339();
        entry.collated_at = ten_days_ago;
        assert!(mgr.is_refine_candidate(&entry));
    }

    #[test]
    fn test_modified_since_collation_are_candidates() {
        let mgr = CollationManager::new(DecayConfig::default());
        let old = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        let now = now_str();
        let mut entry = make_entry("e1", "Updated fact", 0.9, &now);
        entry.collated_at = old; // collated before updated_at
        assert!(mgr.is_refine_candidate(&entry));
    }

    #[test]
    fn test_image_excluded() {
        let mgr = CollationManager::new(DecayConfig::default());
        let now = now_str();

        let mut image_entry = make_entry("img1", "Photo memory", 0.9, &now);
        image_entry.image_path = "attachments/photo.jpg".to_string();
        assert!(!mgr.is_refine_candidate(&image_entry));
    }

    #[tokio::test]
    async fn test_collated_at_prevents_reprocessing() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        let mut entry = make_entry("e1", "A fact", 0.9, &now);
        let later = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        entry.collated_at = later;
        db.create_entry(&entry).unwrap();

        let llm = MockCollationLlm {
            refine_response: vec![RefineAction::Update {
                entry_id: "e1".to_string(),
                result: RefineEntryFields {
                    summary_text: "Should not happen".to_string(),
                    topic_tags: "test".to_string(),
                    topic_key: "test".to_string(),
                    confidence: 0.9,
                },
                reason: "test".to_string(),
            }],
        };

        let mgr = CollationManager::new(DecayConfig::default());
        let outcome = run_pipeline(&db, &llm, &mgr, None).await;

        assert_eq!(outcome.refine_updates, 0);
        let fetched = db.get_entry("e1").unwrap().unwrap();
        assert_eq!(fetched.summary_text, "A fact");
    }

    // -- Batch limit and stamping tests ------------------------------------

    #[tokio::test]
    async fn test_batch_limit_caps_candidates() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        for i in 0..5 {
            db.create_entry(&make_entry(
                &format!("e{i}"),
                &format!("Fact {i}"),
                0.9,
                &now,
            ))
            .unwrap();
        }

        let llm = MockCollationLlm::empty();
        let mgr = CollationManager::new(DecayConfig::default());
        let _outcome = run_pipeline(&db, &llm, &mgr, Some(2)).await;

        let active = db.get_entries_by_status("active").unwrap();
        let stamped = active.iter().filter(|e| !e.collated_at.is_empty()).count();
        assert!(stamped <= 2, "limit=2 should cap stamping, got {stamped}");
    }

    #[tokio::test]
    async fn test_stamping_only_candidates() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();

        let a = make_entry("a", "New fact", 0.9, &now);
        db.create_entry(&a).unwrap();

        let mut b = make_entry("b", "Old fact", 0.9, &now);
        b.collated_at = now.clone();
        db.create_entry(&b).unwrap();

        let llm = MockCollationLlm::empty();
        let mgr = CollationManager::new(DecayConfig::default());
        run_pipeline(&db, &llm, &mgr, None).await;

        let a_after = db.get_entry("a").unwrap().unwrap();
        let b_after = db.get_entry("b").unwrap().unwrap();

        assert!(
            !a_after.collated_at.is_empty(),
            "Candidate A should be stamped"
        );
        assert_eq!(
            b_after.collated_at, now,
            "Non-candidate B should keep original stamp"
        );
    }

    #[tokio::test]
    async fn test_second_run_processes_ttl_expired() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        let mut entry = make_entry("old", "Old fact", 0.9, &now);
        let ten_days_ago = (Utc::now() - chrono::Duration::days(10)).to_rfc3339();
        entry.collated_at = ten_days_ago.clone();
        db.create_entry(&entry).unwrap();

        let llm = MockCollationLlm::empty();
        let mgr = CollationManager::new(DecayConfig {
            reconsider_ttl_days: 7.0,
            ..Default::default()
        });
        run_pipeline(&db, &llm, &mgr, None).await;

        let after = db.get_entry("old").unwrap().unwrap();
        assert_ne!(
            after.collated_at, ten_days_ago,
            "TTL-expired entry should be re-stamped"
        );
        assert!(!after.collated_at.is_empty());
    }

    #[tokio::test]
    async fn test_refine_empty_candidates_skips_llm() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();

        // All entries have a recent collated_at — none are candidates.
        for i in 0..3 {
            let mut entry = make_entry(&format!("e{i}"), &format!("Fact {i}"), 0.9, &now);
            entry.collated_at = now.clone();
            db.create_entry(&entry).unwrap();
        }

        // LLM returns actions — but should never be called.
        let llm = MockCollationLlm {
            refine_response: vec![RefineAction::Update {
                entry_id: "e0".to_string(),
                result: RefineEntryFields {
                    summary_text: "SHOULD NOT APPEAR".to_string(),
                    topic_tags: "bad".to_string(),
                    topic_key: "bad".to_string(),
                    confidence: 0.5,
                },
                reason: "test".to_string(),
            }],
        };
        let mgr = CollationManager::new(DecayConfig::default());
        let outcome = run_pipeline(&db, &llm, &mgr, None).await;

        // 3 from refine (no candidates) + 3 from decay (recent entries not decayed).
        assert_eq!(
            outcome.entries_skipped, 6,
            "all entries should be skipped by both phases"
        );
        assert_eq!(outcome.refine_merges, 0);
        assert_eq!(outcome.refine_splits, 0);
        assert_eq!(outcome.refine_updates, 0);
        assert_eq!(outcome.refine_kept, 0);

        // Entries should be untouched.
        let e0 = db.get_entry("e0").unwrap().unwrap();
        assert_eq!(e0.summary_text, "Fact 0", "entry should not be modified");
    }

    // -- Prompt building tests ---------------------------------------------

    #[test]
    fn test_build_refine_prompt_labels() {
        let now = now_str();
        let candidates = vec![make_entry("c1", "Candidate entry", 0.9, &now)];
        let context = vec![make_entry("ctx1", "Context entry", 0.8, &now)];
        let vars = HashMap::new();

        let prompt = prompt::build_refine_prompt(
            "Candidates:\n{{candidates}}\nContext:\n{{context}}",
            &candidates,
            &context,
            &vars,
        );

        assert!(prompt.contains("[CANDIDATE] ID: c1"));
        assert!(prompt.contains("[CONTEXT] ID: ctx1"));
        assert!(prompt.contains("Candidate entry"));
        assert!(prompt.contains("Context entry"));
        assert!(!prompt.contains("{{candidates}}"));
        assert!(!prompt.contains("{{context}}"));
    }

    #[test]
    fn test_build_refine_prompt_substitutes_vars() {
        let now = now_str();
        let candidates = vec![make_entry("e1", "Test", 0.9, &now)];
        let mut vars = HashMap::new();
        vars.insert("char".into(), "Shore".into());
        vars.insert("user".into(), "Alice".into());
        let prompt = prompt::build_refine_prompt(
            "{{char}} and {{user}}:\n{{candidates}}\n{{context}}",
            &candidates,
            &[],
            &vars,
        );
        assert!(prompt.contains("Shore and Alice:"));
        assert!(!prompt.contains("{{char}}"));
        assert!(!prompt.contains("{{user}}"));
    }

    #[test]
    fn test_build_refine_prompt_includes_timestamps() {
        let ts = "2026-03-15T12:00:00Z";
        let candidates = vec![make_entry("e1", "Test", 0.9, ts)];
        let vars = HashMap::new();
        let prompt =
            prompt::build_refine_prompt("{{candidates}}\n{{context}}", &candidates, &[], &vars);
        assert!(prompt.contains("Time: 2026-03-15T12:00:00Z"));
    }

    // -- Backfill timestamp tests -------------------------------------------

    #[tokio::test]
    async fn test_backfill_from_ancestors() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = Utc::now().to_rfc3339();

        let parent = make_entry("parent1", "Parent entry", 0.9, &now);
        db.create_entry(&parent).unwrap();

        let mut child = make_entry("child1", "Child entry", 0.8, &now);
        child.start_timestamp = String::new();
        child.end_timestamp = String::new();
        child.source_entry_ids = "parent1".to_string();
        db.create_entry(&child).unwrap();

        let mgr = CollationManager::new(DecayConfig::default());
        let mut outcome = CollationOutcome::default();
        mgr.phase_backfill_timestamps(&db, 20, &mut outcome)
            .unwrap();

        assert_eq!(outcome.timestamps_backfilled, 1);
        let updated = db.get_entry("child1").unwrap().unwrap();
        assert_eq!(updated.start_timestamp, now);
        assert_eq!(updated.end_timestamp, now);
    }

    #[tokio::test]
    async fn test_backfill_falls_back_to_created_at() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = Utc::now().to_rfc3339();

        let mut entry = make_entry("orphan1", "Orphan entry", 0.8, &now);
        entry.start_timestamp = String::new();
        entry.end_timestamp = String::new();
        db.create_entry(&entry).unwrap();

        let mgr = CollationManager::new(DecayConfig::default());
        let mut outcome = CollationOutcome::default();
        mgr.phase_backfill_timestamps(&db, 20, &mut outcome)
            .unwrap();

        assert_eq!(outcome.timestamps_backfilled, 1);
        let updated = db.get_entry("orphan1").unwrap().unwrap();
        assert_eq!(updated.start_timestamp, now);
    }

    #[tokio::test]
    async fn test_backfill_respects_batch_size() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = Utc::now().to_rfc3339();

        for i in 0..5 {
            let mut entry = make_entry(&format!("e{i}"), &format!("Entry {i}"), 0.8, &now);
            entry.start_timestamp = String::new();
            entry.end_timestamp = String::new();
            db.create_entry(&entry).unwrap();
        }

        let mgr = CollationManager::new(DecayConfig::default());
        let mut outcome = CollationOutcome::default();
        mgr.phase_backfill_timestamps(&db, 3, &mut outcome).unwrap();
        assert_eq!(outcome.timestamps_backfilled, 3);
    }

    #[tokio::test]
    async fn test_backfill_walks_chain() {
        let db = MemoryDB::open_in_memory().unwrap();
        let ts = "2026-01-15T12:00:00Z".to_string();

        let grandparent = make_entry("gp1", "Grandparent", 0.9, &ts);
        db.create_entry(&grandparent).unwrap();

        let mut parent = make_entry("p1", "Parent", 0.8, &ts);
        parent.start_timestamp = String::new();
        parent.end_timestamp = String::new();
        parent.source_entry_ids = "gp1".to_string();
        db.create_entry(&parent).unwrap();

        let mut child = make_entry("c1", "Child", 0.7, &ts);
        child.start_timestamp = String::new();
        child.end_timestamp = String::new();
        child.source_entry_ids = "p1".to_string();
        db.create_entry(&child).unwrap();

        let mgr = CollationManager::new(DecayConfig::default());
        let mut outcome = CollationOutcome::default();
        mgr.phase_backfill_timestamps(&db, 20, &mut outcome)
            .unwrap();

        assert_eq!(outcome.timestamps_backfilled, 2);
        let updated_child = db.get_entry("c1").unwrap().unwrap();
        assert_eq!(updated_child.start_timestamp, ts);
    }

    // -- Partial failure tests ------------------------------------------------

    #[tokio::test]
    async fn test_partial_failure_merge_valid_split_nonexistent() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        db.create_entry(&make_entry("e1", "Tea preference A", 0.8, &now))
            .unwrap();
        db.create_entry(&make_entry("e2", "Tea preference B", 0.85, &now))
            .unwrap();
        db.create_entry(&make_entry("e3", "Works at ACME", 0.9, &now))
            .unwrap();

        let llm = MockCollationLlm {
            refine_response: vec![
                RefineAction::Merge {
                    source_entry_ids: vec!["e1".to_string(), "e2".to_string()],
                    result: RefineEntryFields {
                        summary_text: "User prefers tea".to_string(),
                        topic_tags: "preference".to_string(),
                        topic_key: "preferences".to_string(),
                        confidence: 0.9,
                    },
                    reason: "Duplicate tea entries".to_string(),
                },
                RefineAction::Split {
                    source_entry_id: "ghost".to_string(),
                    results: vec![
                        RefineEntryFields {
                            summary_text: "Part A".to_string(),
                            topic_tags: "test".to_string(),
                            topic_key: "test".to_string(),
                            confidence: 0.8,
                        },
                        RefineEntryFields {
                            summary_text: "Part B".to_string(),
                            topic_tags: "test".to_string(),
                            topic_key: "test".to_string(),
                            confidence: 0.8,
                        },
                    ],
                    reason: "Split nonexistent entry".to_string(),
                },
            ],
        };

        let mgr = CollationManager::new(DecayConfig::default());
        let outcome = run_pipeline(&db, &llm, &mgr, None).await;

        // Merge should succeed, split should silently fail.
        assert_eq!(outcome.refine_merges, 1);
        assert_eq!(outcome.refine_splits, 0);

        assert_eq!(db.get_entry("e1").unwrap().unwrap().status, "superseded");
        assert_eq!(db.get_entry("e2").unwrap().unwrap().status, "superseded");
        assert_eq!(db.get_entry("e3").unwrap().unwrap().status, "active");

        let active = db.get_entries_by_status("active").unwrap();
        let merged: Vec<&Entry> = active
            .iter()
            .filter(|e| e.source == "collation_refine")
            .collect();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].summary_text, "User prefers tea");
    }

    #[tokio::test]
    async fn test_partial_failure_split_valid_merge_non_candidate() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();

        // s1 is a candidate (no collated_at).
        db.create_entry(&make_entry("s1", "Broad: tea and work", 0.9, &now))
            .unwrap();

        // nc1 is NOT a candidate (collated_at set to future).
        let mut nc = make_entry("nc1", "Non-candidate fact", 0.8, &now);
        nc.collated_at = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        db.create_entry(&nc).unwrap();

        let llm = MockCollationLlm {
            refine_response: vec![
                // Merge tries to include nc1 which is not in candidate_ids.
                RefineAction::Merge {
                    source_entry_ids: vec!["s1".to_string(), "nc1".to_string()],
                    result: RefineEntryFields {
                        summary_text: "Merged".to_string(),
                        topic_tags: "test".to_string(),
                        topic_key: "test".to_string(),
                        confidence: 0.9,
                    },
                    reason: "Merge with non-candidate".to_string(),
                },
                // Split of s1 is valid (s1 is a candidate).
                RefineAction::Split {
                    source_entry_id: "s1".to_string(),
                    results: vec![
                        RefineEntryFields {
                            summary_text: "User likes tea".to_string(),
                            topic_tags: "beverage".to_string(),
                            topic_key: "preferences".to_string(),
                            confidence: 0.9,
                        },
                        RefineEntryFields {
                            summary_text: "User works somewhere".to_string(),
                            topic_tags: "work".to_string(),
                            topic_key: "employment".to_string(),
                            confidence: 0.85,
                        },
                    ],
                    reason: "Two distinct topics".to_string(),
                },
            ],
        };

        let mgr = CollationManager::new(DecayConfig::default());
        let outcome = run_pipeline(&db, &llm, &mgr, None).await;

        // Merge fails (nc1 not a candidate), split succeeds.
        assert_eq!(outcome.refine_merges, 0);
        assert_eq!(outcome.refine_splits, 1);
        assert_eq!(outcome.refine_new_entries, 2);

        assert_eq!(db.get_entry("s1").unwrap().unwrap().status, "superseded");
        assert_eq!(db.get_entry("nc1").unwrap().unwrap().status, "active");
    }

    #[tokio::test]
    async fn test_pipeline_empty_db() {
        let db = MemoryDB::open_in_memory().unwrap();
        let llm = MockCollationLlm::empty();
        let mgr = CollationManager::new(DecayConfig::default());
        let outcome = run_pipeline(&db, &llm, &mgr, None).await;

        assert_eq!(outcome.refine_merges, 0);
        assert_eq!(outcome.refine_splits, 0);
        assert_eq!(outcome.refine_updates, 0);
        assert_eq!(outcome.refine_new_entries, 0);
        assert_eq!(outcome.refine_kept, 0);
        assert_eq!(outcome.entries_decayed, 0);
        assert_eq!(outcome.entries_skipped, 0);
        assert_eq!(outcome.timestamps_backfilled, 0);
    }
}
