//! Individual tool handler functions for the memory agent's 10 tools.
//!
//! Each handler takes a reference to the DB, an optional vector indexer, and the
//! tool input as a JSON value. Returns `Ok(description)` on success or
//! `Err(error_message)` on failure.
//!
//! Ported from V1 `memory_agent.py` lines 582-848.

use chrono::Local;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

use crate::memory::db::{Entry, MemoryDB};
use crate::memory::rag::{EntryMeta, RagPipeline, SourceResult};

use super::types::{AgentIndexer, AgentSearchContext};

// ---------------------------------------------------------------------------
// FTS5 query sanitization
// ---------------------------------------------------------------------------

/// FTS5 operators that should be stripped from natural language queries.
const FTS_OPERATORS: &[&str] = &["AND", "OR", "NOT", "NEAR"];

/// Common stop words that add noise to FTS5 queries without helping relevance.
const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "is", "was", "are", "were", "be", "been", "being", "have", "has", "had",
    "do", "does", "did", "will", "would", "could", "should", "may", "might", "shall", "can", "to",
    "of", "in", "for", "on", "with", "at", "by", "from", "as", "into", "about", "between",
    "through", "during", "before", "after", "above", "below", "up", "down", "out", "off", "over",
    "under", "again", "then", "once", "here", "there", "when", "where", "why", "how", "all",
    "each", "every", "both", "few", "more", "most", "other", "some", "such", "no", "nor", "only",
    "own", "same", "so", "than", "too", "very", "just", "because", "but", "if", "while", "that",
    "this", "these", "those", "what", "which", "who", "whom", "its", "it", "he", "she", "they",
    "them", "his", "her", "their", "my", "your", "our", "me", "him", "us", "i", "we", "you",
    "trying", "like", "also", "any", "get",
];

/// Sanitize a natural language query for FTS5.
///
/// FTS5 implicitly ANDs all tokens, which causes natural language queries to
/// return zero results (every term must appear in the entry). This function
/// strips stop words and FTS5 operators, then joins remaining terms with OR
/// so any matching term contributes to ranking via bm25().
fn sanitize_fts_query(raw: &str) -> String {
    // If the query looks like it was already written as an FTS5 query
    // (contains explicit quoted phrases or boolean operators used intentionally),
    // pass it through as-is.
    if raw.contains('"') {
        return raw.to_string();
    }

    let terms: Vec<&str> = raw
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '\'')
        .filter(|s| !s.is_empty())
        .filter(|s| !FTS_OPERATORS.contains(&s.to_uppercase().as_str()))
        .filter(|s| !STOP_WORDS.contains(&s.to_lowercase().as_str()))
        .collect();

    if terms.is_empty() {
        return String::new();
    }

    terms.join(" OR ")
}

// ---------------------------------------------------------------------------
// search_entries
// ---------------------------------------------------------------------------

pub fn handle_search_entries(db: &MemoryDB, input: &Value) -> Result<String, String> {
    let query = input["query"].as_str().unwrap_or("").trim().to_string();
    if query.is_empty() {
        return Err("Error: empty search query".into());
    }

    let status_raw = input["status"].as_str().unwrap_or("active");
    let status = if status_raw == "all" {
        None
    } else {
        Some(status_raw)
    };

    let fts_query = sanitize_fts_query(&query);
    if fts_query.is_empty() {
        return Err("Error: query produced no searchable terms".into());
    }

    match db.search_entries_fts(&fts_query, status, 20) {
        Ok(hits) => {
            if hits.is_empty() {
                Ok("No results.".into())
            } else {
                // Convert to JSON-friendly format
                let results: Vec<HashMap<String, Value>> = hits
                    .into_iter()
                    .map(|h| {
                        let mut m = HashMap::new();
                        m.insert("id".into(), Value::String(h.entry_id));
                        m.insert("summary_text".into(), Value::String(h.summary_text));
                        m.insert("topic_tags".into(), Value::String(h.topic_tags));
                        m.insert("topic_key".into(), Value::String(h.topic_key));
                        m.insert("status".into(), Value::String(h.status));
                        m.insert("memory_type".into(), Value::String(h.memory_type));
                        m.insert("created_at".into(), Value::String(h.created_at));
                        m.insert(
                            "confidence".into(),
                            serde_json::Number::from_f64(h.confidence)
                                .map(Value::Number)
                                .unwrap_or(Value::Null),
                        );
                        m.insert(
                            "rank".into(),
                            serde_json::Number::from_f64(h.rank)
                                .map(Value::Number)
                                .unwrap_or(Value::Null),
                        );
                        m
                    })
                    .collect();
                serde_json::to_string_pretty(&results).map_err(|e| format!("JSON error: {e}"))
            }
        }
        Err(e) => Err(format!("Search error: {e}")),
    }
}

