//! Model catalog: nested `[chat.<provider>.<model>]` structure with
//! provider-level defaults cascading into per-model entries.
//!
//! The parsing mirrors V1's `_load_category_profiles()` approach:
//! for each provider table, scalar keys become provider defaults and
//! sub-table keys become model entries that inherit those defaults.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::duration::ConfigDuration;

// ── SDK enum ────────────────────────────────────────────────────────────

/// SDK/wire protocol.  Distinguishes the message format from the gateway.
///
/// For example, `Anthropic` with a custom `base_url` pointing at OpenRouter
/// means "use the Anthropic message format, but send requests to OpenRouter."
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Sdk {
    #[default]
    Anthropic,
    Openai,
    /// OpenRouter's first-party SDK (`@openrouter/sdk`) — the normalized path
    /// for non-Anthropic models routed via OpenRouter (DeepSeek, Kimi, GLM,
    /// MiniMax, GPT, xAI). OpenRouter folds each vendor's bespoke reasoning
    /// shape into one typed `reasoning_details` array, round-tripped opaquely.
    Openrouter,
    Gemini,
    Zai,
}

impl<'de> Deserialize<'de> for Sdk {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "anthropic" => Ok(Sdk::Anthropic),
            "openai" => Ok(Sdk::Openai),
            "openrouter" => Ok(Sdk::Openrouter),
            "gemini" => Ok(Sdk::Gemini),
            "zai" => Ok(Sdk::Zai),
            "deepseek" | "zhipuai" => {
                warn!(
                    "sdk = \"{s}\" is deprecated and now maps to \"openai\". \
                     Update your config to use sdk = \"openai\" instead."
                );
                Ok(Sdk::Openai)
            }
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["anthropic", "openai", "openrouter", "gemini", "zai"],
            )),
        }
    }
}

impl Sdk {
    /// Wire protocol string sent to shore-llm.
    pub fn as_str(&self) -> &'static str {
        match self {
            Sdk::Anthropic => "anthropic",
            Sdk::Openai => "openai",
            Sdk::Openrouter => "openrouter",
            Sdk::Gemini => "gemini",
            Sdk::Zai => "zai",
        }
    }

    /// Parse a wire-protocol string back into an `Sdk`. `None` for unknown
    /// strings — the caller decides whether to fall back or error. Named
    /// `parse_wire` rather than `from_str` to avoid colliding with
    /// [`std::str::FromStr`] (which would force a stricter error type).
    pub fn parse_wire(s: &str) -> Option<Self> {
        match s {
            "anthropic" => Some(Sdk::Anthropic),
            "openai" => Some(Sdk::Openai),
            "openrouter" => Some(Sdk::Openrouter),
            "gemini" => Some(Sdk::Gemini),
            "zai" => Some(Sdk::Zai),
            _ => None,
        }
    }

    /// Whether this SDK's wire protocol requires the daemon to echo
    /// **unsigned** reasoning text back to the provider on the next
    /// request. Anthropic signs its thinking blocks and rejects requests
    /// that include unsigned reasoning text from a prior turn; OpenAI and
    /// Z.AI, in contrast, expect the assistant's prior `reasoning_content`
    /// to round-trip verbatim. Gemini doesn't accept reasoning replay.
    ///
    /// Centralized here so chat and heartbeat sites can derive the same
    /// value from one source — keeping the cache prefix consistent
    /// between live chat and the heartbeat tick that reuses it. A bare
    /// `match` at each call site was the kind of drift surface the
    /// 2026-05-14 refactor was set up to eliminate.
    pub fn echoes_unsigned_thinking(&self) -> bool {
        matches!(self, Sdk::Openai | Sdk::Zai)
    }

    /// Whether requests for this SDK ultimately hit Anthropic's prompt-cache
    /// machinery (so a background task that wants to reuse the chat-warmed
    /// prefix has to preserve `request.system` verbatim and attach trailing
    /// instructions as an inline `role:"system"` entry instead).
    ///
    /// True for `Anthropic`. False for OpenAI / Gemini / Z.AI — those
    /// translate an inline `role:"system"` into a mid-history
    /// `<system_instruction>` user-role (or raw `role:"system"` on Z.AI),
    /// which is a materially different wire shape than a top-level system
    /// block. Background tasks should only move their fresh-path system
    /// prompt into an inline `role:"system"` (via
    /// `LlmRequest::push_inline_system`) on SDKs where this is true.
    pub fn uses_anthropic_prompt_cache(&self) -> bool {
        matches!(self, Sdk::Anthropic)
    }
}

// ── Shared model config fields ──────────────────────────────────────────

/// The 20 configuration fields shared by provider configs, model entries,
/// and resolved models.  All fields are `Option<T>` — `None` means "inherit
/// from the next level up" (model → provider → hardcoded defaults).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct ModelConfigFields {
    pub sdk: Option<Sdk>,
    pub api_key_env: Option<String>,
    pub base_url: Option<String>,
    pub max_context_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub reasoning_effort: Option<String>,
    pub budget_tokens: Option<u32>,
    pub cache_ttl: Option<String>,
    pub keepalive_enabled: Option<bool>,
    pub keepalive_ttl: Option<ConfigDuration>,
    pub keepalive_max_pings: Option<u32>,
    pub openrouter_provider: Option<toml::Value>,
    pub vertex_project: Option<String>,
    pub vertex_location: Option<String>,
    pub gemini_generation: Option<u32>,
    pub gemini_web_search: Option<bool>,
    pub zai_clear_thinking: Option<bool>,
    pub zai_subscription: Option<bool>,
}

impl ModelConfigFields {
    /// Overwrite `self` fields with any non-None fields from `overlay`.
    pub fn merge_from(&mut self, overlay: &Self) {
        macro_rules! merge_opt {
            ($field:ident) => {
                if overlay.$field.is_some() {
                    self.$field = overlay.$field.clone();
                }
            };
        }
        merge_opt!(sdk);
        merge_opt!(api_key_env);
        merge_opt!(base_url);
        merge_opt!(max_context_tokens);
        merge_opt!(max_output_tokens);
        merge_opt!(temperature);
        merge_opt!(top_p);
        merge_opt!(reasoning_effort);
        merge_opt!(budget_tokens);
        merge_opt!(cache_ttl);
        merge_opt!(keepalive_enabled);
        merge_opt!(keepalive_ttl);
        merge_opt!(keepalive_max_pings);
        merge_opt!(openrouter_provider);
        merge_opt!(vertex_project);
        merge_opt!(vertex_location);
        merge_opt!(gemini_generation);
        merge_opt!(gemini_web_search);
        merge_opt!(zai_clear_thinking);
        merge_opt!(zai_subscription);
    }

