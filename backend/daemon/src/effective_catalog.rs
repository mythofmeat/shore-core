//! Effective model catalog — Phase 7.
//!
//! Merges three sources into the catalog the daemon resolves against:
//!
//! 1. The static `[chat.<provider>.<model>]` model catalog (Phase 0).
//! 2. The provider registry (Phase 1) plus its on-disk discovery cache
//!    (Phase 5/6) for discovered models.
//! 3. (Saved sampler preferences are applied separately at request time
//!    via [`crate::preferences::apply_sampler_overlay`]; this module
//!    only resolves identity + transport metadata.)
//!
//! Conflict rules (mirrors the plan):
//!
//! * Static catalog entries always win when matched by short or qualified
//!   name (existing `find_model` behavior is preserved verbatim).
//! * When a discovered model and a static entry share the same
//!   `(provider, model_id)`, the static entry wins for explicit fields —
//!   discovered metadata only fills in for providers without a static
//!   override. We achieve this by checking static-by-upstream-id before
//!   constructing a synthetic `ResolvedModel` from the discovered record.
//! * Static catalog entries are never affected by visibility rules; only
//!   discovered models can be hidden.

use std::path::Path;

use shore_config::models::{
    default_sdk, hardcoded_provider_defaults, ModelCatalog, ModelConfigFields, ResolvedModel, Sdk,
};
use shore_config::providers::ProviderEntry;
use shore_config::LoadedConfig;
use shore_llm::discovery::{cache_path, read_cache, DiscoveredModel};

#[derive(Debug, thiserror::Error)]
pub enum EffectiveCatalogError {
    #[error("model {name:?} not found in static catalog or discovered models")]
    NotFound { name: String },

    #[error("model name {name:?} is ambiguous across providers: {locations}")]
    Ambiguous { name: String, locations: String },

    #[error(
        "model {name:?} is hidden by [providers.{provider}.discovery.visibility]; \
         pass include_hidden=true or update the visibility rules to allow it"
    )]
    Hidden { name: String, provider: String },
}

/// Where an effective-catalog entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveSource {
    Static,
    Discovered,
}

/// One row of the merged catalog as surfaced to clients.
#[derive(Debug, Clone)]
pub struct EffectiveModel {
    pub source: EffectiveSource,
    pub resolved: ResolvedModel,
    /// True if visibility rules would normally hide this model. Static
    /// entries are always `false` here.
    pub hidden: bool,
}

/// Look up a model by name across the static catalog and provider
/// discovery caches.
///
/// Resolution order:
///
/// 1. Static catalog by short or qualified name (existing behavior).
/// 2. Provider-prefixed form `provider:model_id`. The provider must be in
///    the registry. If a static entry shares the same `(provider, model_id)`,
///    that wins; otherwise a `ResolvedModel` is synthesized from the cache.
/// 3. Bare upstream `model_id`. Searched across every registered provider's
///    cache. If multiple providers carry the same id, returns
///    `Ambiguous` so the caller can disambiguate with `provider:id`.
///
/// `include_hidden = true` permits resolving discovered models matched by
/// visibility hide patterns. Static entries are never hidden.
pub fn find_effective_model(
    config: &LoadedConfig,
    data_dir: &Path,
    name: &str,
    include_hidden: bool,
) -> Result<ResolvedModel, EffectiveCatalogError> {
    if let Ok(m) = config.models.find_model(name) {
        return Ok(m.clone());
    }

    if let Some((provider, model_id)) = name.split_once(':') {
        if !provider.is_empty() && !model_id.is_empty() {
            if let Some(entry) = config.providers.get(provider) {
                if let Some(disc) = read_provider_discovery(data_dir, provider, model_id) {
                    if let Some(static_match) =
                        find_static_by_upstream(&config.models, provider, model_id)
                    {
                        return Ok(static_match.clone());
                    }
                    let hidden = !entry.discovery.is_visible(&disc.model_id);
                    if hidden && !include_hidden {
                        return Err(EffectiveCatalogError::Hidden {
                            name: name.into(),
                            provider: provider.into(),
                        });
                    }
                    return Ok(build_resolved_from_discovered(provider, entry, &disc));
                }
            }
        }
    }

    // Bare upstream id: collect matches across all providers.
    let mut hits: Vec<(String, ResolvedModel, bool)> = Vec::new();
    for (provider_key, entry) in config.providers.iter() {
        let Some(disc) = read_provider_discovery(data_dir, provider_key, name) else {
            continue;
        };
        if let Some(static_match) = find_static_by_upstream(&config.models, provider_key, name) {
            hits.push((provider_key.into(), static_match.clone(), false));
        } else {
            let hidden = !entry.discovery.is_visible(&disc.model_id);
            let resolved = build_resolved_from_discovered(provider_key, entry, &disc);
            hits.push((provider_key.into(), resolved, hidden));
        }
    }

    match hits.len() {
        0 => Err(EffectiveCatalogError::NotFound { name: name.into() }),
        1 => {
            let (provider, resolved, hidden) = hits.into_iter().next().unwrap();
            if hidden && !include_hidden {
                return Err(EffectiveCatalogError::Hidden {
                    name: name.into(),
                    provider,
                });
            }
            Ok(resolved)
        }
        _ => {
            let locations = hits
                .iter()
                .map(|(p, r, _)| format!("{p}:{}", r.model_id))
                .collect::<Vec<_>>()
                .join(", ");
            Err(EffectiveCatalogError::Ambiguous {
                name: name.into(),
                locations,
            })
        }
    }
}

