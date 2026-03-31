use crate::memory::agent::AgentIndexer;
use crate::memory::db::{Entry, MemoryDB};
use crate::memory::vectorstore::VectorStore;
use chrono::Utc;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Half-life in days for confidence decay.
const DEFAULT_DECAY_HALF_LIFE_DAYS: f64 = 30.0;

/// Floor below which confidence is not further decayed.
const DEFAULT_DECAY_FLOOR: f64 = 0.1;

/// Configuration for the collation pipeline.
#[derive(Debug, Clone)]
pub struct CollationConfig {
    /// Half-life in days for Phase 4 confidence decay.
    pub decay_half_life_days: f64,
    /// Minimum confidence floor — entries at or below this are not decayed further.
    pub decay_floor: f64,
}

impl Default for CollationConfig {
    fn default() -> Self {
        Self {
            decay_half_life_days: DEFAULT_DECAY_HALF_LIFE_DAYS,
            decay_floor: DEFAULT_DECAY_FLOOR,
        }
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum CollationError {
    Llm(String),
    Db(String),
}

impl std::fmt::Display for CollationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CollationError::Llm(e) => write!(f, "llm: {e}"),
            CollationError::Db(e) => write!(f, "db: {e}"),
        }
    }
}

impl std::error::Error for CollationError {}

// ---------------------------------------------------------------------------
// LLM response types
// ---------------------------------------------------------------------------

/// A split produced by Phase 1 (Tidy).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TidySplit {
    /// ID of the original (overly broad) entry.
    pub original_entry_id: String,
    /// New focused entries to replace it.
    pub replacements: Vec<TidyReplacement>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct TidyReplacement {
    pub summary_text: String,
    pub topic_tags: String,
    pub topic_key: String,
    pub confidence: f64,
}

/// A merge produced by Phase 2 (Collate).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CollateMerge {
    /// IDs of entries being merged together.
    pub source_entry_ids: Vec<String>,
    /// The consolidated entry.
    pub merged_summary: String,
    pub merged_topic_tags: String,
    pub merged_topic_key: String,
    pub merged_confidence: f64,
}

/// An entity normalization produced by Phase 3.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct EntityNormalization {
    /// The canonical entity name to keep.
    pub canonical_name: String,
    /// Duplicate names to merge into the canonical.
    pub duplicate_names: Vec<String>,
}

// ---------------------------------------------------------------------------
// Trait for LLM dependency
// ---------------------------------------------------------------------------

