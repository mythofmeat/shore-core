//! Per-sdk capability matrix (issue #138).
//!
//! A **code-level** description of which settings each [`Sdk`] honors, ignores,
//! or rejects — plus the few sdk-keyed default values. It is deliberately *not*
//! a user-facing `[sdk.*]` config table: that would tempt users to set things
//! the upstream API rejects. Instead this is the single source of truth that
//!
//! * lifts the old hardcoded `cache_ttl = "1h"` Anthropic default out of
//!   [`crate::models::ResolvedModel::from_parts`],
//! * lets catalog resolution drop / warn on settings the wire would 400 on, and
//! * drives `shore model setting` (#130) to show only-applicable keys and
//!   reject out-of-domain values at the boundary.
//!
//! ## sdk vs provider — the tiebreak
//!
//! A model resolves to a `(provider, sdk)` pair, and the two dimensions
//! cross-cut: a provider can span sdks (`openrouter-anthropic` routes
//! `anthropic/*` via the Anthropic sdk) and an sdk spans providers. They are
//! **not** a linear chain, so each setting has a natural owner:
//!
//! * **sdk-dimension** owns behavioral defaults ([`default_value`]) and field
//!   *applicability* ([`applicability`]) — e.g. `cache_ttl` only means anything
//!   on the Anthropic sdk.
//! * **provider-dimension** owns transport + routing *values* (`base_url`,
//!   credentials, `openrouter_provider`, `vertex_*`), carried on the provider
//!   entry / `[providers.*.defaults]`.
//!
//! Collisions are rare; when they happen the precedence is, lowest to highest:
//!
//! ```text
//! sdk-dimension code default  (this module — lowest)
//!   < hardcoded provider defaults
//!     < [providers.<p>.defaults]
//!       < per-model overlay      (highest)
//! ```
//!
//! i.e. **user config always beats the sdk code default**. The cascade is
//! realized by merging the higher tiers into `ModelConfigFields` *before*
//! [`crate::models::ResolvedModel::from_parts`], which only fills a field from
//! [`default_value`] when it is still `None`.

use crate::models::Sdk;

// ── Applicability ───────────────────────────────────────────────────────

/// How an [`Sdk`] (for a given model) treats a configuration [`Field`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applicability {
    /// The sdk accepts and acts on the field.
    Honored,
    /// The sdk silently ignores the field (harmless, but not useful — so
    /// `shore model setting` should not offer it and a set is a soft error).
    Ignored,
    /// Sending the field causes an upstream API error (a 400). Catalog
    /// resolution drops it before it reaches the wire.
    Rejected,
}

// ── Fields ──────────────────────────────────────────────────────────────

/// The settable knobs — the non-transport subset of
/// [`crate::models::ModelConfigFields`]. Transport (`sdk` / `api_key_env` /
/// `base_url`) is owned by the provider entry and is intentionally absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    MaxContextTokens,
    MaxOutputTokens,
    Temperature,
    TopP,
    ReasoningEffort,
    BudgetTokens,
    CacheTtl,
    KeepaliveEnabled,
    KeepaliveTtl,
    KeepaliveMaxPings,
    OpenrouterProvider,
    VertexProject,
    VertexLocation,
    GeminiGeneration,
    GeminiWebSearch,
    ZaiClearThinking,
    ZaiSubscription,
}

impl Field {
    /// The TOML key for this field (matches the `serde` name in
    /// [`crate::models::ModelConfigFields`]). Used in warnings and errors.
    pub fn key(self) -> &'static str {
        match self {
            Field::MaxContextTokens => "max_context_tokens",
            Field::MaxOutputTokens => "max_output_tokens",
            Field::Temperature => "temperature",
            Field::TopP => "top_p",
            Field::ReasoningEffort => "reasoning_effort",
            Field::BudgetTokens => "budget_tokens",
            Field::CacheTtl => "cache_ttl",
            Field::KeepaliveEnabled => "keepalive_enabled",
            Field::KeepaliveTtl => "keepalive_ttl",
            Field::KeepaliveMaxPings => "keepalive_max_pings",
            Field::OpenrouterProvider => "openrouter_provider",
            Field::VertexProject => "vertex_project",
            Field::VertexLocation => "vertex_location",
            Field::GeminiGeneration => "gemini_generation",
            Field::GeminiWebSearch => "gemini_web_search",
            Field::ZaiClearThinking => "zai_clear_thinking",
            Field::ZaiSubscription => "zai_subscription",
        }
    }
}

impl std::fmt::Display for Field {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.key())
    }
}

