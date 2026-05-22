use serde_json::Value;
use shore_config::models::Sdk;

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

    /// Whether mid-history `role: "system"` messages should be wrapped
    /// in `<system_instruction>...</system_instruction>` and emitted as
    /// user turns. True for most OpenAI-compatible providers (some
    /// OpenRouter-routed backends reject raw `role: "system"`
    /// mid-conversation); false for providers like Z.ai that accept
    /// inline system messages natively.
    ///
    /// The top-level `request.system` field is always emitted as a
    /// dedicated system message regardless of this flag — only inline
    /// system messages inside `request.messages` are affected.
    pub wrap_inline_system: bool,

    /// When true, drop prior-turn `thinking` blocks during translation
    /// instead of replaying them as `ctx.reasoning_field`. Set by Z.ai's
    /// per-call `zai_clear_thinking` provider option.
    pub drop_prior_thinking: bool,

    /// Wire shape for the reasoning-effort knob.
    /// - `"flat"`: top-level `reasoning_effort: "high"` (OpenAI native)
    /// - `"nested"`: top-level `reasoning: {effort: "high"}` (OpenRouter
    ///   shape for reasoning-capable third-party models including
    ///   Anthropic on /chat/completions)
    pub reasoning_field_shape: &'static str,

    /// Whether to emit `cache_control: {type: "ephemeral"}` extensions on
    /// the system block and the most recent user-side messages. The
    /// extension is a non-OpenAI addition that OpenRouter forwards to
    /// providers that natively understand it (Anthropic, Gemini). Models
    /// that don't honor it ignore it silently — but we only enable it
    /// where we expect a cache benefit, since the marker counts against
    /// the 4-breakpoint provider limit when honored.
    pub emit_cache_control: bool,

    /// Whether to round-trip OpenRouter's `reasoning_details` array on
    /// assistant messages. When the provider returns reasoning_details
    /// alongside a message, replaying them on subsequent turns lets the
    /// provider continue the cached prefix through adaptive-thinking
    /// turns that emit zero inline reasoning blocks.
    pub preserve_reasoning_details: bool,

    /// Whether to emit tool messages on the chat-completions wire in
    /// Anthropic-native content-block shape (assistant content array
    /// with `tool_use` blocks; user content array with `tool_result`
    /// blocks) instead of OpenAI's `role:"tool"` + `tool_calls` shape.
    ///
    /// **Load-bearing for cache continuity** through tool-loop
    /// continuations when routing Anthropic models via OpenRouter's
    /// /chat/completions endpoint. Verified empirically: the OpenAI
    /// shape rewrites the cache on every tool-loop extension (39/39
    /// trials missed); the Anthropic shape hits cache (5/5 trials hit).
    /// OpenRouter accepts the Anthropic content-block shape on
    /// /chat/completions as a non-OpenAI extension and forwards it
    /// upstream so the cache walker traverses it correctly.
    pub emit_anthropic_tool_shape: bool,
}

/// Return true when `model_id` targets an Anthropic Claude model regardless
/// of transport. Anthropic models have no native mid-history `role: "system"`
/// concept, so callers must wrap inline system messages themselves rather
/// than letting the proxy choose what to do with them.
pub(crate) fn is_anthropic_model(model_id: &str) -> bool {
    let id = model_id.to_ascii_lowercase();
    // Common forms:
    //   "anthropic/claude-sonnet-4-6"  (OpenRouter prefix)
    //   "claude-sonnet-4-6"             (bare)
    //   "claude-3-7-sonnet-20250219"    (date-suffixed)
    id.starts_with("anthropic/") || id.starts_with("claude-")
}

/// Return true when the request is aimed at OpenRouter even if the config
/// entry uses a custom static `[chat.*]` provider name with OpenRouter's URL.
pub(crate) fn is_openrouter_request(request: &LlmRequest) -> bool {
    request.provider_key.as_deref() == Some("openrouter")
        || request.base_url.as_deref().is_some_and(|base| {
            reqwest::Url::parse(base)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .is_some_and(|host| host.eq_ignore_ascii_case("openrouter.ai"))
        })
}

fn has_adaptive_anthropic_effort(request: &LlmRequest) -> bool {
    opt_str(request, "reasoning_effort")
        .map(|s| s.to_ascii_lowercase())
        .is_some_and(|s| {
            matches!(
                s.as_str(),
                "adaptive" | "max" | "xhigh" | "high" | "medium" | "low"
            )
        })
}

