//! Model catalog: nested `[chat.<provider>.<model>]` structure with
//! provider-level defaults cascading into per-model entries.
//!
//! The parsing mirrors V1's `_load_category_profiles()` approach:
//! for each provider table, scalar keys become provider defaults and
//! sub-table keys become model entries that inherit those defaults.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tracing::warn;

// ── SDK enum ────────────────────────────────────────────────────────────

/// SDK/wire protocol.  Distinguishes the message format from the gateway.
///
/// For example, `Anthropic` with a custom `base_url` pointing at OpenRouter
/// means "use the Anthropic message format, but send requests to OpenRouter."
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Sdk {
    #[default]
    Anthropic,
    Openai,
    Gemini,
    Zhipuai,
    Deepseek,
}

impl Sdk {
    /// Wire protocol string sent to shore-llm.
    pub fn as_provider_str(&self) -> &'static str {
        match self {
            Sdk::Anthropic => "anthropic",
            Sdk::Openai => "openai",
            Sdk::Gemini => "gemini",
            Sdk::Zhipuai => "zhipuai",
            Sdk::Deepseek => "deepseek",
        }
    }
}

// ── Provider config ─────────────────────────────────────────────────────

/// Provider-level configuration — the scalar keys under `[chat.<provider>]`.
///
/// All fields are optional; they cascade into every model under this provider
/// unless the model overrides them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct ProviderConfig {
    pub sdk: Option<Sdk>,
    pub api_key_env: Option<String>,
    pub base_url: Option<String>,
    pub max_context_tokens: Option<u32>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub reasoning_effort: Option<String>,
    pub budget_tokens: Option<u32>,
    pub cache_ttl: Option<String>,
    pub cache_control_depth: Option<u32>,
    pub keepalive_enabled: Option<bool>,
    pub keepalive_ttl_minutes: Option<u32>,
    pub keepalive_max_pings: Option<u32>,
    pub openrouter_provider: Option<toml::Value>,
    pub vertex_project: Option<String>,
    pub vertex_location: Option<String>,
    pub gemini_generation: Option<u32>,
    pub gemini_web_search: Option<bool>,
}

// ── Model entry ─────────────────────────────────────────────────────────

/// Per-model configuration — sub-tables under `[chat.<provider>.<model>]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelEntry {
    /// The upstream model identifier (e.g. `"claude-opus-4-6"`).  Required.
    pub model_id: Option<String>,
    // All overrides — None means inherit from provider.
    pub sdk: Option<Sdk>,
    pub api_key_env: Option<String>,
    pub base_url: Option<String>,
    pub max_context_tokens: Option<u32>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub reasoning_effort: Option<String>,
    pub budget_tokens: Option<u32>,
    pub cache_ttl: Option<String>,
    pub cache_control_depth: Option<u32>,
    pub keepalive_enabled: Option<bool>,
    pub keepalive_ttl_minutes: Option<u32>,
    pub keepalive_max_pings: Option<u32>,
    pub openrouter_provider: Option<toml::Value>,
    pub vertex_project: Option<String>,
    pub vertex_location: Option<String>,
    pub gemini_generation: Option<u32>,
    pub gemini_web_search: Option<bool>,
}

