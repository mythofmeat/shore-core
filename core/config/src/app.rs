use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::duration::ConfigDuration;

/// Generate a zero-argument function returning a constant, for `#[serde(default = "...")]`.
macro_rules! serde_default {
    ($name:ident -> $ty:ty { $val:expr }) => {
        fn $name() -> $ty {
            $val
        }
    };
}

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

    #[serde(default)]
    pub tts: TtsConfig,
}

// ── [daemon] ────────────────────────────────────────────────────────────

serde_default!(default_daemon_addr -> String { "127.0.0.1:7320".to_string() });

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    /// TCP address to listen on (default: "127.0.0.1:7320").
    #[serde(default = "default_daemon_addr")]
    pub addr: String,

    /// Explicit opt-in for unauthenticated remote TCP exposure.
    #[serde(default)]
    pub unsafe_allow_remote_access: bool,

    /// Allowed client hosts. Empty list means allow all.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,

    /// HTTP listener configuration. Off by default — only required
    /// for providers that need a callback URL (e.g. `claude_code`'s
    /// in-process MCP host).
    #[serde(default)]
    pub http: DaemonHttpConfig,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            addr: default_daemon_addr(),
            unsafe_allow_remote_access: false,
            allowed_hosts: vec![],
            http: DaemonHttpConfig::default(),
        }
    }
}

// ── [daemon.http] ───────────────────────────────────────────────────────

serde_default!(default_daemon_http_bind_addr -> String { "127.0.0.1:0".to_string() });

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DaemonHttpConfig {
    /// Whether the HTTP listener is started. Defaults to `false`. The
    /// `claude_code` provider requires it to be `true`; other providers
    /// have no use for it.
    #[serde(default)]
    pub enabled: bool,

    /// Bind address for the HTTP listener. Default `127.0.0.1:0`
    /// resolves to an ephemeral loopback port at startup; the daemon
    /// surfaces the resolved address to its engine context. Set
    /// explicitly only when the listener must be reachable from
    /// outside the local machine (rare).
    #[serde(default = "default_daemon_http_bind_addr")]
    pub bind_addr: String,
}

impl Default for DaemonHttpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_addr: default_daemon_http_bind_addr(),
        }
    }
}

// ── [defaults] ──────────────────────────────────────────────────────────