    /// Produce a new `ModelConfigFields` where each field is taken from `self`
    /// if present, otherwise from `fallback`.
    #[must_use]
    pub fn or_fallback(&self, fallback: &Self) -> Self {
        macro_rules! or_opt {
            ($field:ident) => {
                self.$field.clone().or(fallback.$field.clone())
            };
        }
        Self {
            sdk: or_opt!(sdk),
            api_key_env: or_opt!(api_key_env),
            base_url: or_opt!(base_url),
            max_context_tokens: or_opt!(max_context_tokens),
            max_output_tokens: or_opt!(max_output_tokens),
            temperature: or_opt!(temperature),
            top_p: or_opt!(top_p),
            reasoning_effort: or_opt!(reasoning_effort),
            budget_tokens: or_opt!(budget_tokens),
            cache_ttl: or_opt!(cache_ttl),
            keepalive_enabled: or_opt!(keepalive_enabled),
            keepalive_ttl: or_opt!(keepalive_ttl),
            keepalive_max_pings: or_opt!(keepalive_max_pings),
            openrouter_provider: or_opt!(openrouter_provider),
            vertex_project: or_opt!(vertex_project),
            vertex_location: or_opt!(vertex_location),
            gemini_generation: or_opt!(gemini_generation),
            gemini_web_search: or_opt!(gemini_web_search),
            zai_clear_thinking: or_opt!(zai_clear_thinking),
            zai_subscription: or_opt!(zai_subscription),
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
    #[serde(flatten)]
    pub fields: ModelConfigFields,
}

// ── Model entry ─────────────────────────────────────────────────────────

/// Per-model configuration — sub-tables under `[chat.<provider>.<model>]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct ModelEntry {
    /// The upstream model identifier (e.g. `"claude-opus-4-6"`).  Required.
    pub model_id: Option<String>,
    /// All overrides — None means inherit from provider.
    #[serde(flatten)]
    pub fields: ModelConfigFields,
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
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub reasoning_effort: Option<String>,
    pub budget_tokens: Option<u32>,
    pub cache_ttl: Option<String>,
    pub keepalive_enabled: Option<bool>,
    pub keepalive_ttl: Option<ConfigDuration>,
    pub keepalive_max_pings: Option<u32>,
    pub openrouter_provider: Option<toml::Value>,
    pub vertex_project: Option<String>,
    pub vertex_location: Option<String>,
    pub gemini_generation: Option<u32>,
    pub gemini_web_search: Option<bool>,
    pub zai_clear_thinking: Option<bool>,
    pub zai_subscription: Option<bool>,
    /// Per-model override for preserving prior-turn extended-thinking blocks
    /// in outgoing requests. `None` means "inherit the global
    /// `[memory.thinking].preserve_prior_turns`". Not sourced from the static
    /// `[chat.*]` catalog — it is stamped here by the runtime preference
    /// overlay (`preferences::apply_sampler_overlay`). The quality effect is
    /// model-dependent (issue #129), so there is no opinionated default.
    pub preserve_prior_turns: Option<bool>,
}

impl ResolvedModel {
    /// Build a `ResolvedModel` from metadata + merged config fields.
    ///
    /// `sdk_fallback` is used if `fields.sdk` is `None`.
    pub fn from_parts(
        name: String,
        qualified_name: String,
        category: String,
        provider_key: String,
        model_id: String,
        sdk_fallback: Sdk,
        fields: ModelConfigFields,
    ) -> Self {
        // Anthropic-slug auto-promotion: OpenRouter (and similar gateways)
        // accept Anthropic-shape `/v1/messages` for `anthropic/*` models, and
        // the Anthropic SDK is the only path that emits `cache_control`. If
        // the user didn't pin an SDK explicitly, route any `anthropic/*`
        // model_id through Sdk::Anthropic so caching works by default.
        let sdk = match fields.sdk.clone() {
            Some(s) => s,
            None if model_id.starts_with("anthropic/") => Sdk::Anthropic,
            None => sdk_fallback,
        };

        // Anthropic prompt caching is opt-in on the wire — `cache_control`
        // blocks are only added when `cache_ttl` is `Some` and non-empty.
        // Default to "1h" for the Anthropic SDK so users get caching without
        // explicit config, matching the billing-side default in
        // backend/ledger/src/pricing.rs. Set `cache_ttl = ""` to disable.
        let cache_ttl = match (&sdk, fields.cache_ttl) {
            (Sdk::Anthropic, None) => Some("1h".to_string()),
            (_, other) => other,
        };

        Self {
            name,
            qualified_name,
            category,
            provider_key,
            sdk,
            model_id,
            api_key_env: fields.api_key_env,
            base_url: fields.base_url,
            max_context_tokens: fields.max_context_tokens,
            max_output_tokens: fields.max_output_tokens,
            temperature: fields.temperature,
            top_p: fields.top_p,
            reasoning_effort: fields.reasoning_effort,
            budget_tokens: fields.budget_tokens,
            cache_ttl,
            keepalive_enabled: fields.keepalive_enabled,
            keepalive_ttl: fields.keepalive_ttl,
            keepalive_max_pings: fields.keepalive_max_pings,
            openrouter_provider: fields.openrouter_provider,
            vertex_project: fields.vertex_project,
            vertex_location: fields.vertex_location,
            gemini_generation: fields.gemini_generation,
            gemini_web_search: fields.gemini_web_search,
            zai_clear_thinking: fields.zai_clear_thinking,
            zai_subscription: fields.zai_subscription,
            // The static catalog has no `preserve_prior_turns` field; the
            // value is supplied later by the runtime preference overlay
            // (issue #129). `None` here means "inherit the global default".
            preserve_prior_turns: None,
        }
    }
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
    /// Explicit `[chat.<provider>]` provider-level config fields, keyed by
    /// provider. Static `[chat.<provider>.<model>]` entries already fold these
    /// in at parse time; this map is retained so the *discovered* model path
    /// (`effective_catalog::build_resolved_from_discovered`) can apply the same
    /// provider-level defaults (routing, cache_ttl, sampler knobs) that static
    /// models inherit. Only providers with at least one explicit field appear.
    pub chat_provider_defaults: BTreeMap<String, ModelConfigFields>,
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
        source: Box<toml::de::Error>,
    },
    #[error("ambiguous model name \"{name}\" — found in: {locations}")]
    AmbiguousName { name: String, locations: String },
    #[error("model \"{name}\" not found")]
    NotFound { name: String },
    #[error(
        "[{category}.claude_code] is no longer supported — the Claude Code transport \
         was removed; drop this section from your config"
    )]
    RemovedProvider { category: String },
}