impl Default for ModelEntry {
    fn default() -> Self {
        Self {
            model_id: None,
            sdk: None,
            api_key_env: None,
            base_url: None,
            max_context_tokens: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            reasoning_effort: None,
            budget_tokens: None,
            cache_ttl: None,
            cache_control_depth: None,
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
}

// ── Resolved model ──────────────────────────────────────────────────────

/// A fully resolved model profile with all provider defaults merged in.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResolvedModel {
    /// Short name — the TOML key under the provider (e.g. `"opus"`).
    pub name: String,
    /// Qualified path (e.g. `"chat.anthropic.opus"`).
    pub qualified_name: String,
    /// Category: `"chat"`, `"tools"`, etc.
    pub category: String,
    /// Provider key (e.g. `"anthropic"`, `"openrouter"`).
    pub provider_key: String,
    /// SDK/protocol to use.
    pub sdk: Sdk,
    /// Upstream model identifier.
    pub model_id: String,
    pub api_key_env: Option<String>,
    pub base_url: Option<String>,
    pub max_context_tokens: Option<u32>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub reasoning_effort: Option<String>,
    pub budget_tokens: Option<u32>,
    pub cache_ttl: Option<String>,
    pub cache_control_depth: Option<u32>,
    pub keepalive_enabled: Option<bool>,
    pub keepalive_ttl_minutes: Option<u32>,
    pub keepalive_max_pings: Option<u32>,
    pub openrouter_provider: Option<toml::Value>,
    pub vertex_project: Option<String>,
    pub vertex_location: Option<String>,
    pub gemini_generation: Option<u32>,
    pub gemini_web_search: Option<bool>,
}

// ── Model catalog ───────────────────────────────────────────────────────

/// The parsed model catalog — replaces the old flat `ModelsConfig`.
#[derive(Debug, Clone, Default)]
pub struct ModelCatalog {
    /// Chat models keyed by short name.
    pub chat: BTreeMap<String, ResolvedModel>,
    /// Tool models keyed by short name.
    pub tools: BTreeMap<String, ResolvedModel>,
    /// Embedding profiles as raw TOML (consumers not yet implemented).
    pub embedding: BTreeMap<String, toml::Value>,
    /// Image generation profiles as raw TOML (consumers not yet implemented).
    pub image_generation: BTreeMap<String, toml::Value>,
}

/// Errors from model catalog parsing.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("model \"{name}\" in [{category}.{provider}] is missing required field `model_id`")]
    MissingModelId {
        category: String,
        provider: String,
        name: String,
    },
    #[error("failed to parse model entry [{category}.{provider}.{name}]: {source}")]
    ParseEntry {
        category: String,
        provider: String,
        name: String,
        source: toml::de::Error,
    },
    #[error("ambiguous model name \"{name}\" — found in: {locations}")]
    AmbiguousName { name: String, locations: String },
}

/// Dict-valued TOML keys at the provider level that are config fields,
/// NOT model sub-tables.  Mirrors V1's `_RESERVED_DICT_KEYS`.
const RESERVED_DICT_KEYS: &[&str] = &["openrouter_provider"];

impl ModelCatalog {
    /// Build a catalog from the raw TOML table (the full merged config).
    ///
    /// Extracts `chat`, `tools`, `embedding`, and `image_generation` sections.
    pub fn from_sections(
        chat: Option<&toml::Table>,
        tools: Option<&toml::Table>,
        embedding: Option<&toml::Table>,
        image_generation: Option<&toml::Table>,
    ) -> Result<Self, CatalogError> {
        let chat_models = match chat {
            Some(t) => parse_category("chat", t)?,
            None => BTreeMap::new(),
        };
        let tool_models = match tools {
            Some(t) => parse_category("tools", t)?,
            None => BTreeMap::new(),
        };

        // Embedding and image_generation are stored as raw TOML for now.
        let embedding_profiles = match embedding {
            Some(t) => t
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            None => BTreeMap::new(),
        };
        let image_gen_profiles = match image_generation {
            Some(t) => t
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            None => BTreeMap::new(),
        };

        Ok(Self {
            chat: chat_models,
            tools: tool_models,
            embedding: embedding_profiles,
            image_generation: image_gen_profiles,
        })
    }

    /// Look up a model by short name or qualified name.
    ///
    /// Qualified names (`"chat.anthropic.opus"`) are tried first.
    /// Short names (`"opus"`) search across all categories and error
    /// on ambiguity.
    pub fn find_model(&self, name: &str) -> Result<&ResolvedModel, CatalogError> {
        // 1. Try qualified name match.
        for model in self.chat.values().chain(self.tools.values()) {
            if model.qualified_name == name {
                return Ok(model);
            }
        }

        // 2. Try short name match.
        let mut matches: Vec<&ResolvedModel> = Vec::new();
        for model in self.chat.values().chain(self.tools.values()) {
            if model.name == name {
                matches.push(model);
            }
        }

        match matches.len() {
            0 => {
                // Not found — return a helpful error.
                Err(CatalogError::AmbiguousName {
                    name: name.to_string(),
                    locations: "nowhere (model not found)".to_string(),
                })
            }
            1 => Ok(matches[0]),
            _ => {
                let locations: Vec<&str> = matches
                    .iter()
                    .map(|m| m.qualified_name.as_str())
                    .collect();
                Err(CatalogError::AmbiguousName {
                    name: name.to_string(),
                    locations: locations.join(", "),
                })
            }
        }
    }

    /// Get the first chat model, if any.
    pub fn first_chat_model(&self) -> Option<&ResolvedModel> {
        self.chat.values().next()
    }

    /// Iterate all chat model names (short names).
    pub fn chat_model_names(&self) -> impl Iterator<Item = &str> {
        self.chat.keys().map(|s| s.as_str())
    }
}

