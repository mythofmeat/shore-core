//! Embedder resolution from config, with a process-wide cache.
//!
//! The hybrid retrieval pipeline lives in
//! [`crate::memory::workspace_index`]. This file used to also host a
//! memory-only retrieval implementation (`search_memory`) — that was
//! superseded by the workspace-wide hybrid path and removed.
//!
//! There is no bundled local embedder. Callers configure an
//! OpenAI-compatible endpoint (hosted or self-hosted, e.g.
//! text-embedding-inference, llama.cpp's `/v1/embeddings`); when nothing is
//! configured, hybrid search degrades to lexical-only at the call site.

use std::collections::BTreeMap;
use std::sync::Arc;

use shore_llm::embed::{Embedder, OpenAIEmbedder};

/// Build (or fetch from the process-wide cache) the configured embedder.
///
/// Returns `Err` when no `[embedding.<name>]` profile is configured (or
/// `defaults.embedding` doesn't reference one). Callers degrade hybrid
/// search to lexical mode in that case.
pub fn resolve_embedder(
    default_name: Option<&str>,
    embedding_catalog: &BTreeMap<String, toml::Value>,
    http_client: &reqwest::Client,
) -> Result<Arc<dyn Embedder>, String> {
    let profile_name = default_name
        .or_else(|| embedding_catalog.keys().next().map(String::as_str))
        .ok_or_else(|| {
            "no embedding profile configured; semantic search disabled. \
             Add an [embedding.<name>] block pointing at an OpenAI-compatible \
             embeddings endpoint (see CONFIGURATION.md)."
                .to_string()
        })?;
    let entry = embedding_catalog.get(profile_name).ok_or_else(|| {
        format!(
            "embedding profile '{profile_name}' is not declared; add an \
             [embedding.{profile_name}] block to your config"
        )
    })?;

    let provider = entry
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("openai");
    if provider == "local" {
        return Err(format!(
            "embedding profile '{profile_name}' uses provider = \"local\", \
             which is no longer supported. Run an OpenAI-compatible \
             embeddings server yourself (e.g. text-embedding-inference, \
             llama.cpp server) and point base_url at it."
        ));
    }
    let model_id = entry
        .get("model_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("embedding profile '{profile_name}' is missing model_id"))?;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve_error(
        default_name: Option<&str>,
        catalog: &BTreeMap<String, toml::Value>,
        http: &reqwest::Client,
    ) -> String {
        match resolve_embedder(default_name, catalog, http) {
            Ok(_) => panic!("resolve_embedder unexpectedly succeeded"),
            Err(err) => err,
        }
    }

    #[test]
    fn empty_catalog_no_default_returns_clear_error() {
        let catalog: BTreeMap<String, toml::Value> = BTreeMap::new();
        let http = reqwest::Client::new();
        let err = resolve_error(None, &catalog, &http);
        assert!(
            err.contains("no embedding profile configured"),
            "expected unconfigured-error, got: {err}"
        );
    }

    #[test]
    fn unknown_default_name_errors_clearly() {
        let catalog: BTreeMap<String, toml::Value> = BTreeMap::new();
        let http = reqwest::Client::new();
        let err = resolve_error(Some("not-a-profile"), &catalog, &http);
        assert!(
            err.contains("not-a-profile"),
            "error should name the missing profile: {err}"
        );
        assert!(
            err.contains("not declared"),
            "error should explain the profile is undeclared: {err}"
        );
    }

    #[test]
    fn local_provider_returns_migration_error() {
        let mut catalog: BTreeMap<String, toml::Value> = BTreeMap::new();
        let entry: toml::Value =
            toml::from_str("provider = \"local\"\nmodel_id = \"bge-small-en-v1.5\"\n").unwrap();
        catalog.insert("x".into(), entry);
        let http = reqwest::Client::new();
        let err = resolve_error(Some("x"), &catalog, &http);
        assert!(
            err.contains("no longer supported"),
            "expected migration error, got: {err}"
        );
    }
}
