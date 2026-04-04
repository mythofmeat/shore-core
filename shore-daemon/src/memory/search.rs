use std::collections::{HashMap, HashSet};

const K1: f64 = 1.2;
const B: f64 = 0.75;

// ---------------------------------------------------------------------------
// Result type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Bm25Result {
    pub entry_id: String,
    pub score: f64,
}

// ---------------------------------------------------------------------------
// BM25 Index
// ---------------------------------------------------------------------------

pub struct Bm25Index {
    /// entry_id → list of tokens
    documents: HashMap<String, Vec<String>>,
    /// term → set of entry_ids containing the term
    doc_freq: HashMap<String, HashSet<String>>,
    /// Total token count across all documents (for average doc length).
    total_tokens: usize,
}

impl Default for Bm25Index {
    fn default() -> Self {
        Self::new()
    }
}

impl Bm25Index {
    pub fn new() -> Self {
        Self {
            documents: HashMap::new(),
            doc_freq: HashMap::new(),
            total_tokens: 0,
        }
    }

    pub fn add_document(&mut self, entry_id: &str, text: &str) {
        // Remove existing document first (idempotent upsert).
        self.remove_document(entry_id);

        let tokens = tokenize(text);
        self.total_tokens += tokens.len();

        let unique_terms: HashSet<&str> = tokens.iter().map(|s| s.as_str()).collect();
        for term in unique_terms {
            self.doc_freq
                .entry(term.to_string())
                .or_default()
                .insert(entry_id.to_string());
        }

        self.documents.insert(entry_id.to_string(), tokens);
    }

    pub fn remove_document(&mut self, entry_id: &str) {
        if let Some(tokens) = self.documents.remove(entry_id) {
            self.total_tokens -= tokens.len();

            let unique_terms: HashSet<&str> = tokens.iter().map(|s| s.as_str()).collect();
            for term in unique_terms {
                if let Some(set) = self.doc_freq.get_mut(term) {
                    set.remove(entry_id);
                    if set.is_empty() {
                        self.doc_freq.remove(term);
                    }
                }
            }
        }
    }

    pub fn search(&self, query: &str, top_k: usize) -> Vec<Bm25Result> {
        if self.documents.is_empty() {
            return vec![];
        }

        let query_terms = tokenize(query);
        let n = self.documents.len() as f64;
        let avgdl = self.total_tokens as f64 / n;

        let mut scores: HashMap<&str, f64> = HashMap::new();

        for term in &query_terms {
            let df = self
                .doc_freq
                .get(term.as_str())
                .map(|s| s.len())
                .unwrap_or(0) as f64;

            if df == 0.0 {
                continue;
            }

            // IDF: log((N - df + 0.5) / (df + 0.5) + 1)
            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();

            for (entry_id, doc_tokens) in &self.documents {
                let tf = doc_tokens.iter().filter(|t| *t == term).count() as f64;
                if tf == 0.0 {
                    continue;
                }

                let dl = doc_tokens.len() as f64;
                let tf_norm = (tf * (K1 + 1.0)) / (tf + K1 * (1.0 - B + B * dl / avgdl));

                *scores.entry(entry_id.as_str()).or_insert(0.0) += idf * tf_norm;
            }
        }

        let mut results: Vec<Bm25Result> = scores
            .into_iter()
            .map(|(entry_id, score)| Bm25Result {
                entry_id: entry_id.to_string(),
                score,
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(top_k);
        results
    }

    pub fn len(&self) -> usize {
        self.documents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_search() {
        let mut index = Bm25Index::new();
        index.add_document("e1", "the cat sat on the mat");
        index.add_document("e2", "the dog ran in the park");
        index.add_document("e3", "cats and dogs are pets");

        let results = index.search("cat", 10);
        assert!(!results.is_empty());
        // "cat" appears literally in e1 ("cat"), not in e3 ("cats" ≠ "cat")
        assert_eq!(results[0].entry_id, "e1");
    }

    #[test]
    fn test_exact_match_scores_higher_than_partial() {
        let mut index = Bm25Index::new();
        index.add_document(
            "exact",
            "machine learning is a field of artificial intelligence",
        );
        index.add_document("partial", "the machine was broken and needed repair");

        let results = index.search("machine learning", 10);
        assert!(!results.is_empty());
        // "exact" contains both "machine" AND "learning" → higher BM25 score
        assert_eq!(results[0].entry_id, "exact");
        if results.len() > 1 {
            assert!(results[0].score > results[1].score);
        }
    }

    #[test]
    fn test_empty_index_returns_empty() {
        let index = Bm25Index::new();
        let results = index.search("anything", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn test_no_match_returns_empty() {
        let mut index = Bm25Index::new();
        index.add_document("e1", "the cat sat on the mat");
        let results = index.search("xyz", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn test_remove_document() {
        let mut index = Bm25Index::new();
        index.add_document("e1", "hello world");
        index.add_document("e2", "hello there");

        index.remove_document("e1");

        let results = index.search("world", 10);
        assert!(results.is_empty());

        let results = index.search("hello", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry_id, "e2");
    }

    #[test]
    fn test_upsert_on_add() {
        let mut index = Bm25Index::new();
        index.add_document("e1", "hello world");
        index.add_document("e1", "goodbye world");

        assert_eq!(index.len(), 1);

        // Should NOT match "hello" anymore
        let results = index.search("hello", 10);
        assert!(results.is_empty());

        // Should match "goodbye"
        let results = index.search("goodbye", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry_id, "e1");
    }

    #[test]
    fn test_top_k_limits_results() {
        let mut index = Bm25Index::new();
        for i in 0..20 {
            index.add_document(&format!("e{i}"), &format!("common term document {i}"));
        }

        let results = index.search("common", 5);
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn test_term_frequency_boosts_score() {
        let mut index = Bm25Index::new();
        index.add_document("repeated", "rust rust rust is great");
        index.add_document("single", "rust is a programming language");

        let results = index.search("rust", 10);
        assert_eq!(results.len(), 2);
        // "repeated" has higher TF for "rust" → should score higher
        assert_eq!(results[0].entry_id, "repeated");
        assert!(results[0].score > results[1].score);
    }
}
