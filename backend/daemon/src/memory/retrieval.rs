//! Embedder resolution from config, with a process-wide cache.
//!
//! The hybrid retrieval pipeline lives in
//! [`crate::memory::workspace_index`]. This file used to also host a
//! memory-only retrieval implementation (`search_memory`) — that was
//! superseded by the workspace-wide hybrid path and removed.

use std::collections::BTreeMap;
use std::sync::Arc;

use shore_llm::embed::{Embedder, OpenAIEmbedder};

/// Default local model when no embedding profile is configured.
const DEFAULT_LOCAL_MODEL_ID: &str = "bge-small-en-v1.5";

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

    match provider {
        "local" => {
            let cache_key = format!("local::{model_id}");
            shore_llm::embed::cache_or_build(&cache_key, || build_local(model_id))
        }
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
            let cache_key = format!(
                "{provider}::{model_id}::{api_key_env}::{}::{dimensions}",
                base_url.as_deref().unwrap_or("default")
            );
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