// ---------------------------------------------------------------------------
// semantic_search
// ---------------------------------------------------------------------------

pub async fn handle_semantic_search(
    db: &MemoryDB,
    search_ctx: Option<&AgentSearchContext>,
    input: &Value,
) -> Result<String, String> {
    let ctx = search_ctx.ok_or_else(|| {
        "Semantic search unavailable: no embedding model configured. Use search_entries instead."
            .to_string()
    })?;

    let query = input["query"].as_str().unwrap_or("").trim();
    if query.is_empty() {
        return Err("Error: empty search query".into());
    }
    let top_k = input["top_k"].as_u64().unwrap_or(20).min(50) as usize;

    // 1. Lazy-populate BM25 index.
    ctx.populate_bm25_if_needed(db)
        .map_err(|e| format!("BM25 init error: {e}"))?;

    // 2. Embed the query.
    let query_embedding = ctx
        .embed_query(query)
        .await
        .map_err(|e| format!("Embedding error: {e}"))?;

    // 3. Vector search (over-retrieve for RRF fusion).
    let fetch_k = top_k * 3;
    let vector_hits = ctx
        .vector_store
        .search(&query_embedding, fetch_k)
        .await
        .map_err(|e| format!("Vector search error: {e}"))?;

    let vector_source: Vec<SourceResult> = vector_hits
        .iter()
        .map(|r| SourceResult {
            entry_id: r.entry_id.clone(),
            score: r.score as f64,
        })
        .collect();

    // 4. BM25 search.
    let bm25_hits = ctx.bm25.lock().unwrap().search(query, fetch_k);
    let bm25_source: Vec<SourceResult> = bm25_hits
        .iter()
        .map(|r| SourceResult {
            entry_id: r.entry_id.clone(),
            score: r.score,
        })
        .collect();

    // 5. Collect entry IDs from both sources.
    let all_ids: HashSet<&str> = vector_source
        .iter()
        .map(|r| r.entry_id.as_str())
        .chain(bm25_source.iter().map(|r| r.entry_id.as_str()))
        .collect();

    if all_ids.is_empty() {
        return Ok("No results.".into());
    }

    // 6. Fetch metadata for lifecycle scoring.
    let metadata: Vec<EntryMeta> = all_ids
        .iter()
        .filter_map(|id| {
            db.get_entry(id).ok().flatten().map(|e| EntryMeta {
                entry_id: e.id,
                status: e.status,
                confidence: e.confidence,
                created_at: e.created_at,
            })
        })
        .collect();

    // 7. RRF fusion + lifecycle scoring.
    let pipeline = RagPipeline::new(top_k);
    let ranked = pipeline.retrieve(&vector_source, &bm25_source, &metadata, false);

    if ranked.is_empty() {
        return Ok("No results.".into());
    }

    // 8. Fetch full entries and format output (same format as search_entries).
    let results: Vec<HashMap<String, Value>> = ranked
        .iter()
        .filter_map(|r| {
            db.get_entry(&r.entry_id).ok().flatten().map(|e| {
                let mut m = HashMap::new();
                m.insert("id".into(), Value::String(e.id));
                m.insert("summary_text".into(), Value::String(e.summary_text));
                m.insert("topic_tags".into(), Value::String(e.topic_tags));
                m.insert("topic_key".into(), Value::String(e.topic_key));
                m.insert("status".into(), Value::String(e.status));
                m.insert("memory_type".into(), Value::String(e.memory_type));
                m.insert("created_at".into(), Value::String(e.created_at));
                m.insert(
                    "confidence".into(),
                    serde_json::Number::from_f64(e.confidence)
                        .map(Value::Number)
                        .unwrap_or(Value::Null),
                );
                m.insert(
                    "relevance_score".into(),
                    serde_json::Number::from_f64(r.score)
                        .map(Value::Number)
                        .unwrap_or(Value::Null),
                );
                m
            })
        })
        .collect();

    if results.is_empty() {
        return Ok("No results.".into());
    }

    serde_json::to_string_pretty(&results).map_err(|e| format!("JSON error: {e}"))
}