// ── Category parser ─────────────────────────────────────────────────────

/// Parse a category section (`[chat]` or `[tools]`) from raw TOML.
///
/// For each provider table, separates scalar keys (provider defaults) from
/// sub-table keys (model entries).  `openrouter_provider` is a reserved
/// dict key treated as a provider scalar.
fn parse_category(
    category: &str,
    section: &toml::Table,
) -> Result<BTreeMap<String, ResolvedModel>, CatalogError> {
    let mut models = BTreeMap::new();

    for (provider_key, provider_value) in section {
        let provider_table = match provider_value.as_table() {
            Some(t) => t,
            None => {
                warn!(
                    category,
                    key = provider_key,
                    "Skipping non-table key in [{category}]"
                );
                continue;
            }
        };

        // ── Extract provider-level scalars ───────────────────────────
        // Build a TOML table containing only the scalar keys (and reserved
        // dict keys), then deserialize into ProviderConfig.
        let mut provider_scalars = toml::Table::new();
        for (k, v) in provider_table {
            if !v.is_table() || RESERVED_DICT_KEYS.contains(&k.as_str()) {
                provider_scalars.insert(k.clone(), v.clone());
            }
        }

        // Start from hardcoded defaults for this provider.
        let mut provider_config = hardcoded_defaults(provider_key);

        // Overlay explicit TOML scalars.
        if let Ok(explicit) =
            toml::Value::Table(provider_scalars).try_into::<ProviderConfig>()
        {
            merge_provider(&mut provider_config, &explicit);
        }

        // ── Extract model sub-tables ────────────────────────────────
        for (model_name, model_value) in provider_table {
            // Skip scalars and reserved dict keys — those are provider config.
            if !model_value.is_table() || RESERVED_DICT_KEYS.contains(&model_name.as_str()) {
                continue;
            }

            let entry: ModelEntry = model_value
                .clone()
                .try_into()
                .map_err(|e| CatalogError::ParseEntry {
                    category: category.to_string(),
                    provider: provider_key.to_string(),
                    name: model_name.to_string(),
                    source: e,
                })?;

            let model_id = entry.model_id.ok_or_else(|| CatalogError::MissingModelId {
                category: category.to_string(),
                provider: provider_key.to_string(),
                name: model_name.to_string(),
            })?;

            let resolved = ResolvedModel {
                name: model_name.clone(),
                qualified_name: format!("{category}.{provider_key}.{model_name}"),
                category: category.to_string(),
                provider_key: provider_key.clone(),
                sdk: entry
                    .sdk
                    .or(provider_config.sdk.clone())
                    .unwrap_or_else(|| default_sdk(provider_key)),
                model_id,
                api_key_env: entry.api_key_env.or(provider_config.api_key_env.clone()),
                base_url: entry.base_url.or(provider_config.base_url.clone()),
                max_context_tokens: entry
                    .max_context_tokens
                    .or(provider_config.max_context_tokens),
                max_tokens: entry.max_tokens.or(provider_config.max_tokens),
                temperature: entry.temperature.or(provider_config.temperature),
                top_p: entry.top_p.or(provider_config.top_p),
                reasoning_effort: entry
                    .reasoning_effort
                    .or(provider_config.reasoning_effort.clone()),
                budget_tokens: entry.budget_tokens.or(provider_config.budget_tokens),
                cache_ttl: entry.cache_ttl.or(provider_config.cache_ttl.clone()),
                cache_control_depth: entry
                    .cache_control_depth
                    .or(provider_config.cache_control_depth),
                keepalive_enabled: entry
                    .keepalive_enabled
                    .or(provider_config.keepalive_enabled),
                keepalive_ttl_minutes: entry
                    .keepalive_ttl_minutes
                    .or(provider_config.keepalive_ttl_minutes),
                keepalive_max_pings: entry
                    .keepalive_max_pings
                    .or(provider_config.keepalive_max_pings),
                openrouter_provider: entry
                    .openrouter_provider
                    .or(provider_config.openrouter_provider.clone()),
                vertex_project: entry
                    .vertex_project
                    .or(provider_config.vertex_project.clone()),
                vertex_location: entry
                    .vertex_location
                    .or(provider_config.vertex_location.clone()),
                gemini_generation: entry
                    .gemini_generation
                    .or(provider_config.gemini_generation),
                gemini_web_search: entry
                    .gemini_web_search
                    .or(provider_config.gemini_web_search),
            };

            models.insert(model_name.clone(), resolved);
        }
    }

    Ok(models)
}