// ── Claude version axis ─────────────────────────────────────────────────

/// The Claude model family that carries the sampler-rejection cutoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeFamily {
    Opus,
    Sonnet,
    Haiku,
}

/// A parsed Claude model version, e.g. `claude-opus-4-8` → `{Opus, 4, 8}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaudeVersion {
    pub family: ClaudeFamily,
    pub major: u32,
    pub minor: u32,
}

impl ClaudeVersion {
    /// Whether this version rejects the sampler knobs (`temperature` / `top_p`
    /// / `budget_tokens`). True for opus & sonnet at **4.7 or later**; haiku and
    /// everything older still honor them.
    pub fn rejects_sampling(self) -> bool {
        let at_least_4_7 = self.major > 4 || (self.major == 4 && self.minor >= 7);
        matches!(self.family, ClaudeFamily::Opus | ClaudeFamily::Sonnet) && at_least_4_7
    }
}

/// Parse a Claude `major.minor` version out of a model id, if it is a Claude 4+
/// id of the shape `claude-<family>-<major><sep><minor>` (where `<sep>` is `-`
/// or `.`). An optional `<gateway>/` prefix (e.g. `anthropic/`) is stripped
/// first. Returns `None` for non-Claude ids and the pre-4 `claude-3.5-sonnet`
/// shape (family-after-version) — neither hits the cutoff.
pub fn parse_claude_version(model_id: &str) -> Option<ClaudeVersion> {
    // Drop a single leading `<gateway>/` segment if present.
    let bare = match model_id.rsplit_once('/') {
        Some((_, tail)) => tail,
        None => model_id,
    };
    let rest = bare.strip_prefix("claude-")?;

    // Split into family + version remainder on the first `-`.
    let (family_str, version_str) = rest.split_once('-')?;
    let family = match family_str {
        "opus" => ClaudeFamily::Opus,
        "sonnet" => ClaudeFamily::Sonnet,
        "haiku" => ClaudeFamily::Haiku,
        // Anything else (incl. the `claude-3.5-sonnet` shape, where this
        // segment is "3.5") is not a recognized 4+ family id.
        _ => return None,
    };

    // `version_str` is `<major><sep><minor>[-<suffix>...]`; take the leading
    // `major` and `minor` numbers, ignoring any trailing date/suffix segments.
    let mut nums = version_str.splitn(3, ['-', '.']);
    let major: u32 = nums.next()?.parse().ok()?;
    let minor: u32 = nums.next()?.parse().ok()?;

    Some(ClaudeVersion {
        family,
        major,
        minor,
    })
}

// ── The matrix ──────────────────────────────────────────────────────────

/// How `sdk` (resolving `model_id`) treats `field`.
///
/// `model_id` only matters for the Claude version cutoff; for every other rule
/// it is unused. Vendor-specific knobs are `Honored` on their owning sdk and
/// `Ignored` everywhere else (harmless but pointless).
pub fn applicability(sdk: &Sdk, model_id: &str, field: Field) -> Applicability {
    match field {
        // Generic knobs every sdk understands.
        Field::MaxContextTokens
        | Field::MaxOutputTokens
        | Field::ReasoningEffort
        | Field::KeepaliveEnabled
        | Field::KeepaliveTtl
        | Field::KeepaliveMaxPings => Applicability::Honored,

        // Sampler knobs: rejected on Claude opus/sonnet >= 4.7, honored
        // otherwise. The cutoff is sdk-independent — it follows the model id,
        // since the same Claude model can be reached via several sdks.
        Field::Temperature | Field::TopP | Field::BudgetTokens => {
            match parse_claude_version(model_id) {
                Some(v) if v.rejects_sampling() => Applicability::Rejected,
                Some(_) | None => Applicability::Honored,
            }
        }

        // `cache_ttl` only produces `cache_control` on the Anthropic sdk.
        Field::CacheTtl => vendor_field(sdk, &Sdk::Anthropic),

        Field::OpenrouterProvider => vendor_field(sdk, &Sdk::Openrouter),

        Field::VertexProject
        | Field::VertexLocation
        | Field::GeminiGeneration
        | Field::GeminiWebSearch => vendor_field(sdk, &Sdk::Gemini),

        Field::ZaiClearThinking | Field::ZaiSubscription => vendor_field(sdk, &Sdk::Zai),
    }
}

/// `Honored` on the owning sdk, `Ignored` elsewhere.
fn vendor_field(sdk: &Sdk, owner: &Sdk) -> Applicability {
    if sdk == owner {
        Applicability::Honored
    } else {
        Applicability::Ignored
    }
}

