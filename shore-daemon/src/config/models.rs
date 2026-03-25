use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Top-level models configuration loaded from models.toml.
///
/// Supports hierarchical provider-level defaults: keys set under
/// `[provider_defaults.<provider>]` are merged into every model that
/// shares that provider, unless the model overrides them explicitly.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelsConfig {
    /// Per-provider default values merged into models of that provider.
    #[serde(default)]
    pub provider_defaults: BTreeMap<String, ProviderDefaults>,

    /// Model profiles.
    #[serde(default)]
    pub models: Vec<ModelProfile>,
}

/// Default values applied to all models sharing a provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProviderDefaults {
    /// Default max tokens for this provider.
    pub max_tokens: Option<u32>,

    /// Default temperature for this provider.
    pub temperature: Option<f64>,

    /// Default top_p for this provider.
    pub top_p: Option<f64>,

    /// Default base URL override for this provider.
    pub base_url: Option<String>,

    /// Default API key environment variable name.
    pub api_key_env: Option<String>,
}

/// A single model profile in models.toml.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelProfile {
    /// User-facing name for this model (e.g. "claude-sonnet").
    pub name: String,

    /// Provider identifier (e.g. "anthropic", "openai", "gemini", "openrouter").
    pub provider: String,

    /// The provider's model identifier.
    pub model_id: String,

    /// Max tokens override.
    pub max_tokens: Option<u32>,

    /// Temperature override.
    pub temperature: Option<f64>,

    /// Top-p override.
    pub top_p: Option<f64>,

    /// Base URL override (for OpenAI-compatible providers).
    pub base_url: Option<String>,

    /// API key environment variable name override.
    pub api_key_env: Option<String>,
}

impl ModelsConfig {
    /// Look up a model profile by name.
    pub fn find_model(&self, name: &str) -> Option<&ModelProfile> {
        self.models.iter().find(|m| m.name == name)
    }

    /// Resolve a model profile with provider defaults merged in.
    /// Model-level values take precedence over provider defaults.
    pub fn resolve_model(&self, name: &str) -> Option<ResolvedModel> {
        let profile = self.find_model(name)?;
        let defaults = self.provider_defaults.get(&profile.provider);
        Some(ResolvedModel {
            name: profile.name.clone(),
            provider: profile.provider.clone(),
            model_id: profile.model_id.clone(),
            max_tokens: profile
                .max_tokens
                .or(defaults.and_then(|d| d.max_tokens)),
            temperature: profile
                .temperature
                .or(defaults.and_then(|d| d.temperature)),
            top_p: profile.top_p.or(defaults.and_then(|d| d.top_p)),
            base_url: profile
                .base_url
                .clone()
                .or(defaults.and_then(|d| d.base_url.clone())),
            api_key_env: profile
                .api_key_env
                .clone()
                .or(defaults.and_then(|d| d.api_key_env.clone())),
        })
    }
}

/// A model profile with provider defaults already merged in.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedModel {
    pub name: String,
    pub provider: String,
    pub model_id: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_models() {
        let toml_str = r#"
[[models]]
name = "claude-sonnet"
provider = "anthropic"
model_id = "claude-sonnet-4-20250514"
"#;
        let config: ModelsConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.models.len(), 1);
        assert_eq!(config.models[0].name, "claude-sonnet");
        assert_eq!(config.models[0].provider, "anthropic");
    }

    #[test]
    fn provider_defaults_merge() {
        let toml_str = r#"
[provider_defaults.anthropic]
max_tokens = 4096
api_key_env = "ANTHROPIC_API_KEY"

[[models]]
name = "claude-sonnet"
provider = "anthropic"
model_id = "claude-sonnet-4-20250514"

[[models]]
name = "claude-opus"
provider = "anthropic"
model_id = "claude-opus-4-20250514"
max_tokens = 8192
"#;
        let config: ModelsConfig = toml::from_str(toml_str).unwrap();

        // Model without override inherits provider defaults.
        let resolved = config.resolve_model("claude-sonnet").unwrap();
        assert_eq!(resolved.max_tokens, Some(4096));
        assert_eq!(resolved.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));

        // Model with override uses its own value.
        let resolved = config.resolve_model("claude-opus").unwrap();
        assert_eq!(resolved.max_tokens, Some(8192));
        assert_eq!(resolved.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn find_model_returns_none_for_missing() {
        let config = ModelsConfig::default();
        assert!(config.find_model("nonexistent").is_none());
        assert!(config.resolve_model("nonexistent").is_none());
    }

    #[test]
    fn empty_toml_gives_defaults() {
        let config: ModelsConfig = toml::from_str("").unwrap();
        assert!(config.models.is_empty());
        assert!(config.provider_defaults.is_empty());
    }
}