// ── Provider defaults ───────────────────────────────────────────────────

/// Hardcoded provider defaults (ported from V1 `PROVIDER_DEFAULTS`).
fn hardcoded_defaults(provider_key: &str) -> ProviderConfig {
    match provider_key {
        "anthropic" => ProviderConfig {
            sdk: Some(Sdk::Anthropic),
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            temperature: Some(1.0),
            max_tokens: Some(8192),
            max_context_tokens: Some(200_000),
            cache_control_depth: Some(2),
            ..Default::default()
        },
        "openrouter" => ProviderConfig {
            sdk: Some(Sdk::Openai),
            api_key_env: Some("OPENROUTER_API_KEY".into()),
            temperature: Some(1.0),
            max_tokens: Some(8192),
            max_context_tokens: Some(200_000),
            ..Default::default()
        },
        "deepseek" => ProviderConfig {
            sdk: Some(Sdk::Deepseek),
            base_url: Some("https://api.deepseek.com/v1".into()),
            api_key_env: Some("DEEPSEEK_API_KEY".into()),
            temperature: Some(1.0),
            max_tokens: Some(8192),
            max_context_tokens: Some(200_000),
            ..Default::default()
        },
        "gemini" => ProviderConfig {
            sdk: Some(Sdk::Gemini),
            api_key_env: Some("GEMINI_API_KEY".into()),
            temperature: Some(1.0),
            max_tokens: Some(8192),
            max_context_tokens: Some(200_000),
            ..Default::default()
        },
        "xai" => ProviderConfig {
            sdk: Some(Sdk::Openai),
            base_url: Some("https://api.x.ai/v1".into()),
            api_key_env: Some("XAI_API_KEY".into()),
            temperature: Some(1.0),
            max_tokens: Some(8192),
            max_context_tokens: Some(200_000),
            ..Default::default()
        },
        "zhipuai" => ProviderConfig {
            sdk: Some(Sdk::Zhipuai),
            base_url: Some("https://open.bigmodel.cn/api/paas/v4".into()),
            api_key_env: Some("ZAI_API_KEY".into()),
            temperature: Some(1.0),
            max_tokens: Some(8192),
            max_context_tokens: Some(200_000),
            ..Default::default()
        },
        _ => ProviderConfig::default(),
    }
}

/// Default SDK for a provider key (used when neither hardcoded nor TOML specifies one).
fn default_sdk(provider_key: &str) -> Sdk {
    match provider_key {
        "anthropic" => Sdk::Anthropic,
        "gemini" => Sdk::Gemini,
        "zhipuai" => Sdk::Zhipuai,
        "deepseek" => Sdk::Deepseek,
        // Everything else (openrouter, xai, custom) defaults to OpenAI-compatible.
        _ => Sdk::Openai,
    }
}

