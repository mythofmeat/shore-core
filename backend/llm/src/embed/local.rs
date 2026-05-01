//! Local ONNX-runtime embedder via [fastembed-rs].
//!
//! Models download once into `$XDG_CACHE_HOME/shore/models/` and then run
//! offline. Default is BGE-small (384 dims, ~33MB).
//!
//! `TextEmbedding::embed` needs `&mut self`, so the inner model lives behind
//! a `Mutex` and runs inside `spawn_blocking` to keep the async runtime free.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use tracing::info;

use super::Embedder;
use crate::LlmError;

#[derive(Debug, thiserror::Error)]
pub enum LocalEmbedderError {
    #[error("unknown local embedding model: {0}")]
    UnknownModel(String),
    #[error("could not determine model cache directory")]
    CacheDir,
    #[error("could not create model cache directory {path}: {source}")]
    CacheDirCreate {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("model load failed: {0}")]
    Load(String),
}

pub struct LocalEmbedder {
    model: Arc<Mutex<TextEmbedding>>,
    model_id: String,
    dimensions: usize,
}

impl LocalEmbedder {
    /// Construct a local embedder from a stable model id.
    ///
    /// First call may download model weights into the shared cache dir; later
    /// calls find them on disk and skip the network entirely.
    pub fn try_new(model_id: &str) -> Result<Self, LocalEmbedderError> {
        let (model_enum, dim) = resolve_model(model_id)?;
        let cache_dir = cache_dir()?;
        if !cache_dir.exists() {
            std::fs::create_dir_all(&cache_dir).map_err(|e| {
                LocalEmbedderError::CacheDirCreate {
                    path: cache_dir.clone(),
                    source: e,
                }
            })?;
        }

        info!(
            model = model_id,
            cache_dir = %cache_dir.display(),
            "loading local embedding model"
        );

        let options = TextInitOptions::new(model_enum)
            .with_cache_dir(cache_dir)
            .with_show_download_progress(false);
        let model =
            TextEmbedding::try_new(options).map_err(|e| LocalEmbedderError::Load(e.to_string()))?;

        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            model_id: model_id.to_string(),
            dimensions: dim,
        })
    }
}

#[async_trait]
impl Embedder for LocalEmbedder {
    async fn embed(&self, inputs: &[&str]) -> Result<Vec<Vec<f32>>, LlmError> {
        let owned: Vec<String> = inputs.iter().map(|s| s.to_string()).collect();
        let model = Arc::clone(&self.model);

        tokio::task::spawn_blocking(move || -> Result<Vec<Vec<f32>>, String> {
            let mut guard = model
                .lock()
                .map_err(|_| "local embedder mutex poisoned".to_string())?;
            guard.embed(owned, None).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| LlmError::Provider {
            message: format!("local embedder task join failed: {e}"),
        })?
        .map_err(|e| LlmError::Provider {
            message: format!("local embedding failed: {e}"),
        })
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

fn resolve_model(id: &str) -> Result<(EmbeddingModel, usize), LocalEmbedderError> {
    match id {
        "bge-small-en-v1.5" => Ok((EmbeddingModel::BGESmallENV15, 384)),
        "bge-base-en-v1.5" => Ok((EmbeddingModel::BGEBaseENV15, 768)),
        "bge-large-en-v1.5" => Ok((EmbeddingModel::BGELargeENV15, 1024)),
        "all-minilm-l6-v2" => Ok((EmbeddingModel::AllMiniLML6V2, 384)),
        "nomic-embed-text-v1.5" => Ok((EmbeddingModel::NomicEmbedTextV15, 768)),
        other => Err(LocalEmbedderError::UnknownModel(other.to_string())),
    }
}

fn cache_dir() -> Result<PathBuf, LocalEmbedderError> {
    dirs::cache_dir()
        .map(|p| p.join("shore").join("models"))
        .ok_or(LocalEmbedderError::CacheDir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_model_id_errors() {
        match LocalEmbedder::try_new("not-a-real-model") {
            Err(LocalEmbedderError::UnknownModel(name)) => assert_eq!(name, "not-a-real-model"),
            Err(other) => panic!("expected UnknownModel error, got {other:?}"),
            Ok(_) => panic!("expected error for unknown model"),
        }
    }

    #[test]
    fn cache_dir_under_shore_models() {
        let dir = cache_dir().expect("cache dir resolved");
        assert!(dir.ends_with("shore/models"));
    }

    // Real model load + embed is gated behind `--ignored` because it
    // hits the network on first run and downloads ~30MB of weights.
    #[tokio::test]
    #[ignore = "downloads model weights on first run"]
    async fn bge_small_embeds_two_inputs() {
        let e = LocalEmbedder::try_new("bge-small-en-v1.5").expect("model loads");
        assert_eq!(e.dimensions(), 384);
        let out = e
            .embed(&["the cat sat on the mat", "rust is a systems language"])
            .await
            .expect("embed succeeds");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].len(), 384);
        assert_eq!(out[1].len(), 384);
        // Different inputs should produce different vectors.
        assert!(out[0]
            .iter()
            .zip(out[1].iter())
            .any(|(a, b)| (a - b).abs() > 1e-3));
    }
}