// ── sdk-keyed defaults ──────────────────────────────────────────────────

/// The canonical code-level default for `field` under `sdk`, as its TOML string
/// form, or `None` if the sdk has no default for it.
///
/// In practice the only entry is `cache_ttl = "1h"` for the Anthropic sdk —
/// prompt caching is opt-in on the wire, and defaulting it on means users get
/// caching without explicit config (set `cache_ttl = ""` to disable). This is
/// the **lowest** tier of the cascade (see the module docs): it fills a field
/// only when nothing higher set it.
pub fn default_value(sdk: &Sdk, field: Field) -> Option<&'static str> {
    match (sdk, field) {
        (Sdk::Anthropic, Field::CacheTtl) => Some("1h"),
        (Sdk::Anthropic | Sdk::Openai | Sdk::Openrouter | Sdk::Gemini | Sdk::Zai, _) => None,
    }
}

// ── Boundary validation (for #130) ──────────────────────────────────────

/// The accepted `reasoning_effort` values. The motivating bug is gpt-5.5
/// silently dropping thinking when handed `reasoning_effort = "max"` — `"max"`
/// is not in this set, so the boundary catches it. Per-model refinement of this
/// domain is a #130 follow-up.
pub const REASONING_EFFORT_DOMAIN: [&str; 4] = ["minimal", "low", "medium", "high"];

