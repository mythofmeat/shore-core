//! Provider discovery commands: `list_providers`, `refresh_provider_models`,
//! `list_provider_models`.
//!
//! These commands are characterless — they expose runtime state about
//! configured providers and discovered models without requiring an
//! attached character session. Secrets never leave this module: only
//! key names, env-var-set booleans, and public model metadata are
//! surfaced. Refresh failures preserve whatever cache was on disk.

use serde_json::{json, Value};
use shore_protocol::error::ErrorCode;
use tracing::{info, warn};

use crate::commands::{CommandContext, CommandResult};

// ── Helpers ────────────────────────────────────────────────────────────

fn require_provider(args: &Value) -> Result<&str, (ErrorCode, String)> {
    args.get("provider")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or((
            ErrorCode::InvalidRequest,
            "missing required argument: provider".into(),
        ))
}

/// Whether the env var holding a key has a non-empty value. Returns just
/// the boolean — the value itself is never exposed.
fn env_set(var: &str) -> bool {
    std::env::var(var)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

// ── list_providers ─────────────────────────────────────────────────────

/// Return all configured providers with their public-facing metadata.
///
/// Output shape (per provider):
/// - `name`, `enabled`, `sdk`, `base_url`, `discovery_enabled`
/// - `keys`: array of `{ name, enabled, warn_on_fallback, env_set }`
/// - `cache`: `{ present: bool, models: u32, fetched_at: Option<String> }`
///
/// No env var names, no key values, nothing that could leak credentials.
pub fn list_providers(ctx: &CommandContext) -> CommandResult {
    let providers: Vec<Value> = ctx
        .config
        .providers
        .iter()
        .map(|(name, entry)| {
            let keys: Vec<Value> = entry
                .keys()
                .iter()
                .map(|k| {
                    json!({
                        "name": k.name,
                        "enabled": k.enabled,
                        "warn_on_fallback": k.warn_on_fallback,
                        "env_set": env_set(&k.env),
                    })
                })
                .collect();

            let cache_path = shore_llm::discovery::cache_path(&ctx.data_dir, name);
            let cache_summary = match shore_llm::discovery::read_cache(&cache_path) {
                Ok(Some(c)) => json!({
                    "present": true,
                    "models": c.models.len(),
                    "fetched_at": c.fetched_at,
                }),
                _ => json!({ "present": false, "models": 0, "fetched_at": Value::Null }),
            };

            json!({
                "name": name,
                "enabled": entry.enabled,
                "sdk": entry.sdk.as_ref().map(|s| s.as_str()),
                "base_url": entry.base_url,
                "discovery_enabled": entry.discovery.enabled,
                "keys": keys,
                "cache": cache_summary,
            })
        })
        .collect();

    Ok(json!({ "providers": providers }))
}

// ── refresh_provider_models ────────────────────────────────────────────

/// Fetch the provider's discovery endpoint with the first usable key,
/// then write the cache atomically. The cache is only updated on success
/// — any failure leaves the previous cache untouched.
pub async fn refresh_provider_models(ctx: &CommandContext, args: &Value) -> CommandResult {
    let provider = require_provider(args)?;

    let entry = ctx.config.providers.get(provider).ok_or((
        ErrorCode::NotFound,
        format!("provider {provider:?} is not configured"),
    ))?;

    if !entry.enabled {
        return Err((
            ErrorCode::InvalidRequest,
            format!("provider {provider:?} is disabled"),
        ));
    }
    if !entry.discovery.enabled {
        return Err((
            ErrorCode::InvalidRequest,
            format!("provider {provider:?} has discovery disabled"),
        ));
    }

    let base_url = entry.base_url.clone().ok_or((
        ErrorCode::InvalidRequest,
        format!("provider {provider:?} has no base_url; required for OpenAI-compatible discovery"),
    ))?;

    // Pick the first enabled key whose env var holds a non-empty value.
    // We do not rotate on failure here (Phase 4 fallback is reserved for
    // chat traffic); a discovery failure is reported to the caller and
    // they can fix their credentials and retry.
    let key_value = entry
        .enabled_keys()
        .find_map(|k| std::env::var(&k.env).ok().filter(|v| !v.trim().is_empty()))
        .ok_or((
            ErrorCode::ProviderError,
            format!(
                "provider {provider:?} has no API key configured (no enabled key's env var is set)"
            ),
        ))?;

    let http = ctx.llm_client.inner().http_client();
    info!(provider = %provider, base_url = %base_url, "Refreshing provider models");

    let models = match shore_llm::discovery::discover_openai_compatible(
        http, provider, &base_url, &key_value,
    )
    .await
    {
        Ok(m) => m,
        Err(e) => {
            warn!(provider = %provider, error = %e, "Provider discovery failed; preserving previous cache");
            return Err((ErrorCode::InternalError, e.to_string()));
        }
    };

    let cache = shore_llm::discovery::ProviderModelsCache {
        version: shore_llm::discovery::CACHE_VERSION,
        provider_key: provider.to_string(),
        fetched_at: chrono::Utc::now().to_rfc3339(),
        base_url: Some(base_url),
        models,
    };

    let path = shore_llm::discovery::cache_path(&ctx.data_dir, provider);
    if let Err(e) = shore_llm::discovery::write_cache(&path, &cache) {
        return Err((
            ErrorCode::InternalError,
            format!("failed to write provider cache: {e}"),
        ));
    }

    Ok(json!({
        "provider": provider,
        "model_count": cache.models.len(),
        "fetched_at": cache.fetched_at,
        "cache_path": path.display().to_string(),
    }))
}

// ── list_provider_models ───────────────────────────────────────────────

/// Return the merged model list for a provider: discovered (from cache)
/// plus statically configured models from `[chat.<provider>.<name>]`.
///
/// Static models are always returned, even when the discovery cache is
/// missing. This preserves the manual escape hatch from Phase 0.
pub fn list_provider_models(ctx: &CommandContext, args: &Value) -> CommandResult {
    let provider = require_provider(args)?;

    // Validate that the provider is at least *known* (configured in the
    // registry, or referenced by a static chat entry). Discovering the
    // hardcoded provider defaults is a Phase 7 concern.
    let known_in_registry = ctx.config.providers.get(provider).is_some();
    let known_in_static = ctx
        .config
        .models
        .chat
        .values()
        .any(|m| m.provider_key == provider);
    if !known_in_registry && !known_in_static {
        return Err((
            ErrorCode::NotFound,
            format!("provider {provider:?} is not configured"),
        ));
    }

    let cache_path = shore_llm::discovery::cache_path(&ctx.data_dir, provider);
    let cache = shore_llm::discovery::read_cache(&cache_path).ok().flatten();

    let discovered: Vec<Value> = cache
        .as_ref()
        .map(|c| c.models.iter().map(discovered_to_json).collect())
        .unwrap_or_default();

    let static_models: Vec<Value> = ctx
        .config
        .models
        .chat
        .values()
        .filter(|m| m.provider_key == provider)
        .map(|m| {
            json!({
                "source": "static",
                "name": m.name,
                "qualified_name": m.qualified_name,
                "model_id": m.model_id,
                "sdk": m.sdk.as_str(),
                "max_tokens": m.max_tokens,
            })
        })
        .collect();

    Ok(json!({
        "provider": provider,
        "discovered": discovered,
        "static": static_models,
        "cache": cache.map(|c| json!({
            "fetched_at": c.fetched_at,
            "model_count": c.models.len(),
        })).unwrap_or(json!({ "fetched_at": Value::Null, "model_count": 0 })),
    }))
}

fn discovered_to_json(m: &shore_llm::discovery::DiscoveredModel) -> Value {
    json!({
        "source": "discovered",
        "model_id": m.model_id,
        "display_name": m.display_name,
        "sdk": m.sdk,
        "owned_by": m.owned_by,
        "context_length": m.context_length,
        "max_output_tokens": m.max_output_tokens,
        "supports_tools": m.supports_tools,
        "supports_images": m.supports_images,
        "supports_reasoning": m.supports_reasoning,
        "supports_prompt_cache": m.supports_prompt_cache,
        "discovered_at": m.discovered_at,
    })
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use shore_config::providers::ProviderRegistry;
    use shore_diagnostics::Diagnostics;
    use shore_ledger::LedgerClient;
    use shore_llm::discovery::{DiscoveredModel, ProviderModelsCache, CACHE_VERSION};
    use shore_protocol::server_msg::ServerMessage;
    use tokio::sync::broadcast;

    use crate::autonomy::manager::AutonomyManager;
    use crate::commands::SessionTokens;

    fn build_ctx_with_registry(tmp: &tempfile::TempDir, toml_str: &str) -> CommandContext {
        let (push_tx, _push_rx) = broadcast::channel(16);
        let data_dir = tmp.path().to_path_buf();

        let providers = if toml_str.is_empty() {
            ProviderRegistry::default()
        } else {
            let table: toml::Table = toml_str.parse().unwrap();
            let section = table.get("providers").and_then(|v| v.as_table());
            ProviderRegistry::from_section(section).unwrap()
        };

        let mut loaded = shore_config::LoadedConfig::new_for_test(
            shore_config::app::AppConfig::default(),
            shore_config::models::ModelCatalog::default(),
            shore_config::ShoreDirs {
                config: tmp.path().join("config"),
                data: data_dir.clone(),
                runtime: tmp.path().join("runtime"),
                cache: tmp.path().join("cache"),
            },
        );
        loaded.providers = providers;

        let (_tx, rx) = tokio::sync::watch::channel(());
        let autonomy =
            AutonomyManager::new(Default::default(), Default::default(), data_dir.clone(), rx);

        CommandContext {
            config: loaded,
            push_tx,
            data_dir: data_dir.clone(),
            character_name: None,
            active_model: None,
            reasoning_effort_override: None,
            session_tokens: Arc::new(Mutex::new(SessionTokens::default())),
            autonomy,
            llm_client: LedgerClient::new(shore_llm::LlmClient::new(), &data_dir.join("ledger.db"))
                .unwrap(),
            diagnostics: Arc::new(Mutex::new(Diagnostics::default())),
        }
    }

    fn write_test_cache(data_dir: &std::path::Path, provider: &str, models: Vec<DiscoveredModel>) {
        let cache = ProviderModelsCache {
            version: CACHE_VERSION,
            provider_key: provider.into(),
            fetched_at: "2026-04-28T10:00:00Z".into(),
            base_url: Some("https://example.test/v1".into()),
            models,
        };
        let path = shore_llm::discovery::cache_path(data_dir, provider);
        shore_llm::discovery::write_cache(&path, &cache).unwrap();
    }

    fn discovered_fixture(provider: &str, model_id: &str) -> DiscoveredModel {
        DiscoveredModel {
            provider_key: provider.into(),
            model_id: model_id.into(),
            display_name: None,
            sdk: "openai".into(),
            base_url: Some("https://example.test/v1".into()),
            created_at: None,
            owned_by: None,
            description: None,
            context_length: None,
            max_output_tokens: None,
            supports_tools: None,
            supports_images: None,
            supports_reasoning: None,
            supports_prompt_cache: None,
            raw_provider_metadata: serde_json::Value::Null,
            discovered_at: "2026-04-28T10:00:00Z".into(),
        }
    }

    // ── list_providers ──────────────────────────────────────────────────

    #[test]
    fn list_providers_no_secrets_in_output() {
        let tmp = tempfile::tempdir().unwrap();
        let unique = format!("PROV_TEST_LIST_KEY_{}", std::process::id());
        std::env::set_var(&unique, "sk-this-must-not-appear");
        let ctx = build_ctx_with_registry(
            &tmp,
            &format!(
                r#"
[providers.openrouter]
sdk = "openai"
base_url = "https://openrouter.ai/api/v1"

[[providers.openrouter.keys]]
name = "main"
env = "{unique}"
warn_on_fallback = true
"#
            ),
        );

        let out = list_providers(&ctx).unwrap();
        let s = serde_json::to_string(&out).unwrap();

        // Secret value never appears.
        assert!(
            !s.contains("sk-this-must-not-appear"),
            "secret leaked into list_providers output: {s}"
        );
        // Env var name never appears either.
        assert!(!s.contains(&unique), "env var name leaked: {s}");

        // But friendly key name and env_set boolean do.
        let provider = &out["providers"][0];
        assert_eq!(provider["name"], "openrouter");
        assert_eq!(provider["keys"][0]["name"], "main");
        assert_eq!(provider["keys"][0]["env_set"], true);
        assert_eq!(provider["keys"][0]["warn_on_fallback"], true);

        std::env::remove_var(&unique);
    }

    #[test]
    fn list_providers_reports_cache_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = build_ctx_with_registry(
            &tmp,
            r#"
[providers.openrouter]
sdk = "openai"
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OR_KEY"
"#,
        );
        write_test_cache(
            &ctx.data_dir,
            "openrouter",
            vec![discovered_fixture(
                "openrouter",
                "anthropic/claude-3.5-sonnet",
            )],
        );

        let out = list_providers(&ctx).unwrap();
        let cache = &out["providers"][0]["cache"];
        assert_eq!(cache["present"], true);
        assert_eq!(cache["models"], 1);
    }

    #[test]
    fn list_providers_reports_absent_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = build_ctx_with_registry(
            &tmp,
            r#"
[providers.openai]
api_key_env = "OPENAI_KEY"
"#,
        );
        let out = list_providers(&ctx).unwrap();
        assert_eq!(out["providers"][0]["cache"]["present"], false);
        assert_eq!(out["providers"][0]["cache"]["models"], 0);
    }

    // ── list_provider_models ────────────────────────────────────────────

    #[test]
    fn list_provider_models_merges_static_and_discovered() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ctx = build_ctx_with_registry(
            &tmp,
            r#"
[providers.openrouter]
api_key_env = "OR_KEY"
"#,
        );
        // Inject a static chat entry under the same provider.
        let table: toml::Table = r#"
