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
    pub usage: UsageConfig,

    #[serde(default)]
    pub advanced: AdvancedConfig,
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
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            addr: default_daemon_addr(),
            unsafe_allow_remote_access: false,
            allowed_hosts: vec![],
        }
    }
}

// ── [defaults] ──────────────────────────────────────────────────────────

/// Background-task model selectors. Resolution chains
/// `<task> → background.model → active chat model → defaults.model →
/// first chat`. The active-chat rung means an unset background section
/// simply tracks whatever model the character is currently using; set
/// `background.model` (or a per-task override) to pin background work
/// to a different model regardless of chat selection.
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
    /// Default chat model name (must match a model in config). Used as
    /// the initial chat model when a character hasn't selected one yet,
    /// and as a late-stage fallback for background tasks (after the
    /// character's active chat model). Optional — if unset, chat starts
    /// on the first chat model declared in the catalog.
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

    /// Resolve the *explicitly configured* background-task model name,
    /// walking `background.<task> → background.model`. Returns `None`
    /// when no background-specific model is set — in that case callers
    /// should fall through to the character's active chat model (then
    /// `defaults.model`, then the first chat model). The active-chat
    /// fallback lives in the per-task resolvers because it requires
    /// per-character runtime state this config struct doesn't see.
    pub fn resolve_background_model_name(&self, task: BackgroundTask) -> Option<&str> {
        let per_task = match task {
            BackgroundTask::Heartbeat => self.background.heartbeat.as_deref(),
            BackgroundTask::Compaction => self.background.compaction.as_deref(),
            BackgroundTask::Dreaming => self.background.dreaming.as_deref(),
        };
        per_task.or(self.background.model.as_deref())
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
    /// Maximum tool-use rounds the compaction LLM can run before the loop
    /// is force-terminated. The model writes memory files by calling the
    /// `write`/`edit` tools; the manager treats a zero-writes outcome as
    /// "do not archive" so the live conversation isn't lost on a stuck
    /// loop.
    #[serde(default = "default_compaction_max_tool_rounds")]
    pub max_tool_rounds: u32,
}

serde_default!(default_idle_trigger -> ConfigDuration { ConfigDuration::from_secs(1800) });
serde_default!(default_min_turns -> usize { 8 });
serde_default!(default_max_turns -> usize { 16 });
serde_default!(default_max_context_tokens -> usize { 200_000 });
serde_default!(default_keep_recent_turns -> usize { 2 });
serde_default!(default_compaction_max_tool_rounds -> u32 { 12 });

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_trigger: default_idle_trigger(),
            min_turns: default_min_turns(),
            max_turns: default_max_turns(),
            max_context_tokens: default_max_context_tokens(),
            keep_recent_turns: default_keep_recent_turns(),
            max_tool_rounds: default_compaction_max_tool_rounds(),
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

    /// Maximum number of characters a single tool result may contribute to the
    /// conversation before it is truncated. Defaults to 20000 (~5k tokens of
    /// code-like output); set to `0` to disable truncation and preserve full
    /// tool output. When a result exceeds the limit it is cut at a character
    /// boundary and a notice is appended so the model knows the output was
    /// truncated. Truncation is persisted, so the shortened result is what
    /// gets replayed on subsequent turns.
    #[serde(default = "default_max_result_chars")]
    pub max_result_chars: usize,

    /// Per-tool enable/disable toggles.
    #[serde(default)]
    pub tools: ToolToggles,

    /// Web search (Tavily) configuration.
    #[serde(default)]
    pub search: SearchConfig,
}

serde_default!(default_max_iterations -> u32 { 10 });
serde_default!(default_max_result_chars -> usize { 20_000 });

impl Default for ToolUseConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_iterations: default_max_iterations(),
            max_result_chars: default_max_result_chars(),
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

    /// Minimum time since the last user message before a scheduled dreaming
    /// sweep is allowed to fire. Heartbeat / autonomy turns do not reset this.
    #[serde(default = "default_dreaming_minimum_inactive_time")]
    pub minimum_inactive_time: ConfigDuration,

    /// How long a scheduled cron occurrence stays eligible to fire after its
    /// scheduled time. If the daemon misses the occurrence by more than this,
    /// it is skipped and the next cron tick takes over (no late catch-up).
    #[serde(default = "default_dreaming_max_lateness")]
    pub max_lateness: ConfigDuration,

    /// When true, run idle-style compaction (if eligible) before the
    /// dreaming sweep. Aborts the sweep on compaction failure.
    #[serde(default = "default_true")]
    pub compact_before: bool,

    /// When true (and `compact_before` is true), the pre-dream compaction
    /// archives every chat turn instead of retaining the configured
    /// `keep_recent_turns` tail.
    #[serde(default)]
    pub compact_to_zero: bool,
}

