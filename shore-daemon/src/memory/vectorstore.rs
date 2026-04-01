use arrow_array::{types::Float32Type, FixedSizeListArray, Float32Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::arrow::SendableRecordBatchStream;
use lancedb::query::{ExecutableQuery, QueryBase};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const TABLE_NAME: &str = "vectors";

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum VectorStoreError {
    #[error("lancedb: {0}")]
    Lance(#[from] lancedb::error::Error),
    #[error("arrow: {0}")]
    Arrow(#[from] arrow_schema::ArrowError),
    #[error("embed: {0}")]
    Embed(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Result type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub entry_id: String,
    /// Similarity score in [0, 1]. Higher is more similar.
    pub score: f32,
}

// ---------------------------------------------------------------------------
// VectorStore
// ---------------------------------------------------------------------------

pub struct VectorStore {
    db: lancedb::Connection,
    dimension: i32,
}

impl VectorStore {
    /// Default path: `$SHORE_DATA_DIR/{character}/memory/vectorstore/`
    pub fn default_path(character: &str) -> PathBuf {
        shore_config::data_dir()
            .join(character)
            .join("memory")
            .join("vectorstore")
    }

    /// Open or create the vector store at the given directory.
    pub async fn open(path: &Path, dimension: i32) -> Result<Self, VectorStoreError> {
        std::fs::create_dir_all(path).map_err(VectorStoreError::Io)?;
        let uri = path.to_str().ok_or_else(|| {
            VectorStoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path is not valid UTF-8",
            ))
        })?;
        let db = lancedb::connect(uri).execute().await?;
        Ok(Self { db, dimension })
    }

    /// Store a pre-computed embedding for the given entry.
    /// If the entry already exists, it is replaced.
    pub async fn index_entry(
        &self,
        entry_id: &str,
        embedding: &[f32],
    ) -> Result<(), VectorStoreError> {
        let batch = self.make_batch(&[(entry_id, embedding)])?;

        match self.db.open_table(TABLE_NAME).execute().await {
            Ok(table) => {
                // Remove stale vector for this entry, then insert new one.
                let _ = table.delete(&format!("entry_id = '{entry_id}'")).await;
                table.add(vec![batch]).execute().await?;
            }
            Err(_) => {
                // Table does not exist yet — create it.
                self.db
                    .create_table(TABLE_NAME, vec![batch])
                    .execute()
                    .await?;
            }
        }
        Ok(())
    }

    /// Find the top-K nearest neighbors for a query embedding.
    /// Returns entry IDs with similarity scores (higher = more similar).
    pub async fn search(
        &self,
        query_embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<SearchResult>, VectorStoreError> {
        let table = match self.db.open_table(TABLE_NAME).execute().await {
            Ok(t) => t,
            Err(_) => return Ok(vec![]),
        };

        let mut stream: SendableRecordBatchStream = table
            .query()
            .nearest_to(query_embedding.to_vec())?
            .limit(top_k)
            .execute()
            .await?;

        let mut results = Vec::new();
        while let Some(rb) = stream.try_next().await? {
            let ids: &StringArray = rb
                .column_by_name("entry_id")
                .expect("missing entry_id column")
                .as_any()
                .downcast_ref()
                .expect("entry_id not StringArray");
            let dists: &Float32Array = rb
                .column_by_name("_distance")
                .expect("missing _distance column")
                .as_any()
                .downcast_ref()
                .expect("_distance not Float32Array");

            for i in 0..rb.num_rows() {
                let distance = dists.value(i);
                results.push(SearchResult {
                    entry_id: ids.value(i).to_string(),
                    // Convert L2 distance to similarity: 1/(1+d)
                    score: 1.0 / (1.0 + distance),
                });
            }
        }

        Ok(results)
    }

    /// Rebuild the entire index from the given entries.
    /// Drops the existing table and creates a fresh one.
    pub async fn reindex(
        &self,
        entries: &[(&str, &[f32])],
    ) -> Result<(), VectorStoreError> {
        let _ = self.db.drop_table(TABLE_NAME, &[]).await;

        if entries.is_empty() {
            return Ok(());
        }

        let batch = self.make_batch(entries)?;
        self.db
            .create_table(TABLE_NAME, vec![batch])
            .execute()
            .await?;

        Ok(())
    }

    /// Retrieve stored embeddings for a set of entry IDs.
    /// Returns a map of entry_id -> embedding for entries that exist in the store.
    /// Entries not in the store are silently omitted.
    pub async fn get_embeddings(
        &self,
        entry_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, Vec<f32>>, VectorStoreError> {
        let mut result = std::collections::HashMap::new();
        if entry_ids.is_empty() {
            return Ok(result);
        }

        let table = match self.db.open_table(TABLE_NAME).execute().await {
            Ok(t) => t,
            Err(_) => return Ok(result),
        };

        // Build a SQL filter for the entry IDs.
        let id_list: Vec<String> = entry_ids.iter().map(|id| format!("'{id}'")).collect();
        let filter = format!("entry_id IN ({})", id_list.join(", "));

        let mut stream: SendableRecordBatchStream = table
            .query()
            .only_if(filter)
            .execute()
            .await?;

        while let Some(rb) = stream.try_next().await? {
            let ids: &StringArray = rb
                .column_by_name("entry_id")
                .expect("missing entry_id column")
                .as_any()
                .downcast_ref()
                .expect("entry_id not StringArray");
            let vectors: &FixedSizeListArray = rb
                .column_by_name("vector")
                .expect("missing vector column")
                .as_any()
                .downcast_ref()
                .expect("vector not FixedSizeListArray");

            for i in 0..rb.num_rows() {
                let entry_id = ids.value(i).to_string();
                let vec_array = vectors.value(i);
                let float_array: &Float32Array = vec_array
                    .as_any()
                    .downcast_ref()
                    .expect("vector values not Float32Array");
                let embedding: Vec<f32> = (0..float_array.len())
                    .map(|j| float_array.value(j))
                    .collect();
                result.insert(entry_id, embedding);
            }
        }

        Ok(result)
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn table_schema(&self) -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("entry_id", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    self.dimension,
                ),
                false,
            ),
        ]))
    }

    fn make_batch(&self, entries: &[(&str, &[f32])]) -> Result<RecordBatch, VectorStoreError> {
        // Validate embedding dimensions before building the arrow batch.
        // Mismatched dimensions cause a panic in FixedSizeListBuilder.
        for (id, vec) in entries {
            if vec.len() != self.dimension as usize {
                return Err(VectorStoreError::Embed(format!(
                    "embedding for '{}' has {} dimensions, expected {}",
                    id,
                    vec.len(),
                    self.dimension
                )));
            }
        }

        let schema = self.table_schema();
        let ids: Vec<&str> = entries.iter().map(|(id, _)| *id).collect();
        let vectors: Vec<Option<Vec<Option<f32>>>> = entries
            .iter()
            .map(|(_, vec)| Some(vec.iter().map(|v| Some(*v)).collect()))
            .collect();

        let id_array = Arc::new(StringArray::from(ids)) as _;
        let vector_array = Arc::new(
            FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(vectors, self.dimension),
        ) as _;

        RecordBatch::try_new(schema, vec![id_array, vector_array]).map_err(Into::into)
    }
}

