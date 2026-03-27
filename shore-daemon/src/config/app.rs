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
    pub notifications: NotificationsConfig,

    #[serde(default)]
    pub advanced: AdvancedConfig,
}

// ── [daemon] ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    /// Override the Unix socket path. Auto-generated if omitted.
    pub socket_path: Option<String>,
}

// ── [defaults] ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DefaultsConfig {
    /// Default chat model name (must match a model in config).
    pub model: Option<String>,

    /// Default tool model name (for tool-use calls).
    pub tool_model: Option<String>,

    /// Default memory agent model name.
    pub memory_agent: Option<String>,

    /// Default embedding profile name.
    pub embedding: Option<String>,

    /// Default image generation profile name.
    pub image_generation: Option<String>,

    /// User's display name for {{user}} template substitution.
    /// Falls back to $USER env var, then "User".
    pub display_name: Option<String>,

    /// Whether to stream responses by default.
    #[serde(default = "default_true")]
    pub stream: bool,
}

impl DefaultsConfig {
    /// Resolve the user's display name: config → $USER → "User".
    pub fn resolve_display_name(&self) -> String {
        self.display_name
            .clone()
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_else(|| "User".to_string())
    }
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            model: None,
            tool_model: None,
            memory_agent: None,
            embedding: None,
            image_generation: None,
            display_name: None,
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
    /// Whether heartbeat scheduling is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,

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
    1
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            session_gap_secs: default_session_gap(),
            session_probe_floor_secs: default_session_probe_floor(),
            dormant_threshold: default_dormant_threshold(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CompactionConfig {
    /// Whether compaction triggers are enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Minutes of idle before compaction triggers.
    #[serde(default = "default_idle_trigger_minutes")]
    pub idle_trigger_minutes: u32,
    /// Minimum messages before any compaction trigger fires.
    #[serde(default = "default_min_messages")]
    pub min_messages: usize,
    /// Force compaction when this message count is reached.
    #[serde(default = "default_max_messages")]
    pub max_messages: usize,
    /// Messages retained in active.jsonl after compaction.
    #[serde(default = "default_keep_recent")]
    pub keep_recent: usize,
}

fn default_idle_trigger_minutes() -> u32 {
    30
}

fn default_min_messages() -> usize {
    20
}

fn default_max_messages() -> usize {
    60
}

fn default_keep_recent() -> usize {
    4
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_trigger_minutes: default_idle_trigger_minutes(),
            min_messages: default_min_messages(),
            max_messages: default_max_messages(),
            keep_recent: default_keep_recent(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CollationConfig {
    /// Whether collation is enabled (gates both auto and manual triggers).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Whether collation runs automatically after compaction.
    #[serde(default = "default_true")]
    pub auto_run: bool,
}

impl Default for CollationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
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

    /// Per-tool enable/disable toggles.
    #[serde(default)]
    pub tools: ToolToggles,

    /// Web search (Tavily) configuration.
    #[serde(default)]
    pub search: SearchConfig,
}

fn default_max_iterations() -> u32 {
    10
}

impl Default for ToolUseConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_iterations: default_max_iterations(),
            tools: ToolToggles::default(),
            search: SearchConfig::default(),
        }
    }
}

/// Per-tool enable/disable toggles. All default to true.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolToggles {
    #[serde(default = "default_true")]
    pub memory: bool,
    #[serde(default = "default_true")]
    pub send_image: bool,
    #[serde(default = "default_true")]
    pub list_images: bool,
    #[serde(default = "default_true")]
    pub recall_image: bool,
    #[serde(default = "default_true")]
    pub generate_image: bool,
    #[serde(default = "default_true")]
    pub web_search: bool,
    #[serde(default = "default_true")]
    pub fetch_url: bool,
    #[serde(default = "default_true")]
    pub check_time: bool,
    #[serde(default = "default_true")]
    pub roll_dice: bool,
    #[serde(default = "default_true")]
    pub activity_heatmap: bool,
}

impl Default for ToolToggles {
    fn default() -> Self {
        Self {
            memory: true,
            send_image: true,
            list_images: true,
            recall_image: true,
            generate_image: true,
            web_search: true,
            fetch_url: true,
            check_time: true,
            roll_dice: true,
            activity_heatmap: true,
        }
    }
}

impl ToolToggles {
    /// Check whether a tool is enabled by name.
    pub fn is_enabled(&self, name: &str) -> bool {
        match name {
            "memory" => self.memory,
            "send_image" => self.send_image,
            "list_images" => self.list_images,
            "recall_image" => self.recall_image,
            "generate_image" => self.generate_image,
            "web_search" => self.web_search,
            "fetch_url" => self.fetch_url,
            "check_time" => self.check_time,
            "roll_dice" => self.roll_dice,
            "activity_heatmap" => self.activity_heatmap,
            // Unknown tools are enabled by default.
            _ => true,
        }
    }
}