// ---------------------------------------------------------------------------
// query_db
// ---------------------------------------------------------------------------

pub fn handle_query_db(db: &MemoryDB, input: &Value) -> Result<String, String> {
    let sql = input["sql"].as_str().unwrap_or("").trim().to_string();
    if sql.is_empty() {
        return Err("Error: empty SQL query".into());
    }

    match db.query_db_readonly(&sql, 50) {
        Ok(rows) => {
            if rows.is_empty() {
                Ok("No results.".into())
            } else {
                serde_json::to_string_pretty(&rows).map_err(|e| format!("JSON error: {e}"))
            }
        }
        Err(e) => Err(format!("SQL error: {e}")),
    }
}

// ---------------------------------------------------------------------------
// create_entry
// ---------------------------------------------------------------------------

pub async fn handle_create_entry(
    db: &MemoryDB,
    indexer: Option<&dyn AgentIndexer>,
    input: &Value,
) -> Result<String, String> {
    let summary_text = input["summary_text"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    if summary_text.is_empty() {
        return Err("Error: summary_text required".into());
    }

    let tags_str = input["topic_tags"].as_str().unwrap_or("").to_string();
    let topic_key = infer_topic_key(&tags_str, &summary_text);

    let now = Local::now();
    let entry_id = format!("{}_{}", now.format("%Y%m%d_%H%M%S"), 0);
    let now_str = now.to_rfc3339();

    let memory_type = input["memory_type"]
        .as_str()
        .unwrap_or("semantic")
        .to_string();
    let confidence = input["confidence"].as_f64().unwrap_or(0.9);
    let reason = input["reason"]
        .as_str()
        .unwrap_or("Created by memory agent")
        .to_string();

    let entry = Entry {
        id: entry_id.clone(),
        memory_type,
        source: "agent".into(),
        reason: "memory_agent".into(),
        status: "active".into(),
        confidence,
        summary_text: summary_text.clone(),
        topic_tags: tags_str,
        topic_key,
        start_timestamp: now_str.clone(),
        end_timestamp: now_str.clone(),
        message_count: 0,
        source_entry_ids: String::new(),
        related_entry_ids: String::new(),
        superseded_by: String::new(),
        created_at: now_str.clone(),
        updated_at: now_str,
        entry_type: String::new(),
        image_path: input["image_path"].as_str().unwrap_or("").to_string(),
        collated_at: String::new(),
    };

    db.create_entry(&entry)
        .map_err(|e| format!("DB error: {e}"))?;

    let cl_id = db
        .append_changelog("agent_create", &reason)
        .map_err(|e| format!("DB error: {e}"))?;
    let _ = db.link_changelog_entry(cl_id, &entry_id);

    // Index to vector store (best-effort)
    if let Some(idx) = indexer {
        let _ = idx.index_entry(&entry_id, &summary_text).await;
    }

    Ok(format!("Created entry {entry_id}"))
}

// ---------------------------------------------------------------------------
// update_entry
// ---------------------------------------------------------------------------

pub async fn handle_update_entry(
    db: &MemoryDB,
    indexer: Option<&dyn AgentIndexer>,
    input: &Value,
) -> Result<String, String> {
    let entry_id = input["entry_id"].as_str().unwrap_or("").to_string();
    if entry_id.is_empty() {
        return Err("Error: entry_id required".into());
    }
    let reason = input["reason"].as_str().unwrap_or("").to_string();

    // Fetch current entry
    let mut entry = db
        .get_entry(&entry_id)
        .map_err(|e| format!("DB error: {e}"))?
        .ok_or_else(|| format!("Entry {entry_id} not found"))?;

    // Apply partial updates
    let mut has_changes = false;
    if let Some(v) = input.get("summary_text").and_then(|v| v.as_str()) {
        entry.summary_text = v.to_string();
        has_changes = true;
    }
    if let Some(v) = input.get("topic_tags").and_then(|v| v.as_str()) {
        entry.topic_tags = v.to_string();
        has_changes = true;
    }
    if let Some(v) = input.get("confidence").and_then(|v| v.as_f64()) {
        entry.confidence = v;
        has_changes = true;
    }
    if let Some(v) = input.get("memory_type").and_then(|v| v.as_str()) {
        entry.memory_type = v.to_string();
        has_changes = true;
    }

    if !has_changes {
        return Err("Error: no fields to update".into());
    }

    entry.updated_at = Local::now().to_rfc3339();

    let rows = db
        .update_entry(&entry)
        .map_err(|e| format!("DB error: {e}"))?;
    if rows == 0 {
        return Err(format!("Entry {entry_id} not found"));
    }

    let cl_id = db
        .append_changelog("agent_update", &reason)
        .map_err(|e| format!("DB error: {e}"))?;
    let _ = db.link_changelog_entry(cl_id, &entry_id);

    // Re-index (best-effort)
    if let Some(idx) = indexer {
        let _ = idx.index_entry(&entry_id, &entry.summary_text).await;
    }

    Ok(format!("Updated {entry_id}"))
}

// ---------------------------------------------------------------------------
// supersede_entry
// ---------------------------------------------------------------------------

pub async fn handle_supersede_entry(
    db: &MemoryDB,
    indexer: Option<&dyn AgentIndexer>,
    input: &Value,
) -> Result<String, String> {
    let entry_id = input["entry_id"].as_str().unwrap_or("").to_string();
    if entry_id.is_empty() {
        return Err("Error: entry_id required".into());
    }
    let reason = input["reason"].as_str().unwrap_or("").to_string();
    let superseded_by = input["superseded_by"].as_str().unwrap_or("").to_string();

    let rows = db
        .supersede_entry(&entry_id, &superseded_by)
        .map_err(|e| format!("DB error: {e}"))?;
    if rows == 0 {
        return Err(format!("Entry {entry_id} not found"));
    }

    let cl_id = db
        .append_changelog("agent_supersede", &reason)
        .map_err(|e| format!("DB error: {e}"))?;
    let _ = db.link_changelog_entry(cl_id, &entry_id);

    // Re-index the superseded entry (best-effort)
    if let Some(idx) = indexer {
        let _ = idx.index_entry(&entry_id, "").await;
    }

    Ok(format!("Superseded {entry_id}"))
}

// ---------------------------------------------------------------------------
// update_entity
// ---------------------------------------------------------------------------

pub fn handle_update_entity(db: &MemoryDB, input: &Value) -> Result<String, String> {
    let name = input["name"].as_str().unwrap_or("").trim().to_string();
    if name.is_empty() {
        return Err("Error: name required".into());
    }

    let entity_type = input["type"].as_str().unwrap_or("").to_string();
    let description = input["description"].as_str().unwrap_or("").to_string();

    let eid = db
        .upsert_entity(&name, &entity_type, &description)
        .map_err(|e| format!("DB error: {e}"))?;

    let cl_id = db
        .append_changelog(
            "entity_update",
            &format!("Memory agent updated entity '{name}'"),
        )
        .map_err(|e| format!("DB error: {e}"))?;

    // Link changelog to entity
    let _ = db.link_changelog_entity(cl_id, eid);

    Ok(format!("Updated entity '{name}' (id={eid})"))
}

// ---------------------------------------------------------------------------
// merge_entity
// ---------------------------------------------------------------------------

pub fn handle_merge_entity(db: &MemoryDB, input: &Value) -> Result<String, String> {
    let from_name = input["from_name"].as_str().unwrap_or("").trim().to_string();
    let to_name = input["to_name"].as_str().unwrap_or("").trim().to_string();
    if from_name.is_empty() || to_name.is_empty() {
        return Err("Error: from_name and to_name required".into());
    }

    let from_entity = db
        .get_entity_by_name(&from_name)
        .map_err(|e| format!("DB error: {e}"))?
        .ok_or_else(|| format!("Error: entity '{from_name}' not found"))?;

    let to_entity = db
        .get_entity_by_name(&to_name)
        .map_err(|e| format!("DB error: {e}"))?
        .ok_or_else(|| format!("Error: entity '{to_name}' not found"))?;

    if from_entity.entity_id == to_entity.entity_id {
        return Err(format!(
            "Error: '{from_name}' and '{to_name}' are the same entity"
        ));
    }

    let count = db
        .merge_entity(from_entity.entity_id, to_entity.entity_id)
        .map_err(|e| format!("DB error: {e}"))?;

    let cl_id = db
        .append_changelog(
            "entity_merge",
            &format!(
                "Merged '{}' (id={}) into '{}' (id={}), re-linked {} entries",
                from_name, from_entity.entity_id, to_name, to_entity.entity_id, count
            ),
        )
        .map_err(|e| format!("DB error: {e}"))?;

    // Link changelog to target entity
    let _ = db.link_changelog_entity(cl_id, to_entity.entity_id);

    Ok(format!(
        "Merged '{from_name}' into '{to_name}': {count} entries re-linked"
    ))
}

// ---------------------------------------------------------------------------
// resolve_flag
// ---------------------------------------------------------------------------

pub fn handle_resolve_flag(db: &MemoryDB, input: &Value) -> Result<String, String> {
    let flag_id = input["flag_id"]
        .as_i64()
        .ok_or_else(|| "Error: flag_id required".to_string())?;
    let resolution = input["resolution"].as_str().unwrap_or("").to_string();

    // Get the flag first so we can boost confidence
    let flag = db.get_flag(flag_id).map_err(|e| format!("DB error: {e}"))?;

    db.resolve_flag(flag_id, &resolution)
        .map_err(|e| format!("DB error: {e}"))?;

    let cl_id = db
        .append_changelog(
            "flag_resolve",
            &format!("Resolved flag {flag_id}: {resolution}"),
        )
        .map_err(|e| format!("DB error: {e}"))?;
    let _ = cl_id; // changelog doesn't need linking for flags

    // Boost confidence of the flagged entry to max(current, 0.8)
    if let Some(flag) = flag {
        if let Ok(Some(entry)) = db.get_entry(&flag.entry_id) {
            let new_conf = entry.confidence.clamp(0.8_f64, 1.0);
            let _ = db.set_confidence(&entry.id, new_conf);
        }
    }

    Ok(format!("Resolved flag {flag_id}"))
}

// ---------------------------------------------------------------------------
// create_flag
// ---------------------------------------------------------------------------

pub fn handle_create_flag(db: &MemoryDB, input: &Value) -> Result<String, String> {
    let entry_id = input["entry_id"].as_str().unwrap_or("").to_string();
    let flag_type = input["flag_type"].as_str().unwrap_or("").to_string();
    let reason = input["reason"].as_str().unwrap_or("").to_string();

    if entry_id.is_empty() || flag_type.is_empty() {
        return Err("Error: entry_id and flag_type required".into());
    }

    let fid = db
        .create_flag(&entry_id, &flag_type, &reason)
        .map_err(|e| format!("DB error: {e}"))?;

    Ok(format!("Created flag {fid} ({flag_type}) on {entry_id}"))
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// Execute a tool by name and return the result string.
///
/// Returns `Ok(result_text)` on success, `Err(error_text)` on failure.
/// Both are returned as tool_result content to the LLM.
pub async fn execute_tool(
    name: &str,
    db: &MemoryDB,
    indexer: Option<&dyn AgentIndexer>,
    search_ctx: Option<&AgentSearchContext>,
    input: &Value,
) -> String {
    let result = match name {
        "search_entries" => handle_search_entries(db, input),
        "semantic_search" => {
            return match handle_semantic_search(db, search_ctx, input).await {
                Ok(s) => s,
                Err(e) => e,
            }
        }
        "query_db" => handle_query_db(db, input),
        "create_entry" => handle_create_entry(db, indexer, input).await,
        "update_entry" => handle_update_entry(db, indexer, input).await,
        "supersede_entry" => handle_supersede_entry(db, indexer, input).await,
        "update_entity" => handle_update_entity(db, input),
        "merge_entity" => handle_merge_entity(db, input),
        "resolve_flag" => handle_resolve_flag(db, input),
        "create_flag" => handle_create_flag(db, input),
        _ => Err(format!("Unknown tool: {name}")),
    };

    match result {
        Ok(s) => s,
        Err(e) => e,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Infer a topic_key from tags and summary text.
///
/// Matches V1 `infer_topic_key_from_parts`: uses first tag if available,
/// otherwise first 3 words of summary lowercased with underscores.
fn infer_topic_key(tags: &str, summary: &str) -> String {
    let tag_list: Vec<&str> = tags
        .split(',')
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect();
    if let Some(first_tag) = tag_list.first() {
        return first_tag
            .to_lowercase()
            .replace(' ', "_")
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_')
            .collect();
    }

    // Fall back to first 3 words of summary
    summary
        .split_whitespace()
        .take(3)
        .collect::<Vec<_>>()
        .join("_")
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::db::MemoryDB;
    use serde_json::json;

    fn test_db() -> MemoryDB {
        MemoryDB::open_in_memory().unwrap()
    }

    fn seed_entry(db: &MemoryDB, id: &str, summary: &str) {
        let now = Local::now().to_rfc3339();
        let entry = Entry {
            id: id.to_string(),
            memory_type: "semantic".to_string(),
            source: "agent".to_string(),
            reason: "test".to_string(),
            status: "active".to_string(),
            confidence: 0.9,
            summary_text: summary.to_string(),
            topic_tags: "test".to_string(),
            topic_key: "test".to_string(),
            start_timestamp: now.clone(),
            end_timestamp: now.clone(),
            message_count: 0,
            source_entry_ids: String::new(),
            related_entry_ids: String::new(),
            superseded_by: String::new(),
            created_at: now.clone(),
            updated_at: now,
            entry_type: String::new(),
            image_path: String::new(),
            collated_at: String::new(),
        };
        db.create_entry(&entry).unwrap();
    }

    // -- search_entries -------------------------------------------------------

    #[test]
    fn search_entries_empty_query_error() {
        let db = test_db();
        let result = handle_search_entries(&db, &json!({"query": ""}));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty"));
    }

    #[test]
    fn search_entries_no_results() {
        let db = test_db();
        let result = handle_search_entries(&db, &json!({"query": "nonexistent"}));
        assert_eq!(result.unwrap(), "No results.");
    }

    // -- sanitize_fts_query ---------------------------------------------------

    #[test]
    fn sanitize_simple_keywords() {
        assert_eq!(sanitize_fts_query("gemini live"), "gemini OR live");
    }

    #[test]
    fn sanitize_strips_stop_words() {
        let result = sanitize_fts_query("ren trying voice or video chat with qifei");
        // "trying", "or", "with" stripped; "ren", "voice", "video", "chat", "qifei" kept
        assert_eq!(result, "ren OR voice OR video OR chat OR qifei");
    }

    #[test]
    fn sanitize_strips_fts_operators() {
        assert_eq!(sanitize_fts_query("cats AND dogs"), "cats OR dogs");
        assert_eq!(sanitize_fts_query("NOT bad"), "bad");
    }

    #[test]
    fn sanitize_preserves_quoted_phrases() {
        let q = r#""gemini live" voice"#;
        assert_eq!(sanitize_fts_query(q), q);
    }

    #[test]
    fn sanitize_all_stop_words_returns_empty() {
        assert_eq!(sanitize_fts_query("the is a"), "");
    }

    #[test]
    fn sanitize_natural_language_query() {
        let result = sanitize_fts_query(
            "ren trying voice or video chat with qifei, streaming video, voice interaction",
        );
        assert!(result.contains("voice"));
        assert!(result.contains("video"));
        assert!(result.contains("qifei"));
        assert!(result.contains("streaming"));
        assert!(result.contains("interaction"));
        assert!(!result.contains("trying"));
        assert!(!result.contains("with"));
        // All terms joined with OR
        for part in result.split(" OR ") {
            assert!(!part.is_empty());
        }
    }

    #[test]
    fn sanitize_handles_punctuation() {
        assert_eq!(sanitize_fts_query("hello, world!"), "hello OR world");
    }

    // -- query_db -------------------------------------------------------------

    #[test]
    fn query_db_select_works() {
        let db = test_db();
        seed_entry(&db, "e1", "Alice likes chocolate");
        let result = handle_query_db(
            &db,
            &json!({"sql": "SELECT id, summary_text FROM entries WHERE id = 'e1'"}),
        );
        let text = result.unwrap();
        assert!(text.contains("Alice likes chocolate"));
    }

    #[test]
    fn query_db_blocks_delete() {
        let db = test_db();
        let result = handle_query_db(&db, &json!({"sql": "DELETE FROM entries"}));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("SELECT"));
    }

    #[test]
    fn query_db_empty_sql_error() {
        let db = test_db();
        let result = handle_query_db(&db, &json!({"sql": ""}));
        assert!(result.is_err());
    }

    // -- create_entry ---------------------------------------------------------

    #[tokio::test]
    async fn create_entry_generates_id() {
        let db = test_db();
        let result = handle_create_entry(
            &db,
            None,
            &json!({
                "summary_text": "Alice likes chocolate",
                "topic_tags": "food, preferences",
                "reason": "test"
            }),
        )
        .await;
        let text = result.unwrap();
        assert!(text.starts_with("Created entry "));

        // Verify in DB
        let entry_id = text.strip_prefix("Created entry ").unwrap();
        let entry = db.get_entry(entry_id).unwrap().unwrap();
        assert_eq!(entry.summary_text, "Alice likes chocolate");
        assert_eq!(entry.source, "agent");
        assert_eq!(entry.confidence, 0.9);
        assert_eq!(entry.topic_key, "food");
    }

    #[tokio::test]
    async fn create_entry_empty_summary_error() {
        let db = test_db();
        let result =
            handle_create_entry(&db, None, &json!({"summary_text": "", "reason": "test"})).await;
        assert!(result.is_err());
    }

    // -- update_entry ---------------------------------------------------------

    #[tokio::test]
    async fn update_entry_partial_update() {
        let db = test_db();
        seed_entry(&db, "e1", "Alice likes chocolate");

        let result = handle_update_entry(
            &db,
            None,
            &json!({
                "entry_id": "e1",
                "summary_text": "Alice loves dark chocolate",
                "reason": "correction"
            }),
        )
        .await;
        assert_eq!(result.unwrap(), "Updated e1");

        let entry = db.get_entry("e1").unwrap().unwrap();
        assert_eq!(entry.summary_text, "Alice loves dark chocolate");
        // Other fields unchanged
        assert_eq!(entry.confidence, 0.9);
    }

    #[tokio::test]
    async fn update_entry_not_found() {
        let db = test_db();
        let result = handle_update_entry(
            &db,
            None,
            &json!({
                "entry_id": "nonexistent",
                "summary_text": "whatever",
                "reason": "test"
            }),
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[tokio::test]
    async fn update_entry_no_fields_error() {
        let db = test_db();
        seed_entry(&db, "e1", "test");
        let result =
            handle_update_entry(&db, None, &json!({"entry_id": "e1", "reason": "test"})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no fields"));
    }

    // -- supersede_entry ------------------------------------------------------

    #[tokio::test]
    async fn supersede_entry_marks_old() {
        let db = test_db();
        seed_entry(&db, "e1", "old info");
        seed_entry(&db, "e2", "new info");

        let result = handle_supersede_entry(
            &db,
            None,
            &json!({
                "entry_id": "e1",
                "superseded_by": "e2",
                "reason": "correction"
            }),
        )
        .await;
        assert_eq!(result.unwrap(), "Superseded e1");

        let entry = db.get_entry("e1").unwrap().unwrap();
        assert_eq!(entry.status, "superseded");
        assert_eq!(entry.superseded_by, "e2");
    }

    // -- update_entity --------------------------------------------------------

    #[test]
    fn update_entity_creates_new() {
        let db = test_db();
        let result = handle_update_entity(
            &db,
            &json!({"name": "Alice", "type": "person", "description": "A friend"}),
        );
        let text = result.unwrap();
        assert!(text.contains("Updated entity 'Alice'"));

        let entity = db.get_entity_by_name("Alice").unwrap().unwrap();
        assert_eq!(entity.name, "Alice");
        assert_eq!(entity.entity_type, "person");
    }

    // -- merge_entity ---------------------------------------------------------

    #[test]
    fn merge_entity_reassigns_links() {
        let db = test_db();
        // Create two entities
        db.upsert_entity("Rosa", "person", "old name").unwrap();
        db.upsert_entity("Rosa Do", "person", "canonical name")
            .unwrap();

        let result = handle_merge_entity(&db, &json!({"from_name": "Rosa", "to_name": "Rosa Do"}));
        let text = result.unwrap();
        assert!(text.contains("Merged 'Rosa' into 'Rosa Do'"));
    }

    #[test]
    fn merge_entity_not_found() {
        let db = test_db();
        db.upsert_entity("Rosa Do", "person", "").unwrap();

        let result = handle_merge_entity(
            &db,
            &json!({"from_name": "Nonexistent", "to_name": "Rosa Do"}),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    // -- resolve_flag ---------------------------------------------------------

    #[test]
    fn resolve_flag_boosts_confidence() {
        let db = test_db();
        seed_entry(&db, "e1", "flagged entry");

        // Set low confidence
        db.set_confidence("e1", 0.5).unwrap();

        let flag_id = db
            .create_flag("e1", "contradiction", "test reason")
            .unwrap();

        let result = handle_resolve_flag(
            &db,
            &json!({"flag_id": flag_id, "resolution": "confirmed correct"}),
        );
        assert!(result
            .unwrap()
            .contains(&format!("Resolved flag {flag_id}")));

        // Confidence should be boosted to 0.8
        let entry = db.get_entry("e1").unwrap().unwrap();
        assert!((entry.confidence - 0.8).abs() < 0.001);
    }

    #[test]
    fn resolve_flag_preserves_high_confidence() {
        let db = test_db();
        seed_entry(&db, "e1", "high confidence entry");
        // confidence is already 0.9 from seed_entry

        let flag_id = db.create_flag("e1", "stale", "check if current").unwrap();

        let result = handle_resolve_flag(
            &db,
            &json!({"flag_id": flag_id, "resolution": "still current"}),
        );
        assert!(result.is_ok());

        // Confidence should remain 0.9 (not reduced to 0.8)
        let entry = db.get_entry("e1").unwrap().unwrap();
        assert!((entry.confidence - 0.9).abs() < 0.001);
    }

    // -- create_flag ----------------------------------------------------------

    #[test]
    fn create_flag_works() {
        let db = test_db();
        seed_entry(&db, "e1", "test entry");

        let result = handle_create_flag(
            &db,
            &json!({
                "entry_id": "e1",
                "flag_type": "contradiction",
                "reason": "conflicts with something"
            }),
        );
        let text = result.unwrap();
        assert!(text.contains("Created flag"));
        assert!(text.contains("contradiction"));
        assert!(text.contains("e1"));
    }

    // -- infer_topic_key ------------------------------------------------------

    #[test]
    fn topic_key_from_tags() {
        assert_eq!(infer_topic_key("food, preferences", "whatever"), "food");
    }

    #[test]
    fn topic_key_from_summary_fallback() {
        assert_eq!(
            infer_topic_key("", "Alice likes dark chocolate"),
            "alice_likes_dark"
        );
    }
}
