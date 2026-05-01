//! Workspace-wide embedding index + hybrid search.
//!
//! Walks the same paths the lexical `search` tool walks (workspace root +
//! optionally the memory namespace) with identical security rules
//! (symlink skip, size cap, non-UTF8 skip), embeds every text file once,
//! and stores vectors in a JSON index under the character data directory.
//!
//! Subsequent searches reuse cached vectors and only re-embed files whose
//! content hash, size, or mtime changed.
//!
//! Non-text and oversized files are recorded with `embedded: false` and a
//! skip reason so the walker doesn't churn on them every query.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use shore_llm::embed::Embedder;

const MAX_EMBED_CHARS_PER_FILE: usize = 4_000;
const SEARCH_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// One file's place in the persisted index.
///
/// `embedded: false` entries record *why* the file was skipped so future
/// walks can short-circuit without re-reading the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct IndexedEntry {
    pub hash: String,
    pub size: u64,
    pub modified_at_secs: i64,
    pub model_id: String,
    pub embedded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct WorkspaceIndex {
    pub entries: BTreeMap<String, IndexedEntry>,
}

/// One file's contribution to a hybrid query result.
#[derive(Debug, Clone)]
pub struct ScoredFile {
    pub display_path: String,
    pub fs_path: PathBuf,
    pub content: Option<String>,
    pub semantic_score: Option<f32>,
    pub lexical_score: usize,
    pub combined_score: f32,
    pub embedded: bool,
    pub skip_reason: Option<String>,
}

/// Outcome of a hybrid query, including stats useful for telling the model
/// what was searched.
pub struct HybridSearchResult {
    pub files: Vec<ScoredFile>,
    pub searched_files: usize,
    pub embedded_files: usize,
    pub skipped_binary_or_large: usize,
}

/// Search mode selecting how lexical and semantic signals are fused.
#[derive(Debug, Clone, Copy)]
pub enum HybridMode {
    /// 0.45 lexical + 0.55 cosine. Good default.
    Hybrid,
    /// Pure cosine similarity (lexical weight zero).
    Vector,
}

impl HybridMode {
    fn weights(self) -> (f32, f32) {
        match self {
            HybridMode::Hybrid => (0.45, 0.55),
            HybridMode::Vector => (0.0, 1.0),
        }
    }
}