/// OpenRouter's Anthropic `/messages` route can return a bare adaptive
/// `tool_use` response with no signed/replayable thinking block. The next
/// tool-result continuation then rewrites the message cache. Its
/// `/chat/completions` route supplies `reasoning_details` for the same
/// adaptive flow; `openai.rs` round-trips that metadata and uses Anthropic
/// tool content blocks for cache continuity.
pub(crate) fn route_anthropic_sdk_via_openrouter_chat(request: &LlmRequest) -> bool {
    request.sdk == Sdk::Anthropic
        && is_openrouter_request(request)
        && is_anthropic_model(&request.model)
        && has_adaptive_anthropic_effort(request)
}

/// Build a `ProviderContext` from the request's `provider_key` and
/// `provider_options`.  This is the **single place** where provider-specific
/// decisions are made — SDK modules never branch on provider identity.
/// JSON field name OpenAI-compatible providers use for reasoning/thinking
/// content. Providers returning `"reasoning_content"` are also the ones
/// whose thinking-mode API requires that field on the way back in — keep
/// the two views in sync via this single source of truth.
pub(crate) fn reasoning_field_for(provider_key: &str) -> &'static str {
    match provider_key {
        "deepseek" | "moonshot" | "zai" => "reasoning_content",
        _ => "reasoning",
    }
}

