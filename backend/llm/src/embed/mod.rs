//! Embedding model abstraction.
//!
//! [`Embedder`] is dyn-compatible so call sites can hold an
//! `Arc<dyn Embedder>` chosen at startup from config without each consumer
//! knowing the provider shape. The shipped impl is OpenAI-compatible,
//! covering OpenAI, Together, Voyage's compat endpoint, OpenRouter, and
//! any self-hosted server that speaks the same shape (e.g.
//! text-embedding-inference, llama.cpp's `/v1/embeddings`).

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use dashmap::DashMap;

use crate::LlmError;

/// Vector embedding provider.
///
/// `model_id` identifies the model that produced a vector — index entries
/// embed it so a model swap invalidates cached vectors.
#[async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, inputs: &[&str]) -> Result<Vec<Vec<f32>>, LlmError>;
    fn model_id(&self) -> &str;
    fn dimensions(&self) -> usize;
}

/// Hosted OpenAI-compatible embedder (`/v1/embeddings`).
///
/// Works with any provider that speaks the OpenAI embeddings shape
/// (OpenAI itself, Together, Voyage's compat endpoint, OpenRouter, etc.).
pub struct OpenAIEmbedder {
    http_client: reqwest::Client,
    model: String,
    api_key: String,
    base_url: Option<String>,
    dimensions: usize,
}

impl OpenAIEmbedder {
    pub fn new(
        http_client: reqwest::Client,
        model: impl Into<String>,
        api_key: impl Into<String>,
        base_url: Option<String>,
        dimensions: usize,
    ) -> Self {
        Self {
            http_client,
            model: model.into(),
            api_key: api_key.into(),
            base_url,
            dimensions,
        }
    }
}

#[async_trait]
impl Embedder for OpenAIEmbedder {
    async fn embed(&self, inputs: &[&str]) -> Result<Vec<Vec<f32>>, LlmError> {
        crate::providers::embed(
            &self.http_client,
            "openai",
            &self.model,
            &self.api_key,
            self.base_url.as_deref(),
            inputs,
        )
        .await
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

/// Process-wide cache so an `Arc<dyn Embedder>` is loaded once and shared
/// across requests, characters, and heartbeat ticks.
///
/// Keyed by an opaque string the caller chooses — typically
/// `"<provider>::<model_id>"` — so a config swap to a different model
/// produces a new entry rather than reusing the old one.
fn embedder_cache() -> &'static DashMap<String, Arc<dyn Embedder>> {
    static CACHE: OnceLock<DashMap<String, Arc<dyn Embedder>>> = OnceLock::new();
    CACHE.get_or_init(DashMap::new)
}

/// Look up an embedder by `key`; if absent, run `build` and cache the
/// result. Subsequent callers get the same `Arc` clone.
pub fn cache_or_build<F, E>(key: &str, build: F) -> Result<Arc<dyn Embedder>, E>
where
    F: FnOnce() -> Result<Arc<dyn Embedder>, E>,
{
    if let Some(e) = embedder_cache().get(key) {
        return Ok(Arc::clone(e.value()));
    }
    let new = build()?;
    embedder_cache().insert(key.to_string(), Arc::clone(&new));
    Ok(new)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeEmbedder {
        dim: usize,
    }

    #[async_trait]
    impl Embedder for FakeEmbedder {
        async fn embed(&self, inputs: &[&str]) -> Result<Vec<Vec<f32>>, LlmError> {
            Ok(inputs
                .iter()
                .map(|s| {
                    let mut v = vec![0.0_f32; self.dim];
                    if !s.is_empty() {
                        let h = s.bytes().fold(0u32, |a, b| a.wrapping_add(b as u32));
                        v[(h as usize) % self.dim] = 1.0;
                    }
                    v
                })
                .collect())
        }
        fn model_id(&self) -> &str {
            "fake"
        }
        fn dimensions(&self) -> usize {
            self.dim
        }
    }

    #[tokio::test]
    async fn dyn_embedder_round_trip() {
        let e: Box<dyn Embedder> = Box::new(FakeEmbedder { dim: 4 });
        let out = e.embed(&["a", "b"]).await.unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].len(), 4);
        assert_eq!(e.model_id(), "fake");
        assert_eq!(e.dimensions(), 4);
    }
}