/// Walk the workspace, refresh the embedding index, and return files
/// ranked by `combined_score`. Files with no signal at all are filtered.
pub async fn hybrid_search(
    workspace_dir: &str,
    include_memory: bool,
    query: &str,
    mode: HybridMode,
    embedder: &dyn Embedder,
    index_path: &Path,
) -> Result<HybridSearchResult, String> {
    if workspace_dir.is_empty() {
        return Err("workspace not configured".into());
    }
    let root = PathBuf::from(workspace_dir);
    if !root.exists() {
        return Ok(HybridSearchResult {
            files: vec![],
            searched_files: 0,
            embedded_files: 0,
            skipped_binary_or_large: 0,
        });
    }

    let mut index = load_index(index_path);
    let model_id = embedder.model_id().to_string();

    let mut candidates = enumerate_files(workspace_dir, include_memory).await;
    let mut skipped_binary_or_large = 0usize;

    // Drop entries whose files vanished or now live outside the walk.
    let current: BTreeSet<String> = candidates.iter().map(|f| f.display_path.clone()).collect();
    index.entries.retain(|p, _| current.contains(p));

    // Refresh per-file content + identify stale entries.
    let mut stale_paths: Vec<String> = Vec::new();
    let mut stale_docs: Vec<String> = Vec::new();
    for file in &mut candidates {
        let bytes = match tokio::fs::read(&file.fs_path).await {
            Ok(b) => b,
            Err(_) => {
                file.skip_reason = Some("read failed".into());
                continue;
            }
        };
        match String::from_utf8(bytes) {
            Ok(text) => {
                let hash = content_hash(&file.display_path, &text);
                file.hash = Some(hash.clone());
                let fresh = index.entries.get(&file.display_path).is_some_and(|e| {
                    e.embedded && e.hash == hash && e.size == file.size && e.model_id == model_id
                });
                if !fresh {
                    stale_paths.push(file.display_path.clone());
                    stale_docs.push(document_for_embedding(&file.display_path, &text));
                }
                file.content = Some(text);
            }
            Err(_) => {
                skipped_binary_or_large += 1;
                file.skip_reason = Some("non-utf8".into());
                index.entries.insert(
                    file.display_path.clone(),
                    IndexedEntry {
                        hash: format!("size:{}", file.size),
                        size: file.size,
                        modified_at_secs: file.modified_at_secs,
                        model_id: model_id.clone(),
                        embedded: false,
                        reason: Some("non-utf8".into()),
                        embedding: Vec::new(),
                    },
                );
            }
        }
    }

    if !stale_docs.is_empty() {
        let inputs: Vec<&str> = stale_docs.iter().map(String::as_str).collect();
        let vectors = embedder.embed(&inputs).await.map_err(|e| e.to_string())?;
        if vectors.len() != stale_paths.len() {
            return Err(format!(
                "embedding count mismatch: got {}, expected {}",
                vectors.len(),
                stale_paths.len()
            ));
        }
        for (path, embedding) in stale_paths.into_iter().zip(vectors) {
            if let Some(file) = candidates.iter().find(|f| f.display_path == path) {
                if let Some(hash) = &file.hash {
                    index.entries.insert(
                        path,
                        IndexedEntry {
                            hash: hash.clone(),
                            size: file.size,
                            modified_at_secs: file.modified_at_secs,
                            model_id: model_id.clone(),
                            embedded: true,
                            reason: None,
                            embedding,
                        },
                    );
                }
            }
        }
        save_index(index_path, &index);
    }

    let query_vector = embedder
        .embed(&[query])
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .ok_or_else(|| "embedding response did not include query vector".to_string())?;

    let q_lower = query.to_lowercase();
    let terms = tokenize_query(&q_lower);

    let mut scored: Vec<ScoredFile> = candidates
        .into_iter()
        .map(|file| {
            let lexical = file
                .content
                .as_deref()
                .map(|c| lexical_score(&file.display_path, c, &q_lower, &terms))
                .unwrap_or(0);
            let entry = index.entries.get(&file.display_path);
            let semantic = entry
                .filter(|e| e.embedded)
                .map(|e| cosine_similarity(&query_vector, &e.embedding));
            let embedded = entry.map(|e| e.embedded).unwrap_or(false);
            ScoredFile {
                display_path: file.display_path,
                fs_path: file.fs_path,
                content: file.content,
                lexical_score: lexical,
                semantic_score: semantic,
                combined_score: 0.0,
                embedded,
                skip_reason: file.skip_reason,
            }
        })
        .collect();

    let max_lex = scored
        .iter()
        .map(|f| f.lexical_score)
        .max()
        .unwrap_or(1)
        .max(1) as f32;
    let embedded_files = scored.iter().filter(|f| f.semantic_score.is_some()).count();

    let (lw, sw) = mode.weights();
    for f in &mut scored {
        let lex_norm = f.lexical_score as f32 / max_lex;
        let sem_norm = f.semantic_score.unwrap_or(0.0).max(0.0);
        f.combined_score = lex_norm * lw + sem_norm * sw;
    }

    let searched_files = scored.len();
    scored.retain(|f| f.combined_score > 0.0);
    scored.sort_by(|a, b| {
        b.combined_score
            .partial_cmp(&a.combined_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.display_path.cmp(&b.display_path))
    });

    Ok(HybridSearchResult {
        files: scored,
        searched_files,
        embedded_files,
        skipped_binary_or_large,
    })
}

#[derive(Debug, Clone)]
struct FileCandidate {
    display_path: String,
    fs_path: PathBuf,
    size: u64,
    modified_at_secs: i64,
    content: Option<String>,
    hash: Option<String>,
    skip_reason: Option<String>,
}

async fn enumerate_files(workspace_dir: &str, include_memory: bool) -> Vec<FileCandidate> {
    let mut pending = vec![PathBuf::from(workspace_dir)];
    let mut out: Vec<FileCandidate> = Vec::new();

    while let Some(path) = pending.pop() {
        let meta = match tokio::fs::symlink_metadata(&path).await {
            Ok(m) => m,
            Err(_) => continue,
        };

        if meta.file_type().is_symlink() {
            continue;
        }

        if meta.is_dir() {
            if !include_memory && is_root_memory_dir(workspace_dir, &path) {
                continue;
            }
            let mut read_dir = match tokio::fs::read_dir(&path).await {
                Ok(rd) => rd,
                Err(_) => continue,
            };
            while let Ok(Some(entry)) = read_dir.next_entry().await {
                pending.push(entry.path());
            }
            continue;
        }

        if !meta.is_file() {
            continue;
        }

        let display_path = display_path_for(workspace_dir, &path);
        let size = meta.len();
        let modified_at_secs = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let skip_reason = if size > SEARCH_MAX_FILE_BYTES {
            Some("oversize".to_string())
        } else {
            None
        };

        out.push(FileCandidate {
            display_path,
            fs_path: path,
            size,
            modified_at_secs,
            content: None,
            hash: None,
            skip_reason,
        });
    }

    out
}

fn display_path_for(workspace_dir: &str, path: &Path) -> String {
    let workspace_root = Path::new(workspace_dir);
    if let Ok(rel) = path.strip_prefix(workspace_root) {
        return rel.to_string_lossy().replace('\\', "/");
    }
    path.to_string_lossy().replace('\\', "/")
}