pub(crate) fn build_provider_context(request: &LlmRequest) -> ProviderContext {
    let pk = if is_openrouter_request(request) {
        "openrouter"
    } else {
        request
            .provider_key
            .as_deref()
            .unwrap_or(request.sdk.as_str())
    };

    let reasoning_field = reasoning_field_for(pk);

    let mut extra_headers = Vec::new();
    if pk == "openrouter" {
        if let Some(referer) = opt_str(request, "http_referer") {
            extra_headers.push(("HTTP-Referer".into(), referer.into()));
        }
        if let Some(title) = opt_str(request, "x_title") {
            extra_headers.push(("X-Title".into(), title.into()));
        }
    }

    let supports_reasoning_effort = matches!(
        pk,
        "deepseek" | "moonshot" | "openrouter" | "xai" | "openai"
    );

    let routing_config = if pk == "openrouter" {
        build_routing_config(request)
    } else {
        None
    };

    let images_via_chat_completions = pk == "openrouter";

    // Wrap mid-history `role: "system"` messages when the target model has
    // no native concept of one (Anthropic) or when the provider rejects them
    // (most OpenAI-compatible backends). Z.ai accepts raw `role: "system"`
    // mid-conversation. Anthropic models routed through OpenRouter's
    // /chat/completions also need wrapping — the proxy re-orders inline
    // system blocks relative to chat history if we send them raw.
    let targets_anthropic = is_anthropic_model(&request.model);
    let wrap_inline_system = pk != "zai" || targets_anthropic;

    // Z.ai's `zai_clear_thinking` flag — when set, prior `thinking` blocks
    // are dropped during translation instead of being replayed as
    // `reasoning_content`. No other provider opts in today.
    let drop_prior_thinking = pk == "zai"
        && request
            .provider_options
            .as_ref()
            .and_then(|opts| opts.get("zai_clear_thinking"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

    // OpenRouter accepts the OpenAI-flat `reasoning_effort` field but its
    // canonical shape for reasoning-capable third-party models is
    // `reasoning: {effort: ...}`. Use nested when going through OpenRouter
    // to a non-OpenAI model, flat everywhere else.
    let reasoning_field_shape = if pk == "openrouter" && !request.model.starts_with("openai/") {
        "nested"
    } else {
        "flat"
    };

    // Cache-control extensions and reasoning_details round-trip are
    // OpenRouter-specific affordances that only pay off for providers
    // that honor them upstream. Today: Anthropic via OpenRouter. Other
    // model families can opt in here as they're validated.
    let emit_cache_control = pk == "openrouter" && targets_anthropic;
    let preserve_reasoning_details = pk == "openrouter" && targets_anthropic;
    let emit_anthropic_tool_shape = pk == "openrouter" && targets_anthropic;

    ProviderContext {
        reasoning_field,
        extra_headers,
        supports_reasoning_effort,
        routing_config,
        images_via_chat_completions,
        wrap_inline_system,
        drop_prior_thinking,
        reasoning_field_shape,
        emit_cache_control,
        preserve_reasoning_details,
        emit_anthropic_tool_shape,
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
            api_key_name: None,
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
            forensic_character: None,
            system_suffix: None,
            retain_long: false,
        }
    }

    #[test]
    fn deepseek_uses_reasoning_content_field() {
        let req = make_request(Sdk::Openai, Some("deepseek"));
        let ctx = build_provider_context(&req);
        assert_eq!(ctx.reasoning_field, "reasoning_content");
    }

    #[test]
    fn moonshot_uses_reasoning_content_field() {
        let req = make_request(Sdk::Openai, Some("moonshot"));
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
        assert_eq!(
            ctx.extra_headers[0],
            ("HTTP-Referer".to_string(), "https://shore.ai".to_string())
        );
        assert_eq!(
            ctx.extra_headers[1],
            ("X-Title".to_string(), "Shore".to_string())
        );
    }

    #[test]
    fn non_openrouter_gets_no_extra_headers() {
        let req = make_request(Sdk::Openai, Some("openai"));
        let ctx = build_provider_context(&req);
        assert!(ctx.extra_headers.is_empty());
    }

    #[test]
    fn reasoning_effort_supported_providers() {
        for pk in &["deepseek", "moonshot", "openrouter", "xai", "openai"] {
            let req = make_request(Sdk::Openai, Some(pk));
            let ctx = build_provider_context(&req);
            assert!(
                ctx.supports_reasoning_effort,
                "{pk} should support reasoning_effort"
            );
        }
    }

    #[test]
    fn reasoning_effort_unsupported_providers() {
        for pk in &["nanogpt", "custom-provider"] {
            let req = make_request(Sdk::Openai, Some(pk));
            let ctx = build_provider_context(&req);
            assert!(
                !ctx.supports_reasoning_effort,
                "{pk} should not support reasoning_effort"
            );
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

    #[test]
    fn openrouter_base_url_activates_openrouter_context_for_static_model() {
        let mut req = make_request(Sdk::Anthropic, Some("custom"));
        req.model = "anthropic/claude-sonnet-4-6".into();
        req.base_url = Some("https://openrouter.ai/api/v1".into());
        req.provider_options = Some(json!({
            "reasoning_effort": "high",
            "openrouter_provider": {"order": ["Anthropic"]}
        }));

        let ctx = build_provider_context(&req);
        assert!(ctx.emit_cache_control);
        assert!(ctx.preserve_reasoning_details);
        assert!(ctx.emit_anthropic_tool_shape);
        assert_eq!(ctx.reasoning_field_shape, "nested");
        assert_eq!(ctx.routing_config.unwrap()["order"], json!(["Anthropic"]));
    }

    #[test]
    fn adaptive_anthropic_openrouter_messages_route_uses_chat_completions() {
        let mut req = make_request(Sdk::Anthropic, Some("openrouter"));
        req.model = "anthropic/claude-sonnet-4-6".into();
        req.provider_options = Some(json!({"reasoning_effort": "high"}));
        assert!(route_anthropic_sdk_via_openrouter_chat(&req));

        req.provider_key = Some("anthropic".into());
        req.base_url = Some("https://api.anthropic.com".into());
        assert!(!route_anthropic_sdk_via_openrouter_chat(&req));
    }

    #[test]
    fn openrouter_anthropic_without_adaptive_effort_keeps_native_sdk_route() {
        let mut req = make_request(Sdk::Anthropic, Some("openrouter"));
        req.model = "anthropic/claude-sonnet-4-6".into();
        req.provider_options = Some(json!({"cache_ttl": "1h"}));
        assert!(!route_anthropic_sdk_via_openrouter_chat(&req));
    }

    #[test]
    fn adaptive_effort_detection_is_case_insensitive() {
        // Config values for reasoning_effort can come from user-typed
        // strings; "High", "HIGH", "Adaptive" must all activate the
        // chat-completions route, not silently fall back to the cache-
        // breaking /messages path.
        for variant in ["HIGH", "High", "hIgH", "ADAPTIVE", "Max"] {
            let mut req = make_request(Sdk::Anthropic, Some("openrouter"));
            req.model = "anthropic/claude-sonnet-4-6".into();
            req.provider_options = Some(json!({"reasoning_effort": variant}));
            assert!(
                route_anthropic_sdk_via_openrouter_chat(&req),
                "reasoning_effort={variant:?} must be accepted case-insensitively",
            );
        }
    }
}