/// LLM client for collation phases. Uses cheap tool_model, falls back to primary.
pub trait CollationLlm: Send + Sync {
    /// Given entries, identify which are overly broad and split them.
    fn tidy(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<TidySplit>, CollationError>> + Send + '_>>;

    /// Given entries, identify semantically similar groups and merge them.
    fn collate(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CollateMerge>, CollationError>> + Send + '_>>;

    /// Given entities, identify duplicates and normalize names.
    fn normalize_entities(
        &self,
        prompt: &str,
    ) -> Pin<
        Box<dyn Future<Output = Result<Vec<EntityNormalization>, CollationError>> + Send + '_>,
    >;
}

// ---------------------------------------------------------------------------
// Default prompt templates
// ---------------------------------------------------------------------------

pub const DEFAULT_TIDY_PROMPT: &str = r#"You are splitting messy, multi-topic memory entries into clean, atomic entries.

User: {{user}}
Character: {{char}}

Analyze the following memory entries and identify any that are overly broad or cover multiple distinct topics. For each such entry, split it into focused, single-topic entries.

Rules:
- Every piece of information from the original entry MUST appear in exactly one output entry
- Do not fabricate or infer anything not present in the source
- Each entry should be atomic — focused on exactly one topic or theme
- If an entry is actually coherent (single topic), return it unchanged as one entry
- Preserve entity names, dates, and specific details

Topic landscape (existing topics in the memory store):
{{topic_landscape}}

Respond with a JSON object:
{"splits":[{"original_entry_id":"...","replacements":[{"summary_text":"...","topic_tags":"tag1,tag2","topic_key":"topic","confidence":0.9}]}]}

If no entries need splitting, return {"splits":[]}.

Entries:
{{entries}}"#;

pub const DEFAULT_COLLATE_PROMPT: &str = r#"You are distilling a cluster of conversation records into stable, durable knowledge about {{user}} and their relationship with {{char}}.

User: {{user}}
Character: {{char}}

Goal: Extract what is durably true — preferences, personal details, important attributes, ongoing truths — not what merely happened in a specific conversation. Write in present tense where possible. The output should read as settled facts, not narrative.

Instructions:
- The primary goal is **consolidation** — reduce the number of entries by merging overlapping or related information
- You MUST produce fewer entries than the number of source entries ({{entry_count}} sources → at most {{max_entries}} output entries)
- Merge redundant information across entries
- Extract stable facts, preferences, and attributes from episodic narratives
- Do not fabricate or infer anything not present in the source entries
- Keep specific names, dates, and details where relevant
- Each entry should be self-contained — but prefer merging related sub-topics into one entry over splitting into many

Contradiction handling:
- When entries contain conflicting information, prefer the more recent entry (by timestamp or position)
- If one entry explicitly corrects or updates another, use the correction
- If the contradiction reflects genuine change over time, preserve both with temporal framing (e.g. "Previously X, but as of [date] Y")
- If unresolvable, preserve both and note the conflict

Drop permission:
- You MAY drop information that is clearly outdated, superseded by newer facts, or no longer relevant
- When dropping information, note what was dropped in the merged_summary
- Only drop when confident the information is stale — when in doubt, preserve it

Respond with a JSON object:
{"merges":[{"source_entry_ids":["id1","id2"],"merged_summary":"...","merged_topic_tags":"tag1,tag2","merged_topic_key":"topic","merged_confidence":0.9}]}

If no entries should be merged, return {"merges":[]}.

Entries:
{{entries}}"#;

pub const DEFAULT_NORMALIZE_PROMPT: &str = r#"You are normalizing entity records in a memory system.

Below are entity records that may have inconsistencies: duplicate names (aliases), conflicting types, or missing information.

For each group of related entities, produce a single canonical record with:
- The most complete, correct name
- The best available type classification

If an entity has no issues, do not include it in the output.

Respond with a JSON object:
{"normalizations":[{"canonical_name":"...","duplicate_names":["alias1","alias2"]}]}

If no entities need normalization, return {"normalizations":[]}.

Entities:
{{entities}}"#;

// ---------------------------------------------------------------------------
// Collation outcome
// ---------------------------------------------------------------------------

/// Summary of what the collation pipeline did.
#[derive(Debug, Default)]
pub struct CollationOutcome {
    pub tidy_splits: usize,
    pub tidy_new_entries: usize,
    pub collate_merges: usize,
    pub collate_new_entries: usize,
    pub entities_normalized: usize,
    pub entries_decayed: usize,
    pub entries_skipped: usize,
    pub timestamps_backfilled: usize,
}

// ---------------------------------------------------------------------------
// CollationManager
// ---------------------------------------------------------------------------

pub struct CollationManager {
    config: CollationConfig,
}

impl CollationManager {
    pub fn new(config: CollationConfig) -> Self {
        Self { config }
    }

    /// Generate an entry ID in the standard format: YYYYMMDD_HHMMSS_N
    fn generate_entry_id(index: usize) -> String {
        let now = Utc::now();
        format!("{}_{}", now.format("%Y%m%d_%H%M%S"), index)
    }

    /// Build a prompt from a template, replacing `{{entries}}` with formatted entries
    /// and any additional template variables from `vars`.
    pub fn build_entries_prompt(
        template: &str,
        entries: &[Entry],
        vars: &HashMap<String, String>,
    ) -> String {
        let mut text = String::new();
        for e in entries {
            text.push_str(&format!(
                "- ID: {} | Type: {} | Confidence: {:.2} | Tags: {} | Summary: {}\n",
                e.id, e.memory_type, e.confidence, e.topic_tags, e.summary_text
            ));
        }
        let mut result = template.replace("{{entries}}", &text);
        for (key, value) in vars {
            let tag = format!("{{{{{key}}}}}");
            result = result.replace(&tag, value);
        }
        result
    }

    /// Build a prompt from a template, replacing `{{entities}}` with formatted entity names
    /// and any additional template variables from `vars`.
    pub fn build_entities_prompt(
        template: &str,
        entities: &[(String, String)],
        vars: &HashMap<String, String>,
    ) -> String {
        let mut text = String::new();
        for (name, etype) in entities {
            text.push_str(&format!("- Name: {} | Type: {}\n", name, etype));
        }
        let mut result = template.replace("{{entities}}", &text);
        for (key, value) in vars {
            let tag = format!("{{{{{key}}}}}");
            result = result.replace(&tag, value);
        }
        result
    }

    /// Run the full 4-phase collation pipeline.
    ///
    /// `vars` provides template variables like `{{char}}` and `{{user}}`.
    /// Phase-specific variables (entry_count, max_entries, topic_landscape)
    /// are computed internally and merged into a copy of `vars` before rendering.
    pub async fn run(
        &self,
        db: &MemoryDB,
        llm: &dyn CollationLlm,
        tidy_template: &str,
        collate_template: &str,
        normalize_template: &str,
        vars: &HashMap<String, String>,
        indexer: Option<&dyn AgentIndexer>,
        vector_store: Option<&VectorStore>,
    ) -> Result<CollationOutcome, CollationError> {
        let mut outcome = CollationOutcome::default();
        // Shared counter to avoid entry ID collisions across phases.
        let mut id_counter: usize = 0;
        // Snapshot time: entries with collated_at before this are candidates.
        // This ensures all phases within one run see the same candidate set.
        let pipeline_start = Utc::now().to_rfc3339();

        // Phase 0: Backfill missing timestamps (incremental, no LLM)
        self.phase_backfill_timestamps(db, 20, &mut outcome)?;

        // Phase 1: Collate (merge similar entries first to reduce count)
        self.phase_collate(db, llm, collate_template, vars, &mut outcome, &mut id_counter, indexer, &pipeline_start, vector_store)
            .await?;

        // Phase 2: Tidy (split overly broad entries, including merged results)
        self.phase_tidy(db, llm, tidy_template, vars, &mut outcome, &mut id_counter, indexer, &pipeline_start)
            .await?;

        // Phase 3: Normalize entities
        self.phase_normalize_entities(db, llm, normalize_template, vars, &mut outcome)
            .await?;

        // Phase 4: Confidence decay
        self.phase_confidence_decay(db, &mut outcome)?;

        // Stamp all active entries as collated at pipeline_start.
        // This marks them as processed in their current form for this run.
        let active = db
            .get_entries_by_status("active")
            .map_err(|e| CollationError::Db(e.to_string()))?;
        for e in &active {
            if e.collated_at.is_empty() || e.collated_at.as_str() < pipeline_start.as_str() {
                db.stamp_collated(&e.id, &pipeline_start)
                    .map_err(|e| CollationError::Db(e.to_string()))?;
            }
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

        for entry in &candidates {
            let (start, end, source) =
                self.resolve_timestamps_from_ancestors(db, entry)?;

            let mut updated = (*entry).clone();
            updated.start_timestamp = start.clone();
            updated.end_timestamp = end.clone();
            updated.updated_at = Utc::now().to_rfc3339();
            db.update_entry(&updated)
                .map_err(|e| CollationError::Db(e.to_string()))?;

            let log_id = db
                .append_changelog(
                    "backfill_timestamp",
                    &format!(
                        "Backfill timestamps: {} -> {} / {} (from {})",
                        entry.id, start, end, source
                    ),
                )
                .map_err(|e| CollationError::Db(e.to_string()))?;
            let _ = db.link_changelog_entry(log_id, &entry.id);

            outcome.timestamps_backfilled += 1;
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
                        if !ancestor.end_timestamp.is_empty() {
                            if max_end.is_empty()
                                || ancestor.end_timestamp.as_str() > max_end.as_str()
                            {
                                max_end = ancestor.end_timestamp.clone();
                            }
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
    // Phase 2: Tidy — split overly broad entries
    // -----------------------------------------------------------------------

    async fn phase_tidy(
        &self,
        db: &MemoryDB,
        llm: &dyn CollationLlm,
        template: &str,
        vars: &HashMap<String, String>,
        outcome: &mut CollationOutcome,
        id_counter: &mut usize,
        indexer: Option<&dyn AgentIndexer>,
        pipeline_start: &str,
    ) -> Result<(), CollationError> {
        let entries = db
            .get_entries_by_status("active")
            .map_err(|e| CollationError::Db(e.to_string()))?;

        // Filter to entries eligible for tidy:
        // - not an image entry (image_path is the primary data)
        // - not canonical (user-verified, do not modify)
        // - never collated, or collated before this pipeline run started
        let candidates: Vec<&Entry> = entries
            .iter()
            .filter(|e| {
                e.image_path.is_empty()
                    && !e.canonical
                    && (e.collated_at.is_empty() || e.collated_at.as_str() < pipeline_start)
            })
            .collect();

        if candidates.is_empty() {
            outcome.entries_skipped += entries.len();
            return Ok(());
        }

        // Build topic landscape from all active entries' topic keys.
        let topic_landscape: String = {
            let mut keys: Vec<&str> = entries
                .iter()
                .map(|e| e.topic_key.as_str())
                .filter(|k| !k.is_empty())
                .collect();
            keys.sort();
            keys.dedup();
            keys.join(", ")
        };
        let mut phase_vars = vars.clone();
        phase_vars.insert("topic_landscape".into(), topic_landscape);

        let owned: Vec<Entry> = candidates.iter().map(|e| (*e).clone()).collect();
        let prompt = Self::build_entries_prompt(template, &owned, &phase_vars);
        let splits = llm.tidy(&prompt).await?;

        let now_str = Utc::now().to_rfc3339();

        for split in &splits {
            // Verify the original entry exists and is active.
            let original = match db
                .get_entry(&split.original_entry_id)
                .map_err(|e| CollationError::Db(e.to_string()))?
            {
                Some(e) if e.status == "active" => e,
                _ => continue,
            };

            // Create replacement entries.
            let mut new_ids = Vec::new();
            for replacement in &split.replacements {
                let new_id = Self::generate_entry_id(*id_counter);
                *id_counter += 1;

                let entry = Entry {
                    id: new_id.clone(),
                    memory_type: original.memory_type.clone(),
                    source: "collation_tidy".to_string(),
                    reason: "tidy_split".to_string(),
                    status: "active".to_string(),
                    canonical: false,
                    confidence: replacement.confidence,
                    summary_text: replacement.summary_text.clone(),
                    topic_tags: replacement.topic_tags.clone(),
                    topic_key: replacement.topic_key.clone(),
                    start_timestamp: original.start_timestamp.clone(),
                    end_timestamp: original.end_timestamp.clone(),
                    message_count: original.message_count,
                    source_entry_ids: split.original_entry_id.clone(),
                    related_entry_ids: String::new(),
                    superseded_by: String::new(),
                    created_at: now_str.clone(),
                    updated_at: now_str.clone(),
                    entry_type: original.entry_type.clone(),
                    image_path: String::new(),
                    collated_at: String::new(),
                };

                db.create_entry(&entry)
                    .map_err(|e| CollationError::Db(e.to_string()))?;
                new_ids.push(new_id.clone());

                // Index new entry into vector store + BM25 if available.
                if let Some(idx) = indexer {
                    let _ = idx.index_entry(&new_id, &replacement.summary_text).await;
                }

                // Changelog for each new entry.
                let cl_id = db
                    .append_changelog(
                        "collation_tidy",
                        &format!(
                            "Tidy split: {} -> {} ({})",
                            split.original_entry_id, new_id, replacement.topic_key
                        ),
                    )
                    .map_err(|e| CollationError::Db(e.to_string()))?;
                db.link_changelog_entry(cl_id, &new_id)
                    .map_err(|e| CollationError::Db(e.to_string()))?;
            }

            // Supersede the original entry (point to all replacements).
            if !new_ids.is_empty() {
                let all_ids = new_ids.join(",");
                db.supersede_entry(&split.original_entry_id, &all_ids)
                    .map_err(|e| CollationError::Db(e.to_string()))?;
            }

            outcome.tidy_splits += 1;
            outcome.tidy_new_entries += new_ids.len();
        }

        let skipped = candidates.len() - splits.len();
        outcome.entries_skipped += skipped;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Clustering — group entries by semantic similarity before LLM calls
    // -----------------------------------------------------------------------

    /// Maximum entries per cluster sent to the LLM.
    const MAX_CLUSTER_SIZE: usize = 15;

    /// Minimum cosine similarity to consider two entries related.
    const SIMILARITY_THRESHOLD: f32 = 0.3;

    /// Group candidate entries into clusters of semantically related entries.
    /// Uses existing embeddings from the vector store for in-memory cosine
    /// similarity. Falls back to a single batch if no vector store is available
    /// or if entries lack embeddings.
    async fn cluster_candidates(
        &self,
        candidates: &[&Entry],
        vector_store: Option<&VectorStore>,
    ) -> Vec<Vec<Entry>> {
        // If few enough candidates, no need to cluster.
        if candidates.len() <= Self::MAX_CLUSTER_SIZE {
            return vec![candidates.iter().map(|e| (*e).clone()).collect()];
        }

        // Try to get embeddings from vector store.
        if let Some(vs) = vector_store {
            let ids: Vec<&str> = candidates.iter().map(|e| e.id.as_str()).collect();
            if let Ok(embeddings) = vs.get_embeddings(&ids).await {
                // Only cluster if we have embeddings for a meaningful fraction.
                let coverage = embeddings.len() as f32 / candidates.len() as f32;
                if coverage >= 0.5 {
                    return self.cluster_by_embeddings(candidates, &embeddings);
                }
            }
        }

        // Fallback: chunk into batches of MAX_CLUSTER_SIZE.
        candidates
            .chunks(Self::MAX_CLUSTER_SIZE)
            .map(|chunk| chunk.iter().map(|e| (*e).clone()).collect())
            .collect()
    }

    /// Greedy clustering using cosine similarity of pre-computed embeddings.
    fn cluster_by_embeddings(
        &self,
        candidates: &[&Entry],
        embeddings: &HashMap<String, Vec<f32>>,
    ) -> Vec<Vec<Entry>> {
        // Build similarity lists: for each entry with an embedding, find its
        // nearest neighbors among other candidates.
        let with_embeddings: Vec<(usize, &[f32])> = candidates
            .iter()
            .enumerate()
            .filter_map(|(i, e)| embeddings.get(&e.id).map(|emb| (i, emb.as_slice())))
            .collect();

        // Precompute pairwise neighbor lists (indices into `candidates`).
        let mut neighbors: HashMap<usize, Vec<(usize, f32)>> = HashMap::new();
        for &(i, emb_i) in &with_embeddings {
            let mut sims: Vec<(usize, f32)> = with_embeddings
                .iter()
                .filter(|&&(j, _)| j != i)
                .map(|&(j, emb_j)| (j, cosine_similarity(emb_i, emb_j)))
                .filter(|&(_, sim)| sim >= Self::SIMILARITY_THRESHOLD)
                .collect();
            sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            neighbors.insert(i, sims);
        }

        let mut clustered = vec![false; candidates.len()];
        let mut clusters: Vec<Vec<Entry>> = Vec::new();

        // Greedy: pick the entry with the most high-similarity neighbors,
        // form a cluster, remove those entries, repeat.
        loop {
            // Find unclustered entry with most unclustered neighbors.
            let best = neighbors
                .iter()
                .filter(|(&idx, _)| !clustered[idx])
                .map(|(&idx, nbrs)| {
                    let count = nbrs.iter().filter(|&&(j, _)| !clustered[j]).count();
                    (idx, count)
                })
                .max_by_key(|&(_, count)| count);

            let (seed, neighbor_count) = match best {
                Some((idx, count)) => (idx, count),
                None => break,
            };

            // If no neighbors left, remaining entries go into individual chunks.
            if neighbor_count == 0 {
                break;
            }

            let mut cluster = vec![seed];
            if let Some(nbrs) = neighbors.get(&seed) {
                for &(j, _) in nbrs {
                    if !clustered[j] && cluster.len() < Self::MAX_CLUSTER_SIZE {
                        cluster.push(j);
                    }
                }
            }

            for &idx in &cluster {
                clustered[idx] = true;
            }

            clusters.push(cluster.iter().map(|&i| candidates[i].clone()).collect());
        }

        // Collect unclustered entries into overflow batches.
        let unclustered: Vec<Entry> = candidates
            .iter()
            .enumerate()
            .filter(|&(i, _)| !clustered[i])
            .map(|(_, e)| (*e).clone())
            .collect();

        for chunk in unclustered.chunks(Self::MAX_CLUSTER_SIZE) {
            clusters.push(chunk.to_vec());
        }

        clusters
    }

    // -----------------------------------------------------------------------
    // Phase 1: Collate — merge semantically similar entries
    // -----------------------------------------------------------------------

    async fn phase_collate(
        &self,
        db: &MemoryDB,
        llm: &dyn CollationLlm,
        template: &str,
        vars: &HashMap<String, String>,
        outcome: &mut CollationOutcome,
        id_counter: &mut usize,
        indexer: Option<&dyn AgentIndexer>,
        pipeline_start: &str,
        vector_store: Option<&VectorStore>,
    ) -> Result<(), CollationError> {
        let entries = db
            .get_entries_by_status("active")
            .map_err(|e| CollationError::Db(e.to_string()))?;

        // Filter to entries eligible for collation:
        // - not an image entry, not canonical
        // - never collated, or collated before this pipeline run started
        let candidates: Vec<&Entry> = entries
            .iter()
            .filter(|e| {
                e.image_path.is_empty()
                    && !e.canonical
                    && (e.collated_at.is_empty() || e.collated_at.as_str() < pipeline_start)
            })
            .collect();

        if candidates.is_empty() {
            outcome.entries_skipped += entries.len();
            return Ok(());
        }

        // Cluster candidates by semantic similarity, then send each cluster
        // to the LLM as a focused prompt instead of one giant batch.
        let clusters = self.cluster_candidates(&candidates, vector_store).await;

        let mut merges = Vec::new();
        for cluster in &clusters {
            let entry_count = cluster.len();
            let max_entries = (entry_count / 2).max(1);
            let mut phase_vars = vars.clone();
            phase_vars.insert("entry_count".into(), entry_count.to_string());
            phase_vars.insert("max_entries".into(), max_entries.to_string());

            let prompt = Self::build_entries_prompt(template, cluster, &phase_vars);
            let cluster_merges = llm.collate(&prompt).await?;
            merges.extend(cluster_merges);
        }

        let now_str = Utc::now().to_rfc3339();

        for merge in &merges {
            if merge.source_entry_ids.len() < 2 {
                continue;
            }

            // Verify all source entries exist and are active.
            let all_valid = merge.source_entry_ids.iter().all(|id| {
                db.get_entry(id)
                    .map(|opt| opt.is_some_and(|e| e.status == "active"))
                    .unwrap_or(false)
            });
            if !all_valid {
                continue;
            }

            // Collect all source entries for metadata computation.
            let sources: Vec<Entry> = merge
                .source_entry_ids
                .iter()
                .filter_map(|id| db.get_entry(id).ok().flatten())
                .collect();
            let first_source = &sources[0];

            // Compute merged timestamps: min(start), max(end), skipping empty strings.
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

            // Sum message counts across sources.
            let message_count: i64 = sources.iter().map(|e| e.message_count).sum();

            let new_id = Self::generate_entry_id(*id_counter);
            *id_counter += 1;

            let entry = Entry {
                id: new_id.clone(),
                memory_type: first_source.memory_type.clone(),
                source: "collation_collate".to_string(),
                reason: "collate_merge".to_string(),
                status: "active".to_string(),
                canonical: false,
                confidence: merge.merged_confidence,
                summary_text: merge.merged_summary.clone(),
                topic_tags: merge.merged_topic_tags.clone(),
                topic_key: merge.merged_topic_key.clone(),
                start_timestamp,
                end_timestamp,
                message_count,
                source_entry_ids: merge.source_entry_ids.join(","),
                related_entry_ids: String::new(),
                superseded_by: String::new(),
                created_at: now_str.clone(),
                updated_at: now_str.clone(),
                entry_type: first_source.entry_type.clone(),
                image_path: String::new(),
                collated_at: String::new(),
            };

            db.create_entry(&entry)
                .map_err(|e| CollationError::Db(e.to_string()))?;

            // Index new entry into vector store + BM25 if available.
            if let Some(idx) = indexer {
                let _ = idx.index_entry(&new_id, &merge.merged_summary).await;
            }

            // Supersede all source entries.
            for source_id in &merge.source_entry_ids {
                db.supersede_entry(source_id, &new_id)
                    .map_err(|e| CollationError::Db(e.to_string()))?;
            }

            // Changelog.
            let cl_id = db
                .append_changelog(
                    "collation_collate",
                    &format!(
                        "Collate merge: [{}] -> {}",
                        merge.source_entry_ids.join(", "),
                        new_id
                    ),
                )
                .map_err(|e| CollationError::Db(e.to_string()))?;
            db.link_changelog_entry(cl_id, &new_id)
                .map_err(|e| CollationError::Db(e.to_string()))?;

            outcome.collate_merges += 1;
            outcome.collate_new_entries += 1;
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Phase 3: Normalize entities — deduplicate entity names
    // -----------------------------------------------------------------------

    async fn phase_normalize_entities(
        &self,
        db: &MemoryDB,
        llm: &dyn CollationLlm,
        template: &str,
        vars: &HashMap<String, String>,
        outcome: &mut CollationOutcome,
    ) -> Result<(), CollationError> {
        // Gather all entities.
        let all_entities = db
            .get_all_entities()
            .map_err(|e| CollationError::Db(e.to_string()))?;

        if all_entities.is_empty() {
            return Ok(());
        }

        let entity_pairs: Vec<(String, String)> = all_entities
            .iter()
            .map(|e| (e.name.clone(), e.entity_type.clone()))
            .collect();

        let prompt = Self::build_entities_prompt(template, &entity_pairs, vars);
        let normalizations = llm.normalize_entities(&prompt).await?;

        for norm in &normalizations {
            // Find the canonical entity.
            let canonical = match db
                .get_entity_by_name(&norm.canonical_name)
                .map_err(|e| CollationError::Db(e.to_string()))?
            {
                Some(e) => e,
                None => continue,
            };

            for dup_name in &norm.duplicate_names {
                let dup = match db
                    .get_entity_by_name(dup_name)
                    .map_err(|e| CollationError::Db(e.to_string()))?
                {
                    Some(e) => e,
                    None => continue,
                };

                if dup.entity_id == canonical.entity_id {
                    continue;
                }

                // Reassign all entry links from duplicate to canonical.
                db.reassign_entity_links(dup.entity_id, canonical.entity_id)
                    .map_err(|e| CollationError::Db(e.to_string()))?;

                // Remove the duplicate entity.
                db.delete_entity(dup.entity_id)
                    .map_err(|e| CollationError::Db(e.to_string()))?;

                // Changelog.
                let cl_id = db
                    .append_changelog(
                        "collation_normalize",
                        &format!(
                            "Normalize entity: '{}' merged into '{}'",
                            dup_name, norm.canonical_name
                        ),
                    )
                    .map_err(|e| CollationError::Db(e.to_string()))?;
                db.link_changelog_entity(cl_id, canonical.entity_id)
                    .map_err(|e| CollationError::Db(e.to_string()))?;

                outcome.entities_normalized += 1;
            }
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Phase 4: Confidence decay — reduce confidence on stale entries
    // -----------------------------------------------------------------------

    fn phase_confidence_decay(
        &self,
        db: &MemoryDB,
        outcome: &mut CollationOutcome,
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
            let days_since = (now - updated_at.with_timezone(&Utc))
                .num_seconds() as f64
                / 86400.0;

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

            // Changelog.
            let cl_id = db
                .append_changelog(
                    "collation_decay",
                    &format!(
                        "Confidence decay: {} {:.3} -> {:.3} ({:.0} days stale)",
                        entry.id, entry.confidence, new_confidence, days_since
                    ),
                )
                .map_err(|e| CollationError::Db(e.to_string()))?;
            db.link_changelog_entry(cl_id, &entry.id)
                .map_err(|e| CollationError::Db(e.to_string()))?;

            outcome.entries_decayed += 1;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Cosine similarity between two vectors. Returns 0.0 for zero-length vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Mock LLM ---------------------------------------------------------

    struct MockCollationLlm {
        tidy_response: Vec<TidySplit>,
        collate_response: Vec<CollateMerge>,
        normalize_response: Vec<EntityNormalization>,
    }

    impl MockCollationLlm {
        fn empty() -> Self {
            Self {
                tidy_response: vec![],
                collate_response: vec![],
                normalize_response: vec![],
            }
        }
    }

    impl CollationLlm for MockCollationLlm {
        fn tidy(
            &self,
            _prompt: &str,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<TidySplit>, CollationError>> + Send + '_>>
        {
            let result = Ok(self.tidy_response.clone());
            Box::pin(async move { result })
        }

        fn collate(
            &self,
            _prompt: &str,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<CollateMerge>, CollationError>> + Send + '_>>
        {
            let result = Ok(self.collate_response.clone());
            Box::pin(async move { result })
        }

        fn normalize_entities(
            &self,
            _prompt: &str,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Vec<EntityNormalization>, CollationError>> + Send + '_,
            >,
        > {
            let result = Ok(self.normalize_response.clone());
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
            canonical: false,
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

    // -- Phase 1: Tidy tests ----------------------------------------------

    #[tokio::test]
    async fn test_tidy_splits_broad_entry() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        let entry = make_entry("entry_001", "User likes tea and works at ACME", 0.9, &now);
        db.create_entry(&entry).unwrap();

        let llm = MockCollationLlm {
            tidy_response: vec![TidySplit {
                original_entry_id: "entry_001".to_string(),
                replacements: vec![
                    TidyReplacement {
                        summary_text: "User likes tea".to_string(),
                        topic_tags: "preference,beverage".to_string(),
                        topic_key: "preferences".to_string(),
                        confidence: 0.9,
                    },
                    TidyReplacement {
                        summary_text: "User works at ACME".to_string(),
                        topic_tags: "work,employer".to_string(),
                        topic_key: "employment".to_string(),
                        confidence: 0.85,
                    },
                ],
            }],
            ..MockCollationLlm::empty()
        };

        let mgr = CollationManager::new(CollationConfig::default());
        let outcome = mgr
            .run(
                &db,
                &llm,
                DEFAULT_TIDY_PROMPT,
                DEFAULT_COLLATE_PROMPT,
                DEFAULT_NORMALIZE_PROMPT,
                &HashMap::new(),
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(outcome.tidy_splits, 1);
        assert_eq!(outcome.tidy_new_entries, 2);

        // Original should be superseded.
        let original = db.get_entry("entry_001").unwrap().unwrap();
        assert_eq!(original.status, "superseded");

        // Two new active entries should exist.
        let active = db.get_entries_by_status("active").unwrap();
        assert_eq!(active.len(), 2);

        let summaries: Vec<&str> = active.iter().map(|e| e.summary_text.as_str()).collect();
        assert!(summaries.contains(&"User likes tea"));
        assert!(summaries.contains(&"User works at ACME"));

        // New entries should reference the original.
        for e in &active {
            assert_eq!(e.source_entry_ids, "entry_001");
            assert_eq!(e.source, "collation_tidy");
        }

        // Changelog should record the operations.
        let logs = db.get_recent_changelog(10).unwrap();
        assert!(logs.iter().any(|l| l.operation == "collation_tidy"));
    }

    #[tokio::test]
    async fn test_tidy_no_splits_needed() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        db.create_entry(&make_entry("entry_001", "Focused entry", 0.9, &now))
            .unwrap();

        let llm = MockCollationLlm::empty();
        let mgr = CollationManager::new(CollationConfig::default());
        let outcome = mgr
            .run(
                &db,
                &llm,
                DEFAULT_TIDY_PROMPT,
                DEFAULT_COLLATE_PROMPT,
                DEFAULT_NORMALIZE_PROMPT,
                &HashMap::new(),
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(outcome.tidy_splits, 0);
        assert_eq!(outcome.tidy_new_entries, 0);

        // Entry should remain active.
        let entry = db.get_entry("entry_001").unwrap().unwrap();
        assert_eq!(entry.status, "active");
    }

    // -- Phase 2: Collate tests -------------------------------------------

    #[tokio::test]
    async fn test_collate_merges_similar_entries() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        db.create_entry(&make_entry("e1", "User prefers tea", 0.8, &now))
            .unwrap();
        db.create_entry(&make_entry("e2", "User drinks tea daily", 0.85, &now))
            .unwrap();
        db.create_entry(&make_entry("e3", "User works at ACME", 0.9, &now))
            .unwrap();

        let llm = MockCollationLlm {
            collate_response: vec![CollateMerge {
                source_entry_ids: vec!["e1".to_string(), "e2".to_string()],
                merged_summary: "User prefers and drinks tea daily".to_string(),
                merged_topic_tags: "preference,beverage".to_string(),
                merged_topic_key: "preferences".to_string(),
                merged_confidence: 0.9,
            }],
            ..MockCollationLlm::empty()
        };

        let mgr = CollationManager::new(CollationConfig::default());
        let outcome = mgr
            .run(
                &db,
                &llm,
                DEFAULT_TIDY_PROMPT,
                DEFAULT_COLLATE_PROMPT,
                DEFAULT_NORMALIZE_PROMPT,
                &HashMap::new(),
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(outcome.collate_merges, 1);
        assert_eq!(outcome.collate_new_entries, 1);

        // Source entries should be superseded.
        let e1 = db.get_entry("e1").unwrap().unwrap();
        let e2 = db.get_entry("e2").unwrap().unwrap();
        assert_eq!(e1.status, "superseded");
        assert_eq!(e2.status, "superseded");

        // e3 should remain active (not merged).
        let e3 = db.get_entry("e3").unwrap().unwrap();
        assert_eq!(e3.status, "active");

        // New merged entry should exist.
        let active = db.get_entries_by_status("active").unwrap();
        let merged: Vec<&Entry> = active
            .iter()
            .filter(|e| e.source == "collation_collate")
            .collect();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].summary_text, "User prefers and drinks tea daily");
        assert_eq!(merged[0].source_entry_ids, "e1,e2");

        // Changelog.
        let logs = db.get_recent_changelog(10).unwrap();
        assert!(logs.iter().any(|l| l.operation == "collation_collate"));
    }

    #[tokio::test]
    async fn test_collate_skips_single_entry_merge() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        db.create_entry(&make_entry("e1", "A fact", 0.8, &now))
            .unwrap();

        // LLM incorrectly suggests merging a single entry — should be ignored.
        let llm = MockCollationLlm {
            collate_response: vec![CollateMerge {
                source_entry_ids: vec!["e1".to_string()],
                merged_summary: "Same fact".to_string(),
                merged_topic_tags: "test".to_string(),
                merged_topic_key: "test".to_string(),
                merged_confidence: 0.8,
            }],
            ..MockCollationLlm::empty()
        };

        let mgr = CollationManager::new(CollationConfig::default());
        let outcome = mgr
            .run(
                &db,
                &llm,
                DEFAULT_TIDY_PROMPT,
                DEFAULT_COLLATE_PROMPT,
                DEFAULT_NORMALIZE_PROMPT,
                &HashMap::new(),
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(outcome.collate_merges, 0);

        // e1 should remain active.
        let e1 = db.get_entry("e1").unwrap().unwrap();
        assert_eq!(e1.status, "active");
    }

    // -- Phase 3: Normalize entities tests --------------------------------

    #[tokio::test]
    async fn test_normalize_entities_deduplicates() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();

        // Create an entry to link entities to.
        db.create_entry(&make_entry("e1", "Met Bob", 0.9, &now))
            .unwrap();

        let id1 = db.upsert_entity("Bob Smith", "person", "A colleague").unwrap();
        let id2 = db.upsert_entity("Robert Smith", "person", "Bob").unwrap();
        let _id3 = db.upsert_entity("Alice", "person", "A friend").unwrap();

        // Link both Bob variants to the entry.
        db.link_entity_to_entry("e1", id1).unwrap();
        db.link_entity_to_entry("e1", id2).unwrap();

        let llm = MockCollationLlm {
            normalize_response: vec![EntityNormalization {
                canonical_name: "Bob Smith".to_string(),
                duplicate_names: vec!["Robert Smith".to_string()],
            }],
            ..MockCollationLlm::empty()
        };

        let mgr = CollationManager::new(CollationConfig::default());
        let outcome = mgr
            .run(
                &db,
                &llm,
                DEFAULT_TIDY_PROMPT,
                DEFAULT_COLLATE_PROMPT,
                DEFAULT_NORMALIZE_PROMPT,
                &HashMap::new(),
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(outcome.entities_normalized, 1);

        // "Robert Smith" should no longer exist.
        assert!(db.get_entity_by_name("Robert Smith").unwrap().is_none());

        // "Bob Smith" and "Alice" should still exist.
        assert!(db.get_entity_by_name("Bob Smith").unwrap().is_some());
        assert!(db.get_entity_by_name("Alice").unwrap().is_some());

        // Changelog.
        let logs = db.get_recent_changelog(10).unwrap();
        assert!(logs.iter().any(|l| l.operation == "collation_normalize"));
    }

    // -- Phase 4: Confidence decay tests ----------------------------------

    #[tokio::test]
    async fn test_confidence_decay_reduces_stale_entries() {
        let db = MemoryDB::open_in_memory().unwrap();

        // Create an entry last updated 30 days ago (one half-life).
        let thirty_days_ago = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        db.create_entry(&make_entry("old_entry", "Old fact", 0.8, &thirty_days_ago))
            .unwrap();

        let llm = MockCollationLlm::empty();
        let mgr = CollationManager::new(CollationConfig::default());
        let outcome = mgr
            .run(
                &db,
                &llm,
                DEFAULT_TIDY_PROMPT,
                DEFAULT_COLLATE_PROMPT,
                DEFAULT_NORMALIZE_PROMPT,
                &HashMap::new(),
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(outcome.entries_decayed, 1);

        let entry = db.get_entry("old_entry").unwrap().unwrap();
        // After one half-life, confidence should be ~0.4 (0.8 * 0.5).
        assert!(entry.confidence > 0.35 && entry.confidence < 0.45,
            "Expected ~0.4, got {}", entry.confidence);
    }

    #[tokio::test]
    async fn test_confidence_decay_respects_floor() {
        let db = MemoryDB::open_in_memory().unwrap();

        // Entry with very old updated_at and low confidence.
        let very_old = (Utc::now() - chrono::Duration::days(365)).to_rfc3339();
        db.create_entry(&make_entry("ancient", "Very old fact", 0.2, &very_old))
            .unwrap();

        let llm = MockCollationLlm::empty();
        let mgr = CollationManager::new(CollationConfig {
            decay_floor: 0.1,
            ..Default::default()
        });
        let outcome = mgr
            .run(
                &db,
                &llm,
                DEFAULT_TIDY_PROMPT,
                DEFAULT_COLLATE_PROMPT,
                DEFAULT_NORMALIZE_PROMPT,
                &HashMap::new(),
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(outcome.entries_decayed, 1);

        let entry = db.get_entry("ancient").unwrap().unwrap();
        // Should not go below floor.
        assert!(
            entry.confidence >= 0.1,
            "Confidence {} is below floor",
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
        let mgr = CollationManager::new(CollationConfig::default());
        let outcome = mgr
            .run(
                &db,
                &llm,
                DEFAULT_TIDY_PROMPT,
                DEFAULT_COLLATE_PROMPT,
                DEFAULT_NORMALIZE_PROMPT,
                &HashMap::new(),
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(outcome.entries_decayed, 0);

        // Confidence should be unchanged.
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
        let mgr = CollationManager::new(CollationConfig::default());
        mgr.run(
            &db,
            &llm,
            DEFAULT_TIDY_PROMPT,
            DEFAULT_COLLATE_PROMPT,
            DEFAULT_NORMALIZE_PROMPT,
            &HashMap::new(),
            None,
            None,
        )
        .await
        .unwrap();

        let logs = db.get_recent_changelog(10).unwrap();
        assert!(logs.iter().any(|l| l.operation == "collation_decay"));
    }

    // -- Skip optimization tests ------------------------------------------

    #[tokio::test]
    async fn test_collated_at_prevents_reprocessing_tidy() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        let mut entry = make_entry("e1", "A fact", 0.9, &now);
        // Mark as already collated with a future timestamp so it's always >= pipeline_start.
        let future = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        entry.collated_at = future;
        db.create_entry(&entry).unwrap();

        let llm = MockCollationLlm {
            tidy_response: vec![TidySplit {
                original_entry_id: "e1".to_string(),
                replacements: vec![TidyReplacement {
                    summary_text: "Should not happen".to_string(),
                    topic_tags: "test".to_string(),
                    topic_key: "test".to_string(),
                    confidence: 0.9,
                }],
            }],
            ..MockCollationLlm::empty()
        };

        let mgr = CollationManager::new(CollationConfig::default());
        let outcome = mgr
            .run(
                &db,
                &llm,
                DEFAULT_TIDY_PROMPT,
                DEFAULT_COLLATE_PROMPT,
                DEFAULT_NORMALIZE_PROMPT,
                &HashMap::new(),
                None,
                None,
            )
            .await
            .unwrap();

        // Tidy should have skipped e1 since it was already collated.
        assert_eq!(outcome.tidy_splits, 0);
        assert_eq!(outcome.tidy_new_entries, 0);

        // e1 should still be active and unchanged.
        let fetched = db.get_entry("e1").unwrap().unwrap();
        assert_eq!(fetched.status, "active");
        assert_eq!(fetched.summary_text, "A fact");
    }

    #[tokio::test]
    async fn test_decay_runs_every_time_regardless_of_collated_at() {
        let db = MemoryDB::open_in_memory().unwrap();
        let old = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let mut entry = make_entry("old_e", "Old fact", 0.8, &old);
        // Even with collated_at set, decay should still run.
        entry.collated_at = old.clone();
        db.create_entry(&entry).unwrap();

        let llm = MockCollationLlm::empty();
        let mgr = CollationManager::new(CollationConfig::default());
        let outcome = mgr
            .run(
                &db,
                &llm,
                DEFAULT_TIDY_PROMPT,
                DEFAULT_COLLATE_PROMPT,
                DEFAULT_NORMALIZE_PROMPT,
                &HashMap::new(),
                None,
                None,
            )
            .await
            .unwrap();

        // Decay should run on all entries regardless of collated_at.
        assert_eq!(outcome.entries_decayed, 1);

        let fetched = db.get_entry("old_e").unwrap().unwrap();
        // After one half-life, confidence should be ~0.4 (0.8 * 0.5).
        assert!(fetched.confidence > 0.35 && fetched.confidence < 0.45,
            "Expected ~0.4, got {}", fetched.confidence);
    }

    // -- Full pipeline test -----------------------------------------------

    #[tokio::test]
    async fn test_full_pipeline_runs_all_phases() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = now_str();
        let old = (Utc::now() - chrono::Duration::days(60)).to_rfc3339();

        // Entry for tidy.
        db.create_entry(&make_entry("broad", "User likes tea and works at ACME", 0.9, &now))
            .unwrap();

        // Entries for collate (will become active after tidy creates new ones).
        db.create_entry(&make_entry("sim1", "User enjoys green tea", 0.8, &now))
            .unwrap();
        db.create_entry(&make_entry("sim2", "User drinks green tea often", 0.85, &now))
            .unwrap();

        // Entry for decay.
        db.create_entry(&make_entry("stale", "Old preference", 0.7, &old))
            .unwrap();

        // Entities for normalization.
        db.upsert_entity("Bob", "person", "Friend").unwrap();
        db.upsert_entity("Bobby", "person", "Also Bob").unwrap();

        let llm = MockCollationLlm {
            tidy_response: vec![TidySplit {
                original_entry_id: "broad".to_string(),
                replacements: vec![
                    TidyReplacement {
                        summary_text: "User likes tea".to_string(),
                        topic_tags: "preference".to_string(),
                        topic_key: "preferences".to_string(),
                        confidence: 0.9,
                    },
                    TidyReplacement {
                        summary_text: "User works at ACME".to_string(),
                        topic_tags: "work".to_string(),
                        topic_key: "employment".to_string(),
                        confidence: 0.85,
                    },
                ],
            }],
            collate_response: vec![CollateMerge {
                source_entry_ids: vec!["sim1".to_string(), "sim2".to_string()],
                merged_summary: "User regularly enjoys green tea".to_string(),
                merged_topic_tags: "preference,beverage".to_string(),
                merged_topic_key: "preferences".to_string(),
                merged_confidence: 0.9,
            }],
            normalize_response: vec![EntityNormalization {
                canonical_name: "Bob".to_string(),
                duplicate_names: vec!["Bobby".to_string()],
            }],
        };

        let mgr = CollationManager::new(CollationConfig::default());
        let outcome = mgr
            .run(
                &db,
                &llm,
                DEFAULT_TIDY_PROMPT,
                DEFAULT_COLLATE_PROMPT,
                DEFAULT_NORMALIZE_PROMPT,
                &HashMap::new(),
                None,
                None,
            )
            .await
            .unwrap();

        // Phase 1: 1 merge producing 1 entry.
        assert_eq!(outcome.collate_merges, 1);
        assert_eq!(outcome.collate_new_entries, 1);

        // Phase 2: 1 split producing 2 entries.
        assert_eq!(outcome.tidy_splits, 1);
        assert_eq!(outcome.tidy_new_entries, 2);

        // Phase 3: 1 entity normalized.
        assert_eq!(outcome.entities_normalized, 1);

        // Phase 4: at least the stale entry should have been decayed.
        assert!(outcome.entries_decayed >= 1, "Expected at least 1 decay, got {}", outcome.entries_decayed);
    }

    // -- Prompt building tests --------------------------------------------

    #[test]
    fn test_build_entries_prompt() {
        let now = now_str();
        let entries = vec![make_entry("e1", "Test summary", 0.9, &now)];
        let vars = HashMap::new();
        let prompt =
            CollationManager::build_entries_prompt("Template:\n{{entries}}", &entries, &vars);
        assert!(prompt.contains("ID: e1"));
        assert!(prompt.contains("Test summary"));
        assert!(!prompt.contains("{{entries}}"));
    }

    #[test]
    fn test_build_entities_prompt() {
        let entities = vec![
            ("Alice".to_string(), "person".to_string()),
            ("ACME Corp".to_string(), "organization".to_string()),
        ];
        let vars = HashMap::new();
        let prompt =
            CollationManager::build_entities_prompt("Entities:\n{{entities}}", &entities, &vars);
        assert!(prompt.contains("Alice"));
        assert!(prompt.contains("ACME Corp"));
        assert!(!prompt.contains("{{entities}}"));
    }

    #[test]
    fn test_build_entries_prompt_substitutes_vars() {
        let now = now_str();
        let entries = vec![make_entry("e1", "Test", 0.9, &now)];
        let mut vars = HashMap::new();
        vars.insert("char".into(), "Shore".into());
        vars.insert("user".into(), "Alice".into());
        let prompt = CollationManager::build_entries_prompt(
            "{{char}} and {{user}}:\n{{entries}}",
            &entries,
            &vars,
        );
        assert!(prompt.contains("Shore and Alice:"));
        assert!(!prompt.contains("{{char}}"));
        assert!(!prompt.contains("{{user}}"));
    }

    // -- Backfill timestamp tests -------------------------------------------

    #[tokio::test]
    async fn test_backfill_from_ancestors() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = Utc::now().to_rfc3339();

        // Parent with timestamps
        let parent = make_entry("parent1", "Parent entry", 0.9, &now);
        db.create_entry(&parent).unwrap();

        // Child with no timestamps, pointing to parent
        let mut child = make_entry("child1", "Child entry", 0.8, &now);
        child.start_timestamp = String::new();
        child.end_timestamp = String::new();
        child.source_entry_ids = "parent1".to_string();
        db.create_entry(&child).unwrap();

        let mgr = CollationManager::new(CollationConfig::default());
        let mut outcome = CollationOutcome::default();
        mgr.phase_backfill_timestamps(&db, 20, &mut outcome).unwrap();

        assert_eq!(outcome.timestamps_backfilled, 1);

        let updated = db.get_entry("child1").unwrap().unwrap();
        assert_eq!(updated.start_timestamp, now);
        assert_eq!(updated.end_timestamp, now);
    }

    #[tokio::test]
    async fn test_backfill_falls_back_to_created_at() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = Utc::now().to_rfc3339();

        // Entry with no timestamps and no source entries (V1 import)
        let mut entry = make_entry("orphan1", "Orphan entry", 0.8, &now);
        entry.start_timestamp = String::new();
        entry.end_timestamp = String::new();
        db.create_entry(&entry).unwrap();

        let mgr = CollationManager::new(CollationConfig::default());
        let mut outcome = CollationOutcome::default();
        mgr.phase_backfill_timestamps(&db, 20, &mut outcome).unwrap();

        assert_eq!(outcome.timestamps_backfilled, 1);

        let updated = db.get_entry("orphan1").unwrap().unwrap();
        assert_eq!(updated.start_timestamp, now);
        assert_eq!(updated.end_timestamp, now);
    }

    #[tokio::test]
    async fn test_backfill_respects_batch_size() {
        let db = MemoryDB::open_in_memory().unwrap();
        let now = Utc::now().to_rfc3339();

        // Create 5 entries with no timestamps
        for i in 0..5 {
            let mut entry = make_entry(&format!("e{i}"), &format!("Entry {i}"), 0.8, &now);
            entry.start_timestamp = String::new();
            entry.end_timestamp = String::new();
            db.create_entry(&entry).unwrap();
        }

        let mgr = CollationManager::new(CollationConfig::default());
        let mut outcome = CollationOutcome::default();
        // Batch size of 3: should only process 3 of 5
        mgr.phase_backfill_timestamps(&db, 3, &mut outcome).unwrap();

        assert_eq!(outcome.timestamps_backfilled, 3);
    }

    #[tokio::test]
    async fn test_backfill_walks_chain() {
        let db = MemoryDB::open_in_memory().unwrap();
        let ts = "2026-01-15T12:00:00Z".to_string();

        // Grandparent with timestamps
        let grandparent = make_entry("gp1", "Grandparent", 0.9, &ts);
        db.create_entry(&grandparent).unwrap();

        // Parent with no timestamps, pointing to grandparent
        let mut parent = make_entry("p1", "Parent", 0.8, &ts);
        parent.start_timestamp = String::new();
        parent.end_timestamp = String::new();
        parent.source_entry_ids = "gp1".to_string();
        db.create_entry(&parent).unwrap();

        // Child with no timestamps, pointing to parent (which also has none)
        let mut child = make_entry("c1", "Child", 0.7, &ts);
        child.start_timestamp = String::new();
        child.end_timestamp = String::new();
        child.source_entry_ids = "p1".to_string();
        db.create_entry(&child).unwrap();

        let mgr = CollationManager::new(CollationConfig::default());
        let mut outcome = CollationOutcome::default();
        mgr.phase_backfill_timestamps(&db, 20, &mut outcome).unwrap();

        // Both parent and child should be backfilled
        assert_eq!(outcome.timestamps_backfilled, 2);

        // Child should have inherited grandparent's timestamps via chain walk
        let updated_child = db.get_entry("c1").unwrap().unwrap();
        assert_eq!(updated_child.start_timestamp, ts);
        assert_eq!(updated_child.end_timestamp, ts);
    }

    // -- Clustering tests ---------------------------------------------------

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);

        let c = vec![0.0, 1.0, 0.0];
        assert!(cosine_similarity(&a, &c).abs() < 1e-6);

        let d = vec![-1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &d) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cluster_by_embeddings_groups_similar() {
        let now = now_str();
        // Create entries: 3 "food" entries and 3 "tech" entries with distinct embeddings.
        let entries: Vec<Entry> = (0..6)
            .map(|i| make_entry(&format!("e{i}"), &format!("Entry {i}"), 0.8, &now))
            .collect();
        let entry_refs: Vec<&Entry> = entries.iter().collect();

        let mut embeddings = HashMap::new();
        // Food cluster: similar embeddings
        embeddings.insert("e0".to_string(), vec![0.9, 0.1, 0.0, 0.0]);
        embeddings.insert("e1".to_string(), vec![0.8, 0.2, 0.0, 0.0]);
        embeddings.insert("e2".to_string(), vec![0.85, 0.15, 0.0, 0.0]);
        // Tech cluster: similar embeddings, orthogonal to food
        embeddings.insert("e3".to_string(), vec![0.0, 0.0, 0.9, 0.1]);
        embeddings.insert("e4".to_string(), vec![0.0, 0.0, 0.8, 0.2]);
        embeddings.insert("e5".to_string(), vec![0.0, 0.0, 0.85, 0.15]);

        let mgr = CollationManager::new(CollationConfig::default());
        let clusters = mgr.cluster_by_embeddings(&entry_refs, &embeddings);

        // Should produce 2 clusters.
        assert_eq!(clusters.len(), 2, "Expected 2 clusters, got {}", clusters.len());

        // Each cluster should have 3 entries.
        let mut sizes: Vec<usize> = clusters.iter().map(|c| c.len()).collect();
        sizes.sort();
        assert_eq!(sizes, vec![3, 3]);

        // Entries in same cluster should be from the same group.
        for cluster in &clusters {
            let ids: Vec<&str> = cluster.iter().map(|e| e.id.as_str()).collect();
            let all_food = ids.iter().all(|id| ["e0", "e1", "e2"].contains(id));
            let all_tech = ids.iter().all(|id| ["e3", "e4", "e5"].contains(id));
            assert!(
                all_food || all_tech,
                "Cluster should be homogeneous, got: {:?}",
                ids
            );
        }
    }

    #[tokio::test]
    async fn test_cluster_candidates_small_set_no_clustering() {
        let now = now_str();
        let entries: Vec<Entry> = (0..5)
            .map(|i| make_entry(&format!("e{i}"), &format!("Entry {i}"), 0.8, &now))
            .collect();
        let entry_refs: Vec<&Entry> = entries.iter().collect();

        let mgr = CollationManager::new(CollationConfig::default());
        // Small set (< MAX_CLUSTER_SIZE) should return a single cluster.
        let clusters = mgr.cluster_candidates(&entry_refs, None).await;
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].len(), 5);
    }

    #[tokio::test]
    async fn test_cluster_candidates_no_vectorstore_chunks() {
        let now = now_str();
        // Create more entries than MAX_CLUSTER_SIZE.
        let entries: Vec<Entry> = (0..30)
            .map(|i| make_entry(&format!("e{i}"), &format!("Entry {i}"), 0.8, &now))
            .collect();
        let entry_refs: Vec<&Entry> = entries.iter().collect();

        let mgr = CollationManager::new(CollationConfig::default());
        // No vector store -> falls back to chunking.
        let clusters = mgr.cluster_candidates(&entry_refs, None).await;
        assert_eq!(clusters.len(), 2); // 30 / 15 = 2 chunks
        assert_eq!(clusters[0].len(), 15);
        assert_eq!(clusters[1].len(), 15);
    }
}
