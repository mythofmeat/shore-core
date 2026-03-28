pub mod providers;
pub mod retry;
pub mod stream;
pub mod types;

use std::path::PathBuf;

use chrono::Utc;
use tokio::io::{BufReader, DuplexStream};
use tracing::{debug, warn};

use shore_config::models::ResolvedModel;
use types::{ImageGenerateResponse, LlmRequest};

/// Errors from the LLM client.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),

    #[error("HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },

    #[error("failed to serialize request: {0}")]
    Serialize(#[source] serde_json::Error),

    #[error("failed to parse response: {0}")]
    Deserialize(#[source] serde_json::Error),

    #[error("stream ended without done event")]
    IncompleteStream,

    #[error("API key environment variable {var} is not set")]
    MissingApiKey { var: String },

    #[error("provider error: {message}")]
    Provider { message: String },

    #[error("model refusal detected")]
    Refusal,
}

/// HTTP client that calls LLM provider APIs directly.
///
/// Uses reqwest for connection pooling and TLS session reuse.
/// Each request is fully self-contained with provider, model, and API key.
#[derive(Debug, Clone)]
pub struct LlmClient {
    http_client: reqwest::Client,
    /// If set, API payloads are logged to `{dir}/api_payloads.jsonl`.
    payload_log_dir: Option<PathBuf>,
}

impl LlmClient {
    /// Create a new LLM client with a shared reqwest connection pool.
    pub fn new() -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("failed to create HTTP client");

        Self {
            http_client,
            payload_log_dir: None,
        }
    }

    /// Enable API payload logging to the given directory.
    pub fn with_payload_logging(mut self, dir: PathBuf) -> Self {
        self.payload_log_dir = Some(dir);
        self
    }

    /// Set the payload log directory.
    pub fn set_payload_log_dir(&mut self, dir: PathBuf) {
        self.payload_log_dir = Some(dir);
    }

    /// Build an `LlmRequest` from a resolved model profile and conversation state.
    ///
    /// Resolves the API key from the environment variable specified in the model
    /// profile. Returns `LlmError::MissingApiKey` if the variable is not set.
    pub fn build_request(
        model: &ResolvedModel,
        messages: Vec<serde_json::Value>,
        system: Option<serde_json::Value>,
        tools: Option<Vec<serde_json::Value>>,
        provider_options: Option<serde_json::Value>,
    ) -> Result<LlmRequest, LlmError> {
        let api_key_env = model
            .api_key_env
            .as_deref()
            .unwrap_or(default_api_key_env(&model.provider_key));

        let api_key =
            std::env::var(api_key_env).map_err(|_| LlmError::MissingApiKey {
                var: api_key_env.to_string(),
            })?;

        // Build provider_options from V1-style fields if not explicitly provided.
        let opts = provider_options.unwrap_or_else(|| {
            let mut map = serde_json::Map::new();
            if let Some(ref effort) = model.reasoning_effort {
                map.insert("reasoning_effort".into(), serde_json::json!(effort));
            }
            if let Some(budget) = model.budget_tokens {
                map.insert("budget_tokens".into(), serde_json::json!(budget));
            }
            if let Some(ref ttl) = model.cache_ttl {
                map.insert("cache_ttl".into(), serde_json::json!(ttl));
            }
            if let Some(ref or_provider) = model.openrouter_provider {
                map.insert("openrouter_provider".into(), serde_json::json!(or_provider.to_string()));
            }
            if let Some(ref project) = model.vertex_project {
                map.insert("vertex_project".into(), serde_json::json!(project));
            }
            if let Some(ref location) = model.vertex_location {
                map.insert("vertex_location".into(), serde_json::json!(location));
            }
            if let Some(gen) = model.gemini_generation {
                map.insert("gemini_generation".into(), serde_json::json!(gen));
            }
            if let Some(ws) = model.gemini_web_search {
                map.insert("gemini_web_search".into(), serde_json::json!(ws));
            }
            if map.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::Object(map)
            }
        });

        let provider_options = if opts.is_null() { None } else { Some(opts) };

        Ok(LlmRequest {
            provider: model.sdk.as_provider_str().to_string(),
            model: model.model_id.clone(),
            api_key,
            base_url: model.base_url.clone(),
            messages,
            system,
            tools,
            max_tokens: model.max_tokens.unwrap_or(4096),
            temperature: model.temperature,
            top_p: model.top_p,
            provider_options,
            provider_key: Some(model.provider_key.clone()),
        })
    }

    /// Send a streaming completion request to the LLM provider.
    ///
    /// Returns a `BufReader` over a DuplexStream that yields NDJSON `StreamEvent`
    /// lines for consumption by the stream consumer.
    pub async fn stream_raw(
        &self,
        request: &LlmRequest,
        _rid: Option<&str>,
    ) -> Result<BufReader<DuplexStream>, LlmError> {
        let body =
            serde_json::to_string(request).map_err(LlmError::Serialize)?;
        self.log_payload("request", &body);

        debug!(
            provider = %request.provider,
            model = %request.model,
            "Sending streaming request to provider"
        );

        let read_half = providers::stream(&self.http_client, request).await?;
        Ok(BufReader::new(read_half))
    }

    /// Send a non-streaming completion request to the LLM provider.
    pub async fn generate(
        &self,
        request: &LlmRequest,
        _rid: Option<&str>,
    ) -> Result<types::GenerateResponse, LlmError> {
        let body =
            serde_json::to_string(request).map_err(LlmError::Serialize)?;
        self.log_payload("request", &body);

        let resp = providers::generate(&self.http_client, request).await?;
        Ok(resp)
    }

    /// Send an embedding request to the provider.
    pub async fn embed(
        &self,
        provider: &str,
        model: &str,
        api_key: &str,
        base_url: Option<&str>,
        input: &[&str],
    ) -> Result<Vec<Vec<f32>>, LlmError> {
        providers::embed(&self.http_client, provider, model, api_key, base_url, input).await
    }

    /// Send an image generation request to the provider.
    pub async fn image_generate(
        &self,
        provider: &str,
        model: &str,
        api_key: &str,
        base_url: Option<&str>,
        prompt: &str,
        size: Option<&str>,
        quality: Option<&str>,
        aspect_ratio: Option<&str>,
        image_size: Option<&str>,
    ) -> Result<ImageGenerateResponse, LlmError> {
        providers::image_generate(
            &self.http_client, provider, model, api_key, base_url, prompt,
            size, quality, aspect_ratio, image_size,
        )
        .await
    }

    /// Log an API payload to `{payload_log_dir}/api_payloads.jsonl` if logging is enabled.
    fn log_payload(&self, direction: &str, payload: &str) {
        let Some(dir) = &self.payload_log_dir else {
            return;
        };
        let path = dir.join("api_payloads.jsonl");
        let ts = Utc::now().to_rfc3339();
        // Redact api_key from request payloads.
        let sanitized = if direction == "request" {
            if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(payload) {
                if let Some(obj) = v.as_object_mut() {
                    if obj.contains_key("api_key") {
                        obj.insert("api_key".into(), serde_json::json!("[REDACTED]"));
                    }
                }
                serde_json::to_string(&v).unwrap_or_else(|_| payload.to_string())
            } else {
                payload.to_string()
            }
        } else {
            payload.to_string()
        };
        let line = serde_json::json!({
            "ts": ts,
            "direction": direction,
            "payload": serde_json::from_str::<serde_json::Value>(&sanitized)
                .unwrap_or_else(|_| serde_json::Value::String(sanitized)),
        });
        if let Err(e) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "{}", line)
            })
        {
            warn!(error = %e, path = %path.display(), "Failed to write API payload log");
        }
    }
}