/// List the merged catalog: every static chat model plus every discovered
/// model. Discovered models are deduplicated against static entries with
/// the same `(provider, model_id)` — only one row appears, sourced from
/// the static side.
///
/// `include_hidden = false` (the default) drops discovered rows hidden by
/// visibility rules. Static rows are always included.
pub fn list_effective_models(
    config: &LoadedConfig,
    data_dir: &Path,
    include_hidden: bool,
) -> Vec<EffectiveModel> {
    let mut out: Vec<EffectiveModel> = config
        .models
        .chat
        .values()
        .map(|m| EffectiveModel {
            source: EffectiveSource::Static,
            resolved: m.clone(),
            hidden: false,
        })
        .collect();

    for (provider_key, entry) in config.providers.iter() {
        let cache_p = cache_path(data_dir, provider_key);
        let Some(cache) = read_cache(&cache_p).ok().flatten() else {
            continue;
        };
        for disc in &cache.models {
            if find_static_by_upstream(&config.models, provider_key, &disc.model_id).is_some() {
                continue;
            }
            let hidden = !entry.discovery.is_visible(&disc.model_id);
            if hidden && !include_hidden {
                continue;
            }
            let resolved = build_resolved_from_discovered(provider_key, entry, disc);
            out.push(EffectiveModel {
                source: EffectiveSource::Discovered,
                resolved,
                hidden,
            });
        }
    }
    out
}

// ── Internals ───────────────────────────────────────────────────────────

fn read_provider_discovery(
    data_dir: &Path,
    provider_key: &str,
    model_id: &str,
) -> Option<DiscoveredModel> {
    let cache = read_cache(&cache_path(data_dir, provider_key))
        .ok()
        .flatten()?;
    cache.models.into_iter().find(|m| m.model_id == model_id)
}

fn find_static_by_upstream<'a>(
    catalog: &'a ModelCatalog,
    provider: &str,
    model_id: &str,
) -> Option<&'a ResolvedModel> {
    catalog
        .chat
        .values()
        .find(|m| m.provider_key == provider && m.model_id == model_id)
}

