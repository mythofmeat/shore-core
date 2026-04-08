use serde_json::Value;

use crate::types::LlmRequest;

/// Provider-specific behavior hints, computed from `provider_key` before
/// the SDK module runs.  SDK modules read these fields instead of
/// inspecting `provider_key` directly.
pub(crate) struct ProviderContext {
    /// JSON field name for reasoning/thinking content in OpenAI-compatible
    /// responses.  `"reasoning"` for most providers, `"reasoning_content"`
    /// for DeepSeek.
    pub reasoning_field: &'static str,

    /// Extra HTTP headers to add to every request (e.g. OpenRouter's
    /// `HTTP-Referer` and `X-Title`).
    pub extra_headers: Vec<(String, String)>,

    /// Whether `reasoning_effort` should be included in the request body.
    pub supports_reasoning_effort: bool,

    /// Optional provider routing object to inject into the body
    /// (OpenRouter's `provider` field with order/allow_fallbacks).
    pub routing_config: Option<Value>,

    /// Whether image generation should use chat completions with
    /// `modalities` (OpenRouter) instead of `/images/generations`.
    /// Note: currently used by `ImageGenerateParams.provider_key` check
    /// in `openai::image_generate` — will be wired through once image gen
    /// is refactored to use LlmRequest dispatch.
    #[allow(dead_code)]
    pub images_via_chat_completions: bool,
}

/// Build a `ProviderContext` from the request's `provider_key` and
/// `provider_options`.  This is the **single place** where provider-specific
/// decisions are made — SDK modules never branch on provider identity.
pub(crate) fn build_provider_context(request: &LlmRequest) -> ProviderContext {
    let pk = request
        .provider_key
        .as_deref()
        .unwrap_or(request.sdk.as_str());

    let reasoning_field = match pk {
        "deepseek" => "reasoning_content",
        _ => "reasoning",
    };

    let mut extra_headers = Vec::new();
    if pk == "openrouter" {
        if let Some(referer) = opt_str(request, "http_referer") {
            extra_headers.push(("HTTP-Referer".into(), referer.into()));
        }
        if let Some(title) = opt_str(request, "x_title") {
            extra_headers.push(("X-Title".into(), title.into()));
        }
    }

    let supports_reasoning_effort =
        matches!(pk, "deepseek" | "openrouter" | "xai" | "openai");

    let routing_config = if pk == "openrouter" {
        build_routing_config(request)
    } else {
        None
    };

    let images_via_chat_completions = pk == "openrouter";

    ProviderContext {
        reasoning_field,
        extra_headers,
        supports_reasoning_effort,
        routing_config,
        images_via_chat_completions,
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Extract a string value from `provider_options`.
fn opt_str<'a>(request: &'a LlmRequest, key: &str) -> Option<&'a str> {
    request
        .provider_options
        .as_ref()
        .and_then(|opts| opts.get(key))
        .and_then(|v| v.as_str())
}