/// Why a setting was rejected at the boundary.
#[derive(Debug, thiserror::Error)]
pub enum CapabilityError {
    #[error("`{field}` is not applicable to the `{sdk}` sdk for this model")]
    Inapplicable { field: Field, sdk: &'static str },

    #[error("`{field}` value {value:?} is out of domain; allowed: {allowed}")]
    OutOfDomain {
        field: Field,
        value: String,
        allowed: String,
    },
}

/// Validate that `value` is an acceptable setting for `field` on `sdk` /
/// `model_id`. Used by `shore model setting` (#130) to reject at the boundary.
///
/// * A non-[`Applicability::Honored`] field is [`CapabilityError::Inapplicable`]
///   (you cannot usefully set a field the sdk ignores or rejects).
/// * A `Honored` field whose value falls outside its domain is
///   [`CapabilityError::OutOfDomain`].
pub fn validate(
    sdk: &Sdk,
    model_id: &str,
    field: Field,
    value: &toml::Value,
) -> Result<(), CapabilityError> {
    match applicability(sdk, model_id, field) {
        Applicability::Honored => {}
        Applicability::Ignored | Applicability::Rejected => {
            return Err(CapabilityError::Inapplicable {
                field,
                sdk: sdk.as_str(),
            });
        }
    }

    // Value-domain checks. Only `reasoning_effort` has a closed string domain
    // today; other honored fields accept any well-typed value.
    if field == Field::ReasoningEffort {
        if let Some(effort) = value.as_str() {
            if !REASONING_EFFORT_DOMAIN.contains(&effort) {
                return Err(CapabilityError::OutOfDomain {
                    field,
                    value: effort.to_string(),
                    allowed: REASONING_EFFORT_DOMAIN.join(", "),
                });
            }
        }
    }

    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ver(id: &str) -> Option<ClaudeVersion> {
        parse_claude_version(id)
    }

    #[test]
    fn parses_dash_and_dot_minor_separators() {
        assert_eq!(
            ver("claude-opus-4-7"),
            Some(ClaudeVersion {
                family: ClaudeFamily::Opus,
                major: 4,
                minor: 7
            })
        );
        assert_eq!(
            ver("claude-opus-4.8"),
            Some(ClaudeVersion {
                family: ClaudeFamily::Opus,
                major: 4,
                minor: 8
            })
        );
        assert_eq!(
            ver("claude-sonnet-4-6"),
            Some(ClaudeVersion {
                family: ClaudeFamily::Sonnet,
                major: 4,
                minor: 6
            })
        );
    }

    #[test]
    fn strips_gateway_prefix() {
        assert_eq!(
            ver("anthropic/claude-opus-4.8"),
            Some(ClaudeVersion {
                family: ClaudeFamily::Opus,
                major: 4,
                minor: 8
            })
        );
    }

    #[test]
    fn rejects_non_claude_and_pre_4_ids() {
        assert_eq!(ver("gpt-5.5"), None);
        assert_eq!(ver("deepseek-chat"), None);
        // Pre-4 family-after-version shape is not a recognized 4+ id.
        assert_eq!(ver("anthropic/claude-3.5-sonnet"), None);
        assert_eq!(ver("anthropic/claude-3-haiku"), None);
    }

    #[test]
    fn sampling_cutoff_boundary() {
        assert!(ver("claude-opus-4-7").unwrap().rejects_sampling());
        assert!(ver("claude-opus-4.8").unwrap().rejects_sampling());
        assert!(ver("claude-sonnet-4-7").unwrap().rejects_sampling());
        // Below the cutoff.
        assert!(!ver("claude-sonnet-4-6").unwrap().rejects_sampling());
        assert!(!ver("claude-opus-4-6").unwrap().rejects_sampling());
        // Haiku is exempt at every version.
        assert!(!ver("claude-haiku-4-8").unwrap().rejects_sampling());
    }

    #[test]
    fn sampler_fields_rejected_only_past_cutoff() {
        for field in [Field::Temperature, Field::TopP, Field::BudgetTokens] {
            assert_eq!(
                applicability(&Sdk::Anthropic, "claude-opus-4-8", field),
                Applicability::Rejected
            );
            assert_eq!(
                applicability(&Sdk::Anthropic, "claude-sonnet-4-6", field),
                Applicability::Honored
            );
            // Same model via a different sdk is still gated by the model id.
            assert_eq!(
                applicability(&Sdk::Openrouter, "anthropic/claude-opus-4.8", field),
                Applicability::Rejected
            );
        }
    }

    #[test]
    fn cache_ttl_only_honored_on_anthropic() {
        assert_eq!(
            applicability(&Sdk::Anthropic, "claude-opus-4-8", Field::CacheTtl),
            Applicability::Honored
        );
        for sdk in [Sdk::Openai, Sdk::Openrouter, Sdk::Gemini, Sdk::Zai] {
            assert_eq!(
                applicability(&sdk, "whatever", Field::CacheTtl),
                Applicability::Ignored
            );
        }
    }

    #[test]
    fn vendor_knobs_owned_by_their_sdk() {
        assert_eq!(
            applicability(&Sdk::Openrouter, "x", Field::OpenrouterProvider),
            Applicability::Honored
        );
        assert_eq!(
            applicability(&Sdk::Anthropic, "x", Field::OpenrouterProvider),
            Applicability::Ignored
        );
        assert_eq!(
            applicability(&Sdk::Gemini, "x", Field::VertexProject),
            Applicability::Honored
        );
        assert_eq!(
            applicability(&Sdk::Zai, "x", Field::ZaiClearThinking),
            Applicability::Honored
        );
        assert_eq!(
            applicability(&Sdk::Openai, "x", Field::ZaiClearThinking),
            Applicability::Ignored
        );
    }

    #[test]
    fn default_value_is_anthropic_cache_ttl_only() {
        assert_eq!(default_value(&Sdk::Anthropic, Field::CacheTtl), Some("1h"));
        assert_eq!(default_value(&Sdk::Openai, Field::CacheTtl), None);
        assert_eq!(default_value(&Sdk::Anthropic, Field::Temperature), None);
    }

    #[test]
    fn validate_accepts_honored_in_domain() {
        assert!(validate(
            &Sdk::Anthropic,
            "claude-opus-4-8",
            Field::CacheTtl,
            &toml::Value::String("5m".into())
        )
        .is_ok());
        assert!(validate(
            &Sdk::Openai,
            "gpt-5.5",
            Field::ReasoningEffort,
            &toml::Value::String("high".into())
        )
        .is_ok());
    }

    #[test]
    fn validate_rejects_inapplicable() {
        let err = validate(
            &Sdk::Openai,
            "gpt-5.5",
            Field::CacheTtl,
            &toml::Value::String("1h".into()),
        )
        .unwrap_err();
        assert!(matches!(err, CapabilityError::Inapplicable { .. }));

        // Sampler on a past-cutoff Claude model is Rejected → Inapplicable.
        let err = validate(
            &Sdk::Anthropic,
            "claude-opus-4-8",
            Field::Temperature,
            &toml::Value::Float(0.5),
        )
        .unwrap_err();
        assert!(matches!(err, CapabilityError::Inapplicable { .. }));
    }

    #[test]
    fn validate_rejects_out_of_domain_reasoning_effort() {
        let err = validate(
            &Sdk::Openai,
            "gpt-5.5",
            Field::ReasoningEffort,
            &toml::Value::String("max".into()),
        )
        .unwrap_err();
        assert!(matches!(err, CapabilityError::OutOfDomain { .. }));
    }
}