// ── [behavior.tool_use.search] ───────────────────────────────────────────

/// Configuration for the web search tool (Tavily API).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    /// Environment variable holding the Tavily API key.
    #[serde(default = "default_search_api_key_env")]
    pub api_key_env: String,

    /// Default max results per search.
    #[serde(default = "default_search_max_results")]
    pub max_results: u32,

    /// Search depth: "basic" or "advanced".
    #[serde(default = "default_search_depth")]
    pub search_depth: String,

    /// Whether to include Tavily's synthesized answer.
    #[serde(default = "default_true")]
    pub include_answer: bool,
}

fn default_search_api_key_env() -> String { "TAVILY_API_KEY".into() }
fn default_search_max_results() -> u32 { 5 }
fn default_search_depth() -> String { "basic".into() }

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            api_key_env: default_search_api_key_env(),
            max_results: default_search_max_results(),
            search_depth: default_search_depth(),
            include_answer: true,
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

    /// Whether the image memory subsystem is enabled.
    #[serde(default = "default_true")]
    pub image_enabled: bool,
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
            image_enabled: true,
        }
    }
}

// ── [connections] ───────────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConnectionsConfig {
    #[serde(default)]
    pub tcp: Option<TcpConfig>,

    #[serde(default)]
    pub matrix: Option<MatrixConfig>,

    #[serde(default)]
    pub telegram: Option<TelegramConfig>,

    #[serde(default)]
    pub discord: Option<DiscordConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TcpConfig {
    /// Whether TCP listening is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// TCP address to listen on (e.g. "127.0.0.1:7320").
    pub addr: Option<String>,

    /// Allowed client hosts. Empty list means allow all.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
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

// ── [notifications] ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NotificationsConfig {
    /// Master switch for push notifications.
    #[serde(default)]
    pub enabled: bool,

    /// Notification backend: notify_send, ntfy, or command.
    #[serde(default)]
    pub backend: NotificationBackend,

    /// ntfy backend configuration.
    #[serde(default)]
    pub ntfy: NtfyConfig,

    /// Custom command backend configuration.
    #[serde(default)]
    pub command: CommandNotifyConfig,

    /// Per-event toggles.
    #[serde(default)]
    pub events: NotificationEventsConfig,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: NotificationBackend::default(),
            ntfy: NtfyConfig::default(),
            command: CommandNotifyConfig::default(),
            events: NotificationEventsConfig::default(),
        }
    }
}

/// Notification delivery backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationBackend {
    /// Linux desktop notifications via notify-send.
    NotifySend,
    /// Push notifications via ntfy server.
    Ntfy,
    /// User-defined shell command.
    Command,
}