serde_default!(default_dreaming_frequency -> String { "0 3 * * *".to_string() });
serde_default!(default_dreaming_max_tool_rounds -> u32 { 12 });
serde_default!(default_dreaming_minimum_inactive_time -> ConfigDuration { ConfigDuration::from_secs(45 * 60) });
serde_default!(default_dreaming_max_lateness -> ConfigDuration { ConfigDuration::from_secs(2 * 60 * 60) });

impl Default for DreamingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            frequency: default_dreaming_frequency(),
            max_tool_rounds: default_dreaming_max_tool_rounds(),
            minimum_inactive_time: default_dreaming_minimum_inactive_time(),
            max_lateness: default_dreaming_max_lateness(),
            compact_before: true,
            compact_to_zero: false,
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
    #[serde(default = "default_true")]
    pub usage_warning: bool,
}

impl Default for NotificationEventsConfig {
    fn default() -> Self {
        Self {
            autonomous_message: true,
            cache_warning: true,
            compaction_complete: true,
            error: true,
            message_complete: false,
            usage_warning: true,
        }
    }
}

// ── [usage] ────────────────────────────────────────────────────────────

serde_default!(default_usage_timezone -> String { "local".into() });
serde_default!(default_usage_budget_warn_at -> Vec<f64> { vec![0.8, 1.0] });
serde_default!(default_spike_multiplier -> f64 { 3.0 });
serde_default!(default_spike_min_cost_usd -> f64 { 1.0 });

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UsageConfig {
    /// Calendar timezone for named windows such as "today", budget days,
    /// weeks, and months. Supported values are "local" and "utc".
    #[serde(default = "default_usage_timezone")]
    pub timezone: String,

    /// Let compaction run even when a blocking budget is already over limit.
    /// Compaction can reduce future prompt size, so this defaults to true.
    #[serde(default = "default_true")]
    pub allow_compaction_over_budget: bool,

    /// User-defined cost budgets.
    #[serde(default)]
    pub budgets: Vec<UsageBudgetConfig>,

    /// Cost spike warning configuration.
    #[serde(default)]
    pub spike_warnings: UsageSpikeWarningsConfig,
}