/// Dict-valued TOML keys at the provider level that are config fields,
/// NOT model sub-tables.  Mirrors V1's `_RESERVED_DICT_KEYS`.
const RESERVED_DICT_KEYS: &[&str] = &["openrouter_provider"];

impl ModelCatalog {
    /// Build a catalog from the raw TOML table (the full merged config).
    ///
    /// Extracts `chat`, `tools`, `embedding`, and `image_generation` sections.
    /// See `from_sections_with_providers` for the variant that lets the
    /// `[providers.<name>]` registry cascade transport defaults (`sdk`,
    /// `base_url`, `api_key_env`) into static model entries.
    pub fn from_sections(
        chat: Option<&toml::Table>,
        tools: Option<&toml::Table>,
        embedding: Option<&toml::Table>,
        image_generation: Option<&toml::Table>,
    ) -> Result<Self, CatalogError> {
        Self::from_sections_with_providers(chat, tools, embedding, image_generation, None)
    }

    /// Build a catalog with optional provider-registry transport overlay.
    ///
    /// When `providers` is `Some`, each `[chat.<name>]` entry inherits
    /// the registry's `sdk`, `base_url`, and `api_key_env` as defaults
    /// (lower precedence than `[chat.<name>]` scalars and per-model
    /// fields, higher than the hardcoded provider defaults). This lets
    /// custom OpenAI-compatible providers configured solely under
    /// `[providers.<name>]` route their static aliases through the
    /// correct transport without duplicating fields under `[chat.<name>]`.
    pub fn from_sections_with_providers(
        chat: Option<&toml::Table>,
        tools: Option<&toml::Table>,
        embedding: Option<&toml::Table>,
        image_generation: Option<&toml::Table>,
        providers: Option<&crate::providers::ProviderRegistry>,
    ) -> Result<Self, CatalogError> {
        let (chat_models, chat_provider_defaults) = match chat {
            Some(t) => parse_category("chat", t, providers)?,
            None => (BTreeMap::new(), BTreeMap::new()),
        };
        // Discovered models are chat-only, so tool-level provider defaults are
        // never consulted; discard them.
        let (tool_models, _) = match tools {
            Some(t) => parse_category("tools", t, providers)?,
            None => (BTreeMap::new(), BTreeMap::new()),
        };

        // Embedding and image_generation are stored as raw TOML for now.
        let embedding_profiles = match embedding {
            Some(t) => t.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            None => BTreeMap::new(),
        };
        let image_gen_profiles = match image_generation {
            Some(t) => t.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            None => BTreeMap::new(),
        };

        let catalog = Self {
            chat: chat_models,
            tools: tool_models,
            embedding: embedding_profiles,
            image_generation: image_gen_profiles,
            chat_provider_defaults,
        };
        info!(
            chat_models = catalog.chat.len(),
            tool_models = catalog.tools.len(),
            "Model catalog initialized"
        );
        Ok(catalog)
    }