impl Default for NotificationBackend {
    fn default() -> Self {
        Self::NotifySend
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NtfyConfig {
    /// ntfy server URL.
    #[serde(default = "default_ntfy_url")]
    pub url: String,

    /// ntfy topic name.
    #[serde(default)]
    pub topic: String,

    /// Optional auth token for self-hosted instances.
    #[serde(default)]
    pub token: String,
}

fn default_ntfy_url() -> String {
    "https://ntfy.sh".into()
}

impl Default for NtfyConfig {
    fn default() -> Self {
        Self {
            url: default_ntfy_url(),
            topic: String::new(),
            token: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CommandNotifyConfig {
    /// Shell command template. Use {title} and {body} as placeholders.
    #[serde(default)]
    pub template: String,
}

impl Default for CommandNotifyConfig {
    fn default() -> Self {
        Self {
            template: String::new(),
        }
    }
}

/// Per-event notification toggles. All default to true (fire when enabled).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NotificationEventsConfig {
    #[serde(default = "default_true")]
    pub autonomous_message: bool,
    #[serde(default = "default_true")]
    pub cache_warning: bool,
    #[serde(default = "default_true")]
    pub compaction_complete: bool,
    #[serde(default = "default_true")]
    pub collation_complete: bool,
    #[serde(default = "default_true")]
    pub error: bool,
}

impl Default for NotificationEventsConfig {
    fn default() -> Self {
        Self {
            autonomous_message: true,
            cache_warning: true,
            compaction_complete: true,
            collation_complete: true,
            error: true,
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

    /// Log full API request/response payloads to api_payloads.jsonl per character.
    #[serde(default)]
    pub api_payload_logging: bool,

    /// Editor command override. Checked before $VISUAL / $EDITOR env vars.
    pub editor: Option<String>,

    /// Maximum LLM retry attempts before giving up. Overrides the default (2).
    pub max_retries: Option<u32>,

    /// Seconds to wait between retry attempts. Overrides the default (no backoff).
    pub retry_backoff_seconds: Option<f64>,
}

impl Default for AdvancedConfig {
    fn default() -> Self {
        Self {
            cache_invalidation_warnings: true,
            api_payload_logging: false,
            editor: None,
            max_retries: None,
            retry_backoff_seconds: None,
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
        // Sub-toggles default to true.
        assert!(config.behavior.autonomy.heartbeat.enabled);
        assert!(config.behavior.autonomy.compaction.enabled);
        assert!(config.behavior.autonomy.collation.enabled);
        assert!(config.memory.image_enabled);
        // Tool toggles default to true.
        assert!(config.behavior.tool_use.tools.is_enabled("memory"));
        assert!(config.behavior.tool_use.tools.is_enabled("roll_dice"));
        // Advanced retry fields default to None.
        assert!(config.advanced.editor.is_none());
        assert!(config.advanced.max_retries.is_none());
        assert!(config.advanced.retry_backoff_seconds.is_none());
    }

    #[test]
    fn tool_toggles_disable_individual_tools() {
        let toml_str = r#"
[behavior.tool_use.tools]
roll_dice = false
web_search = false
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.behavior.tool_use.tools.roll_dice);
        assert!(!config.behavior.tool_use.tools.web_search);
        assert!(config.behavior.tool_use.tools.memory);
        assert!(config.behavior.tool_use.tools.is_enabled("memory"));
        assert!(!config.behavior.tool_use.tools.is_enabled("roll_dice"));
    }

    #[test]
    fn search_config_defaults() {
        let config = AppConfig::default();
        assert_eq!(config.behavior.tool_use.search.api_key_env, "TAVILY_API_KEY");
        assert_eq!(config.behavior.tool_use.search.max_results, 5);
        assert_eq!(config.behavior.tool_use.search.search_depth, "basic");
        assert!(config.behavior.tool_use.search.include_answer);
    }

    #[test]
    fn search_config_parses_from_toml() {
        let toml_str = r#"
[behavior.tool_use.search]
api_key_env = "MY_TAVILY_KEY"
max_results = 10
search_depth = "advanced"
include_answer = false
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.behavior.tool_use.search.api_key_env, "MY_TAVILY_KEY");
        assert_eq!(config.behavior.tool_use.search.max_results, 10);
        assert_eq!(config.behavior.tool_use.search.search_depth, "advanced");
        assert!(!config.behavior.tool_use.search.include_answer);
    }

    #[test]
    fn tcp_config_parses() {
        let toml_str = r#"
[connections.tcp]
enabled = true
addr = "127.0.0.1:7320"
allowed_hosts = ["127.0.0.1", "192.168.1.0/24"]
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        let tcp = config.connections.tcp.unwrap();
        assert!(tcp.enabled);
        assert_eq!(tcp.addr.as_deref(), Some("127.0.0.1:7320"));
        assert_eq!(tcp.allowed_hosts.len(), 2);
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
    fn notifications_config_defaults() {
        let config = AppConfig::default();
        assert!(!config.notifications.enabled);
        assert_eq!(config.notifications.backend, NotificationBackend::NotifySend);
        assert_eq!(config.notifications.ntfy.url, "https://ntfy.sh");
        assert!(config.notifications.ntfy.topic.is_empty());
        assert!(config.notifications.events.autonomous_message);
        assert!(config.notifications.events.cache_warning);
        assert!(config.notifications.events.compaction_complete);
        assert!(config.notifications.events.collation_complete);
        assert!(config.notifications.events.error);
    }

    #[test]
    fn notifications_config_parses_from_toml() {
        let toml_str = r#"
[notifications]
enabled = true
backend = "ntfy"

[notifications.ntfy]
url = "https://ntfy.example.com"
topic = "shore-test"
token = "tk_secret"

[notifications.events]
cache_warning = false
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert!(config.notifications.enabled);
        assert_eq!(config.notifications.backend, NotificationBackend::Ntfy);
        assert_eq!(config.notifications.ntfy.url, "https://ntfy.example.com");
        assert_eq!(config.notifications.ntfy.topic, "shore-test");
        assert_eq!(config.notifications.ntfy.token, "tk_secret");
        assert!(config.notifications.events.autonomous_message);
        assert!(!config.notifications.events.cache_warning);
    }

    #[test]
    fn notifications_command_backend_parses() {
        let toml_str = r#"
[notifications]
enabled = true
backend = "command"

[notifications.command]
template = "echo '{title}: {body}'"
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.notifications.backend, NotificationBackend::Command);
        assert_eq!(config.notifications.command.template, "echo '{title}: {body}'");
    }

    #[test]
    fn rejects_unknown_notifications_key() {
        let toml_str = r#"
[notifications]
enabled = true
bogus_key = "value"
"#;
        let result: Result<AppConfig, _> = toml::from_str(toml_str);
        assert!(result.is_err());
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
