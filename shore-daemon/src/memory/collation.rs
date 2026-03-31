use crate::memory::agent::AgentIndexer;
use crate::memory::db::{Entry, MemoryDB};
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
    /// Phase 1: Given entries, identify which are overly broad and split them.
    fn tidy(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<TidySplit>, CollationError>> + Send + '_>>;

    /// Phase 2: Given entries, identify semantically similar groups and merge them.
    fn collate(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CollateMerge>, CollationError>> + Send + '_>>;

    /// Phase 3: Given entities, identify duplicates and normalize names.
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
    ) -> Result<CollationOutcome, CollationError> {
        let mut outcome = CollationOutcome::default();
        // Shared counter to avoid entry ID collisions across phases.
        let mut id_counter: usize = 0;
        // Snapshot time: entries with collated_at before this are candidates.
        // This ensures all phases within one run see the same candidate set.
        let pipeline_start = Utc::now().to_rfc3339();

        // Phase 1: Tidy
        self.phase_tidy(db, llm, tidy_template, vars, &mut outcome, &mut id_counter, indexer, &pipeline_start)
            .await?;

        // Phase 2: Collate
        self.phase_collate(db, llm, collate_template, vars, &mut outcome, &mut id_counter, indexer, &pipeline_start)
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
    // Phase 1: Tidy — split overly broad entries
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
    // Phase 2: Collate — merge semantically similar entries
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

        // Add entry count caps as template variables.
        let entry_count = candidates.len();
        let max_entries = (entry_count / 2).max(1);
        let mut phase_vars = vars.clone();
        phase_vars.insert("entry_count".into(), entry_count.to_string());
        phase_vars.insert("max_entries".into(), max_entries.to_string());

        let owned: Vec<Entry> = candidates.iter().map(|e| (*e).clone()).collect();
        let prompt = Self::build_entries_prompt(template, &owned, &phase_vars);
        let merges = llm.collate(&prompt).await?;

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
            )
            .await
            .unwrap();

        // Phase 1: 1 split producing 2 entries.
        assert_eq!(outcome.tidy_splits, 1);
        assert_eq!(outcome.tidy_new_entries, 2);

        // Phase 2: 1 merge producing 1 entry.
        assert_eq!(outcome.collate_merges, 1);
        assert_eq!(outcome.collate_new_entries, 1);

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
}