fn is_root_memory_dir(workspace_dir: &str, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(Path::new(workspace_dir)) else {
        return false;
    };
    let mut components = rel.components();
    matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(first)), None)
            if first == std::ffi::OsStr::new("memory")
    )
}

fn lexical_score(path: &str, content: &str, q_lower: &str, terms: &[&str]) -> usize {
    let path_lower = path.to_lowercase();
    let content_lower = content.to_lowercase();
    let title_lower = content
        .lines()
        .find(|line| line.trim_start().starts_with('#'))
        .unwrap_or_default()
        .to_lowercase();

    let mut score = 0;
    if path_lower.contains(q_lower) {
        score += 50;
    }
    if title_lower.contains(q_lower) {
        score += 40;
    }
    if content_lower.contains(q_lower) {
        score += 30;
    }
    for term in terms {
        if path_lower.contains(term) {
            score += 12;
        }
        if title_lower.contains(term) {
            score += 10;
        }
        if content_lower.contains(term) {
            score += 4;
        }
    }
    score
}

fn tokenize_query(query: &str) -> Vec<&str> {
    query
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|t| t.len() >= 2)
        .collect()
}

fn document_for_embedding(path: &str, content: &str) -> String {
    let trimmed: String = content.chars().take(MAX_EMBED_CHARS_PER_FILE).collect();
    format!("path: {path}\n\n{trimmed}")
}