// ---------------------------------------------------------------------------
// Embedding helper — calls shore-llm POST /v1/embed
// ---------------------------------------------------------------------------

/// Call the shore-llm `/v1/embed` endpoint to generate embeddings.
pub async fn embed_text(
    base_url: &str,
    provider: &str,
    model: &str,
    api_key: &str,
    input: &[&str],
) -> Result<Vec<Vec<f32>>, VectorStoreError> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "provider": provider,
        "model": model,
        "api_key": api_key,
        "input": input,
    });

    let resp = client
        .post(format!("{base_url}/v1/embed"))
        .json(&body)
        .send()
        .await
        .map_err(|e| VectorStoreError::Embed(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(VectorStoreError::Embed(format!(
            "embed endpoint returned {}",
            resp.status()
        )));
    }

    #[derive(serde::Deserialize)]
    struct EmbedResponse {
        embeddings: Vec<Vec<f32>>,
    }

    let data: EmbedResponse = resp
        .json()
        .await
        .map_err(|e| VectorStoreError::Embed(e.to_string()))?;

    Ok(data.embeddings)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    async fn open_temp_store(dir: &Path, dim: i32) -> VectorStore {
        VectorStore::open(dir, dim).await.unwrap()
    }

    #[tokio::test]
    async fn test_index_and_search() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = open_temp_store(tmp.path(), 4).await;

        // Index two entries with orthogonal vectors.
        store
            .index_entry("e1", &[1.0, 0.0, 0.0, 0.0])
            .await
            .unwrap();
        store
            .index_entry("e2", &[0.0, 1.0, 0.0, 0.0])
            .await
            .unwrap();

        // Search near e1 — should return e1 first.
        let results = store.search(&[0.9, 0.1, 0.0, 0.0], 2).await.unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].entry_id, "e1");
        assert!(results[0].score > results[1].score);
    }

    #[tokio::test]
    async fn test_index_entry_replaces_existing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = open_temp_store(tmp.path(), 4).await;

        store
            .index_entry("e1", &[1.0, 0.0, 0.0, 0.0])
            .await
            .unwrap();

        // Re-index e1 with a different vector.
        store
            .index_entry("e1", &[0.0, 0.0, 0.0, 1.0])
            .await
            .unwrap();

        // Search should find e1 near the NEW vector, not the old one.
        let results = store.search(&[0.0, 0.0, 0.0, 1.0], 1).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry_id, "e1");
        // High similarity because query == stored vector.
        assert!(results[0].score > 0.9);
    }

    #[tokio::test]
    async fn test_search_empty_store() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = open_temp_store(tmp.path(), 4).await;

        let results = store.search(&[1.0, 0.0, 0.0, 0.0], 5).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_reindex_rebuilds_from_scratch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = open_temp_store(tmp.path(), 4).await;

        // Index an entry, then reindex with different data.
        store
            .index_entry("old", &[1.0, 0.0, 0.0, 0.0])
            .await
            .unwrap();

        store
            .reindex(&[
                ("a", &[1.0, 0.0, 0.0, 0.0][..]),
                ("b", &[0.0, 1.0, 0.0, 0.0][..]),
                ("c", &[0.0, 0.0, 1.0, 0.0][..]),
            ])
            .await
            .unwrap();

        // "old" should no longer exist.
        let results = store.search(&[1.0, 0.0, 0.0, 0.0], 10).await.unwrap();
        let ids: Vec<&str> = results.iter().map(|r| r.entry_id.as_str()).collect();
        assert!(!ids.contains(&"old"));
        assert!(ids.contains(&"a"));
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn test_reindex_empty_clears_index() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = open_temp_store(tmp.path(), 4).await;

        store
            .index_entry("e1", &[1.0, 0.0, 0.0, 0.0])
            .await
            .unwrap();

        store.reindex(&[]).await.unwrap();

        let results = store.search(&[1.0, 0.0, 0.0, 0.0], 5).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_default_path() {
        let path = VectorStore::default_path("test-char");
        assert!(path.ends_with("shore/test-char/memory/vectorstore"));
    }

    #[tokio::test]
    async fn test_dimension_mismatch_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = open_temp_store(tmp.path(), 4).await;

        // Try to index a 3-dimensional vector into a 4-dimensional store.
        let result = store.index_entry("e1", &[1.0, 0.0, 0.0]).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("3 dimensions"), "error should mention actual dimensions: {err}");
        assert!(err.contains("expected 4"), "error should mention expected dimensions: {err}");
    }

    #[tokio::test]
    async fn test_get_embeddings() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = open_temp_store(tmp.path(), 4).await;

        store.index_entry("e1", &[1.0, 0.0, 0.0, 0.0]).await.unwrap();
        store.index_entry("e2", &[0.0, 1.0, 0.0, 0.0]).await.unwrap();
        store.index_entry("e3", &[0.0, 0.0, 1.0, 0.0]).await.unwrap();

        // Retrieve a subset.
        let result = store.get_embeddings(&["e1", "e3"]).await.unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.contains_key("e1"));
        assert!(result.contains_key("e3"));
        assert!(!result.contains_key("e2"));

        // Verify actual values.
        assert_eq!(result["e1"], vec![1.0, 0.0, 0.0, 0.0]);
        assert_eq!(result["e3"], vec![0.0, 0.0, 1.0, 0.0]);
    }

    #[tokio::test]
    async fn test_get_embeddings_missing_ids() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = open_temp_store(tmp.path(), 4).await;

        store.index_entry("e1", &[1.0, 0.0, 0.0, 0.0]).await.unwrap();

        // Request includes non-existent ID.
        let result = store.get_embeddings(&["e1", "nonexistent"]).await.unwrap();
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("e1"));
    }

    #[tokio::test]
    async fn test_get_embeddings_empty_store() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = open_temp_store(tmp.path(), 4).await;

        let result = store.get_embeddings(&["e1"]).await.unwrap();
        assert!(result.is_empty());
    }
}