fn build_resolved_from_discovered(
    provider_key: &str,
    entry: &ProviderEntry,
    disc: &DiscoveredModel,
) -> ResolvedModel {
    let provider_defaults = hardcoded_provider_defaults(provider_key).fields;

    let sdk = entry
        .sdk
        .clone()
        .or_else(|| Sdk::parse_wire(&disc.sdk))
        .or_else(|| provider_defaults.sdk.clone())
        .unwrap_or_else(|| default_sdk(provider_key));

    let base_url = entry
        .base_url
        .clone()
        .or_else(|| disc.base_url.clone())
        .or_else(|| provider_defaults.base_url.clone());

    let max_context_tokens = disc
        .context_length
        .and_then(|v| u32::try_from(v).ok())
        .or(provider_defaults.max_context_tokens);

    let max_tokens = disc
        .max_output_tokens
        .and_then(|v| u32::try_from(v).ok())
        .or(provider_defaults.max_tokens);

    let qualified_name = format!("chat.{provider_key}.{}", disc.model_id);

    let fields = ModelConfigFields {
        sdk: Some(sdk.clone()),
        api_key_env: None,
        base_url,
        max_context_tokens,
        max_tokens,
        temperature: provider_defaults.temperature,
        top_p: None,
        reasoning_effort: None,
        budget_tokens: None,
        cache_ttl: None,
        keepalive_enabled: None,
        keepalive_ttl: None,
        keepalive_max_pings: None,
        openrouter_provider: None,
        vertex_project: None,
        vertex_location: None,
        gemini_generation: None,
        gemini_web_search: None,
        zai_clear_thinking: provider_defaults.zai_clear_thinking,
        zai_subscription: None,
    };

    ResolvedModel::from_parts(
        disc.model_id.clone(),
        qualified_name,
        "chat".into(),
        provider_key.into(),
        disc.model_id.clone(),
        sdk,
        fields,
    )
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use shore_config::models::ModelCatalog;
    use shore_config::providers::ProviderRegistry;
    use shore_config::ShoreDirs;
    use shore_llm::discovery::{ProviderModelsCache, CACHE_VERSION};

    fn make_loaded(tmp: &tempfile::TempDir, providers_toml: &str, chat_toml: &str) -> LoadedConfig {
        let providers = if providers_toml.is_empty() {
            ProviderRegistry::default()
        } else {
            let table: toml::Table = providers_toml.parse().unwrap();
            ProviderRegistry::from_section(table.get("providers").and_then(|v| v.as_table()))
                .unwrap()
        };
        let catalog = if chat_toml.is_empty() {
            ModelCatalog::default()
        } else {
            let table: toml::Table = chat_toml.parse().unwrap();
            ModelCatalog::from_sections(
                table.get("chat").and_then(|v| v.as_table()),
                None,
                None,
                None,
            )
            .unwrap()
        };
        let mut loaded = LoadedConfig::new_for_test(
            shore_config::app::AppConfig::default(),
            catalog,
            ShoreDirs {
                config: tmp.path().join("config"),
                data: tmp.path().to_path_buf(),
                runtime: tmp.path().join("runtime"),
                cache: tmp.path().join("cache"),
            },
        );
        loaded.providers = providers;
        loaded
    }

    fn write_cache_for(tmp: &tempfile::TempDir, provider: &str, model_ids: &[&str]) {
        let models = model_ids
            .iter()
            .map(|id| DiscoveredModel {
                provider_key: provider.into(),
                model_id: (*id).into(),
                display_name: None,
                sdk: "openai".into(),
                base_url: Some("https://example.test/v1".into()),
                created_at: None,
                owned_by: None,
                description: None,
                context_length: Some(200_000),
                max_output_tokens: Some(8192),
                supports_tools: None,
                supports_images: None,
                supports_reasoning: None,
                supports_prompt_cache: None,
                raw_provider_metadata: serde_json::Value::Null,
                discovered_at: "2026-04-29T00:00:00Z".into(),
            })
            .collect();
        let cache = ProviderModelsCache {
            version: CACHE_VERSION,
            provider_key: provider.into(),
            fetched_at: "2026-04-29T00:00:00Z".into(),
            base_url: Some("https://example.test/v1".into()),
            models,
        };
        let path = cache_path(tmp.path(), provider);
        shore_llm::discovery::write_cache(&path, &cache).unwrap();
    }

    // ── find_effective_model ────────────────────────────────────────────

    #[test]
    fn discovered_model_selectable_by_bare_id() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = make_loaded(
            &tmp,
            r#"
[providers.openrouter]
api_key_env = "OR_KEY"
base_url = "https://openrouter.ai/api/v1"
"#,
            "",
        );
        write_cache_for(&tmp, "openrouter", &["anthropic/claude-sonnet-4.5"]);

        let m = find_effective_model(&loaded, tmp.path(), "anthropic/claude-sonnet-4.5", false)
            .unwrap();
        assert_eq!(m.provider_key, "openrouter");
        assert_eq!(m.model_id, "anthropic/claude-sonnet-4.5");
        assert_eq!(
            m.qualified_name,
            "chat.openrouter.anthropic/claude-sonnet-4.5"
        );
        assert_eq!(m.max_context_tokens, Some(200_000));
        // Discovered models inherit the registry's base_url.
        assert_eq!(m.base_url.as_deref(), Some("https://openrouter.ai/api/v1"));
    }

    #[test]
    fn discovered_model_selectable_by_provider_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = make_loaded(
            &tmp,
            r#"
[providers.openrouter]
api_key_env = "OR_KEY"
base_url = "https://openrouter.ai/api/v1"
"#,
            "",
        );
        write_cache_for(&tmp, "openrouter", &["anthropic/claude-sonnet-4.5"]);

        let m = find_effective_model(
            &loaded,
            tmp.path(),
            "openrouter:anthropic/claude-sonnet-4.5",
            false,
        )
        .unwrap();
        assert_eq!(m.provider_key, "openrouter");
        assert_eq!(m.model_id, "anthropic/claude-sonnet-4.5");
    }

    #[test]
    fn static_alias_still_resolves_to_static_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = make_loaded(
            &tmp,
            r#"
[providers.openrouter]
api_key_env = "OR_KEY"
base_url = "https://openrouter.ai/api/v1"
"#,
            r#"
[chat.openrouter.sonnet]
model_id = "anthropic/claude-sonnet-4.5"
cache_ttl = "1h"
max_tokens = 16384
"#,
        );
        write_cache_for(&tmp, "openrouter", &["anthropic/claude-sonnet-4.5"]);

        // Short alias resolves to the static entry, with its overrides intact.
        let m = find_effective_model(&loaded, tmp.path(), "sonnet", false).unwrap();
        assert_eq!(m.qualified_name, "chat.openrouter.sonnet");
        assert_eq!(m.cache_ttl.as_deref(), Some("1h"));
        assert_eq!(m.max_tokens, Some(16384));
    }

    #[test]
    fn static_overrides_discovered_when_same_provider_and_model_id() {
        // Plan requirement: "manual static config wins for explicit fields".
        // When the user types the bare upstream id, return the static entry —
        // not a fresh ResolvedModel built from the discovered record.
        let tmp = tempfile::tempdir().unwrap();
        let loaded = make_loaded(
            &tmp,
            r#"
[providers.openrouter]
api_key_env = "OR_KEY"
base_url = "https://openrouter.ai/api/v1"
"#,
            r#"
[chat.openrouter.sonnet]
model_id = "anthropic/claude-sonnet-4.5"
cache_ttl = "1h"
max_tokens = 16384
"#,
        );
        write_cache_for(&tmp, "openrouter", &["anthropic/claude-sonnet-4.5"]);

        let m = find_effective_model(&loaded, tmp.path(), "anthropic/claude-sonnet-4.5", false)
            .unwrap();
        // Static name wins, not a synthesized discovered name.
        assert_eq!(m.name, "sonnet");
        assert_eq!(m.qualified_name, "chat.openrouter.sonnet");
        assert_eq!(m.cache_ttl.as_deref(), Some("1h"));
        assert_eq!(m.max_tokens, Some(16384));
    }

    #[test]
    fn ambiguous_bare_id_across_providers_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = make_loaded(
            &tmp,
            r#"
[providers.openrouter]
api_key_env = "OR_KEY"
base_url = "https://openrouter.ai/api/v1"

[providers.together]
api_key_env = "TG_KEY"
base_url = "https://api.together.xyz/v1"
"#,
            "",
        );
        write_cache_for(&tmp, "openrouter", &["meta-llama/llama-3-70b"]);
        write_cache_for(&tmp, "together", &["meta-llama/llama-3-70b"]);

        let err =
            find_effective_model(&loaded, tmp.path(), "meta-llama/llama-3-70b", false).unwrap_err();
        match err {
            EffectiveCatalogError::Ambiguous { ref locations, .. } => {
                assert!(locations.contains("openrouter"));
                assert!(locations.contains("together"));
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn provider_prefix_disambiguates_two_providers() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = make_loaded(
            &tmp,
            r#"
[providers.openrouter]
api_key_env = "OR_KEY"
base_url = "https://openrouter.ai/api/v1"

[providers.together]
api_key_env = "TG_KEY"
base_url = "https://api.together.xyz/v1"
"#,
            "",
        );
        write_cache_for(&tmp, "openrouter", &["meta-llama/llama-3-70b"]);
        write_cache_for(&tmp, "together", &["meta-llama/llama-3-70b"]);

        let m = find_effective_model(
            &loaded,
            tmp.path(),
            "together:meta-llama/llama-3-70b",
            false,
        )
        .unwrap();
        assert_eq!(m.provider_key, "together");
    }

    #[test]
    fn unknown_name_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = make_loaded(
            &tmp,
            r#"
[providers.openrouter]
api_key_env = "OR_KEY"
base_url = "https://openrouter.ai/api/v1"
"#,
            "",
        );
        write_cache_for(&tmp, "openrouter", &["anthropic/claude-sonnet-4.5"]);

        let err = find_effective_model(&loaded, tmp.path(), "ghost-model", false).unwrap_err();
        assert!(matches!(err, EffectiveCatalogError::NotFound { .. }));
    }

    #[test]
    fn hidden_discovered_model_rejected_with_clear_error() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = make_loaded(
            &tmp,
            r#"
[providers.openrouter]
api_key_env = "OR_KEY"
base_url = "https://openrouter.ai/api/v1"

[providers.openrouter.discovery]
enabled = true
visibility = ["meta-llama/*"]
"#,
            "",
        );
        write_cache_for(
            &tmp,
            "openrouter",
            &["anthropic/claude-sonnet-4.5", "meta-llama/llama-3-70b"],
        );

        let err =
            find_effective_model(&loaded, tmp.path(), "meta-llama/llama-3-70b", false).unwrap_err();
        match err {
            EffectiveCatalogError::Hidden {
                ref name,
                ref provider,
            } => {
                assert_eq!(name, "meta-llama/llama-3-70b");
                assert_eq!(provider, "openrouter");
            }
            other => panic!("expected Hidden, got {other:?}"),
        }
        // Visible neighbour resolves fine.
        assert!(
            find_effective_model(&loaded, tmp.path(), "anthropic/claude-sonnet-4.5", false,)
                .is_ok()
        );
    }

    #[test]
    fn hidden_discovered_model_resolves_with_include_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = make_loaded(
            &tmp,
            r#"
[providers.openrouter]
api_key_env = "OR_KEY"
base_url = "https://openrouter.ai/api/v1"

[providers.openrouter.discovery]
enabled = true
visibility = ["meta-llama/*"]
"#,
            "",
        );
        write_cache_for(&tmp, "openrouter", &["meta-llama/llama-3-70b"]);

        let m = find_effective_model(&loaded, tmp.path(), "meta-llama/llama-3-70b", true).unwrap();
        assert_eq!(m.provider_key, "openrouter");
        assert_eq!(m.model_id, "meta-llama/llama-3-70b");
    }

    #[test]
    fn provider_prefix_for_hidden_model_also_rejected_without_include_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = make_loaded(
            &tmp,
            r#"
[providers.openrouter]
api_key_env = "OR_KEY"
base_url = "https://openrouter.ai/api/v1"

[providers.openrouter.discovery]
enabled = true
visibility = ["meta-llama/*"]
"#,
            "",
        );
        write_cache_for(&tmp, "openrouter", &["meta-llama/llama-3-70b"]);

        let err = find_effective_model(
            &loaded,
            tmp.path(),
            "openrouter:meta-llama/llama-3-70b",
            false,
        )
        .unwrap_err();
        assert!(matches!(err, EffectiveCatalogError::Hidden { .. }));

        let m = find_effective_model(
            &loaded,
            tmp.path(),
            "openrouter:meta-llama/llama-3-70b",
            true,
        )
        .unwrap();
        assert_eq!(m.provider_key, "openrouter");
    }

    // ── list_effective_models ───────────────────────────────────────────

    #[test]
    fn list_includes_static_and_discovered() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = make_loaded(
            &tmp,
            r#"
[providers.openrouter]
api_key_env = "OR_KEY"
base_url = "https://openrouter.ai/api/v1"
"#,
            r#"
[chat.anthropic.opus]
model_id = "claude-opus-4-7"
"#,
        );
        write_cache_for(
            &tmp,
            "openrouter",
            &["anthropic/claude-sonnet-4.5", "openai/gpt-4o"],
        );

        let list = list_effective_models(&loaded, tmp.path(), false);
        let names: Vec<&str> = list.iter().map(|e| e.resolved.model_id.as_str()).collect();
        assert!(names.contains(&"claude-opus-4-7"));
        assert!(names.contains(&"anthropic/claude-sonnet-4.5"));
        assert!(names.contains(&"openai/gpt-4o"));
        // Sources tagged correctly.
        for entry in &list {
            if entry.resolved.model_id == "claude-opus-4-7" {
                assert_eq!(entry.source, EffectiveSource::Static);
            } else {
                assert_eq!(entry.source, EffectiveSource::Discovered);
            }
        }
    }

    #[test]
    fn list_dedupes_when_static_overrides_discovered() {
        // A static entry with the same (provider, model_id) as a discovered
        // model must appear once, sourced as static.
        let tmp = tempfile::tempdir().unwrap();
        let loaded = make_loaded(
            &tmp,
            r#"
[providers.openrouter]
api_key_env = "OR_KEY"
base_url = "https://openrouter.ai/api/v1"
"#,
            r#"
[chat.openrouter.sonnet]
model_id = "anthropic/claude-sonnet-4.5"
cache_ttl = "1h"
"#,
        );
        write_cache_for(&tmp, "openrouter", &["anthropic/claude-sonnet-4.5"]);

        let list = list_effective_models(&loaded, tmp.path(), false);
        let same_id: Vec<_> = list
            .iter()
            .filter(|e| e.resolved.model_id == "anthropic/claude-sonnet-4.5")
            .collect();
        assert_eq!(same_id.len(), 1, "deduplicated to a single row");
        assert_eq!(same_id[0].source, EffectiveSource::Static);
        assert_eq!(same_id[0].resolved.cache_ttl.as_deref(), Some("1h"));
    }

    #[test]
    fn list_drops_hidden_unless_include_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = make_loaded(
            &tmp,
            r#"
[providers.openrouter]
api_key_env = "OR_KEY"
base_url = "https://openrouter.ai/api/v1"

[providers.openrouter.discovery]
enabled = true
visibility = ["meta-llama/*"]
"#,
            "",
        );
        write_cache_for(
            &tmp,
            "openrouter",
            &["anthropic/claude-sonnet-4.5", "meta-llama/llama-3-70b"],
        );

        let visible_only = list_effective_models(&loaded, tmp.path(), false);
        let names: Vec<&str> = visible_only
            .iter()
            .map(|e| e.resolved.model_id.as_str())
            .collect();
        assert!(names.contains(&"anthropic/claude-sonnet-4.5"));
        assert!(!names.contains(&"meta-llama/llama-3-70b"));

        let with_hidden = list_effective_models(&loaded, tmp.path(), true);
        assert_eq!(with_hidden.len(), 2);
        let llama = with_hidden
            .iter()
            .find(|e| e.resolved.model_id == "meta-llama/llama-3-70b")
            .unwrap();
        assert!(
            llama.hidden,
            "hidden flag set even when include_hidden=true"
        );
    }

    // ── Default sdk for discovered models ───────────────────────────────

    #[test]
    fn discovered_model_picks_up_provider_registry_sdk() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = make_loaded(
            &tmp,
            r#"
[providers.openrouter]
sdk = "anthropic"
api_key_env = "OR_KEY"
base_url = "https://openrouter.ai/api/v1"
"#,
            "",
        );
        write_cache_for(&tmp, "openrouter", &["anthropic/claude-sonnet-4.5"]);

        let m = find_effective_model(&loaded, tmp.path(), "anthropic/claude-sonnet-4.5", false)
            .unwrap();
        assert_eq!(m.sdk, Sdk::Anthropic, "registry sdk wins over discovered");
    }
}