[chat.openrouter.kimi]
model_id = "kimi-k2"
"#
        .parse()
        .unwrap();
        let chat = table.get("chat").and_then(|v| v.as_table());
        ctx.config.models =
            shore_config::models::ModelCatalog::from_sections(chat, None, None, None).unwrap();

        write_test_cache(
            &ctx.data_dir,
            "openrouter",
            vec![discovered_fixture(
                "openrouter",
                "anthropic/claude-3.5-sonnet",
            )],
        );

        let out = list_provider_models(&ctx, &json!({ "provider": "openrouter" })).unwrap();
        assert_eq!(out["provider"], "openrouter");
        assert_eq!(out["discovered"].as_array().unwrap().len(), 1);
        assert_eq!(out["static"].as_array().unwrap().len(), 1);
        assert_eq!(out["static"][0]["model_id"], "kimi-k2");
        assert_eq!(
            out["discovered"][0]["model_id"],
            "anthropic/claude-3.5-sonnet"
        );
    }

    #[test]
    fn list_provider_models_returns_static_when_cache_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ctx = build_ctx_with_registry(
            &tmp,
            r#"
[providers.openrouter]
api_key_env = "OR_KEY"
"#,
        );
        let table: toml::Table = r#"
[chat.openrouter.kimi]
model_id = "kimi-k2"
"#
        .parse()
        .unwrap();
        ctx.config.models = shore_config::models::ModelCatalog::from_sections(
            table.get("chat").and_then(|v| v.as_table()),
            None,
            None,
            None,
        )
        .unwrap();

        let out = list_provider_models(&ctx, &json!({ "provider": "openrouter" })).unwrap();
        assert!(out["discovered"].as_array().unwrap().is_empty());
        assert_eq!(out["static"].as_array().unwrap().len(), 1);
        assert_eq!(out["cache"]["model_count"], 0);
    }

    #[test]
    fn list_provider_models_unknown_provider_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = build_ctx_with_registry(&tmp, "");
        let err = list_provider_models(&ctx, &json!({ "provider": "ghost" })).unwrap_err();
        assert_eq!(err.0, ErrorCode::NotFound);
    }

    #[test]
    fn list_provider_models_missing_arg() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = build_ctx_with_registry(&tmp, "");
        let err = list_provider_models(&ctx, &json!({})).unwrap_err();
        assert_eq!(err.0, ErrorCode::InvalidRequest);
    }

    // ── refresh_provider_models ─────────────────────────────────────────

    #[tokio::test]
    async fn refresh_unknown_provider_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = build_ctx_with_registry(&tmp, "");
        let err = refresh_provider_models(&ctx, &json!({ "provider": "ghost" }))
            .await
            .unwrap_err();
        assert_eq!(err.0, ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn refresh_disabled_provider_is_invalid_request() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = build_ctx_with_registry(
            &tmp,
            r#"
[providers.openrouter]
enabled = false
api_key_env = "OR_KEY"
"#,
        );
        let err = refresh_provider_models(&ctx, &json!({ "provider": "openrouter" }))
            .await
            .unwrap_err();
        assert_eq!(err.0, ErrorCode::InvalidRequest);
        assert!(err.1.contains("disabled"));
    }

    #[tokio::test]
    async fn refresh_discovery_disabled_is_invalid_request() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = build_ctx_with_registry(
            &tmp,
            r#"
[providers.openrouter]
api_key_env = "OR_KEY"
base_url = "https://example.test/v1"

[providers.openrouter.discovery]
enabled = false
"#,
        );
        let err = refresh_provider_models(&ctx, &json!({ "provider": "openrouter" }))
            .await
            .unwrap_err();
        assert_eq!(err.0, ErrorCode::InvalidRequest);
        assert!(err.1.contains("discovery"));
    }

    #[tokio::test]
    async fn refresh_with_no_keys_set_is_unauthorized() {
        let tmp = tempfile::tempdir().unwrap();
        let unique = format!("PROV_TEST_NOKEY_{}", std::process::id());
        // Make sure it's not set.
        std::env::remove_var(&unique);
        let ctx = build_ctx_with_registry(
            &tmp,
            &format!(
                r#"
[providers.openrouter]
base_url = "https://example.test/v1"

[[providers.openrouter.keys]]
name = "main"
env = "{unique}"

[providers.openrouter.discovery]
enabled = true
"#
            ),
        );
        let err = refresh_provider_models(&ctx, &json!({ "provider": "openrouter" }))
            .await
            .unwrap_err();
        assert_eq!(err.0, ErrorCode::ProviderError);
    }

    #[tokio::test]
    async fn refresh_failure_preserves_previous_cache() {
        // Pre-populate a cache, then run a refresh that fails because the
        // server returns 500. The old cache must remain on disk.
        let tmp = tempfile::tempdir().unwrap();
        let unique = format!("PROV_TEST_FAILKEEP_{}", std::process::id());
        std::env::set_var(&unique, "sk-test");

        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/v1/models"))
            .respond_with(wiremock::ResponseTemplate::new(500).set_body_string("server boom"))
            .mount(&mock)
            .await;

        let ctx = build_ctx_with_registry(
            &tmp,
            &format!(
                r#"
[providers.upstream]
base_url = "{base}/v1"

[[providers.upstream.keys]]
name = "main"
env = "{unique}"

[providers.upstream.discovery]
enabled = true
"#,
                base = mock.uri()
            ),
        );

        let prior = vec![discovered_fixture("upstream", "kept-after-failure")];
        write_test_cache(&ctx.data_dir, "upstream", prior.clone());

        let err = refresh_provider_models(&ctx, &json!({ "provider": "upstream" }))
            .await
            .unwrap_err();
        assert_eq!(err.0, ErrorCode::InternalError);

        let path = shore_llm::discovery::cache_path(&ctx.data_dir, "upstream");
        let still_there = shore_llm::discovery::read_cache(&path)
            .unwrap()
            .expect("cache present");
        assert_eq!(
            still_there.models, prior,
            "previous cache must survive failed refresh"
        );

        std::env::remove_var(&unique);
        // Silence the unused warning.
        let _ = ServerMessage::Error(shore_protocol::server_msg::Error {
            rid: None,
            code: ErrorCode::InternalError,
            message: String::new(),
        });
    }

    #[tokio::test]
    async fn refresh_writes_cache_and_returns_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let unique = format!("PROV_TEST_REFRESH_{}", std::process::id());
        std::env::set_var(&unique, "sk-fixture");

        let body = serde_json::json!({
            "object": "list",
            "data": [
                { "id": "openai/gpt-4o", "object": "model", "owned_by": "openai" },
                { "id": "anthropic/claude-3.5-sonnet", "object": "model", "context_length": 200000 }
            ]
        });
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/v1/models"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&body))
            .mount(&mock)
            .await;

        let ctx = build_ctx_with_registry(
            &tmp,
            &format!(
                r#"
[providers.upstream]
base_url = "{base}/v1"

[[providers.upstream.keys]]
name = "main"
env = "{unique}"

[providers.upstream.discovery]
enabled = true
"#,
                base = mock.uri()
            ),
        );

        let out = refresh_provider_models(&ctx, &json!({ "provider": "upstream" }))
            .await
            .unwrap();
        assert_eq!(out["model_count"], 2);
        assert_eq!(out["provider"], "upstream");

        // Cache file is on disk and contains both models.
        let path = shore_llm::discovery::cache_path(&ctx.data_dir, "upstream");
        let cache = shore_llm::discovery::read_cache(&path)
            .unwrap()
            .expect("cache");
        assert_eq!(cache.models.len(), 2);
        assert!(cache
            .models
            .iter()
            .any(|m| m.model_id == "anthropic/claude-3.5-sonnet"));
        assert_eq!(cache.models[1].context_length, Some(200_000));

        std::env::remove_var(&unique);
    }
}
