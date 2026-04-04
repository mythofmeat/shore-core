pub(crate) mod anthropic;
pub(crate) mod gemini;
pub(crate) mod openai;
pub(crate) mod sse;
pub(crate) mod stream_helpers;
pub(crate) mod zai;

use crate::types::{GenerateResponse, ImageGenerateResponse, LlmRequest};
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

/// Dispatch a streaming request to the correct provider.
///
/// Returns the read half of a DuplexStream that yields NDJSON `StreamEvent` lines.
/// A background task reads SSE from the provider and writes NDJSON to the stream.
pub async fn stream(
    client: &reqwest::Client,
    request: &LlmRequest,
) -> Result<DuplexStream, LlmError> {
    match request.provider.as_str() {
        "anthropic" => anthropic::stream(client, request).await,
        "openai" | "deepseek" | "zhipuai" | "xai" | "nanogpt" => {
            openai::stream(client, request).await
        }
        "zai" => zai::stream(client, request).await,
        "gemini" => gemini::stream(client, request).await,
        other => Err(LlmError::Provider {
            message: format!("unsupported provider: {other}"),
        }),
    }
}

/// Dispatch a non-streaming generate request to the correct provider.
pub async fn generate(
    client: &reqwest::Client,
    request: &LlmRequest,
) -> Result<GenerateResponse, LlmError> {
    match request.provider.as_str() {
        "anthropic" => anthropic::generate(client, request).await,
        "openai" | "deepseek" | "zhipuai" | "xai" | "nanogpt" => {
            openai::generate(client, request).await
        }
        "zai" => zai::generate(client, request).await,
        "gemini" => gemini::generate(client, request).await,
        other => Err(LlmError::Provider {
            message: format!("unsupported provider: {other}"),
        }),
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
    openai::image_generate(
        client,
        provider,
        model,
        api_key,
        base_url,
        prompt,
        size,
        quality,
        aspect_ratio,
        image_size,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_request(provider: &str) -> LlmRequest {
        LlmRequest {
            provider: provider.into(),
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
    async fn stream_unsupported_provider_returns_error() {
        let client = reqwest::Client::new();
        let request = make_request("unsupported");
        let result = stream(&client, &request).await;
        match result {
            Err(LlmError::Provider { message }) => {
                assert!(message.contains("unsupported provider: unsupported"));
            }
            other => panic!("expected Provider error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn generate_unsupported_provider_returns_error() {
        let client = reqwest::Client::new();
        let request = make_request("unsupported");
        let result = generate(&client, &request).await;
        match result {
            Err(LlmError::Provider { message }) => {
                assert!(message.contains("unsupported provider: unsupported"));
            }
            other => panic!("expected Provider error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_known_providers_do_not_hit_unsupported_error() {
        let client = reqwest::Client::new();
        // These should route to real provider impls and fail on HTTP, not on
        // the "unsupported provider" dispatch branch.
        for provider in &[
            "anthropic",
            "openai",
            "deepseek",
            "zhipuai",
            "xai",
            "zai",
            "nanogpt",
            "gemini",
        ] {
            let request = make_request(provider);
            let result = stream(&client, &request).await;
            match &result {
                Err(LlmError::Provider { message }) => {
                    assert!(
                        !message.contains("unsupported provider"),
                        "{provider} should not hit unsupported provider path, got: {message}"
                    );
                }
                _ => {} // Any other error (HTTP, connection) or Ok is fine.
            }
        }
    }
}