    /// Look up a model by short name or qualified name.
    ///
    /// Qualified names (`"chat.anthropic.opus"`) are tried first.
    /// Short names (`"opus"`) search across all categories and error
    /// on ambiguity.
    pub fn find_model(&self, name: &str) -> Result<&ResolvedModel, CatalogError> {
        debug!(name, "Looking up model in catalog");

        // 1. Try qualified name match.
        for model in self.chat.values().chain(self.tools.values()) {
            if model.qualified_name == name {
                debug!(
                    name,
                    qualified_name = name,
                    "Model resolved by qualified name"
                );
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

        // Both miss arms return a descriptive `CatalogError`; the caller owns
        // the severity decision. Don't `warn!` here — this lookup is also used
        // as a speculative probe (e.g. `effective_catalog::find_effective_model`
        // tries the static catalog first, then falls back to discovery), where
        // a miss is expected and not worth a warning. Terminal callers that
        // treat a miss as a real misconfiguration log it themselves with
        // context (see `resolve_background_model`, `apply_heartbeat_model_override`).
        match matches.len() {
            0 => {
                debug!(name, "Model not found in static catalog");
                Err(CatalogError::NotFound {
                    name: name.to_string(),
                })
            }
            1 => {
                debug!(
                    name,
                    qualified_name = matches[0].qualified_name,
                    "Model resolved by short name"
                );
                Ok(matches[0])
            }
            _ => {
                let locations: Vec<&str> =
                    matches.iter().map(|m| m.qualified_name.as_str()).collect();
                debug!(
                    name,
                    locations = locations.join(", "),
                    "Ambiguous model name — found in multiple providers"
                );
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

    /// Iterate all chat model qualified names (e.g. `"chat.anthropic.opus"`).
    pub fn chat_model_names(&self) -> impl Iterator<Item = &str> {
        self.chat.keys().map(std::string::String::as_str)
    }
}

// ── Category parser ─────────────────────────────────────────────────────

/// Parse a category section (`[chat]` or `[tools]`) from raw TOML.
///
/// For each provider table, separates scalar keys (provider defaults) from
/// sub-table keys (model entries).  `openrouter_provider` is a reserved
/// dict key treated as a provider scalar.
///
/// Returns `(models, provider_defaults)` where `provider_defaults` carries the
/// explicit `[<category>.<provider>]` scalar fields per provider (only entries
/// with at least one set field). Static models already fold these in, but the
/// discovered-model path reuses them so discovery inherits the same defaults.
/// `(models, provider_defaults)` — the resolved models for a category plus the
/// explicit `[<category>.<provider>]` scalar fields, keyed by provider.
type ParsedCategory = (
    BTreeMap<String, ResolvedModel>,
    BTreeMap<String, ModelConfigFields>,
);

fn parse_category(
    category: &str,
    section: &toml::Table,
    providers: Option<&crate::providers::ProviderRegistry>,
) -> Result<ParsedCategory, CatalogError> {
    let mut models = BTreeMap::new();
    let mut provider_defaults: BTreeMap<String, ModelConfigFields> = BTreeMap::new();
    debug!(
        category,
        providers = section.len(),
        "Parsing model category"
    );

    for (provider_key, provider_value) in section {
        // The Claude Code transport was removed; reject leftover
        // `[chat.claude_code.*]` / `[tools.claude_code.*]` sections explicitly
        // so the breaking change surfaces as a clear config error rather than
        // silently routing through `default_sdk("claude_code") == Openai`.
        if provider_key == "claude_code" {
            return Err(CatalogError::RemovedProvider {
                category: category.to_string(),
            });
        }

        let Some(provider_table) = provider_value.as_table() else {
            warn!(
                category,
                key = provider_key,
                "Skipping non-table key in [{category}]"
            );
            continue;
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

        // Cascade order (lowest → highest precedence):
        //   1. hardcoded provider defaults
        //   2. `[providers.<provider>]` registry transport (sdk + base_url)
        //      — lets custom OpenAI-compatible providers configured only
        //      under `[providers]` route static aliases through the
        //      right transport without duplicating fields under `[chat]`
        //   3. `[chat.<provider>]` scalar fields
        //   4. `[chat.<provider>.<model>]` per-model fields (applied below)
        //
        // Credentials (`api_key_env` and the named-key list) intentionally
        // do NOT cascade through this path; the registry's compact
        // `api_key_env` is folded into `keys[]` at parse time, and the
        // credential resolver reads that list directly. Overlaying a
        // single env name back onto the static model would defeat the
        // multi-key fallback machinery.
        let mut provider_config = hardcoded_defaults(provider_key);

        if let Some(registry) = providers {
            if let Some(entry) = registry.get(provider_key) {
                let registry_overlay = ModelConfigFields {
                    sdk: entry.sdk.clone(),
                    base_url: entry.base_url.clone(),
                    ..ModelConfigFields::default()
                };
                provider_config.fields.merge_from(&registry_overlay);
            }
        }

        // Overlay explicit TOML scalars. Also retain them per-provider so the
        // discovered-model path can apply the same `[<category>.<provider>]`
        // defaults; only the user's explicit fields are kept (not the merged
        // hardcoded/registry baseline), so discovery's own lower-precedence
        // cascade stays intact.
        if let Ok(explicit) = toml::Value::Table(provider_scalars).try_into::<ProviderConfig>() {
            provider_config.fields.merge_from(&explicit.fields);
            if explicit.fields != ModelConfigFields::default() {
                provider_defaults.insert(provider_key.clone(), explicit.fields);
            }
        }

        // ── Extract model sub-tables ────────────────────────────────
        for (model_name, model_value) in provider_table {
            // Skip scalars and reserved dict keys — those are provider config.
            if !model_value.is_table() || RESERVED_DICT_KEYS.contains(&model_name.as_str()) {
                continue;
            }

            let entry: ModelEntry =
                model_value
                    .clone()
                    .try_into()
                    .map_err(|e| CatalogError::ParseEntry {
                        category: category.to_string(),
                        provider: provider_key.clone(),
                        name: model_name.clone(),
                        source: Box::new(e),
                    })?;

            let model_id = entry.model_id.ok_or_else(|| CatalogError::MissingModelId {
                category: category.to_string(),
                provider: provider_key.clone(),
                name: model_name.clone(),
            })?;

            let merged = entry.fields.or_fallback(&provider_config.fields);
            let resolved = ResolvedModel::from_parts(
                model_name.clone(),
                format!("{category}.{provider_key}.{model_name}"),
                category.to_string(),
                provider_key.clone(),
                model_id,
                default_sdk(provider_key),
                merged,
            );

            let qualified = format!("{category}.{provider_key}.{model_name}");
            debug!(
                category,
                provider = provider_key,
                model = model_name,
                qualified,
                "Resolved model entry"
            );
            models.insert(qualified, resolved);
        }
    }

    debug!(category, models = models.len(), "Category parsing complete");
    Ok((models, provider_defaults))
}

// ── Provider defaults ───────────────────────────────────────────────────

/// Shared baseline for all known providers.
fn base_provider_defaults() -> ModelConfigFields {
    ModelConfigFields {
        temperature: Some(1.0),
        max_output_tokens: Some(8192),
        max_context_tokens: Some(200_000),
        ..Default::default()
    }
}

/// Hardcoded provider defaults (ported from V1 `PROVIDER_DEFAULTS`).
///
/// Public so the effective-catalog merger (Phase 7) can synthesize
/// `ResolvedModel` records for discovered models that have no TOML
/// scalars under `[chat.<provider>]`.
pub fn hardcoded_provider_defaults(provider_key: &str) -> ProviderConfig {
    hardcoded_defaults(provider_key)
}

fn hardcoded_defaults(provider_key: &str) -> ProviderConfig {
    let fields = match provider_key {
        "anthropic" => ModelConfigFields {
            sdk: Some(Sdk::Anthropic),
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            ..base_provider_defaults()
        },
        "openrouter" => ModelConfigFields {
            // Non-Anthropic OpenRouter models route through the first-party
            // `@openrouter/sdk` adapter (the sidecar's normalized path that
            // folds each vendor's reasoning shape into one `reasoning_details`
            // array). Claude-over-OpenRouter uses a separate `openrouter-anthropic`
            // provider with an explicit `sdk = "anthropic"`.
            sdk: Some(Sdk::Openrouter),
            api_key_env: Some("OPENROUTER_API_KEY".into()),
            base_url: Some("https://openrouter.ai/api/v1".into()),
            ..base_provider_defaults()
        },
        "deepseek" => ModelConfigFields {
            sdk: Some(Sdk::Openai),
            api_key_env: Some("DEEPSEEK_API_KEY".into()),
            base_url: Some("https://api.deepseek.com/v1".into()),
            ..base_provider_defaults()
        },
        "gemini" => ModelConfigFields {
            sdk: Some(Sdk::Gemini),
            api_key_env: Some("GEMINI_API_KEY".into()),
            ..base_provider_defaults()
        },
        "xai" => ModelConfigFields {
            sdk: Some(Sdk::Openai),
            api_key_env: Some("XAI_API_KEY".into()),
            base_url: Some("https://api.x.ai/v1".into()),
            ..base_provider_defaults()
        },
        "zhipuai" => ModelConfigFields {
            sdk: Some(Sdk::Openai),
            api_key_env: Some("ZAI_API_KEY".into()),
            base_url: Some("https://open.bigmodel.cn/api/paas/v4".into()),
            ..base_provider_defaults()
        },
        "zai" => ModelConfigFields {
            sdk: Some(Sdk::Zai),
            api_key_env: Some("ZAI_API_KEY".into()),
            zai_clear_thinking: Some(false),
            ..base_provider_defaults()
        },
        "nanogpt" => ModelConfigFields {
            sdk: Some(Sdk::Openai),
            api_key_env: Some("NANOGPT_API_KEY".into()),
            base_url: Some("https://nano-gpt.com/api/v1".into()),
            ..base_provider_defaults()
        },
        _ => ModelConfigFields::default(),
    };
    ProviderConfig { fields }
}

/// Default SDK for a provider key (used when neither hardcoded nor TOML specifies one).
pub fn default_sdk(provider_key: &str) -> Sdk {
    match provider_key {
        "anthropic" => Sdk::Anthropic,
        // OpenRouter's non-Anthropic models route through the first-party
        // `@openrouter/sdk` adapter by default (`anthropic/*` model_ids are
        // still auto-promoted to Sdk::Anthropic in `from_parts`).
        "openrouter" => Sdk::Openrouter,
        "gemini" => Sdk::Gemini,
        "zai" => Sdk::Zai,
        // Everything else (xai, deepseek, zhipuai, custom) defaults to the
        // direct OpenAI-compatible path.
        _ => Sdk::Openai,
    }
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
        let (models, _) = parse_category("chat", &table, None).unwrap();
        assert_eq!(models.len(), 1);

        let opus = &models["chat.anthropic.opus"];
        assert_eq!(opus.name, "opus");
        assert_eq!(opus.qualified_name, "chat.anthropic.opus");
        assert_eq!(opus.category, "chat");
        assert_eq!(opus.provider_key, "anthropic");
        assert_eq!(opus.sdk, Sdk::Anthropic);
        assert_eq!(opus.model_id, "claude-opus-4-6");
        assert_eq!(opus.api_key_env.as_deref(), Some("MY_KEY"));
    }

    #[test]
    fn openrouter_provider_defaults_to_openrouter_sdk() {
        let table = parse_table(
            r#"
[openrouter]
api_key_env = "OPENROUTER_API_KEY"

[openrouter.deepseek]
model_id = "deepseek/deepseek-v4"

[openrouter.glm]
model_id = "z-ai/glm-5.1"
sdk = "zai"
"#,
        );
        let (models, _) = parse_category("chat", &table, None).unwrap();

        // Non-Anthropic OpenRouter model → first-party OpenRouter SDK by default.
        assert_eq!(models["chat.openrouter.deepseek"].sdk, Sdk::Openrouter);
        // An explicit per-model `sdk` pin is always honored over the default.
        assert_eq!(models["chat.openrouter.glm"].sdk, Sdk::Zai);
    }

    #[test]
    fn anthropic_slug_auto_promotes_when_sdk_unset() {
        // A provider with no hardcoded SDK + an `anthropic/*` model_id and no
        // explicit `sdk` auto-promotes to the Anthropic SDK (cache_control
        // path). This is the seam `openrouter-anthropic` relies on.
        let table = parse_table(
            r#"
[myrouter.claude]
model_id = "anthropic/claude-sonnet-4-6"
api_key_env = "MY_KEY"
"#,
        );
        let (models, _) = parse_category("chat", &table, None).unwrap();
        assert_eq!(models["chat.myrouter.claude"].sdk, Sdk::Anthropic);
    }

    #[test]
    fn openrouter_sdk_wire_string_round_trips() {
        assert_eq!(default_sdk("openrouter"), Sdk::Openrouter);
        assert_eq!(Sdk::Openrouter.as_str(), "openrouter");
        assert_eq!(Sdk::parse_wire("openrouter"), Some(Sdk::Openrouter));
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
        let (models, _) = parse_category("chat", &table, None).unwrap();

        // opus inherits provider defaults
        let opus = &models["chat.anthropic.opus"];
        assert_eq!(opus.api_key_env.as_deref(), Some("SHARED_KEY"));
        assert_eq!(opus.max_context_tokens, Some(65536));
        assert_eq!(opus.cache_ttl.as_deref(), Some("1h"));

        // sonnet overrides cache_ttl
        let sonnet = &models["chat.anthropic.sonnet"];
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
        let (models, _) = parse_category("chat", &table, None).unwrap();
        let opus = &models["chat.anthropic.opus"];

        // Should get hardcoded anthropic defaults.
        assert_eq!(opus.sdk, Sdk::Anthropic);
        assert_eq!(opus.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
        assert_eq!(opus.temperature, Some(1.0));
        assert_eq!(opus.max_output_tokens, Some(8192));
        assert_eq!(opus.max_context_tokens, Some(200_000));
        // Anthropic SDK auto-enables prompt caching at 1h.
        assert_eq!(opus.cache_ttl.as_deref(), Some("1h"));
    }

    #[test]
    fn anthropic_sdk_auto_enables_cache_ttl_via_explicit_sdk_override() {
        // Custom provider key with explicit `sdk = "anthropic"` — the
        // hardcoded `"anthropic"` provider-key path isn't involved, so this
        // pins that the default fires off the resolved SDK, not the key.
        let table = parse_table(
            r#"
[my_anthropic_proxy]
sdk = "anthropic"
api_key_env = "MY_KEY"

[my_anthropic_proxy.opus]
model_id = "claude-opus-4-6"
"#,
        );
        let (models, _) = parse_category("chat", &table, None).unwrap();
        let opus = &models["chat.my_anthropic_proxy.opus"];
        assert_eq!(opus.sdk, Sdk::Anthropic);
        assert_eq!(opus.cache_ttl.as_deref(), Some("1h"));
    }

    #[test]
    fn non_anthropic_sdk_does_not_get_default_cache_ttl() {
        let table = parse_table(
            r#"
[openrouter.foo]
model_id = "anthropic/claude-opus-4.6"
"#,
        );
        let (models, _) = parse_category("chat", &table, None).unwrap();
        let foo = &models["chat.openrouter.foo"];
        // openrouter -> Sdk::Openrouter via hardcoded_defaults (non-Anthropic),
        // so it still gets no automatic "1h" cache_ttl.
        assert_eq!(foo.sdk, Sdk::Openrouter);
        assert_eq!(foo.cache_ttl, None);
    }

    #[test]
    fn explicit_empty_cache_ttl_disables_anthropic_caching() {
        let table = parse_table(
            r#"
[anthropic.opus]
model_id = "claude-opus-4-6"
cache_ttl = ""
"#,
        );
        let (models, _) = parse_category("chat", &table, None).unwrap();
        let opus = &models["chat.anthropic.opus"];
        // Explicit empty string survives — the runtime treats it as disabled.
        assert_eq!(opus.cache_ttl.as_deref(), Some(""));
    }

    #[test]
    fn explicit_cache_ttl_overrides_anthropic_default() {
        let table = parse_table(
            r#"
[anthropic.opus]
model_id = "claude-opus-4-6"
cache_ttl = "5m"
"#,
        );
        let (models, _) = parse_category("chat", &table, None).unwrap();
        let opus = &models["chat.anthropic.opus"];
        assert_eq!(opus.cache_ttl.as_deref(), Some("5m"));
    }

    #[test]
    fn provider_registry_transport_cascades_into_static_models() {
        // Regression pin: a custom OpenAI-compatible provider configured
        // only under `[providers.<name>]` should propagate sdk and
        // base_url into static `[chat.<name>]` aliases, so the request
        // routes through the right transport without users duplicating
        // those fields under `[chat.<name>]`. Credentials cascade
        // through the provider key resolver instead, not through this
        // overlay (see the `_credentials_do_not_cascade` test).
        use crate::providers::ProviderRegistry;

        let providers_table: toml::Table = r#"
[providers.acme]
sdk = "openai"
base_url = "https://acme.example.com/v1"
"#
        .parse()
        .unwrap();
        let registry = ProviderRegistry::from_section(
            providers_table.get("providers").and_then(|v| v.as_table()),
        )
        .unwrap();

        let table = parse_table(
            r#"
[acme.fast]
model_id = "acme/fast"
"#,
        );
        let (models, _) = parse_category("chat", &table, Some(&registry)).unwrap();

        let fast = &models["chat.acme.fast"];
        assert_eq!(fast.sdk, Sdk::Openai);
        assert_eq!(
            fast.base_url.as_deref(),
            Some("https://acme.example.com/v1")
        );
    }

    #[test]
    fn provider_registry_credentials_do_not_cascade_into_static_models() {
        // The registry's compact `api_key_env` is folded into `keys[]`
        // at parse time. Overlaying a single env name back onto the
        // static model would bypass the multi-key fallback resolver,
        // so credentials must stay out of the transport cascade.
        use crate::providers::ProviderRegistry;

        let providers_table: toml::Table = r#"
[providers.acme]
sdk = "openai"
base_url = "https://acme.example.com/v1"
api_key_env = "ACME_KEY"
"#
        .parse()
        .unwrap();
        let registry = ProviderRegistry::from_section(
            providers_table.get("providers").and_then(|v| v.as_table()),
        )
        .unwrap();

        let table = parse_table(
            r#"
[acme.fast]
model_id = "acme/fast"
"#,
        );
        let (models, _) = parse_category("chat", &table, Some(&registry)).unwrap();

        let fast = &models["chat.acme.fast"];
        // sdk and base_url cascade.
        assert_eq!(fast.sdk, Sdk::Openai);
        assert_eq!(
            fast.base_url.as_deref(),
            Some("https://acme.example.com/v1")
        );
        // api_key_env stays None — the credential resolver reads
        // `[providers.acme].keys` directly.
        assert!(fast.api_key_env.is_none());
    }

    #[test]
    fn chat_section_scalars_win_over_provider_registry() {
        // Cascade ordering: explicit `[chat.<provider>]` scalars must
        // override the registry's defaults, just as model-level fields
        // override `[chat.<provider>]` scalars.
        use crate::providers::ProviderRegistry;

        let providers_table: toml::Table = r#"
[providers.acme]
sdk = "openai"
base_url = "https://acme.example.com/v1"
"#
        .parse()
        .unwrap();
        let registry = ProviderRegistry::from_section(
            providers_table.get("providers").and_then(|v| v.as_table()),
        )
        .unwrap();

        let table = parse_table(
            r#"
[acme]
base_url = "https://override.example.com/v2"

[acme.fast]
model_id = "acme/fast"
"#,
        );
        let (models, _) = parse_category("chat", &table, Some(&registry)).unwrap();

        let fast = &models["chat.acme.fast"];
        // sdk still inherited from the registry (chat section did not set it)
        assert_eq!(fast.sdk, Sdk::Openai);
        // base_url from the chat section wins over the registry
        assert_eq!(
            fast.base_url.as_deref(),
            Some("https://override.example.com/v2")
        );
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
        let (models, _) = parse_category("chat", &table, None).unwrap();
        let opus = &models["chat.anthropic.opus"];

        assert_eq!(opus.api_key_env.as_deref(), Some("CUSTOM_KEY"));
        assert_eq!(opus.temperature, Some(0.5));
        // max_output_tokens still from hardcoded defaults.
        assert_eq!(opus.max_output_tokens, Some(8192));
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
        let (models, _) = parse_category("chat", &table, None).unwrap();

        // Should only have "opus", not "openrouter_provider".
        assert_eq!(models.len(), 1);
        assert!(models.contains_key("chat.anthropic.opus"));

        // And the provider-level openrouter_provider should cascade.
        let opus = &models["chat.anthropic.opus"];
        assert!(opus.openrouter_provider.is_some());
    }

    #[test]
    fn rejects_removed_claude_code_provider_key() {
        let table = parse_table(
            r#"
[claude_code.opus]
model_id = "claude-opus-4-6"
"#,
        );
        let err = parse_category("chat", &table, None).unwrap_err();
        match err {
            CatalogError::RemovedProvider { category } => assert_eq!(category, "chat"),
            other => panic!("expected RemovedProvider, got {other:?}"),
        }
    }

    #[test]
    fn missing_model_id_is_error() {
        let table = parse_table(
            r"
[anthropic.opus]
temperature = 0.5
",
        );
        let err = parse_category("chat", &table, None).unwrap_err();
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
        let (models, _) = parse_category("chat", &table, None).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models["chat.anthropic.opus"].sdk, Sdk::Anthropic);
        assert_eq!(models["chat.openrouter.gemini-pro"].sdk, Sdk::Openrouter); // openrouter default
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
        let catalog = ModelCatalog::from_sections(Some(&chat), Some(&tools), None, None).unwrap();

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
        assert!(matches!(err, CatalogError::NotFound { .. }));
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
    fn no_cross_provider_clobbering_same_short_name() {
        // Regression for SHA 99354fd: two providers with the same short model
        // name must produce distinct catalog entries, not overwrite each other.
        let chat = parse_table(
            r#"
[anthropic.opus]
model_id = "claude-opus-4-6"

[openrouter.opus]
model_id = "anthropic/claude-opus-4-6"
"#,
        );
        let catalog = ModelCatalog::from_sections(Some(&chat), None, None, None).unwrap();

        // Both entries must exist under their qualified names.
        assert_eq!(catalog.chat.len(), 2);
        assert!(catalog.chat.contains_key("chat.anthropic.opus"));
        assert!(catalog.chat.contains_key("chat.openrouter.opus"));

        // Each must carry the correct provider-specific model_id.
        assert_eq!(
            catalog.chat["chat.anthropic.opus"].model_id,
            "claude-opus-4-6"
        );
        assert_eq!(
            catalog.chat["chat.openrouter.opus"].model_id,
            "anthropic/claude-opus-4-6"
        );

        // Looking up by qualified name returns the correct one.
        let anthr = catalog.find_model("chat.anthropic.opus").unwrap();
        assert_eq!(anthr.model_id, "claude-opus-4-6");
        assert_eq!(anthr.provider_key, "anthropic");

        let orouter = catalog.find_model("chat.openrouter.opus").unwrap();
        assert_eq!(orouter.model_id, "anthropic/claude-opus-4-6");
        assert_eq!(orouter.provider_key, "openrouter");

        // Short name "opus" is ambiguous — must not silently return one.
        let err = catalog.find_model("opus").unwrap_err();
        assert!(matches!(err, CatalogError::AmbiguousName { .. }));
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
        assert_eq!(serde_json::to_value(Sdk::Anthropic).unwrap(), "anthropic");
        assert_eq!(serde_json::to_value(Sdk::Openai).unwrap(), "openai");
        assert_eq!(serde_json::to_value(Sdk::Gemini).unwrap(), "gemini");
        assert_eq!(serde_json::to_value(Sdk::Zai).unwrap(), "zai");
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

    // ── ModelConfigFields ────────────────────────────────────────────

    #[test]
    fn merge_from_overwrites_some_fields() {
        let mut base = ModelConfigFields {
            sdk: Some(Sdk::Anthropic),
            api_key_env: Some("BASE_KEY".into()),
            max_output_tokens: Some(1024),
            temperature: Some(0.5),
            ..Default::default()
        };
        let overlay = ModelConfigFields {
            max_output_tokens: Some(4096),
            top_p: Some(0.9),
            ..Default::default()
        };
        base.merge_from(&overlay);

        // Overwritten by overlay.
        assert_eq!(base.max_output_tokens, Some(4096));
        assert_eq!(base.top_p, Some(0.9));
        // Preserved from base (overlay had None).
        assert_eq!(base.sdk, Some(Sdk::Anthropic));
        assert_eq!(base.api_key_env.as_deref(), Some("BASE_KEY"));
        assert_eq!(base.temperature, Some(0.5));
    }

    #[test]
    fn merge_from_none_overlay_is_noop() {
        let mut base = ModelConfigFields {
            sdk: Some(Sdk::Openai),
            max_output_tokens: Some(2048),
            ..Default::default()
        };
        let empty = ModelConfigFields::default();
        base.merge_from(&empty);

        assert_eq!(base.sdk, Some(Sdk::Openai));
        assert_eq!(base.max_output_tokens, Some(2048));
    }

    #[test]
    fn or_fallback_prefers_self() {
        let primary = ModelConfigFields {
            max_output_tokens: Some(1024),
            temperature: Some(0.3),
            ..Default::default()
        };
        let fallback = ModelConfigFields {
            max_output_tokens: Some(4096),
            temperature: Some(0.9),
            api_key_env: Some("FALLBACK_KEY".into()),
            ..Default::default()
        };
        let result = primary.or_fallback(&fallback);

        assert_eq!(result.max_output_tokens, Some(1024), "self value wins");
        assert_eq!(result.temperature, Some(0.3), "self value wins");
        assert_eq!(
            result.api_key_env.as_deref(),
            Some("FALLBACK_KEY"),
            "fallback fills gap"
        );
    }

    #[test]
    fn or_fallback_both_none_stays_none() {
        let a = ModelConfigFields::default();
        let b = ModelConfigFields::default();
        let result = a.or_fallback(&b);
        assert!(result.max_output_tokens.is_none());
        assert!(result.sdk.is_none());
    }

    // ── chat_model_names ───────────────────────────────────────────

    #[test]
    fn chat_model_names_returns_qualified_names() {
        let table = parse_table(
            r#"
[anthropic.sonnet]
model_id = "claude-sonnet-4-6"

[anthropic.opus]
model_id = "claude-opus-4-6"

[openrouter.kimi]
model_id = "kimi-k2"
"#,
        );
        let catalog = ModelCatalog::from_sections(Some(&table), None, None, None).unwrap();

        let mut names: Vec<&str> = catalog.chat_model_names().collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec![
                "chat.anthropic.opus",
                "chat.anthropic.sonnet",
                "chat.openrouter.kimi"
            ]
        );
    }

    #[test]
    fn chat_model_names_empty_catalog() {
        let catalog = ModelCatalog::from_sections(None, None, None, None).unwrap();
        assert_eq!(catalog.chat_model_names().count(), 0);
    }

    #[test]
    fn same_short_name_across_providers_both_findable() {
        let chat = parse_table(
            r#"
[anthropic.opus]
model_id = "claude-opus-4-6"

[openrouter.opus]
model_id = "anthropic/claude-opus-4.6"
"#,
        );
        let catalog = ModelCatalog::from_sections(Some(&chat), None, None, None).unwrap();

        // Both findable by qualified name.
        let a = catalog.find_model("chat.anthropic.opus").unwrap();
        assert_eq!(a.model_id, "claude-opus-4-6");
        let b = catalog.find_model("chat.openrouter.opus").unwrap();
        assert_eq!(b.model_id, "anthropic/claude-opus-4.6");

        // Short name is ambiguous.
        let err = catalog.find_model("opus").unwrap_err();
        assert!(matches!(err, CatalogError::AmbiguousName { .. }));
    }

    #[test]
    fn sdk_as_str() {
        assert_eq!(Sdk::Anthropic.as_str(), "anthropic");
        assert_eq!(Sdk::Openai.as_str(), "openai");
        assert_eq!(Sdk::Gemini.as_str(), "gemini");
        assert_eq!(Sdk::Zai.as_str(), "zai");
    }

    #[test]
    fn sdk_deserialize_legacy_variants() {
        // "deepseek" and "zhipuai" should deserialize to Openai with a deprecation warning.
        let sdk: Sdk = toml::from_str::<ModelConfigFields>("sdk = \"deepseek\"")
            .unwrap()
            .sdk
            .unwrap();
        assert_eq!(sdk, Sdk::Openai);

        let sdk: Sdk = toml::from_str::<ModelConfigFields>("sdk = \"zhipuai\"")
            .unwrap()
            .sdk
            .unwrap();
        assert_eq!(sdk, Sdk::Openai);
    }

    #[test]
    fn openrouter_model_with_anthropic_sdk_override() {
        // The key use case: OpenRouter provider with sdk = "anthropic"
        // for Claude models.
        let table = parse_table(
            r#"
[openrouter]

[openrouter.claude-opus]
model_id = "anthropic/claude-opus-4.6"
sdk = "anthropic"
"#,
        );
        let (models, _) = parse_category("chat", &table, None).unwrap();
        let opus = &models["chat.openrouter.claude-opus"];

        // SDK should be overridden to Anthropic
        assert_eq!(opus.sdk, Sdk::Anthropic);
        // Provider key should still be "openrouter"
        assert_eq!(opus.provider_key, "openrouter");
        // Should inherit OpenRouter's base_url
        assert_eq!(
            opus.base_url.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
        // Should inherit OpenRouter's API key env
        assert_eq!(opus.api_key_env.as_deref(), Some("OPENROUTER_API_KEY"));
    }

    #[test]
    fn anthropic_slug_auto_promotes_to_anthropic_sdk() {
        // Custom provider key with no hardcoded default and no explicit
        // `sdk = `. Any model_id matching `anthropic/*` should still resolve
        // to Sdk::Anthropic so cache_control markers reach the wire.
        let table = parse_table(
            r#"
[openrouter-anthropic]
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"

[openrouter-anthropic.opus]
model_id = "anthropic/claude-opus-4.6"

[openrouter-anthropic.non-anthropic]
model_id = "openai/gpt-5"
"#,
        );
        let (models, _) = parse_category("chat", &table, None).unwrap();

        let opus = &models["chat.openrouter-anthropic.opus"];
        assert_eq!(opus.sdk, Sdk::Anthropic);
        // cache_ttl defaults to "1h" once promoted.
        assert_eq!(opus.cache_ttl.as_deref(), Some("1h"));

        // Non-anthropic slug under the same provider stays on the fallback
        // (Sdk::Openai for unknown provider keys).
        let other = &models["chat.openrouter-anthropic.non-anthropic"];
        assert_eq!(other.sdk, Sdk::Openai);
    }

    #[test]
    fn explicit_sdk_wins_over_anthropic_slug_auto_promotion() {
        // Explicit `sdk = "openai"` on an `anthropic/*` model_id must be
        // respected (e.g. for testing the chat-completions path).
        let table = parse_table(
            r#"
[openrouter-anthropic]
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"

[openrouter-anthropic.opus-via-openai]
model_id = "anthropic/claude-opus-4.6"
sdk = "openai"
"#,
        );
        let (models, _) = parse_category("chat", &table, None).unwrap();
        let opus = &models["chat.openrouter-anthropic.opus-via-openai"];
        assert_eq!(opus.sdk, Sdk::Openai);
    }

    #[test]
    fn anthropic_model_with_openrouter_base_url_override() {
        // Alternative config style: anthropic provider with manual overrides.
        let table = parse_table(
            r#"
[anthropic.opus-via-or]
model_id = "anthropic/claude-opus-4.6"
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"
"#,
        );
        let (models, _) = parse_category("chat", &table, None).unwrap();
        let opus = &models["chat.anthropic.opus-via-or"];

        assert_eq!(opus.sdk, Sdk::Anthropic);
        assert_eq!(opus.provider_key, "anthropic");
        assert_eq!(
            opus.base_url.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(opus.api_key_env.as_deref(), Some("OPENROUTER_API_KEY"));
    }
}