/// Build the OpenRouter `provider` routing object from provider_options.
///
/// When `order` is specified, defaults `allow_fallbacks` to `false` so
/// OpenRouter actually respects the preferred provider list.
fn build_routing_config(request: &LlmRequest) -> Option<Value> {
    let or_provider = request
        .provider_options
        .as_ref()
        .and_then(|opts| opts.get("openrouter_provider"))?;

    let mut provider = or_provider.clone();
    if let Some(obj) = provider.as_object_mut() {
        if obj.contains_key("order") {
            obj.entry("allow_fallbacks".to_string())
                .or_insert(serde_json::json!(false));
        }
    }
    Some(provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use shore_config::models::Sdk;

    fn make_request(sdk: Sdk, provider_key: Option<&str>) -> LlmRequest {
        LlmRequest {
            sdk,
            model: "test".into(),
            api_key: "sk-test".into(),
            base_url: None,
            messages: vec![],
            system: None,
            tools: None,
            max_tokens: 4096,
            temperature: None,
            top_p: None,
            provider_options: None,
            provider_key: provider_key.map(String::from),
            rid: None,
        }
    }

    #[test]
    fn deepseek_uses_reasoning_content_field() {
        let req = make_request(Sdk::Openai, Some("deepseek"));
        let ctx = build_provider_context(&req);
        assert_eq!(ctx.reasoning_field, "reasoning_content");
    }

    #[test]
    fn non_deepseek_uses_reasoning_field() {
        let req = make_request(Sdk::Openai, Some("openai"));
        let ctx = build_provider_context(&req);
        assert_eq!(ctx.reasoning_field, "reasoning");
    }

    #[test]
    fn openrouter_gets_extra_headers() {
        let mut req = make_request(Sdk::Openai, Some("openrouter"));
        req.provider_options = Some(json!({
            "http_referer": "https://shore.ai",
            "x_title": "Shore"
        }));
        let ctx = build_provider_context(&req);
        assert_eq!(ctx.extra_headers.len(), 2);
        assert_eq!(ctx.extra_headers[0], ("HTTP-Referer".to_string(), "https://shore.ai".to_string()));
        assert_eq!(ctx.extra_headers[1], ("X-Title".to_string(), "Shore".to_string()));
    }

    #[test]
    fn non_openrouter_gets_no_extra_headers() {
        let req = make_request(Sdk::Openai, Some("openai"));
        let ctx = build_provider_context(&req);
        assert!(ctx.extra_headers.is_empty());
    }

    #[test]
    fn reasoning_effort_supported_providers() {
        for pk in &["deepseek", "openrouter", "xai", "openai"] {
            let req = make_request(Sdk::Openai, Some(pk));
            let ctx = build_provider_context(&req);
            assert!(ctx.supports_reasoning_effort, "{pk} should support reasoning_effort");
        }
    }

    #[test]
    fn reasoning_effort_unsupported_providers() {
        for pk in &["nanogpt", "custom-provider"] {
            let req = make_request(Sdk::Openai, Some(pk));
            let ctx = build_provider_context(&req);
            assert!(!ctx.supports_reasoning_effort, "{pk} should not support reasoning_effort");
        }
    }

    #[test]
    fn openrouter_routing_config_with_order() {
        let mut req = make_request(Sdk::Openai, Some("openrouter"));
        req.provider_options = Some(json!({
            "openrouter_provider": {
                "order": ["anthropic"]
            }
        }));
        let ctx = build_provider_context(&req);
        let routing = ctx.routing_config.unwrap();
        assert_eq!(routing["order"], json!(["anthropic"]));
        assert_eq!(routing["allow_fallbacks"], false);
    }

    #[test]
    fn openrouter_images_via_chat_completions() {
        let req = make_request(Sdk::Openai, Some("openrouter"));
        let ctx = build_provider_context(&req);
        assert!(ctx.images_via_chat_completions);

        let req2 = make_request(Sdk::Openai, Some("openai"));
        let ctx2 = build_provider_context(&req2);
        assert!(!ctx2.images_via_chat_completions);
    }

    #[test]
    fn falls_back_to_sdk_name_when_no_provider_key() {
        // When provider_key is None, should use sdk.as_str() as fallback
        let req = make_request(Sdk::Openai, None);
        let ctx = build_provider_context(&req);
        // "openai" supports reasoning_effort
        assert!(ctx.supports_reasoning_effort);
        assert_eq!(ctx.reasoning_field, "reasoning");
    }

    #[test]
    fn anthropic_sdk_with_openrouter_provider_key() {
        // The key new use case: Anthropic SDK routed through OpenRouter
        let mut req = make_request(Sdk::Anthropic, Some("openrouter"));
        req.provider_options = Some(json!({
            "http_referer": "https://shore.ai"
        }));
        let ctx = build_provider_context(&req);
        // Should get OpenRouter headers even though SDK is Anthropic
        assert_eq!(ctx.extra_headers.len(), 1);
        assert!(ctx.images_via_chat_completions);
    }
}
