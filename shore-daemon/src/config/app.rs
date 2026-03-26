use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Top-level daemon configuration loaded from config.toml.
///
/// Covers all sections from §8: [defaults], [models], [behavior.autonomy],
/// [behavior.tool_use], [memory], [connections], [services], [advanced].
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    #[serde(default)]
    pub daemon: DaemonConfig,

    #[serde(default)]
    pub defaults: DefaultsConfig,

    #[serde(default)]
    pub behavior: BehaviorConfig,

    #[serde(default)]
    pub memory: MemoryConfig,

    #[serde(default)]
    pub connections: ConnectionsConfig,

    #[serde(default)]
    pub services: ServicesConfig,

    #[serde(default)]
    pub advanced: AdvancedConfig,
}

// ── [daemon] ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    /// Override the Unix socket path. Auto-generated if omitted.
    pub socket_path: Option<String>,

    /// Optional TCP address to listen on (e.g. "127.0.0.1:7320").
    pub tcp_addr: Option<String>,
}

// ── [defaults] ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DefaultsConfig {
    /// Default model name (must match a name in models.toml).
    pub model: Option<String>,

    /// Whether to stream responses by default.
    #[serde(default = "default_true")]
    pub stream: bool,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            model: None,
            stream: true,
        }
    }
}

// ── [behavior] ──────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BehaviorConfig {
    #[serde(default)]
    pub autonomy: AutonomyConfig,

    #[serde(default)]
    pub tool_use: ToolUseConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AutonomyConfig {
    /// Master switch for autonomous behavior.
    #[serde(default)]
    pub enabled: bool,

    /// Personality factor (0.0–1.0).
    #[serde(default = "default_personality")]
    pub personality: f64,

    /// Max unanswered messages before backing off.
    #[serde(default = "default_max_unanswered")]
    pub max_unanswered: u32,

    /// Maximum hours a character can defer a message.
    #[serde(default = "default_max_deferral_hours")]
    pub max_deferral_hours: f64,

    #[serde(default)]
    pub heartbeat: HeartbeatConfig,

    #[serde(default)]
    pub compaction: CompactionConfig,

    #[serde(default)]
    pub collation: CollationConfig,

    #[serde(default)]
    pub cache_keepalive: CacheKeepaliveConfig,
}

fn default_personality() -> f64 {
    0.5
}
fn default_max_unanswered() -> u32 {
    1
}
fn default_max_deferral_hours() -> f64 {
    24.0
}

impl Default for AutonomyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            personality: default_personality(),
            max_unanswered: default_max_unanswered(),
            max_deferral_hours: default_max_deferral_hours(),
            heartbeat: HeartbeatConfig::default(),
            compaction: CompactionConfig::default(),
            collation: CollationConfig::default(),
            cache_keepalive: CacheKeepaliveConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatConfig {
    /// Session gap in seconds — idle time marking a session boundary.
    #[serde(default = "default_session_gap")]
    pub session_gap_secs: u64,

    /// Minimum idle seconds before post-session probe.
    #[serde(default = "default_session_probe_floor")]
    pub session_probe_floor_secs: u64,

    /// Max consecutive unanswered probes before dormancy.
    #[serde(default = "default_dormant_threshold")]
    pub dormant_threshold: u32,
}

fn default_session_gap() -> u64 {
    1800
}
fn default_session_probe_floor() -> u64 {
    180
}
fn default_dormant_threshold() -> u32 {
    8
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            session_gap_secs: default_session_gap(),
            session_probe_floor_secs: default_session_probe_floor(),
            dormant_threshold: default_dormant_threshold(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CompactionConfig {
    /// Minutes of idle before compaction triggers.
    #[serde(default = "default_idle_trigger_minutes")]
    pub idle_trigger_minutes: u32,
}

fn default_idle_trigger_minutes() -> u32 {
    30
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            idle_trigger_minutes: default_idle_trigger_minutes(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CollationConfig {
    /// Whether collation runs automatically after compaction.
    #[serde(default = "default_true")]
    pub auto_run: bool,
}

impl Default for CollationConfig {
    fn default() -> Self {
        Self {
            auto_run: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CacheKeepaliveConfig {
    /// Whether cache keepalive is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Cache TTL in minutes.
    #[serde(default = "default_cache_ttl_minutes")]
    pub ttl_minutes: u32,

    /// Max keepalive pings before giving up.
    #[serde(default = "default_max_pings")]
    pub max_pings: u32,
}

fn default_cache_ttl_minutes() -> u32 {
    5
}
fn default_max_pings() -> u32 {
    12
}

impl Default for CacheKeepaliveConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ttl_minutes: default_cache_ttl_minutes(),
            max_pings: default_max_pings(),
        }
    }
}

// ── [behavior.tool_use] ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolUseConfig {
    /// Whether tool use is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Maximum tool loop iterations per turn.
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
}

fn default_max_iterations() -> u32 {
    10
}

impl Default for ToolUseConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_iterations: default_max_iterations(),
        }
    }
}