/// Merge explicit provider config on top of defaults (only overwrite non-None fields).
fn merge_provider(base: &mut ProviderConfig, overlay: &ProviderConfig) {
    macro_rules! merge_opt {
        ($field:ident) => {
            if overlay.$field.is_some() {
                base.$field = overlay.$field.clone();
            }
        };
    }
    merge_opt!(sdk);
    merge_opt!(api_key_env);
    merge_opt!(base_url);
    merge_opt!(max_context_tokens);
    merge_opt!(max_tokens);
    merge_opt!(temperature);
    merge_opt!(top_p);
    merge_opt!(reasoning_effort);
    merge_opt!(budget_tokens);
    merge_opt!(cache_ttl);
    merge_opt!(cache_control_depth);
    merge_opt!(keepalive_enabled);
    merge_opt!(keepalive_ttl_minutes);
    merge_opt!(keepalive_max_pings);
    merge_opt!(openrouter_provider);
    merge_opt!(vertex_project);
    merge_opt!(vertex_location);
    merge_opt!(gemini_generation);
    merge_opt!(gemini_web_search);
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: parse a TOML string as a table.
    fn parse_table(s: &str) -> toml::Table {
        s.parse::<toml::Table>().unwrap()
    }

    // ── parse_category ──────────────────────────────────────────────

    #[test]
    fn parse_single_provider_single_model() {
        let table = parse_table(
            r#"
[anthropic]
sdk = "anthropic"
api_key_env = "MY_KEY"

[anthropic.opus]
model_id = "claude-opus-4-6"
"#,
        );
        let models = parse_category("chat", &table).unwrap();
        assert_eq!(models.len(), 1);

        let opus = &models["opus"];
        assert_eq!(opus.name, "opus");
        assert_eq!(opus.qualified_name, "chat.anthropic.opus");
        assert_eq!(opus.category, "chat");
        assert_eq!(opus.provider_key, "anthropic");
        assert_eq!(opus.sdk, Sdk::Anthropic);
        assert_eq!(opus.model_id, "claude-opus-4-6");
        assert_eq!(opus.api_key_env.as_deref(), Some("MY_KEY"));
    }

    #[test]
    fn provider_defaults_cascade_into_models() {
        let table = parse_table(
            r#"
[anthropic]
api_key_env = "SHARED_KEY"
max_context_tokens = 65536
cache_ttl = "1h"

[anthropic.opus]
model_id = "claude-opus-4-6"

[anthropic.sonnet]
model_id = "claude-sonnet-4-6"
cache_ttl = "5m"
"#,
        );
        let models = parse_category("chat", &table).unwrap();

        // opus inherits provider defaults
        let opus = &models["opus"];
        assert_eq!(opus.api_key_env.as_deref(), Some("SHARED_KEY"));
        assert_eq!(opus.max_context_tokens, Some(65536));
        assert_eq!(opus.cache_ttl.as_deref(), Some("1h"));

        // sonnet overrides cache_ttl
        let sonnet = &models["sonnet"];
        assert_eq!(sonnet.api_key_env.as_deref(), Some("SHARED_KEY"));
        assert_eq!(sonnet.cache_ttl.as_deref(), Some("5m"));
    }

    #[test]
    fn hardcoded_defaults_apply_when_no_toml_scalars() {
        let table = parse_table(
            r#"
[anthropic.opus]
model_id = "claude-opus-4-6"
"#,
        );
        let models = parse_category("chat", &table).unwrap();
        let opus = &models["opus"];

        // Should get hardcoded anthropic defaults.
        assert_eq!(opus.sdk, Sdk::Anthropic);
        assert_eq!(opus.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
        assert_eq!(opus.temperature, Some(1.0));
        assert_eq!(opus.max_tokens, Some(8192));
        assert_eq!(opus.max_context_tokens, Some(200_000));
    }

    #[test]
    fn toml_scalars_override_hardcoded_defaults() {
        let table = parse_table(
            r#"
[anthropic]
api_key_env = "CUSTOM_KEY"
temperature = 0.5

[anthropic.opus]
model_id = "claude-opus-4-6"
"#,
        );
        let models = parse_category("chat", &table).unwrap();
        let opus = &models["opus"];

        assert_eq!(opus.api_key_env.as_deref(), Some("CUSTOM_KEY"));
        assert_eq!(opus.temperature, Some(0.5));
        // max_tokens still from hardcoded defaults.
        assert_eq!(opus.max_tokens, Some(8192));
    }

    #[test]
    fn reserved_dict_key_not_treated_as_model() {
        let table = parse_table(
            r#"
[anthropic]
openrouter_provider = {order = ["Vertex AI"]}

[anthropic.opus]
model_id = "claude-opus-4-6"
"#,
        );
        let models = parse_category("chat", &table).unwrap();

        // Should only have "opus", not "openrouter_provider".
        assert_eq!(models.len(), 1);
        assert!(models.contains_key("opus"));

        // And the provider-level openrouter_provider should cascade.
        let opus = &models["opus"];
        assert!(opus.openrouter_provider.is_some());
    }

    #[test]
    fn missing_model_id_is_error() {
        let table = parse_table(
            r#"
[anthropic.opus]
temperature = 0.5
"#,
        );
        let err = parse_category("chat", &table).unwrap_err();
        assert!(matches!(err, CatalogError::MissingModelId { .. }));
    }

    #[test]
    fn multiple_providers() {
        let table = parse_table(
            r#"
[anthropic.opus]
model_id = "claude-opus-4-6"

[openrouter.gemini-pro]
model_id = "google/gemini-3.1-pro-preview"
"#,
        );
        let models = parse_category("chat", &table).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models["opus"].sdk, Sdk::Anthropic);
        assert_eq!(models["gemini-pro"].sdk, Sdk::Openai); // openrouter default
    }

    // ── ModelCatalog ────────────────────────────────────────────────

    #[test]
    fn find_model_by_short_name() {
        let chat = parse_table(
            r#"
[anthropic.opus]
model_id = "claude-opus-4-6"
"#,
        );
        let catalog = ModelCatalog::from_sections(Some(&chat), None, None, None).unwrap();
        let model = catalog.find_model("opus").unwrap();
        assert_eq!(model.model_id, "claude-opus-4-6");
    }

    #[test]
    fn find_model_by_qualified_name() {
        let chat = parse_table(
            r#"
[anthropic.opus]
model_id = "claude-opus-4-6"
"#,
        );
        let catalog = ModelCatalog::from_sections(Some(&chat), None, None, None).unwrap();
        let model = catalog.find_model("chat.anthropic.opus").unwrap();
        assert_eq!(model.name, "opus");
    }

    #[test]
    fn find_model_ambiguous_across_categories() {
        let chat = parse_table(
            r#"
[openrouter.fast]
model_id = "chat-fast"
"#,
        );
        let tools = parse_table(
            r#"
[openrouter.fast]
model_id = "tools-fast"
"#,
        );
        let catalog =
            ModelCatalog::from_sections(Some(&chat), Some(&tools), None, None).unwrap();

        // Ambiguous short name.
        let err = catalog.find_model("fast").unwrap_err();
        assert!(matches!(err, CatalogError::AmbiguousName { .. }));

        // Qualified names still work.
        let chat_model = catalog.find_model("chat.openrouter.fast").unwrap();
        assert_eq!(chat_model.model_id, "chat-fast");
        let tool_model = catalog.find_model("tools.openrouter.fast").unwrap();
        assert_eq!(tool_model.model_id, "tools-fast");
    }

    #[test]
    fn find_model_not_found() {
        let catalog = ModelCatalog::default();
        let err = catalog.find_model("nonexistent").unwrap_err();
        assert!(matches!(err, CatalogError::AmbiguousName { .. }));
    }

    #[test]
    fn empty_sections_produce_empty_catalog() {
        let catalog = ModelCatalog::from_sections(None, None, None, None).unwrap();
        assert!(catalog.chat.is_empty());
        assert!(catalog.tools.is_empty());
        assert!(catalog.embedding.is_empty());
        assert!(catalog.image_generation.is_empty());
    }

    #[test]
    fn embedding_and_image_gen_stored_as_raw_toml() {
        let embedding = parse_table(
            r#"
[text-large]
model_id = "openai/text-embedding-3-large"
api_key_env = "EMBED_KEY"
"#,
        );
        let image_gen = parse_table(
            r#"
[gemini-flash]
model_id = "google/gemini-3.1-flash-image-preview"
size = "1024x1024"
"#,
        );
        let catalog =
            ModelCatalog::from_sections(None, None, Some(&embedding), Some(&image_gen)).unwrap();
        assert!(catalog.embedding.contains_key("text-large"));
        assert!(catalog.image_generation.contains_key("gemini-flash"));
    }

    #[test]
    fn first_chat_model() {
        let chat = parse_table(
            r#"
[anthropic.opus]
model_id = "claude-opus-4-6"
"#,
        );
        let catalog = ModelCatalog::from_sections(Some(&chat), None, None, None).unwrap();
        assert!(catalog.first_chat_model().is_some());

        let empty = ModelCatalog::default();
        assert!(empty.first_chat_model().is_none());
    }

    // ── Sdk ─────────────────────────────────────────────────────────

    #[test]
    fn sdk_serialization() {
        assert_eq!(
            serde_json::to_value(Sdk::Anthropic).unwrap(),
            "anthropic"
        );
        assert_eq!(serde_json::to_value(Sdk::Openai).unwrap(), "openai");
        assert_eq!(serde_json::to_value(Sdk::Deepseek).unwrap(), "deepseek");
    }

    #[test]
    fn sdk_deserialization_from_toml() {
        #[derive(Deserialize)]
        struct T {
            sdk: Sdk,
        }
        let t: T = toml::from_str("sdk = \"anthropic\"").unwrap();
        assert_eq!(t.sdk, Sdk::Anthropic);
        let t: T = toml::from_str("sdk = \"openai\"").unwrap();
        assert_eq!(t.sdk, Sdk::Openai);
    }

    #[test]
    fn sdk_as_provider_str() {
        assert_eq!(Sdk::Anthropic.as_provider_str(), "anthropic");
        assert_eq!(Sdk::Openai.as_provider_str(), "openai");
        assert_eq!(Sdk::Gemini.as_provider_str(), "gemini");
        assert_eq!(Sdk::Zhipuai.as_provider_str(), "zhipuai");
        assert_eq!(Sdk::Deepseek.as_provider_str(), "deepseek");
    }
}
