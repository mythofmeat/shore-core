//! Top-level provider registry: `[providers.<name>]` config sections.
//!
//! The registry is a first-class concept that lets users configure a
//! provider once (sdk, base_url, ordered API keys, discovery) instead of
//! defining every model inline. This module is parse-only — request
//! resolution, key fallback, and discovery live in their respective
//! phases. Existing `[chat.<provider>.<model>]` static catalog entries
//! are unaffected and continue to work as before.
//!
//! Schema:
//!
//! ```toml
//! [providers.openrouter]
//! enabled = true
//! sdk = "openai"
//! base_url = "https://openrouter.ai/api/v1"
//!
//! [[providers.openrouter.keys]]
//! name = "budget"
//! env = "OPENROUTER_API_KEY_BUDGET"
//! warn_on_fallback = true
//!
//! [[providers.openrouter.keys]]
//! name = "overflow"
//! env = "OPENROUTER_API_KEY_OVERFLOW"
//!
//! [providers.openrouter.discovery]
//! enabled = true
//! ```
//!
//! Compact compatibility form (single key, no name):
//!
//! ```toml
//! [providers.openai]
//! api_key_env = "OPENAI_API_KEY"
//! ```
//!
//! When `api_key_env` is present, the registry synthesizes a single
//! `ProviderKeyEntry` named `"default"`. Combining `api_key_env` with an
//! explicit `[[keys]]` array is rejected — pick one form per provider.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::models::Sdk;

// ── Errors ──────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ProviderRegistryError {
    #[error("failed to parse [providers.{provider}]: {source}")]
    ParseEntry {
        provider: String,
        source: Box<toml::de::Error>,
    },

    #[error(
        "[providers.{provider}] declares both `api_key_env` and explicit `[[keys]]`; \
         use one form per provider"
    )]
    ConflictingKeyForms { provider: String },

    #[error("[providers.{provider}] key #{index} is missing `{field}`")]
    MissingKeyField {
        provider: String,
        index: usize,
        field: &'static str,
    },

    #[error(
        "[providers.{provider}] has duplicate key name {name:?}; \
         each key under a provider must have a unique name"
    )]
    DuplicateKeyName { provider: String, name: String },
}

// ── Discovery ───────────────────────────────────────────────────────────

/// `[providers.<name>.discovery]` sub-block.
///
/// Phase 1 only carries `enabled`. Visibility filtering (Phase 6) and other
/// discovery knobs land in their own phases.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderDiscovery {
    /// Whether discovery is enabled for this provider.
    pub enabled: bool,
}

// ── Per-key entry ───────────────────────────────────────────────────────

fn default_key_enabled() -> bool {
    true
}

/// One entry from `[[providers.<name>.keys]]`.
///
/// Resolved in configured order on every request (Phase 4). Disabled keys
/// are parsed and preserved but excluded from active credential resolution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderKeyEntry {
    /// Friendly key name surfaced in fallback warnings (e.g. `"budget"`).
    /// Must be unique within a provider.
    pub name: String,

    /// Env var that holds the actual API key value.
    pub env: String,

    /// Whether to consider this key when resolving credentials.
    #[serde(default = "default_key_enabled")]
    pub enabled: bool,

    /// If set, a fallback away from this key emits a visible client warning.
    /// Used to flag a "budget" key whose exhaustion is significant enough
    /// to surface to the user.
    #[serde(default)]
    pub warn_on_fallback: bool,
}

// ── Per-provider entry ──────────────────────────────────────────────────

fn default_provider_enabled() -> bool {
    true
}

/// One `[providers.<name>]` entry.
///
/// `enabled = false` parses but the provider is excluded from
/// runtime resolution (discovery, credential lookup, etc.). The plumbing
/// to honor this lands in Phases 4–5; Phase 1 only validates the schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProviderEntry {
    #[serde(default = "default_provider_enabled")]
    pub enabled: bool,

    /// Wire protocol (e.g. `"openai"`, `"anthropic"`). Optional here;
    /// downstream resolution can fall back to per-model SDK or the
    /// hardcoded `models::default_sdk(provider_key)`.
    #[serde(default)]
    pub sdk: Option<Sdk>,

    /// Base URL for the provider's HTTP API.
    #[serde(default)]
    pub base_url: Option<String>,

    /// Compact single-key form. Mutually exclusive with `keys`.
    /// Normalized into a synthetic `ProviderKeyEntry` named `"default"`.
    #[serde(default)]
    pub api_key_env: Option<String>,

    /// Ordered list of API keys to try (Phase 4 fallback order).
    /// Empty when only `api_key_env` is used or when the provider
    /// inherits credentials elsewhere.
    #[serde(default)]
    pub keys: Vec<ProviderKeyEntry>,

    #[serde(default)]
    pub discovery: ProviderDiscovery,
}