impl Default for UsageConfig {
    fn default() -> Self {
        Self {
            timezone: default_usage_timezone(),
            allow_compaction_over_budget: true,
            budgets: Vec::new(),
            spike_warnings: UsageSpikeWarningsConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum UsageBudgetPeriod {
    Hour,
    #[default]
    Day,
    Week,
    Month,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum UsageBudgetAction {
    /// Report status and warnings, but never prevent calls.
    #[default]
    Warn,
    /// Prevent all matching LLM calls after the budget is over limit.
    Block,
    /// Prevent matching background calls after the budget is over limit.
    PauseBackground,
}

/// Weekday name for `[[usage.budgets]].reset_day_of_week`. Lowercase in TOML.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BudgetWeekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl BudgetWeekday {
    /// 0 = Monday .. 6 = Sunday, matching chrono's `num_days_from_monday`.
    pub fn num_days_from_monday(self) -> u32 {
        match self {
            BudgetWeekday::Monday => 0,
            BudgetWeekday::Tuesday => 1,
            BudgetWeekday::Wednesday => 2,
            BudgetWeekday::Thursday => 3,
            BudgetWeekday::Friday => 4,
            BudgetWeekday::Saturday => 5,
            BudgetWeekday::Sunday => 6,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UsageBudgetConfig {
    /// Display name for status, warnings, and errors. If empty, Shore uses a
    /// stable fallback such as "budget 1".
    #[serde(default)]
    pub name: String,

    /// Calendar window for the budget.
    #[serde(default)]
    pub period: UsageBudgetPeriod,

    /// USD cost limit for the period.
    pub cost_usd: f64,

    /// Fractions of `cost_usd` considered warning thresholds.
    #[serde(default = "default_usage_budget_warn_at")]
    pub warn_at: Vec<f64>,

    /// Enforcement action once `cost_usd` is reached.
    #[serde(default)]
    pub limit: UsageBudgetAction,

    /// Optional scope filters. All configured filters must match.
    #[serde(default)]
    pub character: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub call_type: Option<String>,
    #[serde(default)]
    pub usage_kind: Vec<String>,

    /// Per-budget override for `[usage].allow_compaction_over_budget`.
    #[serde(default)]
    pub allow_compaction_over_budget: Option<bool>,

    /// Hour-of-day at which the budget window resets (0-23). Applies to
    /// `period = day`, `week`, or `month`. Defaults to 0 (midnight).
    #[serde(default)]
    pub reset_hour: Option<u32>,

    /// Weekday on which a `period = "week"` budget resets. Defaults to Monday.
    #[serde(default)]
    pub reset_day_of_week: Option<BudgetWeekday>,

    /// Day-of-month on which a `period = "month"` budget resets (1-31).
    /// Values past the end of a short month clamp to the last day of that
    /// month. Defaults to 1.
    #[serde(default)]
    pub reset_day_of_month: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UsageSpikeWarningsConfig {
    /// Whether `shore usage --budget` should report cost spikes.
    #[serde(default)]
    pub enabled: bool,

    /// Window to compare against the immediately previous window.
    #[serde(default = "default_spike_period")]
    pub period: UsageBudgetPeriod,

    /// Current-period cost must be at least this multiple of the previous
    /// period cost to warn.
    #[serde(default = "default_spike_multiplier")]
    pub multiplier: f64,

    /// Current-period cost floor before a spike can warn.
    #[serde(default = "default_spike_min_cost_usd")]
    pub min_cost_usd: f64,
}

serde_default!(default_spike_period -> UsageBudgetPeriod { UsageBudgetPeriod::Hour });

impl Default for UsageSpikeWarningsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            period: UsageBudgetPeriod::Hour,
            multiplier: default_spike_multiplier(),
            min_cost_usd: default_spike_min_cost_usd(),
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
        assert_eq!(config.usage.timezone, "local");
        assert!(config.usage.allow_compaction_over_budget);
        assert!(config.usage.budgets.is_empty());
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
    fn usage_config_parses() {
        let toml_str = r#"
[usage]
timezone = "utc"
allow_compaction_over_budget = false

[[usage.budgets]]
name = "daily"
period = "day"
cost_usd = 10.0
warn_at = [0.5, 0.8]
limit = "block"
provider = "openrouter"
api_key = "overflow"
usage_kind = ["message_with_tools"]

[usage.spike_warnings]
enabled = true
period = "hour"
multiplier = 4.0
min_cost_usd = 2.5
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.usage.timezone, "utc");
        assert!(!config.usage.allow_compaction_over_budget);
        assert_eq!(config.usage.budgets.len(), 1);
        let budget = &config.usage.budgets[0];
        assert_eq!(budget.period, UsageBudgetPeriod::Day);
        assert_eq!(budget.limit, UsageBudgetAction::Block);
        assert_eq!(budget.provider.as_deref(), Some("openrouter"));
        assert_eq!(budget.api_key.as_deref(), Some("overflow"));
        assert_eq!(budget.usage_kind, vec!["message_with_tools"]);
        assert!(config.usage.spike_warnings.enabled);
        assert_eq!(config.usage.spike_warnings.period, UsageBudgetPeriod::Hour);
        assert_eq!(config.usage.spike_warnings.multiplier, 4.0);
        assert_eq!(config.usage.spike_warnings.min_cost_usd, 2.5);
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
        assert!(!config.notifications.events.message_complete);
        assert!(config.notifications.events.usage_warning);
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
        assert!(config.notifications.events.usage_warning);
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
    fn background_resolver_ignores_defaults_model() {
        // `defaults.model` is *not* a background fallback — it's only
        // for chat. Background tasks fall through to the per-character
        // active chat model (handled by the per-task resolvers), so
        // this struct-level helper returns None when no background-
        // specific model is configured.
        let d_only_chat = DefaultsConfig {
            model: Some("chat".into()),
            ..Default::default()
        };
        for task in [
            BackgroundTask::Heartbeat,
            BackgroundTask::Compaction,
            BackgroundTask::Dreaming,
        ] {
            assert_eq!(d_only_chat.resolve_background_model_name(task), None);
        }

        // `background.model` is honored for every task without a
        // per-task override.
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
}