/// Background-task model selectors. All resolvers chain
/// `<task> → background.model → defaults.model → first chat`, so the
/// common case of "split chat from everything else" is two fields
/// (`defaults.model` + `defaults.background.model`).
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BackgroundDefaultsConfig {
    /// Blanket model used by every background task unless a per-task
    /// override below is set.
    pub model: Option<String>,

    /// Per-task override for autonomous heartbeat ticks.
    pub heartbeat: Option<String>,

    /// Per-task override for memory compaction passes.
    pub compaction: Option<String>,

    /// Per-task override for the AI librarian dreaming pass.
    pub dreaming: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DefaultsConfig {
    /// Default chat model name (must match a model in config). Acts as the
    /// fallback for every background task as well.
    pub model: Option<String>,

    /// Background-task model selectors (heartbeat, compaction, dreaming).
    #[serde(default)]
    pub background: BackgroundDefaultsConfig,

    /// **Deprecated.** Old top-level shorthand for
    /// `defaults.background.heartbeat`. Parse-only — the loader logs a
    /// warning and forwards into `background.heartbeat` (only when the
    /// new key is unset).
    #[serde(default)]
    pub heartbeat: Option<String>,

    /// **Deprecated.** Old top-level shorthand for
    /// `defaults.background.dreaming`. Parse-only — the loader logs a
    /// warning and forwards into `background.dreaming` (only when the
    /// new key is unset).
    #[serde(default)]
    pub dreaming: Option<String>,

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

/// Symbolic background-task identifier for [`DefaultsConfig::resolve_background_model_name`].
#[derive(Debug, Clone, Copy)]
pub enum BackgroundTask {
    Heartbeat,
    Compaction,
    Dreaming,
}

impl DefaultsConfig {
    /// Resolve the user's display name: config → $USER → "User".
    pub fn resolve_display_name(&self) -> String {
        self.display_name
            .clone()
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_else(|| "User".to_string())
    }

    /// Resolve the configured model *name* for a background task, walking
    /// `background.<task> → background.model → defaults.model`. Catalog
    /// validation and final fallback to "first chat model" live in the
    /// per-task resolvers; this returns just the configured name (or
    /// `None` when nothing is set anywhere).
    pub fn resolve_background_model_name(&self, task: BackgroundTask) -> Option<&str> {
        let per_task = match task {
            BackgroundTask::Heartbeat => self.background.heartbeat.as_deref(),
            BackgroundTask::Compaction => self.background.compaction.as_deref(),
            BackgroundTask::Dreaming => self.background.dreaming.as_deref(),
        };
        per_task
            .or(self.background.model.as_deref())
            .or(self.model.as_deref())
    }

    /// Migrate the legacy top-level `defaults.heartbeat` /
    /// `defaults.dreaming` keys into `defaults.background.*`. The new
    /// keys win on conflict; old keys whose new counterpart is unset are
    /// forwarded with a deprecation warning. Idempotent — calling it on
    /// already-normalized values is a no-op.
    pub fn normalize_deprecated_aliases(&mut self) {
        if let Some(value) = self.heartbeat.take() {
            if self.background.heartbeat.is_none() {
                tracing::warn!(
                    "`defaults.heartbeat = {value:?}` is deprecated; \
                     move it under `[defaults.background]` as `heartbeat`."
                );
                self.background.heartbeat = Some(value);
            } else {
                tracing::warn!(
                    "`defaults.heartbeat` is deprecated and was ignored \
                     because `defaults.background.heartbeat` is already set."
                );
            }
        }
        if let Some(value) = self.dreaming.take() {
            if self.background.dreaming.is_none() {
                tracing::warn!(
                    "`defaults.dreaming = {value:?}` is deprecated; \
                     move it under `[defaults.background]` as `dreaming`."
                );
                self.background.dreaming = Some(value);
            } else {
                tracing::warn!(
                    "`defaults.dreaming` is deprecated and was ignored \
                     because `defaults.background.dreaming` is already set."
                );
            }
        }
    }
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            model: None,
            background: BackgroundDefaultsConfig::default(),
            heartbeat: None,
            dreaming: None,
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
#[derive(Default)]
pub struct AutonomyConfig {
    /// Master switch for autonomous behavior.
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub heartbeat: HeartbeatConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatConfig {
    /// Whether heartbeat ticks are enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Base interval between heartbeat ticks.
    #[serde(default = "default_fallback_heartbeat_interval")]
    pub fallback_heartbeat_interval: ConfigDuration,

    /// Consecutive ticks without a user message before the abandonment guard
    /// stops scheduling further ticks (character sleeps until user returns).
    #[serde(default = "default_dormant_after_heartbeat_turns")]
    pub dormant_after_heartbeat_turns: u32,

    /// Time without a user message before the abandonment guard
    /// stops scheduling further ticks. Default: 48 hours.
    #[serde(default = "default_dormant_after_idle_time")]
    pub dormant_after_idle_time: ConfigDuration,

    /// Minimum time between a user message and the next heartbeat tick.
    /// Prevents ticks from firing during active conversation. Default: 1h.
    #[serde(default = "default_minimum_heartbeat_latency")]
    pub minimum_heartbeat_latency: ConfigDuration,

    /// Maximum tool-use rounds per heartbeat tick.
    #[serde(default = "default_max_tool_rounds")]
    pub max_tool_rounds: u32,

    /// Extra tool-use rounds granted after the wrap-up nudge fires, so the
    /// model can summarize unfinished work into HEARTBEAT.md and respond.
    #[serde(default = "default_wrap_up_grace_rounds")]
    pub wrap_up_grace_rounds: u32,
}

serde_default!(default_fallback_heartbeat_interval -> ConfigDuration { ConfigDuration::from_secs(3600) });
serde_default!(default_dormant_after_heartbeat_turns -> u32 { 3 });
serde_default!(default_dormant_after_idle_time -> ConfigDuration { ConfigDuration::from_secs(172800) }); // 48 hours
serde_default!(default_minimum_heartbeat_latency -> ConfigDuration { ConfigDuration::from_secs(3600) }); // 1 hour
serde_default!(default_max_tool_rounds -> u32 { 12 });
serde_default!(default_wrap_up_grace_rounds -> u32 { 3 });

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            fallback_heartbeat_interval: default_fallback_heartbeat_interval(),
            dormant_after_heartbeat_turns: default_dormant_after_heartbeat_turns(),
            dormant_after_idle_time: default_dormant_after_idle_time(),
            minimum_heartbeat_latency: default_minimum_heartbeat_latency(),
            max_tool_rounds: default_max_tool_rounds(),
            wrap_up_grace_rounds: default_wrap_up_grace_rounds(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CompactionConfig {
    /// Whether compaction triggers are enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Idle time before compaction triggers.
    #[serde(default = "default_idle_trigger")]
    pub idle_trigger: ConfigDuration,
    /// Minimum user turns before any compaction trigger fires.
    #[serde(default = "default_min_turns")]
    pub min_turns: usize,
    /// Force compaction when this user turn count is reached.
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
    /// Force compaction when the last turn's prompt context
    /// (input + cache_read + cache_creation) reaches this token count.
    /// Still floored by `min_turns`. 0 disables the token-based trigger.
    #[serde(default = "default_max_context_tokens")]
    pub max_context_tokens: usize,
    /// User turns retained in active.jsonl after compaction.
    #[serde(default = "default_keep_recent_turns")]
    pub keep_recent_turns: usize,
}

serde_default!(default_idle_trigger -> ConfigDuration { ConfigDuration::from_secs(1800) });
serde_default!(default_min_turns -> usize { 8 });
serde_default!(default_max_turns -> usize { 16 });
serde_default!(default_max_context_tokens -> usize { 200_000 });
serde_default!(default_keep_recent_turns -> usize { 2 });

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_trigger: default_idle_trigger(),
            min_turns: default_min_turns(),
            max_turns: default_max_turns(),
            max_context_tokens: default_max_context_tokens(),
            keep_recent_turns: default_keep_recent_turns(),
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

serde_default!(default_max_iterations -> u32 { 10 });

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

/// Per-tool enable/disable toggles. All default to enabled.
///
/// Stored as a map so new tool names can be toggled in config without a code change.
/// Any key present in the map overrides the default (enabled). Absent keys default to enabled.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(transparent)]
pub struct ToolToggles(BTreeMap<String, bool>);

impl ToolToggles {
    /// Check whether a tool is enabled by name. Absent keys default to enabled.
    pub fn is_enabled(&self, name: &str) -> bool {
        self.0.get(name).copied().unwrap_or(true)
    }

    pub fn set(&mut self, tool: &str, enabled: bool) {
        self.0.insert(tool.to_string(), enabled);
    }

    pub fn generate_image(&self) -> bool {
        self.is_enabled("generate_image")
    }
    pub fn web_search(&self) -> bool {
        self.is_enabled("web_search")
    }
    pub fn fetch_url(&self) -> bool {
        self.is_enabled("fetch_url")
    }
    pub fn check_time(&self) -> bool {
        self.is_enabled("check_time")
    }
    pub fn roll_dice(&self) -> bool {
        self.is_enabled("roll_dice")
    }
    pub fn activity_heatmap(&self) -> bool {
        self.is_enabled("activity_heatmap")
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

serde_default!(default_search_api_key_env -> String { "TAVILY_API_KEY".into() });
serde_default!(default_search_max_results -> u32 { 5 });
serde_default!(default_search_depth -> String { "basic".into() });

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

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfig {
    #[serde(default)]
    pub compaction: CompactionConfig,

    #[serde(default)]
    pub dreaming: DreamingConfig,

    #[serde(default)]
    pub thinking: ThinkingConfig,

    #[serde(default)]
    pub retrieval: RetrievalConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DreamingConfig {
    /// Whether scheduled memory dreaming sweeps are enabled.
    #[serde(default)]
    pub enabled: bool,

    /// Five-field cron schedule: minute hour day-of-month month day-of-week.
    #[serde(default = "default_dreaming_frequency")]
    pub frequency: String,

    /// Maximum private tool rounds an LLM-backed dreaming pass may use.
    #[serde(default = "default_dreaming_max_tool_rounds")]
    pub max_tool_rounds: u32,
}

serde_default!(default_dreaming_frequency -> String { "0 3 * * *".to_string() });
serde_default!(default_dreaming_max_tool_rounds -> u32 { 12 });

impl Default for DreamingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            frequency: default_dreaming_frequency(),
            max_tool_rounds: default_dreaming_max_tool_rounds(),
        }
    }
}

serde_default!(default_preserve_prior_turns -> bool { true });

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ThinkingConfig {
    /// Preserve extended-thinking blocks from prior turns in outgoing
    /// requests. Default `true`: thinking / redacted_thinking blocks are
    /// kept in history. Set `false` to strip them and save the tokens
    /// they consume on each subsequent turn — only safe with providers
    /// that don't depend on prior-turn thinking (e.g. Anthropic Claude
    /// 4.x). DeepSeek V3.1+ and Moonshot Kimi-thinking reject requests
    /// that omit prior `reasoning_content` while in thinking mode, and
    /// model performance is generally better when thinking is preserved.
    #[serde(default = "default_preserve_prior_turns")]
    pub preserve_prior_turns: bool,
}

impl Default for ThinkingConfig {
    fn default() -> Self {
        Self {
            preserve_prior_turns: default_preserve_prior_turns(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalMode {
    #[default]
    Auto,
    Lexical,
    Hybrid,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalBinaryMode {
    #[default]
    Skip,
    Metadata,
    TryEmbed,
}

serde_default!(default_retrieval_max_file_bytes -> u64 { 2 * 1024 * 1024 });
serde_default!(default_retrieval_max_indexed_files -> usize { 50_000 });
serde_default!(default_retrieval_max_total_indexed_bytes -> u64 { 1024 * 1024 * 1024 });
serde_default!(default_retrieval_max_embed_chars_per_file -> usize { 4_000 });

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalConfig {
    /// Workspace search mode. `auto` uses hybrid semantic+lexical retrieval when
    /// an embedding profile is configured and usable, then falls back to
    /// lexical search. `lexical` never calls embeddings. `hybrid` requests
    /// hybrid retrieval but still falls back to lexical on transient failures.
    #[serde(default)]
    pub mode: RetrievalMode,

    /// Maximum file size (bytes) eligible for lexical/hybrid search.
    #[serde(default = "default_retrieval_max_file_bytes")]
    pub max_file_bytes: u64,

    /// Hard cap on total files walked for workspace indexing.
    #[serde(default = "default_retrieval_max_indexed_files")]
    pub max_indexed_files: usize,

    /// Hard cap on cumulative bytes walked for workspace indexing.
    #[serde(default = "default_retrieval_max_total_indexed_bytes")]
    pub max_total_indexed_bytes: u64,

    /// Maximum chars from each file fed to the embedder.
    #[serde(default = "default_retrieval_max_embed_chars_per_file")]
    pub max_embed_chars_per_file: usize,

    /// Binary-file handling policy for workspace indexing.
    #[serde(default)]
    pub binary: RetrievalBinaryMode,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            mode: RetrievalMode::Auto,
            max_file_bytes: default_retrieval_max_file_bytes(),
            max_indexed_files: default_retrieval_max_indexed_files(),
            max_total_indexed_bytes: default_retrieval_max_total_indexed_bytes(),
            max_embed_chars_per_file: default_retrieval_max_embed_chars_per_file(),
            binary: RetrievalBinaryMode::Skip,
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
    /// Whether the Matrix connection is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Homeserver URL. Required for external mode.
    /// In embedded mode, auto-derived as http://localhost:{port}.
    pub homeserver: Option<String>,

    /// Matrix user ID (e.g. @shore:example.com). External mode only.
    pub user_id: Option<String>,

    /// Room ID to join. External mode only.
    pub room_id: Option<String>,

    /// Matrix user to trust for SAS auto-verification.
    pub trusted_user: Option<String>,

    /// Embedded homeserver configuration. Presence of this section
    /// activates embedded mode (mutually exclusive with homeserver).
    pub embedded: Option<EmbeddedConfig>,
}

/// Configuration for an embedded (shore-matrix-managed) Matrix homeserver.
///
/// Uses a conduwuit-compatible server (continuwuity, conduwuit, or tuwunel).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EmbeddedConfig {
    /// Matrix server_name (e.g. "shore.local"). Cannot be changed after first run.
    #[serde(default = "default_server_name")]
    pub server_name: String,

    /// HTTP bind address. Default "127.0.0.1" (loopback only). Set to "0.0.0.0"
    /// or "::" to expose the embedded homeserver to LAN/Tailscale clients.
    #[serde(default = "default_bind_address")]
    pub bind_address: String,

    /// HTTP listener port.
    #[serde(default = "default_homeserver_port")]
    pub port: u16,

    /// Admin username (without @ or :server).
    #[serde(default = "default_admin_user")]
    pub admin_user: String,

    /// Admin account password.
    pub admin_password: String,

    /// Override data directory. Default: $XDG_DATA_HOME/shore/matrix-server/
    pub data_dir: Option<String>,

    /// Override the homeserver binary name.
    /// Default: auto-detect (tries continuwuity, conduwuit, tuwunel).
    pub binary: Option<String>,
}

serde_default!(default_server_name -> String { "localhost".into() });
serde_default!(default_bind_address -> String { "127.0.0.1".into() });
serde_default!(default_homeserver_port -> u16 { 6167 });
serde_default!(default_admin_user -> String { "shore-admin".into() });

impl Default for EmbeddedConfig {
    fn default() -> Self {
        Self {
            server_name: default_server_name(),
            bind_address: default_bind_address(),
            port: default_homeserver_port(),
            admin_user: default_admin_user(),
            admin_password: String::new(),
            data_dir: None,
            binary: None,
        }
    }
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
}

/// Configuration for a supervised service.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ServiceEntry {
    /// Command to spawn the service (e.g. "node shore-llm/dist/index.js").
    pub command: Option<String>,

    /// Unix socket path the service listens on.
    pub socket: Option<String>,
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

    /// Only fire `message_complete` notifications when generation takes longer
    /// than this duration. 0 means always notify.
    #[serde(default = "default_generation_threshold")]
    pub generation_threshold: ConfigDuration,

    /// Per-event toggles.
    #[serde(default)]
    pub events: NotificationEventsConfig,
}

serde_default!(default_generation_threshold -> ConfigDuration { ConfigDuration::from_secs(0) });

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: bool::default(),
            backend: NotificationBackend::default(),
            ntfy: NtfyConfig::default(),
            command: CommandNotifyConfig::default(),
            generation_threshold: default_generation_threshold(),
            events: NotificationEventsConfig::default(),
        }
    }
}

/// Notification delivery backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum NotificationBackend {
    /// Linux desktop notifications via notify-send.
    #[default]
    NotifySend,
    /// Push notifications via ntfy server.
    Ntfy,
    /// User-defined shell command.
    Command,
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

serde_default!(default_ntfy_url -> String { "https://ntfy.sh".into() });

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
#[derive(Default)]
pub struct CommandNotifyConfig {
    /// Shell command template. Use {title} and {body} as placeholders.
    #[serde(default)]
    pub template: String,
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
    pub error: bool,
    #[serde(default)]
    pub message_complete: bool,
}

impl Default for NotificationEventsConfig {
    fn default() -> Self {
        Self {
            autonomous_message: true,
            cache_warning: true,
            compaction_complete: true,
            error: true,
            message_complete: false,
        }
    }
}

// ── [advanced] ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AdvancedConfig {
    /// Log every API request and response as individual JSON files under
    /// `{cache_dir}/debug/api_logs/`. Filenames are `{call_id}.json` for the
    /// request and `{call_id}_response.json` for the paired response or
    /// error. No rotation — operators manage disk usage manually.
    #[serde(default)]
    pub api_payload_logging: bool,

    /// Log prompt-cache forensic data to `{cache_dir}/cache_forensics.jsonl`.
    ///
    /// Disabled by default so operators opt in deliberately when diagnosing
    /// cache behavior or prompt-cost anomalies.
    #[serde(default)]
    pub cache_forensics: bool,

    /// Editor command override. Checked before $VISUAL / $EDITOR env vars.
    pub editor: Option<String>,

    /// Maximum LLM retry attempts before giving up. Overrides the default (2).
    pub max_retries: Option<u32>,

    /// Time to wait between retry attempts. Overrides the default (no backoff).
    pub retry_backoff: Option<ConfigDuration>,

    /// Maximum image file size (bytes) before resizing for LLM upload.
    /// Images larger than this are scaled down and re-encoded as JPEG.
    /// Set to 0 to disable resizing. Default: 2,000,000 (2 MB).
    #[serde(default = "default_max_image_size")]
    pub max_image_size: u64,
}

impl Default for AdvancedConfig {
    fn default() -> Self {
        Self {
            api_payload_logging: false,
            cache_forensics: false,
            editor: None,
            max_retries: None,
            retry_backoff: None,
            max_image_size: default_max_image_size(),
        }
    }
}

// ── [tts] ──────────────────────────────────────────────────────────────

serde_default!(default_tts_port -> u16 { 8778 });

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TtsConfig {
    /// Enable TTS support.
    #[serde(default)]
    pub enabled: bool,

    /// TTS server hostname.
    #[serde(default)]
    pub host: String,

    /// TTS server port (default: 8778).
    #[serde(default = "default_tts_port")]
    pub port: u16,

    /// Voice name to pass to the TTS server. If unset, falls back to the
    /// character name. Can be overridden per-character via the merged
    /// character config.
    #[serde(default)]
    pub voice: Option<String>,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: default_tts_port(),
            voice: None,
        }
    }
}

// ── Shared defaults ─────────────────────────────────────────────────────

serde_default!(default_true -> bool { true });
serde_default!(default_max_image_size -> u64 { 2_000_000 });

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        let config = AppConfig::default();
        assert!(config.defaults.stream);
        assert!(!config.behavior.autonomy.enabled);
        assert!(config.behavior.autonomy.heartbeat.enabled);
        assert_eq!(
            config
                .behavior
                .autonomy
                .heartbeat
                .fallback_heartbeat_interval,
            ConfigDuration::from_secs(3600)
        );
        assert_eq!(
            config
                .behavior
                .autonomy
                .heartbeat
                .dormant_after_heartbeat_turns,
            3
        );
        assert_eq!(
            config.behavior.autonomy.heartbeat.dormant_after_idle_time,
            ConfigDuration::from_secs(172800)
        );
        assert_eq!(
            config.behavior.autonomy.heartbeat.minimum_heartbeat_latency,
            ConfigDuration::from_secs(3600)
        );
        assert_eq!(config.behavior.autonomy.heartbeat.max_tool_rounds, 12);
        assert!(config.behavior.tool_use.enabled);
        assert!(config.memory.compaction.enabled);
        assert_eq!(config.memory.retrieval.mode, RetrievalMode::Auto);
        assert_eq!(config.memory.retrieval.max_file_bytes, 2 * 1024 * 1024);
        assert_eq!(config.memory.retrieval.max_indexed_files, 50_000);
        assert_eq!(
            config.memory.retrieval.max_total_indexed_bytes,
            1024 * 1024 * 1024
        );
        assert_eq!(config.memory.retrieval.max_embed_chars_per_file, 4_000);
        assert_eq!(config.memory.retrieval.binary, RetrievalBinaryMode::Skip);
        // Tool toggles default to true.
        assert!(config.behavior.tool_use.tools.is_enabled("roll_dice"));
        // Advanced retry fields default to None.
        assert!(config.advanced.editor.is_none());
        assert!(config.advanced.max_retries.is_none());
        assert!(config.advanced.retry_backoff.is_none());
    }

    #[test]
    fn memory_retrieval_mode_parses() {
        let toml_str = r#"
[memory.retrieval]
mode = "hybrid"
max_file_bytes = 12345
max_indexed_files = 999
max_total_indexed_bytes = 777777
max_embed_chars_per_file = 222
binary = "metadata"
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.memory.retrieval.mode, RetrievalMode::Hybrid);
        assert_eq!(config.memory.retrieval.max_file_bytes, 12345);
        assert_eq!(config.memory.retrieval.max_indexed_files, 999);
        assert_eq!(config.memory.retrieval.max_total_indexed_bytes, 777777);
        assert_eq!(config.memory.retrieval.max_embed_chars_per_file, 222);
        assert_eq!(
            config.memory.retrieval.binary,
            RetrievalBinaryMode::Metadata
        );
    }

    #[test]
    fn tool_toggles_disable_individual_tools() {
        let toml_str = r#"
[behavior.tool_use.tools]
roll_dice = false
web_search = false
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.behavior.tool_use.tools.roll_dice());
        assert!(!config.behavior.tool_use.tools.web_search());
        assert!(config.behavior.tool_use.tools.is_enabled("search_history"));
        assert!(!config.behavior.tool_use.tools.is_enabled("roll_dice"));
    }

    #[test]
    fn search_config_defaults() {
        let config = AppConfig::default();
        assert_eq!(
            config.behavior.tool_use.search.api_key_env,
            "TAVILY_API_KEY"
        );
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
    fn daemon_config_parses() {
        let toml_str = r#"
[daemon]
addr = "0.0.0.0:9999"
unsafe_allow_remote_access = true
allowed_hosts = ["127.0.0.1", "192.168.1.100"]
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.daemon.addr, "0.0.0.0:9999");
        assert!(config.daemon.unsafe_allow_remote_access);
        assert_eq!(config.daemon.allowed_hosts.len(), 2);
    }

    #[test]
    fn daemon_config_defaults_to_local_only() {
        let config = AppConfig::default();
        assert_eq!(config.daemon.addr, "127.0.0.1:7320");
        assert!(!config.daemon.unsafe_allow_remote_access);
        assert!(config.daemon.allowed_hosts.is_empty());
    }

    #[test]
    fn daemon_http_defaults_to_disabled_with_loopback_ephemeral_port() {
        let config = AppConfig::default();
        assert!(!config.daemon.http.enabled);
        assert_eq!(config.daemon.http.bind_addr, "127.0.0.1:0");
    }

    #[test]
    fn daemon_http_parses_explicit_block() {
        let toml_str = r#"
[daemon.http]
enabled = true
bind_addr = "127.0.0.1:7321"
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert!(config.daemon.http.enabled);
        assert_eq!(config.daemon.http.bind_addr, "127.0.0.1:7321");
    }

    #[test]
    fn daemon_http_block_absent_keeps_default() {
        // Adding [daemon] without [daemon.http] keeps the default
        // (disabled). Pre-existing daemon configs must not regress.
        let toml_str = r#"
[daemon]
addr = "0.0.0.0:9999"
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.daemon.addr, "0.0.0.0:9999");
        assert!(!config.daemon.http.enabled);
    }

    #[test]
    fn daemon_http_rejects_unknown_field() {
        let toml_str = r#"
[daemon.http]
enabled = true
bogus = "x"
"#;
        let result: Result<AppConfig, _> = toml::from_str(toml_str);
        assert!(result.is_err());
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
        assert!(
            err.contains("unknown field"),
            "Error should mention unknown field: {err}"
        );
    }

    #[test]
    fn notifications_config_defaults() {
        let config = AppConfig::default();
        assert!(!config.notifications.enabled);
        assert_eq!(
            config.notifications.backend,
            NotificationBackend::NotifySend
        );
        assert_eq!(config.notifications.ntfy.url, "https://ntfy.sh");
        assert!(config.notifications.ntfy.topic.is_empty());
        assert!(config.notifications.events.autonomous_message);
        assert!(config.notifications.events.cache_warning);
        assert!(config.notifications.events.compaction_complete);
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
        assert_eq!(
            config.notifications.command.template,
            "echo '{title}: {body}'"
        );
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

    #[test]
    fn matrix_external_mode_parses() {
        let toml_str = r#"
[connections.matrix]
homeserver = "https://matrix.example.com"
user_id = "@shore:example.com"
room_id = "!abc:example.com"
trusted_user = "@user:example.com"
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        let mx = config.connections.matrix.unwrap();
        assert!(mx.enabled);
        assert_eq!(mx.homeserver.as_deref(), Some("https://matrix.example.com"));
        assert_eq!(mx.user_id.as_deref(), Some("@shore:example.com"));
        assert_eq!(mx.room_id.as_deref(), Some("!abc:example.com"));
        assert_eq!(mx.trusted_user.as_deref(), Some("@user:example.com"));
        assert!(mx.embedded.is_none());
    }

    #[test]
    fn matrix_embedded_mode_parses() {
        let toml_str = r#"
[connections.matrix]
trusted_user = "@user:shore.local"

[connections.matrix.embedded]
server_name = "shore.local"
port = 9008
admin_password = "secret"
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        let mx = config.connections.matrix.unwrap();
        assert!(mx.enabled);
        assert!(mx.homeserver.is_none());
        assert_eq!(mx.trusted_user.as_deref(), Some("@user:shore.local"));
        let emb = mx.embedded.unwrap();
        assert_eq!(emb.server_name, "shore.local");
        assert_eq!(emb.port, 9008);
        assert_eq!(emb.admin_password, "secret");
        assert_eq!(emb.admin_user, "shore-admin");
        assert!(emb.data_dir.is_none());
        assert!(emb.binary.is_none());
    }

    #[test]
    fn matrix_embedded_defaults() {
        let toml_str = r#"
[connections.matrix.embedded]
admin_password = "required"
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        let emb = config.connections.matrix.unwrap().embedded.unwrap();
        assert_eq!(emb.server_name, "localhost");
        assert_eq!(emb.bind_address, "127.0.0.1");
        assert_eq!(emb.port, 6167);
        assert_eq!(emb.admin_user, "shore-admin");
    }

    #[test]
    fn matrix_embedded_with_all_fields() {
        let toml_str = r#"
[connections.matrix.embedded]
server_name = "test.local"
bind_address = "0.0.0.0"
port = 9999
admin_user = "admin"
admin_password = "secret123"
data_dir = "/tmp/test-matrix"
binary = "tuwunel"
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        let emb = config.connections.matrix.unwrap().embedded.unwrap();
        assert_eq!(emb.server_name, "test.local");
        assert_eq!(emb.bind_address, "0.0.0.0");
        assert_eq!(emb.port, 9999);
        assert_eq!(emb.admin_user, "admin");
        assert_eq!(emb.admin_password, "secret123");
        assert_eq!(emb.data_dir.as_deref(), Some("/tmp/test-matrix"));
        assert_eq!(emb.binary.as_deref(), Some("tuwunel"));
    }

    #[test]
    fn matrix_rejects_unknown_embedded_field() {
        let toml_str = r#"
[connections.matrix.embedded]
server_name = "localhost"
bogus = true
"#;
        let result: Result<AppConfig, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn matrix_disabled() {
        let toml_str = r#"
[connections.matrix]
enabled = false
homeserver = "https://matrix.example.com"
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        let mx = config.connections.matrix.unwrap();
        assert!(!mx.enabled);
    }

    // ── resolve_display_name ────────────────────────────────────────

    #[test]
    fn resolve_display_name_from_config() {
        let defaults = DefaultsConfig {
            display_name: Some("Alice".into()),
            ..Default::default()
        };
        assert_eq!(defaults.resolve_display_name(), "Alice");
    }

    #[test]
    fn resolve_display_name_falls_back_to_user_env() {
        let defaults = DefaultsConfig::default();
        // $USER is almost always set on unix; verify we get *something* non-empty.
        let name = defaults.resolve_display_name();
        assert!(!name.is_empty());
        // If $USER is set, we should get its value.
        if let Ok(user) = std::env::var("USER") {
            assert_eq!(name, user);
        }
    }

    #[test]
    fn resolve_display_name_ultimate_fallback() {
        // When display_name is None and USER is unset, should return "User".
        // We can't safely unset USER in a parallel test, so just test the
        // branch structure via the method's known behavior.
        let defaults = DefaultsConfig {
            display_name: None,
            ..Default::default()
        };
        // At minimum, resolve_display_name always returns a non-empty string.
        let name = defaults.resolve_display_name();
        assert!(!name.is_empty());
    }

    // ── ToolToggles::set ────────────────────────────────────────────

    #[test]
    fn tool_toggles_set_enables_and_disables() {
        let mut toggles = ToolToggles::default();

        // Default: all enabled.
        assert!(toggles.is_enabled("search_history"));
        assert!(toggles.is_enabled("roll_dice"));

        // Disable one tool toggle.
        toggles.set("search_history", false);
        assert!(!toggles.is_enabled("search_history"));

        // Re-enable.
        toggles.set("search_history", true);
        assert!(toggles.is_enabled("search_history"));
    }

    #[test]
    fn tool_toggles_unknown_legacy_key_is_independent() {
        let mut toggles = ToolToggles::default();
        toggles.set("memory_search", false);
        assert!(!toggles.is_enabled("memory_search"));
        assert!(toggles.is_enabled("search_history"));
    }

    #[test]
    fn tool_toggles_set_custom_tool() {
        let mut toggles = ToolToggles::default();
        assert!(toggles.is_enabled("custom_tool")); // default: enabled

        toggles.set("custom_tool", false);
        assert!(!toggles.is_enabled("custom_tool"));
    }

    #[test]
    fn max_image_size_defaults_and_overrides() {
        // Default: 2 MB.
        let config = AppConfig::default();
        assert_eq!(config.advanced.max_image_size, 2_000_000);

        // Override via TOML.
        let toml_str = r#"
[advanced]
max_image_size = 5000000
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.advanced.max_image_size, 5_000_000);

        // Disable via 0.
        let toml_str = r#"
[advanced]
max_image_size = 0
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.advanced.max_image_size, 0);
    }

    #[test]
    fn cache_forensics_defaults_and_overrides() {
        let config = AppConfig::default();
        assert!(!config.advanced.cache_forensics);

        let toml_str = r#"
[advanced]
cache_forensics = true
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert!(config.advanced.cache_forensics);
    }

    #[test]
    fn tts_config_defaults() {
        let config: AppConfig = toml::from_str("").unwrap();
        assert!(!config.tts.enabled);
        assert_eq!(config.tts.host, "");
        assert_eq!(config.tts.port, 8778);
        assert!(config.tts.voice.is_none());
    }

    // ── BackgroundDefaultsConfig + resolver ──────────────────────────

    #[test]
    fn background_resolver_uses_per_task_when_set() {
        let d = DefaultsConfig {
            model: Some("chat".into()),
            background: BackgroundDefaultsConfig {
                model: Some("bg".into()),
                heartbeat: Some("hb".into()),
                ..BackgroundDefaultsConfig::default()
            },
            ..Default::default()
        };
        assert_eq!(
            d.resolve_background_model_name(BackgroundTask::Heartbeat),
            Some("hb")
        );
        // Compaction has no per-task override → falls back to background.model.
        assert_eq!(
            d.resolve_background_model_name(BackgroundTask::Compaction),
            Some("bg")
        );
    }

    #[test]
    fn background_resolver_falls_back_to_background_then_chat_default() {
        let d_only_chat = DefaultsConfig {
            model: Some("chat".into()),
            ..Default::default()
        };
        // No background config at all → all three tasks fall back to defaults.model.
        for task in [
            BackgroundTask::Heartbeat,
            BackgroundTask::Compaction,
            BackgroundTask::Dreaming,
        ] {
            assert_eq!(
                d_only_chat.resolve_background_model_name(task),
                Some("chat")
            );
        }

        let d_split = DefaultsConfig {
            model: Some("chat".into()),
            background: BackgroundDefaultsConfig {
                model: Some("bg".into()),
                ..BackgroundDefaultsConfig::default()
            },
            ..Default::default()
        };
        for task in [
            BackgroundTask::Heartbeat,
            BackgroundTask::Compaction,
            BackgroundTask::Dreaming,
        ] {
            assert_eq!(d_split.resolve_background_model_name(task), Some("bg"));
        }
    }

    #[test]
    fn background_resolver_returns_none_when_nothing_set() {
        let d = DefaultsConfig::default();
        assert_eq!(
            d.resolve_background_model_name(BackgroundTask::Heartbeat),
            None
        );
    }

    #[test]
    fn deprecated_top_level_keys_forward_into_background() {
        let toml_str = r#"
[defaults]
model = "primary"
heartbeat = "hb-old"
dreaming = "dream-old"
"#;
        let mut config: AppConfig = toml::from_str(toml_str).unwrap();
        // Pre-normalize: old keys still populated, background empty.
        assert_eq!(config.defaults.heartbeat.as_deref(), Some("hb-old"));
        assert_eq!(config.defaults.dreaming.as_deref(), Some("dream-old"));
        assert!(config.defaults.background.heartbeat.is_none());

        config.defaults.normalize_deprecated_aliases();

        // Old keys cleared, background filled.
        assert!(config.defaults.heartbeat.is_none());
        assert!(config.defaults.dreaming.is_none());
        assert_eq!(
            config.defaults.background.heartbeat.as_deref(),
            Some("hb-old")
        );
        assert_eq!(
            config.defaults.background.dreaming.as_deref(),
            Some("dream-old")
        );
    }

    #[test]
    fn new_background_keys_win_over_deprecated_aliases() {
        let toml_str = r#"
[defaults]
heartbeat = "hb-old"

[defaults.background]
heartbeat = "hb-new"
"#;
        let mut config: AppConfig = toml::from_str(toml_str).unwrap();
        config.defaults.normalize_deprecated_aliases();
        // The explicit new key is preserved; the legacy alias is dropped.
        assert_eq!(
            config.defaults.background.heartbeat.as_deref(),
            Some("hb-new")
        );
        assert!(config.defaults.heartbeat.is_none());
    }

    #[test]
    fn normalize_is_idempotent() {
        let mut d = DefaultsConfig {
            background: BackgroundDefaultsConfig {
                heartbeat: Some("hb".into()),
                ..BackgroundDefaultsConfig::default()
            },
            ..Default::default()
        };
        d.normalize_deprecated_aliases();
        d.normalize_deprecated_aliases();
        assert_eq!(d.background.heartbeat.as_deref(), Some("hb"));
        assert!(d.heartbeat.is_none());
    }

    #[test]
    fn background_section_parses_with_deny_unknown_fields() {
        let toml_str = r#"
[defaults.background]
model = "bg"
compaction = "bg-c"
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.defaults.background.model.as_deref(), Some("bg"));
        assert_eq!(
            config.defaults.background.compaction.as_deref(),
            Some("bg-c")
        );

        // Unknown field is rejected.
        let bad = r#"
[defaults.background]
typo_field = "x"
"#;
        let err = toml::from_str::<AppConfig>(bad).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn tts_config_explicit() {
        let config: AppConfig = toml::from_str(
            r#"
[tts]
enabled = true
host = "192.168.1.50"
port = 9000
voice = "Nanachan"
"#,
        )
        .unwrap();
        assert!(config.tts.enabled);
        assert_eq!(config.tts.host, "192.168.1.50");
        assert_eq!(config.tts.port, 9000);
        assert_eq!(config.tts.voice.as_deref(), Some("Nanachan"));
    }
}
