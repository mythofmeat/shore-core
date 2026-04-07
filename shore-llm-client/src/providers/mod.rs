pub(crate) mod anthropic;
pub(crate) mod context;
pub(crate) mod gemini;
pub(crate) mod openai;
pub(crate) mod sse;
pub(crate) mod stream_helpers;
pub(crate) mod zai;

use shore_config::models::Sdk;

use crate::types::{GenerateResponse, ImageGenerateParams, ImageGenerateResponse, LlmRequest};
use crate::LlmError;
use tokio::io::DuplexStream;

/// Check an HTTP response status, returning the response on success or
/// an `HttpStatus` error with the body text on failure.
pub(crate) async fn check_response(
    response: reqwest::Response,
) -> Result<reqwest::Response, LlmError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let status_code = status.as_u16();
    let body = response.text().await.unwrap_or_default();
    Err(LlmError::HttpStatus {
        status: status_code,
        body,
    })
}

/// Dispatch a streaming request to the correct SDK.
///
/// Returns the read half of a DuplexStream that yields NDJSON `StreamEvent` lines.
/// A background task reads SSE from the provider and writes NDJSON to the stream.
pub async fn stream(
    client: &reqwest::Client,
    request: &LlmRequest,
) -> Result<DuplexStream, LlmError> {
    let ctx = context::build_provider_context(request);
    match request.sdk {
        Sdk::Anthropic => anthropic::stream(client, request).await,
        Sdk::Openai => openai::stream(client, request, &ctx).await,
        Sdk::Zai => zai::stream(client, request).await,
        Sdk::Gemini => gemini::stream(client, request).await,
    }
}

/// Dispatch a non-streaming generate request to the correct SDK.
pub async fn generate(
    client: &reqwest::Client,
    request: &LlmRequest,
) -> Result<GenerateResponse, LlmError> {
    let ctx = context::build_provider_context(request);
    match request.sdk {
        Sdk::Anthropic => anthropic::generate(client, request).await,
        Sdk::Openai => openai::generate(client, request, &ctx).await,
        Sdk::Zai => zai::generate(client, request).await,
        Sdk::Gemini => gemini::generate(client, request).await,
    }
}

/// Dispatch an embedding request.
pub async fn embed(
    client: &reqwest::Client,
    provider: &str,
    model: &str,
    api_key: &str,
    base_url: Option<&str>,
    input: &[&str],
) -> Result<Vec<Vec<f32>>, LlmError> {
    // Embeddings are currently only supported via OpenAI-compatible API.
    openai::embed(client, provider, model, api_key, base_url, input).await
}

/// Dispatch an image generation request.
pub async fn image_generate(
    client: &reqwest::Client,
    params: &ImageGenerateParams<'_>,
) -> Result<ImageGenerateResponse, LlmError> {
    openai::image_generate(client, params).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_request(sdk: Sdk) -> LlmRequest {
        LlmRequest {
            sdk,
            model: "test-model".into(),
            api_key: "sk-test".into(),
            base_url: Some("http://127.0.0.1:1".into()), // unreachable
            messages: vec![json!({"role": "user", "content": "hi"})],
            system: None,
            tools: None,
            max_tokens: 100,
            temperature: None,
            top_p: None,
            provider_options: None,
            provider_key: None,
        }
    }

    #[tokio::test]
    async fn stream_all_sdks_dispatch_without_panic() {
        let client = reqwest::Client::new();
        // All SDK variants should route to real impls and fail on HTTP
        // (unreachable base_url), not panic.
        for sdk in [Sdk::Anthropic, Sdk::Openai, Sdk::Zai, Sdk::Gemini] {
            let request = make_request(sdk);
            let result = stream(&client, &request).await;
            // Any error is fine (HTTP, connection) — we just confirm dispatch works.
            assert!(result.is_err(), "expected connection error for unreachable host");
        }
    }
}
