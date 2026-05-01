use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use shore_config::app::RetrievalMode;
use shore_llm::embed::{Embedder, OpenAIEmbedder};
use tracing::warn;

use crate::memory::markdown_store::{MarkdownEntry, MarkdownMemoryStore};

const MAX_EMBED_CHARS_PER_FILE: usize = 4_000;

/// Default local model when no embedding profile is configured.
const DEFAULT_LOCAL_MODEL_ID: &str = "bge-small-en-v1.5";

#[derive(Debug, Clone)]
pub struct RetrievalHit {
    pub entry: MarkdownEntry,
    pub lexical_score: usize,
    pub semantic_score: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct RetrievalResults {
    pub hits: Vec<RetrievalHit>,
    pub mode: &'static str,
    pub semantic_unavailable: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RetrievalError {
    #[error("markdown store: {0}")]
    Store(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct MemoryIndex {
    entries: BTreeMap<String, IndexedEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexedEntry {
    hash: String,
    size: u64,
    modified_at: String,
    embedding: Vec<f32>,
}

/// Build (or fetch from the process-wide cache) the configured embedder.
///
/// If the embedding catalog is empty and no default profile is named, falls
/// back to a local BGE-small embedder so the daemon stays useful with no
/// API keys.
pub fn resolve_embedder(
    default_name: Option<&str>,
    embedding_catalog: &BTreeMap<String, toml::Value>,
    http_client: &reqwest::Client,
) -> Result<Arc<dyn Embedder>, String> {
    if embedding_catalog.is_empty() && default_name.is_none() {
        return build_local_embedder(DEFAULT_LOCAL_MODEL_ID);
    }

    let profile_name = default_name
        .or_else(|| embedding_catalog.keys().next().map(String::as_str))
        .ok_or_else(|| "no embedding profile configured".to_string())?;
    let entry = embedding_catalog
        .get(profile_name)
        .ok_or_else(|| format!("embedding profile '{profile_name}' not found"))?;

    let provider = entry
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("openai");
    let model_id = entry
        .get("model_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("embedding profile '{profile_name}' is missing model_id"))?;

    let cache_key = format!("{provider}::{model_id}");
    match provider {
        "local" => shore_llm::embed::cache_or_build(&cache_key, || build_local(model_id)),
        _ => {
            let api_key_env = entry
                .get("api_key_env")
                .and_then(|v| v.as_str())
                .unwrap_or("OPENAI_API_KEY");
            let api_key = std::env::var(api_key_env)
                .map_err(|_| format!("embedding API key env var '{api_key_env}' is not set"))?;
            let base_url = entry
                .get("base_url")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let dimensions = entry
                .get("dimensions")
                .and_then(|v| v.as_integer())
                .unwrap_or(1536) as usize;
            let http_client = http_client.clone();
            shore_llm::embed::cache_or_build(&cache_key, move || {
                Ok::<Arc<dyn Embedder>, String>(Arc::new(OpenAIEmbedder::new(
                    http_client,
                    model_id,
                    api_key,
                    base_url,
                    dimensions,
                )))
            })
        }
    }
}

fn build_local_embedder(model_id: &str) -> Result<Arc<dyn Embedder>, String> {
    let cache_key = format!("local::{model_id}");
    shore_llm::embed::cache_or_build(&cache_key, || build_local(model_id))
}

#[cfg(feature = "local-embeddings")]
fn build_local(model_id: &str) -> Result<Arc<dyn Embedder>, String> {
    Ok(Arc::new(
        shore_llm::embed::LocalEmbedder::try_new(model_id).map_err(|e| e.to_string())?,
    ))
}

#[cfg(not(feature = "local-embeddings"))]
fn build_local(_model_id: &str) -> Result<Arc<dyn Embedder>, String> {
    Err("local-embeddings feature is not enabled in this build".to_string())
}

pub async fn search_memory(
    store: &MarkdownMemoryStore,
    query: &str,
    mode: &RetrievalMode,
    embedder: Option<&dyn Embedder>,
    index_path: Option<&Path>,
) -> Result<RetrievalResults, RetrievalError> {
    let entries = store
        .list_all()
        .await
        .map_err(|e| RetrievalError::Store(e.to_string()))?;

    let lexical_hits = lexical_rank_entries(entries.clone(), query);
    if entries.is_empty() {
        return Ok(RetrievalResults {
            hits: vec![],
            mode: "lexical",
            semantic_unavailable: None,
        });
    }

    if matches!(mode, RetrievalMode::Lexical) {
        return Ok(RetrievalResults {
            hits: lexical_hits,
            mode: "lexical",
            semantic_unavailable: None,
        });
    }

    let Some(embedder) = embedder else {
        return Ok(fallback(lexical_hits, "embedder unavailable"));
    };
    let Some(index_path) = index_path else {
        return Ok(fallback(lexical_hits, "memory index path unavailable"));
    };

    match hybrid_rank_entries(entries, query, embedder, index_path).await {
        Ok(hits) => Ok(RetrievalResults {
            hits,
            mode: "hybrid",
            semantic_unavailable: None,
        }),
        Err(e) => {
            warn!(error = %e, "hybrid memory retrieval unavailable; falling back to lexical");
            Ok(fallback(lexical_hits, &e))
        }
    }
}

fn fallback(hits: Vec<RetrievalHit>, reason: &str) -> RetrievalResults {
    RetrievalResults {
        hits,
        mode: "lexical",
        semantic_unavailable: Some(reason.to_string()),
    }
}

async fn hybrid_rank_entries(
    entries: Vec<MarkdownEntry>,
    query: &str,
    embedder: &dyn Embedder,
    index_path: &Path,
) -> Result<Vec<RetrievalHit>, String> {
    let mut index = load_index(index_path);
    let mut stale_docs = Vec::new();
    let mut stale_paths = Vec::new();
    let current_paths = entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<std::collections::BTreeSet<_>>();

    index
        .entries
        .retain(|path, _| current_paths.contains(path.as_str()));

    for entry in &entries {
        let hash = entry_hash(entry);
        let fresh = index.entries.get(&entry.path).is_some_and(|indexed| {
            indexed.hash == hash
                && indexed.size == entry.size
                && indexed.modified_at == entry.modified_at
        });
        if !fresh {
            stale_paths.push(entry.path.clone());
            stale_docs.push(document_for_embedding(entry));
        }
    }

    if !stale_docs.is_empty() {
        let inputs = stale_docs.iter().map(String::as_str).collect::<Vec<_>>();
        let embeddings = embedder.embed(&inputs).await.map_err(|e| e.to_string())?;
        if embeddings.len() != stale_paths.len() {
            return Err(format!(
                "embedding count mismatch: got {}, expected {}",
                embeddings.len(),
                stale_paths.len()
            ));
        }
        for (path, embedding) in stale_paths.into_iter().zip(embeddings) {
            if let Some(entry) = entries.iter().find(|entry| entry.path == path) {
                index.entries.insert(
                    path,
                    IndexedEntry {
                        hash: entry_hash(entry),
                        size: entry.size,
                        modified_at: entry.modified_at.clone(),
                        embedding,
                    },
                );
            }
        }
        save_index(index_path, &index);
    }

    let query_embeddings = embedder.embed(&[query]).await.map_err(|e| e.to_string())?;
    let query_embedding = query_embeddings
        .first()
        .ok_or_else(|| "embedding response did not include query vector".to_string())?;

    Ok(hybrid_rank_with_query_embedding(
        entries,
        query,
        &index,
        query_embedding,
    ))
}

fn hybrid_rank_with_query_embedding(
    entries: Vec<MarkdownEntry>,
    query: &str,
    index: &MemoryIndex,
    query_embedding: &[f32],
) -> Vec<RetrievalHit> {
    let q = query.to_lowercase();
    let terms = tokenize_query(&q);
    let mut hits = entries
        .into_iter()
        .map(|entry| RetrievalHit {
            lexical_score: lexical_score(&entry, &q, &terms),
            semantic_score: None,
            entry,
        })
        .collect::<Vec<_>>();

    let max_lexical = hits
        .iter()
        .map(|hit| hit.lexical_score)
        .max()
        .unwrap_or(1)
        .max(1) as f32;

    let mut scored = hits
        .drain(..)
        .map(|mut hit| {
            let semantic = index
                .entries
                .get(&hit.entry.path)
                .map(|indexed| cosine_similarity(query_embedding, &indexed.embedding));
            hit.semantic_score = semantic;
            let lexical_norm = hit.lexical_score as f32 / max_lexical;
            let semantic_norm = semantic.unwrap_or(0.0).max(0.0);
            let combined = lexical_norm * 0.45 + semantic_norm * 0.55;
            (combined, hit)
        })
        .filter(|(combined, _)| *combined > 0.0)
        .collect::<Vec<_>>();

    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.entry.path.cmp(&b.1.entry.path))
    });
    scored.into_iter().map(|(_, hit)| hit).collect()
}

fn lexical_rank_entries(entries: Vec<MarkdownEntry>, query: &str) -> Vec<RetrievalHit> {
    let q = query.to_lowercase();
    let terms = tokenize_query(&q);
    let mut scored = Vec::new();
    for entry in entries {
        let score = lexical_score(&entry, &q, &terms);
        if score > 0 {
            scored.push(RetrievalHit {
                entry,
                lexical_score: score,
                semantic_score: None,
            });
        }
    }
    scored.sort_by(|a, b| {
        b.lexical_score
            .cmp(&a.lexical_score)
            .then_with(|| a.entry.path.cmp(&b.entry.path))
    });
    scored
}

fn lexical_score(entry: &MarkdownEntry, query: &str, terms: &[&str]) -> usize {
    let path = entry.path.to_lowercase();
    let content = entry.content.to_lowercase();
    let title = entry
        .content
        .lines()
        .find(|line| line.trim_start().starts_with('#'))
        .unwrap_or_default()
        .to_lowercase();

    let mut score = 0;
    if path.contains(query) {
        score += 50;
    }
    if title.contains(query) {
        score += 40;
    }
    if content.contains(query) {
        score += 30;
    }

    for term in terms {
        if path.contains(term) {
            score += 12;
        }
        if title.contains(term) {
            score += 10;
        }
        if content.contains(term) {
            score += 4;
        }
    }

    score
}

fn tokenize_query(query: &str) -> Vec<&str> {
    query
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|term| term.len() >= 2)
        .collect()
}

fn document_for_embedding(entry: &MarkdownEntry) -> String {
    format!(
        "path: {}\nmodified_at: {}\n\n{}",
        entry.path,
        entry.modified_at,
        entry
            .content
            .chars()
            .take(MAX_EMBED_CHARS_PER_FILE)
            .collect::<String>()
    )
}

fn entry_hash(entry: &MarkdownEntry) -> String {
    let mut hasher = Sha256::new();
    hasher.update(entry.path.as_bytes());
    hasher.update(b"\0");
    hasher.update(entry.content.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn load_index(path: &Path) -> MemoryIndex {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn save_index(path: &Path, index: &MemoryIndex) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(index) {
        let _ = std::fs::write(path, json);
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, content: &str) -> MarkdownEntry {
        MarkdownEntry {
            path: path.to_string(),
            content: content.to_string(),
            size: content.len() as u64,
            modified_at: "2026-01-01T00:00:00+00:00".to_string(),
        }
    }

    #[test]
    fn lexical_ranking_prefers_path_and_title_matches() {
        let hits = lexical_rank_entries(
            vec![
                entry("people/ren.md", "# Ren\n\nLikes tea"),
                entry("topics/tea.md", "# Beverages\n\nRen likes tea"),
            ],
            "ren",
        );
        assert_eq!(hits[0].entry.path, "people/ren.md");
    }

    #[test]
    fn hybrid_ranking_can_promote_semantic_match() {
        let entries = vec![
            entry("a.md", "# Alpha\n\nneedle appears here"),
            entry("b.md", "# Beta\n\nneedle appears here too"),
        ];
        let mut index = MemoryIndex::default();
        index.entries.insert(
            "a.md".to_string(),
            IndexedEntry {
                hash: "a".to_string(),
                size: 1,
                modified_at: "t".to_string(),
                embedding: vec![0.0, 1.0],
            },
        );
        index.entries.insert(
            "b.md".to_string(),
            IndexedEntry {
                hash: "b".to_string(),
                size: 1,
                modified_at: "t".to_string(),
                embedding: vec![1.0, 0.0],
            },
        );

        let hits = hybrid_rank_with_query_embedding(entries, "needle", &index, &[0.0, 1.0]);
        assert_eq!(hits[0].entry.path, "a.md");
        assert!(hits[0].semantic_score.is_some());
    }

    #[test]
    fn hybrid_ranking_includes_semantic_only_matches() {
        let entries = vec![entry("a.md", "# Alpha\n\ncontains no query words")];
        let mut index = MemoryIndex::default();
        index.entries.insert(
            "a.md".to_string(),
            IndexedEntry {
                hash: "a".to_string(),
                size: 1,
                modified_at: "t".to_string(),
                embedding: vec![1.0, 0.0],
            },
        );

        let hits =
            hybrid_rank_with_query_embedding(entries, "completely different", &index, &[1.0, 0.0]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].lexical_score, 0);
    }

    #[test]
    fn cosine_handles_zero_vectors() {
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
    }
}