impl Default for ProviderEntry {
    fn default() -> Self {
        Self {
            enabled: default_provider_enabled(),
            sdk: None,
            base_url: None,
            api_key_env: None,
            keys: Vec::new(),
            discovery: ProviderDiscovery::default(),
        }
    }
}

impl ProviderEntry {
    /// Iterate the provider's keys in configured order, including disabled ones.
    /// After parsing, this is the canonical key list (compact `api_key_env`
    /// has been folded in already).
    pub fn keys(&self) -> &[ProviderKeyEntry] {
        &self.keys
    }

    /// Iterate only the enabled keys, in configured order.
    pub fn enabled_keys(&self) -> impl Iterator<Item = &ProviderKeyEntry> {
        self.keys.iter().filter(|k| k.enabled)
    }
}

// ── Registry ────────────────────────────────────────────────────────────

/// Parsed `[providers]` section: provider key → entry.
///
/// Iteration order is the BTreeMap's lexicographic order over provider
/// names, which matches the existing model catalog's behavior.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderRegistry {
    providers: BTreeMap<String, ProviderEntry>,
}

impl ProviderRegistry {
    /// Build a registry from the raw `providers` section, if present.
    pub fn from_section(section: Option<&toml::Table>) -> Result<Self, ProviderRegistryError> {
        let Some(table) = section else {
            return Ok(Self::default());
        };

        let mut providers = BTreeMap::new();
        for (name, value) in table {
            let entry = parse_entry(name, value.clone())?;
            providers.insert(name.clone(), entry);
        }
        Ok(Self { providers })
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn get(&self, provider_key: &str) -> Option<&ProviderEntry> {
        self.providers.get(provider_key)
    }

    /// Iterate `(provider_key, entry)` pairs in lexicographic order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ProviderEntry)> {
        self.providers.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Iterate only enabled providers.
    pub fn enabled(&self) -> impl Iterator<Item = (&str, &ProviderEntry)> {
        self.iter().filter(|(_, e)| e.enabled)
    }
}

// ── Parsing ─────────────────────────────────────────────────────────────

fn parse_entry(provider: &str, value: toml::Value) -> Result<ProviderEntry, ProviderRegistryError> {
    let mut entry: ProviderEntry =
        value
            .try_into()
            .map_err(|e| ProviderRegistryError::ParseEntry {
                provider: provider.to_string(),
                source: Box::new(e),
            })?;

    // Reject the "both forms" case explicitly — silent precedence between
    // compact and named-key forms invites surprise. Pick one per provider.
    if entry.api_key_env.is_some() && !entry.keys.is_empty() {
        return Err(ProviderRegistryError::ConflictingKeyForms {
            provider: provider.to_string(),
        });
    }

    // Validate explicit `[[keys]]` entries (name and env are non-empty,
    // names are unique within the provider).
    let mut seen = std::collections::HashSet::new();
    for (idx, key) in entry.keys.iter().enumerate() {
        if key.name.is_empty() {
            return Err(ProviderRegistryError::MissingKeyField {
                provider: provider.to_string(),
                index: idx,
                field: "name",
            });
        }
        if key.env.is_empty() {
            return Err(ProviderRegistryError::MissingKeyField {
                provider: provider.to_string(),
                index: idx,
                field: "env",
            });
        }
        if !seen.insert(key.name.clone()) {
            return Err(ProviderRegistryError::DuplicateKeyName {
                provider: provider.to_string(),
                name: key.name.clone(),
            });
        }
    }

    // Fold compact `api_key_env` into a synthetic `default` key so
    // downstream consumers only ever see the named-key form.
    if let Some(env) = entry.api_key_env.take() {
        entry.keys.push(ProviderKeyEntry {
            name: "default".into(),
            env,
            enabled: true,
            warn_on_fallback: false,
        });
    }

    Ok(entry)
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_table(s: &str) -> toml::Table {
        s.parse::<toml::Table>().unwrap()
    }

    fn registry_from(s: &str) -> ProviderRegistry {
        let table = parse_table(s);
        let providers = table.get("providers").and_then(|v| v.as_table());
        ProviderRegistry::from_section(providers).unwrap()
    }

    #[test]
    fn empty_section_yields_empty_registry() {
        let r = ProviderRegistry::from_section(None).unwrap();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn full_named_key_form() {
        let r = registry_from(
            r#"
[providers.openrouter]
enabled = true
sdk = "openai"
base_url = "https://openrouter.ai/api/v1"

[[providers.openrouter.keys]]
name = "budget"
env = "OPENROUTER_API_KEY_BUDGET"
warn_on_fallback = true

[[providers.openrouter.keys]]
name = "overflow"
env = "OPENROUTER_API_KEY_OVERFLOW"

[providers.openrouter.discovery]
enabled = true
"#,
        );

        let or = r.get("openrouter").expect("openrouter present");
        assert!(or.enabled);
        assert_eq!(or.sdk, Some(Sdk::Openai));
        assert_eq!(or.base_url.as_deref(), Some("https://openrouter.ai/api/v1"));
        assert!(or.api_key_env.is_none(), "compact form folded");
        assert_eq!(or.keys.len(), 2);
        assert_eq!(or.keys[0].name, "budget");
        assert_eq!(or.keys[0].env, "OPENROUTER_API_KEY_BUDGET");
        assert!(or.keys[0].enabled);
        assert!(or.keys[0].warn_on_fallback);
        assert_eq!(or.keys[1].name, "overflow");
        assert_eq!(or.keys[1].env, "OPENROUTER_API_KEY_OVERFLOW");
        assert!(!or.keys[1].warn_on_fallback);
        assert!(or.discovery.enabled);
    }

    #[test]
    fn keys_preserve_configured_order() {
        let r = registry_from(
            r#"
[[providers.openrouter.keys]]
name = "first"
env = "A"

[[providers.openrouter.keys]]
name = "second"
env = "B"

[[providers.openrouter.keys]]
name = "third"
env = "C"
"#,
        );
        let names: Vec<&str> = r
            .get("openrouter")
            .unwrap()
            .keys()
            .iter()
            .map(|k| k.name.as_str())
            .collect();
        assert_eq!(names, vec!["first", "second", "third"]);
    }

    #[test]
    fn compact_api_key_env_form_synthesizes_default_key() {
        let r = registry_from(
            r#"
[providers.openai]
api_key_env = "OPENAI_API_KEY"
"#,
        );
        let p = r.get("openai").unwrap();
        assert!(p.enabled, "default enabled");
        assert!(p.api_key_env.is_none(), "compact form folded into keys[]");
        assert_eq!(p.keys.len(), 1);
        assert_eq!(p.keys[0].name, "default");
        assert_eq!(p.keys[0].env, "OPENAI_API_KEY");
        assert!(p.keys[0].enabled);
        assert!(!p.keys[0].warn_on_fallback);
    }

    #[test]
    fn provider_enabled_defaults_to_true() {
        let r = registry_from(
            r#"
[providers.openai]
api_key_env = "OPENAI_API_KEY"
"#,
        );
        assert!(r.get("openai").unwrap().enabled);
    }

    #[test]
    fn disabled_provider_parses_but_filtered_from_enabled() {
        let r = registry_from(
            r#"
[providers.disabled_provider]
enabled = false
api_key_env = "FOO"

[providers.active_provider]
api_key_env = "BAR"
"#,
        );
        assert_eq!(r.len(), 2, "disabled provider still in registry");
        let enabled_names: Vec<&str> = r.enabled().map(|(name, _)| name).collect();
        assert_eq!(enabled_names, vec!["active_provider"]);
    }

    #[test]
    fn disabled_keys_parsed_but_filtered_from_enabled_keys() {
        let r = registry_from(
            r#"
[[providers.openrouter.keys]]
name = "active"
env = "A"

[[providers.openrouter.keys]]
name = "off"
env = "B"
enabled = false

[[providers.openrouter.keys]]
name = "also_active"
env = "C"
"#,
        );
        let p = r.get("openrouter").unwrap();
        assert_eq!(p.keys().len(), 3, "disabled keys preserved");
        let enabled: Vec<&str> = p.enabled_keys().map(|k| k.name.as_str()).collect();
        assert_eq!(enabled, vec!["active", "also_active"]);
    }

    #[test]
    fn warn_on_fallback_defaults_to_false() {
        let r = registry_from(
            r#"
[[providers.openrouter.keys]]
name = "k"
env = "E"
"#,
        );
        assert!(!r.get("openrouter").unwrap().keys[0].warn_on_fallback);
    }

    #[test]
    fn discovery_enabled_defaults_to_false() {
        let r = registry_from(
            r#"
[providers.openrouter]
api_key_env = "K"
"#,
        );
        assert!(!r.get("openrouter").unwrap().discovery.enabled);
    }

    #[test]
    fn rejects_both_compact_and_named_key_forms() {
        let table = parse_table(
            r#"
[providers.openrouter]
api_key_env = "OR_KEY"

[[providers.openrouter.keys]]
name = "explicit"
env = "OTHER"
"#,
        );
        let providers = table.get("providers").and_then(|v| v.as_table());
        let err = ProviderRegistry::from_section(providers).unwrap_err();
        assert!(matches!(
            err,
            ProviderRegistryError::ConflictingKeyForms { ref provider }
                if provider == "openrouter"
        ));
    }

    #[test]
    fn rejects_duplicate_key_names() {
        let table = parse_table(
            r#"
[[providers.openrouter.keys]]
name = "dup"
env = "A"

[[providers.openrouter.keys]]
name = "dup"
env = "B"
"#,
        );
        let providers = table.get("providers").and_then(|v| v.as_table());
        let err = ProviderRegistry::from_section(providers).unwrap_err();
        assert!(matches!(
            err,
            ProviderRegistryError::DuplicateKeyName { ref name, .. } if name == "dup"
        ));
    }

    #[test]
    fn rejects_key_with_missing_env() {
        // toml `name = "x"` without `env` triggers serde missing-field, which
        // surfaces as ParseEntry. (We don't reach the post-deserialize
        // empty-string branch in normal config — that catches programmatic
        // construction errors.)
        let table = parse_table(
            r#"
[[providers.openrouter.keys]]
name = "k"
"#,
        );
        let providers = table.get("providers").and_then(|v| v.as_table());
        let err = ProviderRegistry::from_section(providers).unwrap_err();
        assert!(
            matches!(err, ProviderRegistryError::ParseEntry { .. }),
            "expected parse error, got {err:?}"
        );
    }

    #[test]
    fn rejects_unknown_provider_field() {
        let table = parse_table(
            r#"
[providers.openrouter]
api_key_env = "K"
typo_field = true
"#,
        );
        let providers = table.get("providers").and_then(|v| v.as_table());
        let err = ProviderRegistry::from_section(providers).unwrap_err();
        match err {
            ProviderRegistryError::ParseEntry { source, .. } => {
                assert!(
                    source.to_string().contains("unknown field"),
                    "expected unknown-field error, got: {source}"
                );
            }
            other => panic!("expected ParseEntry, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_key_field() {
        let table = parse_table(
            r#"
[[providers.openrouter.keys]]
name = "k"
env = "E"
typo = "bad"
"#,
        );
        let providers = table.get("providers").and_then(|v| v.as_table());
        let err = ProviderRegistry::from_section(providers).unwrap_err();
        match err {
            ProviderRegistryError::ParseEntry { source, .. } => {
                assert!(source.to_string().contains("unknown field"));
            }
            other => panic!("expected ParseEntry, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_discovery_field() {
        let table = parse_table(
            r#"
[providers.openrouter]
api_key_env = "K"

[providers.openrouter.discovery]
enabled = true
visibility = ["*"]
"#,
        );
        let providers = table.get("providers").and_then(|v| v.as_table());
        let err = ProviderRegistry::from_section(providers).unwrap_err();
        // Visibility is a Phase 6 field; until then it must be rejected so
        // typos surface as errors instead of silently no-op'ing.
        match err {
            ProviderRegistryError::ParseEntry { source, .. } => {
                assert!(source.to_string().contains("unknown field"));
            }
            other => panic!("expected ParseEntry, got {other:?}"),
        }
    }

    #[test]
    fn multiple_providers_iterate_in_lexicographic_order() {
        let r = registry_from(
            r#"
[providers.openrouter]
api_key_env = "OR"

[providers.anthropic]
api_key_env = "ANTH"

[providers.gemini]
api_key_env = "GEM"
"#,
        );
        let names: Vec<&str> = r.iter().map(|(k, _)| k).collect();
        assert_eq!(names, vec!["anthropic", "gemini", "openrouter"]);
    }
}