impl Default for LlmClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Return the conventional API key env var name for a provider key.
fn default_api_key_env(provider_key: &str) -> &str {
    match provider_key {
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "gemini" => "GEMINI_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        "zhipuai" => "ZHIPUAI_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        "xai" => "XAI_API_KEY",
        _ => "LLM_API_KEY",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shore_config::models::{ResolvedModel, Sdk};

    /// Helper to build a minimal test ResolvedModel.
    fn test_model(name: &str, provider_key: &str, sdk: Sdk) -> ResolvedModel {
        ResolvedModel {
            name: name.into(),
            qualified_name: format!("chat.{provider_key}.{name}"),
            category: "chat".into(),
            provider_key: provider_key.into(),
            sdk,
            model_id: "claude-test".into(),
            api_key_env: None,
            base_url: None,
            max_context_tokens: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            reasoning_effort: None,
            budget_tokens: None,
            cache_ttl: None,
            keepalive_enabled: None,
            keepalive_ttl_minutes: None,
            keepalive_max_pings: None,
            openrouter_provider: None,
            vertex_project: None,
            vertex_location: None,
            gemini_generation: None,
            gemini_web_search: None,
        }
    }

    #[test]
    fn build_request_resolves_api_key() {
        std::env::set_var("TEST_API_KEY_015", "sk-test-key");

        let mut model = test_model("test-model", "anthropic", Sdk::Anthropic);
        model.max_tokens = Some(2048);
        model.temperature = Some(0.5);
        model.api_key_env = Some("TEST_API_KEY_015".into());

        let req = LlmClient::build_request(
            &model,
            vec![serde_json::json!({"role": "user", "content": "Hi"})],
            None,
            None,
            None,
        )
        .unwrap();

        assert_eq!(req.provider, "anthropic");
        assert_eq!(req.model, "claude-test");
        assert_eq!(req.api_key, "sk-test-key");
        assert_eq!(req.max_tokens, 2048);
        assert_eq!(req.temperature, Some(0.5));
        assert!(req.base_url.is_none());
        assert_eq!(req.provider_key.as_deref(), Some("anthropic"));

        std::env::remove_var("TEST_API_KEY_015");
    }

    #[test]
    fn build_request_uses_default_api_key_env() {
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");

        let model = test_model("test", "anthropic", Sdk::Anthropic);

        let req = LlmClient::build_request(
            &model,
            vec![],
            None,
            None,
            None,
        )
        .unwrap();

        assert_eq!(req.api_key, "sk-ant-test");
        assert_eq!(req.max_tokens, 4096);

        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[test]
    fn build_request_errors_on_missing_api_key() {
        std::env::remove_var("NONEXISTENT_KEY_015");

        let mut model = test_model("test", "anthropic", Sdk::Anthropic);
        model.api_key_env = Some("NONEXISTENT_KEY_015".into());

        let err = LlmClient::build_request(&model, vec![], None, None, None)
            .unwrap_err();
        match err {
            LlmError::MissingApiKey { var } => {
                assert_eq!(var, "NONEXISTENT_KEY_015");
            }
            other => panic!("Expected MissingApiKey, got {:?}", other),
        }
    }

    #[test]
    fn default_api_key_env_for_providers() {
        assert_eq!(default_api_key_env("anthropic"), "ANTHROPIC_API_KEY");
        assert_eq!(default_api_key_env("openai"), "OPENAI_API_KEY");
        assert_eq!(default_api_key_env("gemini"), "GEMINI_API_KEY");
        assert_eq!(default_api_key_env("openrouter"), "OPENROUTER_API_KEY");
        assert_eq!(default_api_key_env("zhipuai"), "ZHIPUAI_API_KEY");
        assert_eq!(default_api_key_env("deepseek"), "DEEPSEEK_API_KEY");
        assert_eq!(default_api_key_env("xai"), "XAI_API_KEY");
        assert_eq!(default_api_key_env("unknown"), "LLM_API_KEY");
    }

    #[test]
    fn client_is_clone_and_send() {
        fn assert_clone_send<T: Clone + Send>() {}
        assert_clone_send::<LlmClient>();
    }
}