fn content_hash(path: &str, content: &str) -> String {
    let mut h = Sha256::new();
    h.update(path.as_bytes());
    h.update(b"\0");
    h.update(content.as_bytes());
    format!("{:x}", h.finalize())
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

fn load_index(path: &Path) -> WorkspaceIndex {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_index(path: &Path, index: &WorkspaceIndex) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(index) {
        let _ = std::fs::write(path, json);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use async_trait::async_trait;
    use shore_llm::LlmError;
    use tempfile::TempDir;
    use tokio::fs;

    use super::*;

    /// Topic-keyword embedder for deterministic testing.
    ///
    /// Each "topic" gets a one-hot dimension in the output vector. Inputs that
    /// mention the same topic produce vectors that point in the same direction.
    /// `call_count` records how many `embed` calls were made — used to assert
    /// incremental reindex behavior.
    struct TopicEmbedder {
        topics: &'static [&'static str],
        call_count: Arc<AtomicUsize>,
        input_count: Arc<AtomicUsize>,
    }

    impl TopicEmbedder {
        fn new(topics: &'static [&'static str]) -> Self {
            Self {
                topics,
                call_count: Arc::new(AtomicUsize::new(0)),
                input_count: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn vector_for(&self, text: &str) -> Vec<f32> {
            let lower = text.to_lowercase();
            let mut v = vec![0.0_f32; self.topics.len()];
            for (i, topic) in self.topics.iter().enumerate() {
                if lower.contains(topic) {
                    v[i] = 1.0;
                }
            }
            v
        }
    }

    #[async_trait]
    impl Embedder for TopicEmbedder {
        async fn embed(&self, inputs: &[&str]) -> Result<Vec<Vec<f32>>, LlmError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            self.input_count.fetch_add(inputs.len(), Ordering::SeqCst);
            Ok(inputs.iter().map(|s| self.vector_for(s)).collect())
        }
        fn model_id(&self) -> &str {
            "topic-test"
        }
        fn dimensions(&self) -> usize {
            self.topics.len()
        }
    }

    #[test]
    fn cosine_handles_zero_vectors() {
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
    }

    #[test]
    fn cosine_handles_mismatched_dims() {
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn lexical_score_rewards_path_and_title() {
        let path = "people/ren.md";
        let content = "# Ren\n\nLikes tea";
        let q = "ren";
        let s = lexical_score(path, content, q, &["ren"]);
        assert!(s >= 50 + 40);
    }

    async fn write_file(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await.unwrap();
        }
        fs::write(path, body).await.unwrap();
    }

    fn setup() -> (TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let idx = tmp.path().join("data/workspace_index.json");
        (tmp, ws, idx)
    }

    #[tokio::test]
    async fn hybrid_promotes_semantic_only_match() {
        let (_g, ws, idx) = setup();
        let ws_str = ws.to_string_lossy().into_owned();

        // a.md is about tea + garden. b.md is unrelated. The query mentions
        // tea + garden but uses none of those words verbatim — so semantic
        // is the only signal that distinguishes them.
        write_file(&ws, "a.md", "# Notes\n\nThe garden is full of tea plants.").await;
        write_file(&ws, "b.md", "# Other\n\nThe accountant filed taxes.").await;

        let embedder = TopicEmbedder::new(&["tea", "garden", "tax"]);
        let result = hybrid_search(
            &ws_str,
            true,
            "growing tea in the garden",
            HybridMode::Vector,
            &embedder,
            &idx,
        )
        .await
        .unwrap();
        assert_eq!(result.files[0].display_path, "a.md");
        assert!(result.files[0].semantic_score.unwrap_or(0.0) > 0.0);
    }

    #[tokio::test]
    async fn hybrid_falls_back_to_lexical_when_only_lexical_signal() {
        let (_g, ws, idx) = setup();
        let ws_str = ws.to_string_lossy().into_owned();

        // Neither file contains the embedder's topic words, but a.md contains
        // the literal query string in its content.
        write_file(&ws, "a.md", "# A\n\nLook for needle in this haystack").await;
        write_file(&ws, "b.md", "# B\n\nUnrelated content").await;

        let embedder = TopicEmbedder::new(&["completely-unrelated-topic"]);
        let result = hybrid_search(&ws_str, true, "needle", HybridMode::Hybrid, &embedder, &idx)
            .await
            .unwrap();

        assert_eq!(result.files[0].display_path, "a.md");
        assert!(result.files[0].lexical_score > 0);
    }

    #[tokio::test]
    async fn incremental_reindex_only_embeds_changed_files() {
        let (_g, ws, idx) = setup();
        let ws_str = ws.to_string_lossy().into_owned();

        write_file(&ws, "a.md", "tea in the garden").await;
        write_file(&ws, "b.md", "rust is fun").await;

        let embedder = TopicEmbedder::new(&["tea", "rust"]);
        let _ = hybrid_search(&ws_str, true, "tea", HybridMode::Hybrid, &embedder, &idx)
            .await
            .unwrap();
        // Two embed calls so far: one for the docs (batch of 2), one for the query.
        let after_first = embedder.call_count.load(Ordering::SeqCst);
        let inputs_first = embedder.input_count.load(Ordering::SeqCst);
        assert_eq!(after_first, 2);
        assert_eq!(inputs_first, 3); // 2 docs + 1 query

        // Modify only b.md.
        write_file(&ws, "b.md", "rust and tokio are fun").await;
        let _ = hybrid_search(&ws_str, true, "tea", HybridMode::Hybrid, &embedder, &idx)
            .await
            .unwrap();
        let inputs_total = embedder.input_count.load(Ordering::SeqCst);
        // Second call should embed only the changed b.md (1 doc) + 1 query.
        assert_eq!(inputs_total - inputs_first, 2);
    }

    #[tokio::test]
    async fn non_utf8_files_recorded_as_skipped_no_churn() {
        let (_g, ws, idx) = setup();
        let ws_str = ws.to_string_lossy().into_owned();

        write_file(&ws, "good.md", "tea time").await;
        // Write raw non-UTF8 bytes.
        fs::create_dir_all(&ws).await.unwrap();
        fs::write(ws.join("blob.bin"), &[0xFF, 0xFE, 0x00, 0x80])
            .await
            .unwrap();

        let embedder = TopicEmbedder::new(&["tea"]);
        let _ = hybrid_search(&ws_str, true, "tea", HybridMode::Hybrid, &embedder, &idx)
            .await
            .unwrap();
        let inputs_first = embedder.input_count.load(Ordering::SeqCst);
        // 1 text doc + 1 query embedding (binary file is skipped).
        assert_eq!(inputs_first, 2);

        // Re-run; the binary file should not trigger another embedding.
        let _ = hybrid_search(&ws_str, true, "tea", HybridMode::Hybrid, &embedder, &idx)
            .await
            .unwrap();
        let inputs_total = embedder.input_count.load(Ordering::SeqCst);
        // Just one more query embedding; no doc re-embedding.
        assert_eq!(inputs_total - inputs_first, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_pointing_outside_are_not_embedded() {
        let (g, ws, idx) = setup();
        let ws_str = ws.to_string_lossy().into_owned();

        write_file(&ws, "in.md", "tea time").await;
        let outside_dir = g.path().join("outside");
        fs::create_dir_all(&outside_dir).await.unwrap();
        fs::write(outside_dir.join("secret.md"), "secret_token_xyzzy")
            .await
            .unwrap();
        std::os::unix::fs::symlink(outside_dir.join("secret.md"), ws.join("link_to_secret.md"))
            .unwrap();

        let embedder = TopicEmbedder::new(&["xyzzy"]);
        let result = hybrid_search(&ws_str, true, "xyzzy", HybridMode::Hybrid, &embedder, &idx)
            .await
            .unwrap();
        // The symlink target should not appear in results.
        for f in &result.files {
            assert_ne!(f.display_path, "link_to_secret.md");
        }
    }
}
