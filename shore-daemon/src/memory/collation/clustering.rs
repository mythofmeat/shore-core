use crate::memory::db::Entry;
use crate::memory::vectorstore::VectorStore;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum entries per cluster sent to the LLM.
pub(super) const MAX_CLUSTER_SIZE: usize = 15;

/// Minimum cosine similarity to consider two entries related.
pub(super) const SIMILARITY_THRESHOLD: f32 = 0.3;

// ---------------------------------------------------------------------------
// Clustering
// ---------------------------------------------------------------------------

/// Group candidate entries into clusters of semantically related entries.
/// Uses existing embeddings from the vector store for in-memory cosine
/// similarity. Falls back to a single batch if no vector store is available
/// or if entries lack embeddings.
pub(super) async fn cluster_candidates(
    candidates: &[&Entry],
    vector_store: Option<&VectorStore>,
) -> Vec<Vec<Entry>> {
    // If few enough candidates, no need to cluster.
    if candidates.len() <= MAX_CLUSTER_SIZE {
        return vec![candidates.iter().map(|e| (*e).clone()).collect()];
    }

    // Try to get embeddings from vector store.
    if let Some(vs) = vector_store {
        let ids: Vec<&str> = candidates.iter().map(|e| e.id.as_str()).collect();
        if let Ok(embeddings) = vs.get_embeddings(&ids).await {
            // Only cluster if we have embeddings for a meaningful fraction.
            let coverage = embeddings.len() as f32 / candidates.len() as f32;
            if coverage >= 0.5 {
                return cluster_by_embeddings(candidates, &embeddings);
            }
        }
    }

    // Fallback: chunk into batches of MAX_CLUSTER_SIZE.
    candidates
        .chunks(MAX_CLUSTER_SIZE)
        .map(|chunk| chunk.iter().map(|e| (*e).clone()).collect())
        .collect()
}

/// Greedy clustering using cosine similarity of pre-computed embeddings.
fn cluster_by_embeddings(
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
            .filter(|&(_, sim)| sim >= SIMILARITY_THRESHOLD)
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
                if !clustered[j] && cluster.len() < MAX_CLUSTER_SIZE {
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

    for chunk in unclustered.chunks(MAX_CLUSTER_SIZE) {
        clusters.push(chunk.to_vec());
    }

    clusters
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

/// Compute the centroid (element-wise average) of a set of embeddings.
pub(super) fn compute_centroid(embeddings: &HashMap<String, Vec<f32>>) -> Option<Vec<f32>> {
    if embeddings.is_empty() {
        return None;
    }
    let dim = embeddings.values().next()?.len();
    let mut centroid = vec![0.0f32; dim];
    for emb in embeddings.values() {
        for (i, v) in emb.iter().enumerate() {
            centroid[i] += v;
        }
    }
    let n = embeddings.len() as f32;
    for v in &mut centroid {
        *v /= n;
    }
    Some(centroid)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

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
    fn test_compute_centroid() {
        let mut embs = HashMap::new();
        embs.insert("a".to_string(), vec![1.0, 0.0, 0.0]);
        embs.insert("b".to_string(), vec![0.0, 1.0, 0.0]);
        let centroid = compute_centroid(&embs).unwrap();
        assert!((centroid[0] - 0.5).abs() < 1e-6);
        assert!((centroid[1] - 0.5).abs() < 1e-6);
        assert!((centroid[2] - 0.0).abs() < 1e-6);

        let empty: HashMap<String, Vec<f32>> = HashMap::new();
        assert!(compute_centroid(&empty).is_none());
    }

    #[test]
    fn test_cluster_by_embeddings_groups_similar() {
        let now = now_str();
        let entries: Vec<Entry> = (0..6)
            .map(|i| make_entry(&format!("e{i}"), &format!("Entry {i}"), 0.8, &now))
            .collect();
        let entry_refs: Vec<&Entry> = entries.iter().collect();

        let mut embeddings = HashMap::new();
        embeddings.insert("e0".to_string(), vec![0.9, 0.1, 0.0, 0.0]);
        embeddings.insert("e1".to_string(), vec![0.8, 0.2, 0.0, 0.0]);
        embeddings.insert("e2".to_string(), vec![0.85, 0.15, 0.0, 0.0]);
        embeddings.insert("e3".to_string(), vec![0.0, 0.0, 0.9, 0.1]);
        embeddings.insert("e4".to_string(), vec![0.0, 0.0, 0.8, 0.2]);
        embeddings.insert("e5".to_string(), vec![0.0, 0.0, 0.85, 0.15]);

        let clusters = cluster_by_embeddings(&entry_refs, &embeddings);

        assert_eq!(clusters.len(), 2);
        let mut sizes: Vec<usize> = clusters.iter().map(|c| c.len()).collect();
        sizes.sort();
        assert_eq!(sizes, vec![3, 3]);

        for cluster in &clusters {
            let ids: Vec<&str> = cluster.iter().map(|e| e.id.as_str()).collect();
            let all_food = ids.iter().all(|id| ["e0", "e1", "e2"].contains(id));
            let all_tech = ids.iter().all(|id| ["e3", "e4", "e5"].contains(id));
            assert!(all_food || all_tech, "Cluster should be homogeneous, got: {:?}", ids);
        }
    }

    #[tokio::test]
    async fn test_cluster_candidates_small_set_no_clustering() {
        let now = now_str();
        let entries: Vec<Entry> = (0..5)
            .map(|i| make_entry(&format!("e{i}"), &format!("Entry {i}"), 0.8, &now))
            .collect();
        let entry_refs: Vec<&Entry> = entries.iter().collect();

        let clusters = cluster_candidates(&entry_refs, None).await;
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].len(), 5);
    }

    #[tokio::test]
    async fn test_cluster_candidates_no_vectorstore_chunks() {
        let now = now_str();
        let entries: Vec<Entry> = (0..30)
            .map(|i| make_entry(&format!("e{i}"), &format!("Entry {i}"), 0.8, &now))
            .collect();
        let entry_refs: Vec<&Entry> = entries.iter().collect();

        let clusters = cluster_candidates(&entry_refs, None).await;
        assert_eq!(clusters.len(), 2);
        assert_eq!(clusters[0].len(), 15);
        assert_eq!(clusters[1].len(), 15);
    }
}
