pub(crate) mod anthropic;
pub(crate) mod context;
pub(crate) mod gemini;
pub(crate) mod openai;
pub(crate) mod sse;
pub(crate) mod stream_helpers;
pub(crate) mod zai;

use shore_config::models::Sdk;
use tracing::{debug, error, warn};

use crate::types::{GenerateResponse, ImageGenerateParams, ImageGenerateResponse, LlmRequest};
use crate::LlmError;
use tokio::io::DuplexStream;

/// Truncate a string for log preview, respecting UTF-8 char boundaries.
fn body_preview(body: &str, max: usize) -> &str {
    if body.len() > max {
        &body[..body.floor_char_boundary(max)]
    } else {
        body
    }
}

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
    error!(
        status = status_code,
        body_len = body.len(),
        body_preview = %body_preview(&body, 200),
        "LLM API returned error status"
    );
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
    debug!(
        sdk = ?request.sdk,
        model = %request.model,
        max_tokens = request.max_tokens,
        message_count = request.messages.len(),
        has_tools = request.tools.is_some(),
        "dispatching streaming LLM request"
    );
    let ctx = context::build_provider_context(request);
    let result = match request.sdk {
        Sdk::Anthropic => anthropic::stream(client, request).await,
        Sdk::Openai => openai::stream(client, request, &ctx).await,
        Sdk::Zai => zai::stream(client, request).await,
        Sdk::Gemini => gemini::stream(client, request).await,
    };
    if let Err(e) = &result {
        warn!(sdk = ?request.sdk, model = %request.model, error = %e, "streaming request failed");
    }
    result
}

/// Dispatch a non-streaming generate request to the correct SDK.
pub async fn generate(
    client: &reqwest::Client,
    request: &LlmRequest,
) -> Result<GenerateResponse, LlmError> {
    debug!(
        sdk = ?request.sdk,
        model = %request.model,
        max_tokens = request.max_tokens,
        message_count = request.messages.len(),
        "dispatching non-streaming LLM request"
    );
    let ctx = context::build_provider_context(request);
    let result = match request.sdk {
        Sdk::Anthropic => anthropic::generate(client, request).await,
        Sdk::Openai => openai::generate(client, request, &ctx).await,
        Sdk::Zai => zai::generate(client, request).await,
        Sdk::Gemini => gemini::generate(client, request).await,
    };
    match &result {
        Ok(resp) => debug!(
            model = %resp.model,
            finish_reason = %resp.finish_reason,
            input_tokens = resp.usage.input_tokens,
            output_tokens = resp.usage.output_tokens,
            total_ms = resp.timing.total_ms,
            "non-streaming request completed"
        ),
        Err(e) => warn!(sdk = ?request.sdk, model = %request.model, error = %e, "non-streaming request failed"),
    }
    result
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
    debug!(provider = %provider, model = %model, input_count = input.len(), "dispatching embedding request");
    openai::embed(client, provider, model, api_key, base_url, input).await
}

/// Dispatch an image generation request.
pub async fn image_generate(
    client: &reqwest::Client,
    params: &ImageGenerateParams<'_>,
) -> Result<ImageGenerateResponse, LlmError> {
    debug!(model = %params.model, "dispatching image generation request");
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
            rid: None,
            forensic_character: None,
        }
    }

    #[test]
    fn body_preview_handles_multibyte_at_boundary() {
        // 199 ASCII bytes + "é" (2 bytes) = 201 bytes total.
        // Slicing at byte 200 lands inside "é" → must not panic.
        let body = format!("{}{}", "x".repeat(199), "é");
        assert_eq!(body.len(), 201);
        let preview = body_preview(&body, 200);
        assert!(preview.len() <= 200);
        assert!(preview.is_char_boundary(preview.len()));
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
