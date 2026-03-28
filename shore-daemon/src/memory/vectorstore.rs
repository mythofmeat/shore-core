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

#[derive(Debug)]
pub enum VectorStoreError {
    Lance(lancedb::error::Error),
    Arrow(arrow_schema::ArrowError),
    Embed(String),
    Io(std::io::Error),
}

impl From<lancedb::error::Error> for VectorStoreError {
    fn from(e: lancedb::error::Error) -> Self {
        VectorStoreError::Lance(e)
    }
}

impl From<arrow_schema::ArrowError> for VectorStoreError {
    fn from(e: arrow_schema::ArrowError) -> Self {
        VectorStoreError::Arrow(e)
    }
}

impl std::fmt::Display for VectorStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VectorStoreError::Lance(e) => write!(f, "lancedb: {e}"),
            VectorStoreError::Arrow(e) => write!(f, "arrow: {e}"),
            VectorStoreError::Embed(e) => write!(f, "embed: {e}"),
            VectorStoreError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for VectorStoreError {}

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
    /// Default path: `$XDG_DATA_HOME/shore/{character}/memory/vectorstore/`
    pub fn default_path(character: &str) -> PathBuf {
        let data_dir = dirs::data_dir().unwrap_or_else(|| PathBuf::from(".local/share"));
        data_dir
            .join("shore")
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
}