// ── [memory] ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfig {
    /// Number of RAG results to include in prompt context.
    #[serde(default = "default_rag_results")]
    pub rag_results: u32,

    /// Minimum relevance score (0.0–1.0) for RAG results.
    #[serde(default = "default_rag_threshold")]
    pub rag_threshold: f64,
}

fn default_rag_results() -> u32 {
    5
}
fn default_rag_threshold() -> f64 {
    0.3
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            rag_results: default_rag_results(),
            rag_threshold: default_rag_threshold(),
        }
    }
}

// ── [connections] ───────────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConnectionsConfig {
    #[serde(default)]
    pub matrix: Option<MatrixConfig>,

    #[serde(default)]
    pub telegram: Option<TelegramConfig>,

    #[serde(default)]
    pub discord: Option<DiscordConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MatrixConfig {
    /// Homeserver URL.
    pub homeserver: String,

    /// Matrix user ID (e.g. @shore:example.com).
    pub user_id: Option<String>,

    /// Room ID to join.
    pub room_id: Option<String>,
}

/// Reserved for future use.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TelegramConfig {
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

/// Reserved for future use.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DiscordConfig {
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

// ── [services] ──────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ServicesConfig {
    #[serde(default)]
    pub llm: ServiceEntry,

    #[serde(default)]
    pub matrix: Option<ServiceEntry>,
}

/// Configuration for a supervised service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ServiceEntry {
    /// Command to spawn the service (e.g. "node shore-llm/dist/index.js").
    pub command: Option<String>,

    /// Unix socket path the service listens on.
    pub socket: Option<String>,

    /// Whether the service is enabled. Defaults to true.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for ServiceEntry {
    fn default() -> Self {
        Self {
            command: None,
            socket: None,
            enabled: true,
        }
    }
}

// ── [advanced] ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AdvancedConfig {
    /// Warn when prompt cache is unexpectedly invalidated (§13.3).
    #[serde(default = "default_true")]
    pub cache_invalidation_warnings: bool,
}

impl Default for AdvancedConfig {
    fn default() -> Self {
        Self {
            cache_invalidation_warnings: true,
        }
    }
}

// ── Shared defaults ─────────────────────────────────────────────────────

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        let config = AppConfig::default();
        assert!(config.defaults.stream);
        assert!(!config.behavior.autonomy.enabled);
        assert_eq!(config.behavior.autonomy.personality, 0.5);
        assert_eq!(config.behavior.autonomy.max_unanswered, 1);
        assert_eq!(config.behavior.autonomy.max_deferral_hours, 24.0);
        assert!(config.advanced.cache_invalidation_warnings);
        assert!(config.behavior.tool_use.enabled);
    }

    #[test]
    fn minimal_toml_parses_to_defaults() {
        let toml_str = "";
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config, AppConfig::default());
    }

    #[test]
    fn rejects_unknown_top_level_section() {
        let toml_str = r#"
[bogus_section]
key = "value"
"#;
        let result: Result<AppConfig, _> = toml::from_str(toml_str);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown field"), "Error should mention unknown field: {err}");
    }

    #[test]
    fn rejects_unknown_nested_key() {
        let toml_str = r#"
[behavior.autonomy]
enabled = true
bogus_key = 42
"#;
        let result: Result<AppConfig, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }
}
