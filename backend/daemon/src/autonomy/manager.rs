//! AutonomyManager — per-character scheduler state with background tick tasks.
//!
//! Each character gets its own tokio task that ticks the heartbeat clock on a
//! fixed interval. Cache keepalive is a separate cost-saving subsystem driven
//! from the same background loop.
//! State is persisted to `{data_dir}/{character}/autonomy_state.json`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dashmap::DashMap;
use std::time::Duration;
use tokio::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shore_protocol::server_msg::ServerMessage;
use shore_protocol::types::{ContentBlock, Message, Role, derive_content_from_blocks};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use super::activity::ActivityTracker;
use super::heartbeat::{HeartbeatAction, HeartbeatClock};
use super::{AutonomyStatus, HeartbeatEventKind, HeartbeatLog};
use crate::cache_keepalive::{CacheKeepalive, CacheKeepaliveAction};
use crate::characters::CharacterRegistry;
use crate::memory::compaction_impls::resolve_image_gen_config;
use crate::memory::retrieval::resolve_embedder;
use crate::notifications::{NotificationEvent, NotificationService};
use crate::tools as tool_system;
use crate::tools::context::SharedToolContext;
use crate::tools::{ToolContext, ToolError};
use shore_config::LoadedConfig;
use shore_config::app::{AutonomyConfig, CompactionConfig, DreamingConfig};
use shore_config::{
    HEARTBEAT_FILE, character_data_dir, character_memory_dir, character_workspace_dir,
};
use shore_diagnostics::truncate_summary;
use shore_ledger::{CallType, CredentialFallbackEvent, LedgerClient};
use shore_llm::types::LlmRequest;

use crate::sync::lock_or_recover;

// ---------------------------------------------------------------------------
// Tick context — shared state for the per-character autonomy loop
// ---------------------------------------------------------------------------

/// Shared context passed to the per-character tick loop.
struct TickContext {
    state: Arc<Mutex<AutonomyState>>,
    config: Arc<AutonomyConfig>,
    compaction: Arc<CompactionConfig>,
    data_dir: PathBuf,
    llm_client: Option<LedgerClient>,
    push_tx: Option<broadcast::Sender<ServerMessage>>,
    loaded_config: Option<Arc<LoadedConfig>>,
    notifier: Option<NotificationService>,
    registry: Option<Arc<tokio::sync::Mutex<CharacterRegistry>>>,
}

struct HeartbeatToolContext {
    inner: SharedToolContext,
    state: Arc<Mutex<AutonomyState>>,
}

impl ToolContext for HeartbeatToolContext {
    fn image_dir(&self) -> &str {
        self.inner.image_dir()
    }
    fn llm_client(&self) -> Option<&shore_llm::LlmClient> {
        self.inner.llm_client()
    }
    fn image_gen_config(&self) -> Option<&crate::memory::compaction_impls::ImageGenConfig> {
        self.inner.image_gen_config()
    }
    fn search_config(&self) -> &shore_config::app::SearchConfig {
        self.inner.search_config()
    }
    fn character_name(&self) -> &str {
        self.inner.character_name()
    }
    fn schedule_next_wake(&self, input: &Value) -> Option<Result<Value, ToolError>> {
        Some(Ok(schedule_next_wake_in_state(self.state.as_ref(), input)))
    }
    fn workspace_dir(&self) -> &str {
        self.inner.workspace_dir()
    }
    fn character_data_dir(&self) -> &str {
        self.inner.character_data_dir()
    }
    fn markdown_store(&self) -> Option<&crate::memory::markdown_store::MarkdownMemoryStore> {
        self.inner.markdown_store()
    }
    fn memory_retrieval_config(&self) -> &shore_config::app::RetrievalConfig {
        self.inner.memory_retrieval_config()
    }
    fn embedder(&self) -> Option<&dyn shore_llm::embed::Embedder> {
        self.inner.embedder()
    }
    fn memory_index_path(&self) -> Option<&std::path::Path> {
        self.inner.memory_index_path()
    }
    fn config_dir(&self) -> &str {
        self.inner.config_dir()
    }
    fn defer_edit(&self, path: &str) {
        self.inner.defer_edit(path);
    }
}

// ---------------------------------------------------------------------------
// Per-character state
// ---------------------------------------------------------------------------

/// All autonomy state for a single character.
#[expect(
    clippy::struct_excessive_bools,
    reason = "autonomy state tracks independent persisted and runtime flags"
)]
pub struct AutonomyState {
    pub heartbeat: HeartbeatClock,
    pub cache_keepalive: CacheKeepalive,
    pub activity: ActivityTracker,
    /// Ring buffer of heartbeat events for `shore log --heartbeat`.
    pub heartbeat_log: HeartbeatLog,
    /// Whether autonomy is paused (moved from HeartbeatClock).
    paused: bool,
    /// Whether state has changed since last save.
    dirty: bool,
    /// Last message activity timestamp for compaction idle trigger.
    last_compaction_activity: Instant,
    /// Whether compaction was already triggered for this idle period.
    compaction_triggered: bool,
    /// Current number of messages in active.jsonl (updated on each message notification).
    active_turn_count: usize,
    /// Set by the idle trigger tick — the handler checks and clears this after
    /// each generation to run compaction inline (synchronously with the handler).
    compaction_pending: bool,
    /// Cached last LLM request for heartbeat tick reuse.
    last_request: Option<LlmRequest>,
    /// Next allowed scheduled dreaming attempt after a failure.
    next_dream_attempt_at: Option<Instant>,
    /// Consecutive scheduled dreaming failures.
    dream_failure_count: u32,
}

impl AutonomyState {
    fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}

fn background_retry_delay(failure_count: u32) -> Duration {
    let exponent = failure_count.saturating_sub(1).min(6);
    let secs = 60u64.saturating_mul(1u64 << exponent);
    Duration::from_secs(secs.min(3_600))
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

const STATE_VERSION: u32 = 4;
const STATE_FILENAME: &str = "autonomy_state.json";

#[derive(Serialize, Deserialize)]
struct PersistedState {
    version: u32,
    ticks_without_user: u32,
    #[serde(default)]
    next_wake_at: Option<String>,
    #[serde(default)]
    last_user_at: Option<String>,
}

fn state_path(data_dir: &Path, character: &str) -> PathBuf {
    character_data_dir(data_dir, character).join(STATE_FILENAME)
}

/// Convert a `tokio::time::Instant` to an RFC3339 wall-clock string.
/// Approximate: uses the delta from `Instant::now()` applied to `Utc::now()`.
fn instant_to_rfc3339(instant: Instant) -> String {
    let now_instant = Instant::now();
    let now_utc = chrono::Utc::now();
    let wall = if instant > now_instant {
        now_utc
            + chrono::Duration::from_std(instant.duration_since(now_instant))
                .unwrap_or(chrono::Duration::MAX)
    } else {
        now_utc
            - chrono::Duration::from_std(now_instant.duration_since(instant))
                .unwrap_or(chrono::Duration::MAX)
    };
    wall.to_rfc3339()
}

fn duration_secs_i64(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}

/// Convert an RFC3339 string back to an `Instant` via the delta from current wall time.
fn rfc3339_to_instant(s: &str) -> Option<Instant> {
    let parsed = chrono::DateTime::parse_from_rfc3339(s).ok()?;
    let now_utc = chrono::Utc::now();
    let now_instant = Instant::now();
    let delta = parsed.signed_duration_since(now_utc);
    if delta >= chrono::Duration::zero() {
        let std_delta = delta.to_std().ok()?;
        Some(now_instant + std_delta)
    } else {
        let std_delta = (-delta).to_std().ok()?;
        now_instant.checked_sub(std_delta)
    }
}

/// Convert a Local `NaiveDateTime` (as stored in the activity tracker) to a
/// monotonic `Instant` via the delta from current wall time.
fn naive_local_to_instant(naive: chrono::NaiveDateTime) -> Option<Instant> {
    use chrono::TimeZone;
    let dt = chrono::Local.from_local_datetime(&naive).single()?;
    let now_local = chrono::Local::now();
    let now_instant = Instant::now();
    let delta = dt.signed_duration_since(now_local);
    if delta >= chrono::Duration::zero() {
        Some(now_instant + delta.to_std().ok()?)
    } else {
        now_instant.checked_sub((-delta).to_std().ok()?)
    }
}

fn save_state(data_dir: &Path, character: &str, state: &mut AutonomyState) {
    if !state.dirty {
        return;
    }

    let persisted = PersistedState {
        version: STATE_VERSION,
        ticks_without_user: state.heartbeat.ticks_without_user(),
        next_wake_at: state.heartbeat.next_wake().map(instant_to_rfc3339),
        last_user_at: state.heartbeat.last_user_at().map(instant_to_rfc3339),
    };

    let path = state_path(data_dir, character);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    match serde_json::to_string_pretty(&persisted) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                warn!(character, error = %e, "Failed to save autonomy state");
            } else {
                debug!(character, "Autonomy state saved");
                state.dirty = false;
            }
        }
        Err(e) => {
            warn!(character, error = %e, "Failed to serialize autonomy state");
        }
    }
}

fn load_state(data_dir: &Path, character: &str) -> Option<PersistedState> {
    let path = state_path(data_dir, character);
    let data = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<PersistedState>(&data) {
        Ok(state) if state.version == STATE_VERSION => Some(state),
        Ok(state) => {
            warn!(
                character,
                version = state.version,
                expected = STATE_VERSION,
                "Ignoring autonomy state with unknown version (migration)"
            );
            None
        }
        Err(e) => {
            warn!(character, error = %e, "Failed to parse autonomy state (may be v1 format)");
            None
        }
    }
}

fn restore_from_persisted(persisted: &PersistedState, heartbeat: &mut HeartbeatClock) {
    let next_wake = persisted
        .next_wake_at
        .as_deref()
        .and_then(rfc3339_to_instant);
    let last_user = persisted
        .last_user_at
        .as_deref()
        .and_then(rfc3339_to_instant);
    heartbeat.restore(persisted.ticks_without_user, next_wake, last_user);
}

/// Whether the dreaming inactivity window is satisfied: enough time has elapsed
/// since the last user message that a scheduled sweep won't disturb an active
/// conversation. `None` last-user means there's no inactivity timer to enforce.
fn dream_inactivity_satisfied(
    dreaming_cfg: Option<&DreamingConfig>,
    last_user_at: Option<Instant>,
    now: Instant,
) -> bool {
    dreaming_cfg.is_some_and(|cfg| match last_user_at {
        Some(last_user) => now.duration_since(last_user) >= cfg.minimum_inactive_time.as_duration(),
        None => true,
    })
}

fn sanitize_compaction_config(mut compaction: CompactionConfig) -> CompactionConfig {
    // Validate: turns thresholds must exceed keep_recent_turns, otherwise
    // there would never be anything to actually compact.
    if compaction.enabled {
        let k = compaction.keep_recent_turns;
        if compaction.min_turns <= k || compaction.max_turns <= k {
            tracing::error!(
                min_turns = compaction.min_turns,
                max_turns = compaction.max_turns,
                keep_recent_turns = k,
                "Compaction disabled: min_turns and max_turns must be greater than keep_recent_turns"
            );
            compaction.enabled = false;
        }
        if compaction.enabled && compaction.max_turns < compaction.min_turns {
            tracing::error!(
                min_turns = compaction.min_turns,
                max_turns = compaction.max_turns,
                "Compaction disabled: max_turns must be >= min_turns"
            );
            compaction.enabled = false;
        }
    }

    compaction
}

// ---------------------------------------------------------------------------
// AutonomyManager
// ---------------------------------------------------------------------------

/// Shared handle to per-character autonomy state.
///
/// Cheap to clone (wraps `Arc`s). The message handler, command context, and
/// per-character tick tasks all hold clones.
#[derive(Clone)]
pub struct AutonomyManager {
    states: Arc<DashMap<String, Arc<Mutex<AutonomyState>>>>,
    handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
    config: Arc<AutonomyConfig>,
    compaction: Arc<CompactionConfig>,
    data_dir: PathBuf,
    shutdown_rx: tokio::sync::watch::Receiver<()>,
    /// LLM client for heartbeat ticks and cache keepalive pings.
    llm_client: Option<LedgerClient>,
    /// Broadcast sender for pushing autonomous messages to SWP clients.
    push_tx: Option<broadcast::Sender<ServerMessage>>,
    /// Full config for model resolution in autonomous actions.
    loaded_config: Option<Arc<LoadedConfig>>,
    /// Push notification service for autonomous events.
    notifier: Option<NotificationService>,
    /// Character engine registry for safe message persistence.
    registry: Option<Arc<tokio::sync::Mutex<CharacterRegistry>>>,
}

impl AutonomyManager {
    pub fn new(
        config: AutonomyConfig,
        compaction: CompactionConfig,
        data_dir: PathBuf,
        shutdown_rx: tokio::sync::watch::Receiver<()>,
    ) -> Self {
        Self {
            states: Arc::new(DashMap::new()),
            handles: Arc::new(Mutex::new(Vec::new())),
            config: Arc::new(config),
            compaction: Arc::new(sanitize_compaction_config(compaction)),
            data_dir,
            shutdown_rx,
            llm_client: None,
            push_tx: None,
            loaded_config: None,
            notifier: None,
            registry: None,
        }
    }

    /// Set the LLM client and push channel for autonomous actions.
    /// Called once after creation, before any characters are ensured.
    pub fn set_resources(
        &mut self,
        llm_client: LedgerClient,
        push_tx: broadcast::Sender<ServerMessage>,
        loaded_config: LoadedConfig,
        notifier: NotificationService,
    ) {
        self.llm_client = Some(llm_client);
        self.push_tx = Some(push_tx);
        self.loaded_config = Some(Arc::new(loaded_config));
        self.notifier = Some(notifier);
    }

    /// Reload runtime autonomy and compaction configuration after `config_reset`.
    ///
    /// This updates the manager-held config used for future status checks,
    /// future `ensure_state*` calls, and fresh command contexts. Already-running
    /// per-character tick tasks keep the config snapshot they were spawned with
    /// until the daemon is restarted.
    pub fn reload_runtime_config(&mut self, loaded_config: LoadedConfig) {
        let autonomy = loaded_config.app.behavior.autonomy.clone();
        let compaction = sanitize_compaction_config(loaded_config.app.memory.compaction.clone());

        self.config = Arc::new(autonomy);
        self.compaction = Arc::new(compaction);
        self.loaded_config = Some(Arc::new(loaded_config));

        info!("Reloaded autonomy runtime configuration");
    }

    /// Set the character engine registry for safe autonomous message persistence.
    /// Called once after creation, before any characters are ensured.
    pub fn set_registry(&mut self, registry: Arc<tokio::sync::Mutex<CharacterRegistry>>) {
        self.registry = Some(registry);
    }

    /// Ensure autonomy state exists for a character. On first call for a
    /// character, creates the state (restoring from disk if available) and
    /// spawns a per-character tick task.
    pub fn ensure_state(&self, character: &str, cache_ttl_secs: Option<u64>) -> bool {
        self.ensure_state_with_config(character, cache_ttl_secs, None)
    }

    /// Like `ensure_state`, but accepts an optional per-character effective config
    /// that overrides the global config for model resolution and autonomy settings.
    pub fn ensure_state_with_config(
        &self,
        character: &str,
        cache_ttl_secs: Option<u64>,
        effective_config: Option<&LoadedConfig>,
    ) -> bool {
        if self.states.contains_key(character) {
            return false;
        }

        // Use per-character autonomy config if available, otherwise global.
        let autonomy_cfg = effective_config.map_or_else(
            || self.config.clone(),
            |c| Arc::new(c.app.behavior.autonomy.clone()),
        );

        // Create heartbeat clock with config values.
        let mut heartbeat = HeartbeatClock::with_config(&autonomy_cfg.heartbeat);
        // cache_ttl_secs is no longer consumed here — CacheKeepalive handles
        // keepalive pings independently (added in Phase 3).
        let _ = cache_ttl_secs;

        // Restore persisted state if available.
        if let Some(persisted) = load_state(&self.data_dir, character) {
            restore_from_persisted(&persisted, &mut heartbeat);
            info!(character, "Autonomy state restored from disk");
        } else {
            info!(character, "Autonomy state created (no prior state)");
        }

        let mut cache_keepalive = CacheKeepalive::new();
        // If the clock has a next_wake set (restored or bootstrapped), mirror
        // it to the keepalive so it can decide whether to bridge, and prime
        // the ping timer so keepalive pings begin immediately (rather than
        // waiting for the first user message or heartbeat tick).
        if let Some(wake) = heartbeat.next_wake() {
            cache_keepalive.set_next_wake(Some(wake));
            cache_keepalive.on_cache_warmed(Instant::now());
        }

        let heartbeat_log_path =
            character_data_dir(&self.data_dir, character).join("heartbeat.jsonl");
        let heartbeat_log = HeartbeatLog::load_from(heartbeat_log_path.clone())
            .unwrap_or_else(|| HeartbeatLog::with_path(heartbeat_log_path));

        let state = Arc::new(Mutex::new(AutonomyState {
            heartbeat,
            cache_keepalive,
            activity: ActivityTracker::new(),
            heartbeat_log,
            paused: false,
            dirty: false,
            last_compaction_activity: Instant::now(),
            compaction_triggered: false,
            active_turn_count: 0,
            compaction_pending: false,
            last_request: None,
            next_dream_attempt_at: None,
            dream_failure_count: 0,
        }));

        self.states.insert(character.to_string(), state.clone());

        // Spawn per-character tick task.
        let name = character.to_string();
        let config = autonomy_cfg;
        let compaction = self.compaction.clone();
        let data_dir = self.data_dir.clone();
        let shutdown_rx = self.shutdown_rx.clone();
        let llm_client = self.llm_client.clone();
        let push_tx = self.push_tx.clone();
        let loaded_config = effective_config
            .map(|c| Arc::new(c.clone()))
            .or_else(|| self.loaded_config.clone());
        let notifier = self.notifier.clone();
        let registry = self.registry.clone();

        let tick_ctx = TickContext {
            state,
            config,
            compaction,
            data_dir,
            llm_client,
            push_tx,
            loaded_config,
            notifier,
            registry,
        };
        let handle = tokio::spawn(async move {
            character_tick_loop(name, tick_ctx, shutdown_rx).await;
        });

        lock_or_recover("autonomy task handle list", &self.handles).push(handle);

        true
    }

    // -- state access helper ---------------------------------------------------

    /// Find the character's state, lock it, and call `f`.
    /// Returns `None` if the character has no autonomy state.
    fn with_state<R, F: FnOnce(&mut AutonomyState) -> R>(
        &self,
        character: &str,
        f: F,
    ) -> Option<R> {
        let state = self.states.get(character)?;
        let mut s = lock_state(&state);
        Some(f(&mut s))
    }

    // -- event notifications from the message handler -------------------------

    /// Call after a user message is appended.
    pub fn notify_user_message(&self, character: &str, message_count: usize) {
        self.with_state(character, |s| {
            let was_idle = s.heartbeat.ticks_without_user() > 0;
            let now = Instant::now();
            s.heartbeat.on_user_message(now);
            // Mirror the new wake deadline to the keepalive subsystem.
            if let Some(wake) = s.heartbeat.next_wake() {
                s.cache_keepalive.set_next_wake(Some(wake));
            }
            // The user message will trigger an LLM response — cache-warming event.
            s.cache_keepalive.on_cache_warmed(now);
            if was_idle {
                info!(character, "User returned — resetting idle counter");
                s.heartbeat_log.push(
                    HeartbeatEventKind::Wake,
                    "User returned — idle counter reset",
                );
            }
            s.activity.record_message();
            s.last_compaction_activity = now;
            s.active_turn_count = message_count;
            debug!(character, message_count, "User message notified");

            s.mark_dirty();
        });
    }

    /// Call after an assistant message is appended.
    pub fn notify_assistant_message(&self, character: &str, message_count: usize) {
        self.with_state(character, |s| {
            s.last_compaction_activity = Instant::now();
            s.active_turn_count = message_count;
            debug!(character, message_count, "Assistant message notified");
            s.mark_dirty();
        });
    }

    /// Backfill the activity tracker with historical message timestamps.
    ///
    /// Called once after `ensure_state` returns `true` (newly created state)
    /// to seed the tracker from existing chat history.
    pub fn backfill_activity(&self, character: &str, timestamps: &[chrono::NaiveDateTime]) {
        let count = timestamps.len();
        // Seed the heartbeat's last_user_at from the most recent backfilled user
        // turn so dreaming's inactivity gate isn't bypassed for characters
        // bootstrapped from existing history (where last_user_at would otherwise
        // be None until the next live user message).
        let latest_user = timestamps
            .iter()
            .max()
            .and_then(|n| naive_local_to_instant(*n));
        self.with_state(character, |s| {
            s.activity.backfill(timestamps);
            if let Some(at) = latest_user {
                s.heartbeat.seed_last_user_at_if_unset(at);
            }
        });
        debug!(character, count, "Activity backfilled from history");
    }

    /// Cache the last LLM request for heartbeat tick reuse.
    pub fn notify_last_request(&self, character: &str, request: LlmRequest) {
        self.with_state(character, |s| {
            cache_last_request(s, character, request);
        });
    }

    /// Clone the cached LLM request for private background work that should
    /// preserve the same provider-side prompt-cache prefix.
    pub fn cached_last_request(&self, character: &str) -> Option<LlmRequest> {
        self.with_state(character, |s| s.last_request.clone())
            .flatten()
    }

    /// Call after compaction completes successfully. Updates the turn count
    /// and resets compaction state so future triggers can fire.
    ///
    /// The handler calls this inline after running compaction and reloading the
    /// engine — no deferred reload flag is needed.
    pub fn notify_compaction_complete(&self, character: &str, new_turn_count: usize) {
        self.with_state(character, |s| {
            s.active_turn_count = new_turn_count;
            // Invalidate the cached request — it contains the pre-compaction
            // conversation. The next heartbeat/keepalive call can rebuild from
            // disk while preserving the existing keepalive deadline. Compaction
            // changes the conversation tail, but the pinned system prompt
            // prefix is often still the expensive cache entry worth keeping.
            invalidate_cached_request(s, character, CachedRequestInvalidationReason::Compaction);
            // Compaction cycle complete — allow future triggers.
            s.compaction_triggered = false;
            s.compaction_pending = false;
            s.last_compaction_activity = Instant::now();
            s.mark_dirty();
            info!(
                character = %character,
                new_turn_count,
                "Compaction complete — last_request invalidated"
            );
        });
    }

    /// Call after compaction fails. Resets the trigger so it can retry.
    pub fn notify_compaction_failed(&self, character: &str) {
        warn!(character, "Compaction failed — resetting trigger for retry");
        self.with_state(character, |s| {
            s.compaction_triggered = false;
            s.last_compaction_activity = Instant::now();
            s.mark_dirty();
        });
    }

    /// Check if compaction should run for this character: either the max_turns
    /// threshold was reached, the last turn's context tokens crossed the
    /// `max_context_tokens` threshold, or an idle trigger set the pending
    /// flag. `context_tokens` is the sum of input + cache_read +
    /// cache_creation from the just-completed turn's usage (0 when no signal
    /// is available, e.g. from the idle-tick path). Returns true (and clears
    /// the pending flag) if compaction should run. Called by the handler
    /// inline after persist_and_notify.
    pub fn should_compact_now(
        &self,
        character: &str,
        turn_count: usize,
        context_tokens: usize,
    ) -> bool {
        let compaction = &self.compaction;
        if !compaction.enabled {
            return false;
        }
        // Max-turns trigger: immediate, checked every generation.
        if compaction.max_turns > 0
            && turn_count >= compaction.max_turns
            && turn_count >= compaction.min_turns
        {
            // Mark compaction_triggered so the tick doesn't also fire.
            self.with_state(character, |s| {
                s.compaction_triggered = true;
                s.mark_dirty();
            });
            return true;
        }
        // Token-based trigger: fires when the just-completed turn's prompt
        // context crossed the configured threshold. Still floored by
        // min_turns to prevent early-conversation thrash.
        if compaction.max_context_tokens > 0
            && context_tokens >= compaction.max_context_tokens
            && turn_count >= compaction.min_turns
        {
            self.with_state(character, |s| {
                s.compaction_triggered = true;
                s.mark_dirty();
            });
            return true;
        }
        // Idle trigger: set by the tick loop, consumed here.
        self.take_compaction_pending(character)
    }

    /// Check if the idle trigger has requested compaction for this character.
    /// Returns true (and clears the pending flag) if compaction should run.
    /// The handler calls this after each generation to decide whether to run
    /// inline compaction.
    fn take_compaction_pending(&self, character: &str) -> bool {
        self.with_state(character, |s| {
            if s.compaction_pending {
                s.compaction_pending = false;
                info!(
                    character,
                    "Idle-triggered compaction pending taken by handler"
                );
                return true;
            }
            false
        })
        .unwrap_or(false)
    }

    /// Schedule an immediate heartbeat tick. Returns Some(dormant) where
    /// dormant indicates whether the clock is currently in abandoned state
    /// (meaning the tick will be suppressed). Returns None if no state found.
    pub fn heartbeat_tick_now(&self, character: &str) -> Option<bool> {
        info!(character, "Debug: scheduling immediate heartbeat tick");
        self.with_state(character, |s| {
            let dormant = s.heartbeat.is_dormant(Instant::now());
            s.heartbeat.force_wake();
            s.mark_dirty();
            dormant
        })
    }

    /// Force heartbeat into dormant state. Returns true if state was found.
    pub fn heartbeat_set_dormant(&self, character: &str) -> bool {
        info!(character, "Debug: forcing heartbeat dormant");
        self.with_state(character, |s| {
            s.heartbeat.force_dormant();
            s.mark_dirty();
        })
        .is_some()
    }

    /// Force heartbeat into active state. Returns true if state was found.
    pub fn heartbeat_set_active(&self, character: &str) -> bool {
        info!(character, "Debug: forcing heartbeat active");
        self.with_state(character, |s| {
            s.heartbeat.force_active();
            s.mark_dirty();
        })
        .is_some()
    }

    pub fn set_paused(&self, character: &str, paused: bool) -> Option<bool> {
        info!(character, paused, "Autonomy pause state changed");
        self.with_state(character, |s| {
            s.paused = paused;
            s.mark_dirty();
            paused
        })
    }

    // -- activity stats --------------------------------------------------------

    /// Return a clone of the `ActivityStats` and recorded user-turn count.
    pub fn activity_stats(
        &self,
        character: &str,
    ) -> Option<(super::activity::ActivityStats, usize)> {
        self.with_state(character, |s| {
            let stats = s.activity.stats().clone();
            let count = s.activity.message_count();
            (stats, count)
        })
    }

    // -- status snapshot ------------------------------------------------------

    /// Build an `AutonomyStatus` snapshot for the status command.
    pub fn status(&self, character: &str) -> Option<AutonomyStatus> {
        const RECENT_EVENT_LIMIT: usize = 5;
        self.with_state(character, |s| {
            let now = Instant::now();
            let next_wake_at = s.heartbeat.next_wake().map(instant_to_rfc3339);
            let seconds_until_wake = s.heartbeat.next_wake().map(|w| {
                if w >= now {
                    duration_secs_i64(w.duration_since(now))
                } else {
                    -duration_secs_i64(now.duration_since(w))
                }
            });
            let last_user_at = s.heartbeat.last_user_at().map(instant_to_rfc3339);
            let seconds_since_user = s
                .heartbeat
                .last_user_at()
                .map(|u| duration_secs_i64(now.duration_since(u)));
            let recent_events = s
                .heartbeat_log
                .recent(RECENT_EVENT_LIMIT)
                .into_iter()
                .cloned()
                .collect();
            AutonomyStatus {
                paused: s.paused,
                heartbeat_state: s.heartbeat.state_at(now).to_string(),
                ticks_without_user: s.heartbeat.ticks_without_user(),
                dormant_after_heartbeat_turns: s.heartbeat.max_idle_ticks(),
                effective_interval_secs: s.heartbeat.default_interval().as_secs(),
                next_wake_at,
                seconds_until_wake,
                last_user_at,
                seconds_since_user,
                minimum_heartbeat_latency_secs: s.heartbeat.min_wake_interval().as_secs(),
                dormant_after_idle_time_secs: s.heartbeat.max_silent_duration().as_secs(),
                recent_events,
            }
        })
    }

    /// Return recent heartbeat events for `shore log --heartbeat`.
    pub fn heartbeat_log(&self, character: &str, limit: usize) -> Vec<super::HeartbeatEvent> {
        self.with_state(character, |s| {
            s.heartbeat_log.recent(limit).into_iter().cloned().collect()
        })
        .unwrap_or_default()
    }

    // -- shutdown -------------------------------------------------------------

    /// Wait for all per-character tick tasks to finish.
    pub async fn shutdown(&self) {
        let handles: Vec<JoinHandle<()>> = {
            let mut h = lock_or_recover("autonomy task handle list", &self.handles);
            h.drain(..).collect()
        };
        let count = handles.len();
        info!(task_count = count, "Autonomy manager shutting down");
        for handle in handles {
            let _ = handle.await;
        }
        info!("Autonomy manager shutdown complete");
    }
}

// ---------------------------------------------------------------------------
// Per-character tick loop
// ---------------------------------------------------------------------------

/// Tick interval for each character's autonomy loop.
/// 10s gives ±10s precision on keepalive timing (vs ±30s before).
/// The per-tick work is microseconds (Instant comparisons + mutex lock)
/// unless an actual action is triggered, so the overhead is negligible.
const TICK_INTERVAL: Duration = Duration::from_secs(10);

/// Soft deadline for the heartbeat tool loop (all iterations combined). The
/// loop checks this before each iteration and ends the tick if
/// exceeded. Generous enough that a slow memory query + slow LLM across
/// `max_tool_rounds` iterations normally fits; tight enough that a runaway
/// loop can't block subsequent ticks for an hour. Per-call HTTP timeouts
/// (300s, enforced by `LlmClient`) still bound each individual request.
const HEARTBEAT_LOOP_DEADLINE: Duration = Duration::from_mins(30);

/// Lock the per-character autonomy state, recovering from mutex poisoning
/// instead of panicking. A poisoned mutex means a previous holder panicked,
/// but the state inside is still usable — letting the tick loop die would be
/// worse (no more keepalive, no more heartbeat, permanent silent failure).
fn lock_state(m: &Mutex<AutonomyState>) -> std::sync::MutexGuard<'_, AutonomyState> {
    lock_or_recover("autonomy state mutex", m)
}

/// Single point of mutation for `AutonomyState::last_request`. All
/// callers — the public `notify_last_request` and the internal
/// heartbeat/dormant-ping paths that already hold the state lock — go
/// through this so the log line stays consistent and nobody silently
/// forgets to emit it.
fn cache_last_request(state: &mut AutonomyState, character: &str, request: LlmRequest) {
    state.last_request = Some(request);
    debug!(character, "Cached last LLM request for heartbeat reuse");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CachedRequestInvalidationReason {
    Compaction,
    IdleCompaction,
    PreDreamCompaction,
}

/// Single point of invalidation for `AutonomyState::last_request`.
///
/// This is not the provider-side Anthropic cache itself. It is Shore's cached
/// request body used by heartbeat, dreaming, compaction, and keepalive reuse.
/// Any new path that clears it should add a reason here so cache-sensitive
/// behavior remains searchable and reviewable.
fn invalidate_cached_request(
    state: &mut AutonomyState,
    character: &str,
    reason: CachedRequestInvalidationReason,
) {
    let had_request = state.last_request.take().is_some();
    debug!(
        character,
        reason = ?reason,
        had_request,
        "Invalidated cached LLM request"
    );
}

fn push_provider_fallback_events(
    state: &mut AutonomyState,
    kind: HeartbeatEventKind,
    events: &[CredentialFallbackEvent],
) {
    for event in events {
        let to_key = event.to_key.as_deref().unwrap_or("none");
        state.heartbeat_log.push(
            kind,
            format!(
                "Provider key fallback: {} -> {} ({})",
                event.from_key, to_key, event.kind
            ),
        );
    }
}

fn schedule_next_wake_in_state(state: &Mutex<AutonomyState>, input: &Value) -> Value {
    let hours = input
        .get("hours_from_now")
        .and_then(Value::as_f64)
        .unwrap_or(1.0);
    let reason = input
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let clamped = hours.clamp(1.0, 48.0);
    let now = Instant::now();
    let when = now + Duration::from_secs_f64(clamped * 3600.0);

    let mut s = lock_state(state);
    s.heartbeat.schedule(when, now);
    let scheduled = s.heartbeat.next_wake().unwrap_or(when);
    s.cache_keepalive.set_next_wake(Some(scheduled));
    s.heartbeat_log.push(
        HeartbeatEventKind::ToolUse,
        format!("set_next_wake: {clamped:.1}h - {reason}"),
    );
    s.mark_dirty();

    json!(format!("Scheduled next moment in {clamped:.1} hours."))
}

async fn character_tick_loop(
    character: String,
    ctx: TickContext,
    mut shutdown_rx: tokio::sync::watch::Receiver<()>,
) {
    let mut interval = tokio::time::interval(TICK_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    info!(
        character = %character,
        interval_secs = TICK_INTERVAL.as_secs(),
        "Autonomy tick task started"
    );

    loop {
        tokio::select! {
            _ = interval.tick() => {
                tick_character(&character, &ctx).await;
            }
            _ = shutdown_rx.changed() => {
                // Final save before shutdown.
                let mut s = lock_state(&ctx.state);
                s.mark_dirty();
                save_state(&ctx.data_dir, &character, &mut s);
                s.heartbeat_log.flush_if_dirty();
                info!(character = %character, "Autonomy tick task shutting down");
                break;
            }
        }
    }
}

/// One tick for a single character.
#[expect(
    clippy::too_many_lines,
    reason = "autonomy tick orchestration split is tracked in #109"
)]
async fn tick_character(character: &str, ctx: &TickContext) {
    let now = Instant::now();

    // Collect actions under the lock, then release before any async work.
    let (int_action, keepalive_action, compaction_needed, dream_needed) = {
        let mut s = lock_state(&ctx.state);
        debug!(
            character,
            state = %s.heartbeat.state_at(now),
            ticks_without_user = s.heartbeat.ticks_without_user(),
            turn_count = s.active_turn_count,
            "tick"
        );

        // -- heartbeat ------------------------------------------------------
        let int_action = if ctx.config.enabled && ctx.config.heartbeat.enabled && !s.paused {
            let had_deadline = s.heartbeat.next_wake().is_some();
            let action = s.heartbeat.tick(now);

            if !matches!(action, HeartbeatAction::None) {
                s.mark_dirty();
            }

            // Detect guard trip: had a deadline, tick returned None, deadline now cleared.
            if had_deadline
                && matches!(action, HeartbeatAction::None)
                && s.heartbeat.next_wake().is_none()
            {
                let ticks = s.heartbeat.ticks_without_user();
                s.heartbeat_log.push(
                    HeartbeatEventKind::Dormant,
                    format!("Abandonment guard tripped (ticks without user: {ticks})"),
                );
                // Guard-trip propagation: stop cache keepalive pings.
                s.cache_keepalive.set_next_wake(None);
            }
            action
        } else {
            HeartbeatAction::None
        };

        // -- cache keepalive -------------------------------------------------
        let keepalive_action = s.cache_keepalive.tick(now);

        let dream_backoff_elapsed = s
            .next_dream_attempt_at
            .is_none_or(|next_attempt| now >= next_attempt);
        let dreaming_cfg = ctx.loaded_config.as_ref().map(|lc| &lc.app.memory.dreaming);
        let dream_needed = dream_backoff_elapsed
            && ctx.config.enabled
            && dreaming_cfg.is_some_and(|cfg| cfg.enabled)
            && dream_inactivity_satisfied(dreaming_cfg, s.heartbeat.last_user_at(), now);

        // -- compaction triggers ---------------------------------------------
        let mut compaction_needed = false;
        if ctx.config.enabled && ctx.compaction.enabled && !s.compaction_triggered {
            if ctx.compaction.max_turns > 0
                && s.active_turn_count >= ctx.compaction.max_turns
                && s.active_turn_count >= ctx.compaction.min_turns
            {
                s.compaction_triggered = true;
                compaction_needed = true;
                info!(
                    character = %character,
                    turn_count = s.active_turn_count,
                    max_turns = ctx.compaction.max_turns,
                    "Compaction: max turns trigger fired"
                );
            } else if s.active_turn_count >= ctx.compaction.min_turns {
                let idle_secs = now.duration_since(s.last_compaction_activity).as_secs();
                let threshold_secs = ctx.compaction.idle_trigger.as_secs();
                if threshold_secs > 0 && idle_secs >= threshold_secs {
                    s.compaction_triggered = true;
                    compaction_needed = true;
                    info!(
                        character = %character,
                        idle_secs,
                        threshold_secs,
                        turn_count = s.active_turn_count,
                        "Compaction: idle trigger fired"
                    );
                }
            }
        }

        save_state(&ctx.data_dir, character, &mut s);
        s.heartbeat_log.flush_if_dirty();
        (
            int_action,
            keepalive_action,
            compaction_needed,
            dream_needed,
        )
    };

    // Idle-triggered compaction: when the tick has the dependencies it needs
    // (LLM client, config, notifier, registry), run compaction inline so idle
    // periods actually produce the work. When any dependency is missing
    // (unit-test contexts), fall back to setting the pending flag so the
    // handler's post-generation path picks it up on the user's next message.
    let mut run_compaction_now = false;
    if compaction_needed {
        let have_deps = ctx.llm_client.is_some()
            && ctx.loaded_config.is_some()
            && ctx.notifier.is_some()
            && ctx.registry.is_some();
        if have_deps {
            run_compaction_now = true;
        } else {
            let mut s = lock_state(&ctx.state);
            s.compaction_pending = true;
            s.mark_dirty();
            info!(
                character,
                "Compaction pending flag set for handler pickup (tick missing deps)"
            );
        }
    }

    // -- execute heartbeat action (async, outside lock) -----------------
    // No outer tokio::time::timeout wrapper: `execute_heartbeat_tick` enforces
    // its own soft deadline on the tool loop so a slow loop can't starve
    // later autonomy work.
    match int_action {
        HeartbeatAction::None => {}
        HeartbeatAction::RunTick => {
            {
                let mut s = lock_state(&ctx.state);
                s.heartbeat_log
                    .push(HeartbeatEventKind::TickFired, "Heartbeat tick fired");
            }
            execute_heartbeat_tick(
                character,
                &ctx.state,
                &ctx.data_dir,
                ctx.llm_client.as_ref(),
                ctx.push_tx.as_ref(),
                ctx.loaded_config.as_deref(),
                ctx.notifier.as_ref(),
                ctx.registry.as_ref(),
            )
            .await;
        }
    }

    // -- cache keepalive ping (async, outside lock) -------------------------
    if keepalive_action == CacheKeepaliveAction::Ping {
        let ping_result = execute_dormant_ping(
            character,
            &ctx.state,
            &ctx.data_dir,
            ctx.llm_client.as_ref(),
            ctx.loaded_config.as_deref(),
        )
        .await;
        let mut s = lock_state(&ctx.state);
        match ping_result {
            DormantPingOutcome::Success {
                usage,
                fallback_events,
            } => {
                // Ping actually sent and succeeded — confirm to the keepalive
                // so it schedules the next ping 55 minutes from now.
                s.cache_keepalive.on_cache_warmed(Instant::now());
                push_provider_fallback_events(
                    &mut s,
                    HeartbeatEventKind::DormantPing,
                    &fallback_events,
                );
                s.heartbeat_log.push(
                    HeartbeatEventKind::DormantPing,
                    format!(
                        "Cache refresh ping (cache_read: {}, input: {})",
                        usage.cache_read_tokens, usage.input_tokens
                    ),
                );
                s.heartbeat_log
                    .push(HeartbeatEventKind::DormantPing, "Cache keepalive ping");
                s.mark_dirty();
            }
            DormantPingOutcome::Failed(reason) => {
                s.cache_keepalive.on_ping_failed(Instant::now());
                s.heartbeat_log.push(
                    HeartbeatEventKind::DormantPing,
                    format!(
                        "Cache keepalive ping failed: {}",
                        truncate_summary(&reason, 160)
                    ),
                );
                s.mark_dirty();
            }
            DormantPingOutcome::Skipped(reason) => {
                s.cache_keepalive.on_ping_failed(Instant::now());
                s.heartbeat_log.push(
                    HeartbeatEventKind::DormantPing,
                    format!(
                        "Cache keepalive ping skipped: {}",
                        truncate_summary(&reason, 160)
                    ),
                );
                s.mark_dirty();
            }
        }
    }

    // -- idle-triggered compaction (async, outside lock) -------------------
    if run_compaction_now {
        execute_idle_compaction(character, ctx).await;
    }

    if dream_needed {
        // Revalidate the inactivity gate: the keepalive/compaction awaits above
        // may have yielded long enough for a user message to land (updating
        // last_user_at). The `dream_needed` boolean was snapshotted before those
        // awaits, so recheck now to avoid disturbing a freshly-active conversation.
        let still_inactive = {
            let s = lock_state(&ctx.state);
            let dreaming_cfg = ctx.loaded_config.as_ref().map(|lc| &lc.app.memory.dreaming);
            dream_inactivity_satisfied(dreaming_cfg, s.heartbeat.last_user_at(), Instant::now())
        };
        if still_inactive {
            execute_scheduled_dream(character, ctx).await;
        } else {
            debug!(
                character,
                "Dreaming: skipping scheduled sweep — user became active during tick"
            );
        }
    }

    // -- final persist (in case async actions dirtied state) ---------------
    {
        let mut s = lock_state(&ctx.state);
        save_state(&ctx.data_dir, character, &mut s);
        s.heartbeat_log.flush_if_dirty();
    }
}

/// Run compaction for a character during an autonomy tick, without waiting
/// for the user's next message. Resets the compaction state flags and reloads
/// the engine's cached messages on success so the next turn (or heartbeat
/// tick) sees the compacted `active.jsonl`.
async fn execute_idle_compaction(character: &str, ctx: &TickContext) {
    let Some(llm_client) = ctx.llm_client.as_ref() else {
        return;
    };
    let Some(loaded_config) = ctx.loaded_config.as_deref() else {
        return;
    };
    let Some(notifier) = ctx.notifier.as_ref() else {
        return;
    };
    let Some(registry) = ctx.registry.as_ref() else {
        return;
    };

    info!(
        character,
        "Autonomy tick: running idle-triggered compaction"
    );

    let cached_request = lock_state(&ctx.state).last_request.clone();

    match crate::memory::compaction::run_compaction(
        character,
        loaded_config,
        llm_client,
        notifier,
        cached_request,
        None,
    )
    .await
    {
        Ok(retained_count) => {
            let engine_arc = {
                let mut r = registry.lock().await;
                r.get_or_create(character)
            };
            match engine_arc {
                Ok(engine_arc) => {
                    let mut engine = engine_arc.lock().await;
                    if let Err(e) = engine.reload() {
                        warn!(
                            character,
                            error = %e,
                            "Idle compaction: engine reload failed"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        character,
                        error = %e,
                        "Idle compaction: failed to fetch engine for reload"
                    );
                }
            }

            // Apply deferred character self-edits now that the cache has
            // been bust by the engine reload.
            let character_data_dir = character_data_dir(&ctx.data_dir, character);
            if let Some(lc) = ctx.loaded_config.as_deref() {
                if let Err(e) = crate::memory::deferred_edits::apply_deferred_edits(
                    &character_data_dir,
                    &lc.dirs.config,
                    character,
                ) {
                    warn!(
                        character,
                        error = %e,
                        "Idle compaction: failed to apply deferred edits"
                    );
                }
            }

            let mut s = lock_state(&ctx.state);
            invalidate_cached_request(
                &mut s,
                character,
                CachedRequestInvalidationReason::IdleCompaction,
            );
            s.active_turn_count = retained_count;
            s.compaction_triggered = false;
            s.compaction_pending = false;
            s.last_compaction_activity = Instant::now();
            s.mark_dirty();
            info!(
                character,
                retained_count, "Idle compaction complete, state reset"
            );
        }
        Err(e) => {
            warn!(
                character,
                error = %e,
                "Idle compaction failed, will retry on next idle tick"
            );
            let mut s = lock_state(&ctx.state);
            s.compaction_triggered = false;
            s.compaction_pending = false;
            s.last_compaction_activity = Instant::now();
            s.mark_dirty();
        }
    }
}

async fn execute_scheduled_dream(character: &str, ctx: &TickContext) {
    let Some(loaded_config) = ctx.loaded_config.as_deref() else {
        return;
    };
    let Some(llm_client) = ctx.llm_client.as_ref() else {
        return;
    };
    let dreaming_cfg = &loaded_config.app.memory.dreaming;
    // Gate on the sanitized compaction snapshot (ctx.compaction), not the raw
    // loaded config — AutonomyManager::new / reload_runtime_config disable
    // invalid compaction settings, and pre-dream compaction must honor that
    // just like the idle-compaction path does.
    let compaction_cfg = &ctx.compaction;

    // Pre-dream compaction. Failure aborts the sweep this cycle so the
    // librarian doesn't run against an oversized / stale prompt cache.
    if dreaming_cfg.compact_before
        && compaction_cfg.enabled
        && lock_state(&ctx.state).active_turn_count >= compaction_cfg.min_turns
    {
        let keep_override = if dreaming_cfg.compact_to_zero {
            Some(0)
        } else {
            None
        };
        if let Err(e) = run_pre_dream_compaction(character, ctx, keep_override).await {
            warn!(
                character,
                error = %e,
                "Dreaming: pre-dream compaction failed; skipping sweep this cycle"
            );
            let now = Instant::now();
            let mut s = lock_state(&ctx.state);
            s.dream_failure_count = s.dream_failure_count.saturating_add(1);
            let delay = background_retry_delay(s.dream_failure_count);
            s.next_dream_attempt_at = Some(now + delay);
            s.mark_dirty();
            return;
        }
    }

    let cached_request = {
        let s = lock_state(&ctx.state);
        s.last_request.clone()
    };
    match crate::memory::dreaming::run_librarian_sweep(
        loaded_config,
        &ctx.data_dir,
        llm_client,
        character,
        cached_request.as_ref(),
        false,
        false,
    )
    .await
    {
        Ok(Some(result)) => {
            let mut s = lock_state(&ctx.state);
            s.dream_failure_count = 0;
            s.next_dream_attempt_at = None;
            s.mark_dirty();
            drop(s);
            info!(
                character,
                tool_rounds = result.tool_rounds,
                changed = result.changed.len(),
                audit_appended = result.audit_appended,
                "Dreaming: scheduled AI librarian pass complete"
            );
        }
        Ok(None) => {
            let mut s = lock_state(&ctx.state);
            if s.dream_failure_count != 0 || s.next_dream_attempt_at.is_some() {
                s.dream_failure_count = 0;
                s.next_dream_attempt_at = None;
                s.mark_dirty();
            }
        }
        Err(e) => {
            warn!(character, error = %e, "Dreaming: scheduled sweep failed");
            let now = Instant::now();
            let mut s = lock_state(&ctx.state);
            s.dream_failure_count = s.dream_failure_count.saturating_add(1);
            let delay = background_retry_delay(s.dream_failure_count);
            s.next_dream_attempt_at = Some(now + delay);
            s.mark_dirty();
            debug!(
                character,
                retry_in_secs = delay.as_secs(),
                failure_count = s.dream_failure_count,
                "Dreaming: scheduled retry backed off"
            );
        }
    }
}

/// Run a background compaction immediately before a scheduled dreaming pass.
/// Mirrors the post-success bookkeeping of `execute_idle_compaction` (engine
/// reload, deferred-edit apply, cached-request invalidation, turn-count and
/// activity updates) but does NOT touch the idle-compaction trigger flags —
/// pre-dream compaction is not an idle trigger.
async fn run_pre_dream_compaction(
    character: &str,
    ctx: &TickContext,
    keep_turns_override: Option<usize>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let llm_client = ctx
        .llm_client
        .as_ref()
        .ok_or("no llm_client for pre-dream compaction")?;
    let loaded_config = ctx
        .loaded_config
        .as_deref()
        .ok_or("no loaded_config for pre-dream compaction")?;
    let notifier = ctx
        .notifier
        .as_ref()
        .ok_or("no notifier for pre-dream compaction")?;
    let registry = ctx
        .registry
        .as_ref()
        .ok_or("no engine registry for pre-dream compaction")?;

    info!(
        character,
        keep_turns_override = ?keep_turns_override,
        "Dreaming: running pre-dream compaction"
    );

    let cached_request = lock_state(&ctx.state).last_request.clone();
    let retained_count = crate::memory::compaction::run_compaction(
        character,
        loaded_config,
        llm_client,
        notifier,
        cached_request,
        keep_turns_override,
    )
    .await?;

    let engine_arc = {
        let mut r = registry.lock().await;
        r.get_or_create(character)
    };
    match engine_arc {
        Ok(engine_arc) => {
            let mut engine = engine_arc.lock().await;
            if let Err(e) = engine.reload() {
                warn!(
                    character,
                    error = %e,
                    "Pre-dream compaction: engine reload failed"
                );
            }
        }
        Err(e) => {
            warn!(
                character,
                error = %e,
                "Pre-dream compaction: failed to fetch engine for reload"
            );
        }
    }

    let character_data_dir = character_data_dir(&ctx.data_dir, character);
    if let Err(e) = crate::memory::deferred_edits::apply_deferred_edits(
        &character_data_dir,
        &loaded_config.dirs.config,
        character,
    ) {
        warn!(
            character,
            error = %e,
            "Pre-dream compaction: failed to apply deferred edits"
        );
    }

    let mut s = lock_state(&ctx.state);
    invalidate_cached_request(
        &mut s,
        character,
        CachedRequestInvalidationReason::PreDreamCompaction,
    );
    s.active_turn_count = retained_count;
    s.last_compaction_activity = Instant::now();
    s.mark_dirty();
    info!(character, retained_count, "Pre-dream compaction complete");
    Ok(())
}

// ---------------------------------------------------------------------------
// Heartbeat tick executor
// ---------------------------------------------------------------------------

/// Build the dynamic heartbeat prompt.
///
/// This intentionally keeps heartbeat-specific behavior in HEARTBEAT.md and
/// only documents the scheduler affordances that the runtime understands.
/// A `[Current time: ...]` line is prepended so the character has a fresh
/// time anchor on every tick without needing to call `check_time`.
fn build_heartbeat_prompt(user_name: &str, default_interval: &str) -> String {
    let now = chrono::Local::now()
        .format("%A %Y-%m-%d · %-I:%M %p")
        .to_string();
    format!(
        "\
[Current time: {now}]

[This is a private heartbeat turn governed by the active HEARTBEAT.md content above. \
You have real tools and can search or write workspace and memory files, search \
your conversation history, check the web, generate images, and schedule the next wake.

In addition, you can:

- Schedule your next heartbeat session: use set_next_wake(hours_from_now, \
reason). The minimum is 1 hour, the maximum is 48 hours. Sooner if you want \
to come back to something, later if you'd rather rest. If you don't \
schedule, your next moment will arrive in {default_interval}. This is the \
next opportunity you will have to send {user_name} an autonomous message or \
to continue any unfinished or ongoing work from this current heartbeat \
session.

- Send a message to {user_name}: wrap it in <sendMessage>...</sendMessage>. \
You have the ability to autonomously and spontaneously send messages to \
{user_name}. Any text included in the `sendMessage` tags will be delivered \
to {user_name}.

Thoughts, tool-use results, and any text in your response that is not part \
of `<sendMessage>` tags are private and ephemeral. If you want to carry \
something forward, write it down with a workspace tool.

If you have a multi-step task in progress and want future-you to pick it up, \
edit HEARTBEAT.md to record what you were doing and what to come back to. \
HEARTBEAT.md is read into your prompt at the start of every heartbeat tick, \
so notes you leave there will be visible to your next session.

Changes you make to workspace files, including files under memory/, will persist. \
If nothing needs doing right now, respond with HEARTBEAT_OK and stop.]"
    )
}

fn default_heartbeat_instructions() -> &'static str {
    "# HEARTBEAT\n\n- Use this private turn however seems useful.\n- You may use tools, schedule the next wake, or send {user} a message.\n- If nothing needs action, respond HEARTBEAT_OK."
}

fn load_heartbeat_instructions(character_data_dir: &Path) -> String {
    crate::memory::deferred_edits::load_active_prompt_file(character_data_dir, HEARTBEAT_FILE)
        .unwrap_or_else(|| default_heartbeat_instructions().to_string())
}

fn history_is_between_turns(messages: &[Message]) -> bool {
    matches!(messages.last().map(|m| &m.role), Some(Role::Assistant))
}

/// Rebuild an `LlmRequest` from the compacted conversation on disk.
///
/// Called when `last_request` is `None` (e.g. after compaction invalidated the
/// conversation tail, or after a daemon restart).
/// Returns `None` if there are no messages, the conversation is mid-turn, or
/// the model can't be resolved.
fn rebuild_request_from_disk(
    character: &str,
    data_dir: &Path,
    config: &LoadedConfig,
) -> Option<LlmRequest> {
    use crate::engine::messages::MessageStore;
    use crate::handler::{PrepareChatContextParams, PreparedChatContext, prepare_chat_context};
    use shore_config::character_active_jsonl;

    let char_dir = character_data_dir(data_dir, character);
    let active_path = character_active_jsonl(data_dir, character);

    let store = MessageStore::load(active_path)
        .map_err(|e| warn!(character, error = %e, "Heartbeat rebuild: failed to load messages"))
        .ok()?;
    if store.messages().is_empty() {
        return None;
    }
    let has_prior_context = crate::engine::segments::SegmentReader::load(&char_dir)
        .is_ok_and(|r| r.segment_count() > 0);
    if !history_is_between_turns(store.messages()) {
        info!(
            character,
            "Heartbeat rebuild: skipping tick because conversation is mid-turn"
        );
        return None;
    }

    // Resolve the normal chat model with the per-character preference
    // overlay applied. Heartbeat callers apply their background model
    // override after rebuilding; keepalive must refresh the chat cache
    // prefix using the same model+sampler chat would have produced, not
    // a heartbeat-only model or an un-overlaid `defaults.model`.
    let resolved = crate::preferences::resolve_chat_model_for_character(config, character)?;

    let PreparedChatContext {
        llm_messages,
        system,
        tool_defs,
        ..
    } = prepare_chat_context(PrepareChatContextParams {
        character,
        character_data_dir: &char_dir,
        config,
        resolved: &resolved,
        messages: store.messages(),
        has_prior_context,
        is_private: false,
        // Must mirror the live chat path: a rebuild reconstructs the
        // request chat would have produced, so the unsigned-thinking
        // shape has to match too. Otherwise the heartbeat-rebuilt cache
        // prefix diverges from the chat-warmed prefix on OpenAI/Z.AI
        // SDKs, invalidating the cache the next chat call would have hit.
        include_unsigned_thinking: resolved.sdk.echoes_unsigned_thinking(),
    });

    match LedgerClient::build_request_with_provider_keys(
        &resolved,
        &config.providers,
        llm_messages,
        system,
        tool_defs,
        None,
    ) {
        Ok(req) => {
            info!(
                character,
                "Heartbeat: rebuilt request from compacted conversation"
            );
            Some(req)
        }
        Err(e) => {
            warn!(character, error = %e, "Heartbeat: failed to rebuild request");
            None
        }
    }
}

fn apply_heartbeat_model_override(
    request: &mut LlmRequest,
    config: &LoadedConfig,
    character: &str,
) -> bool {
    // If `defaults.background.heartbeat` (or its fallbacks) is not set,
    // we have no override to apply and keep the chat model.
    let Some(configured_name) = config
        .app
        .defaults
        .resolve_background_model_name(shore_config::app::BackgroundTask::Heartbeat)
    else {
        return false;
    };
    // The configured name must actually resolve to a catalog entry. If it
    // doesn't (typo, removed model, etc.), don't silently fall back to
    // whichever chat model the catalog returns first — keep the chat
    // model the user is currently using and warn so the misconfig is
    // visible. (resolve_background_model's silent fallback is fine for
    // compaction/dreaming where some model is better than none.)
    if let Err(e) = config.models.find_model(configured_name) {
        warn!(
            character,
            configured_model = %configured_name,
            error = %e,
            "Heartbeat: configured model not found in catalog; keeping chat model"
        );
        return false;
    }
    let Some(resolved) = crate::preferences::resolve_background_model(
        config,
        shore_config::app::BackgroundTask::Heartbeat,
        character,
    ) else {
        return false;
    };
    if resolved.model_id == request.model {
        return false;
    }
    match LedgerClient::build_request_with_provider_keys(
        &resolved,
        &config.providers,
        request.messages.clone(),
        request.system.clone(),
        request.tools.clone(),
        None,
    ) {
        Ok(mut new_req) => {
            info!(
                character,
                heartbeat_model = %resolved.name,
                model_id = %new_req.model,
                "Heartbeat: using configured heartbeat model"
            );
            new_req.forensic_character = Some(character.to_owned());
            *request = new_req;
            true
        }
        Err(e) => {
            warn!(
                character,
                error = %e,
                heartbeat_model = %resolved.name,
                "Heartbeat: failed to build override request, falling back to chat model"
            );
            false
        }
    }
}

/// Execute a heartbeat tick: a real tool loop using non-streaming
/// generate() calls. Tool loop messages are ephemeral — only <sendMessage>
/// output persists to active.jsonl. All activity is logged to the ring buffer
/// for `shore log --heartbeat`.
#[expect(
    clippy::too_many_arguments,
    reason = "heartbeat tick boundary carries scheduler dependencies"
)]
#[expect(
    clippy::too_many_lines,
    reason = "heartbeat tick tool-loop orchestration split is tracked in #109"
)]
async fn execute_heartbeat_tick(
    character: &str,
    state: &Arc<Mutex<AutonomyState>>,
    data_dir: &Path,
    llm_client: Option<&LedgerClient>,
    push_tx: Option<&broadcast::Sender<ServerMessage>>,
    loaded_config: Option<&LoadedConfig>,
    notifier: Option<&NotificationService>,
    registry: Option<&Arc<tokio::sync::Mutex<CharacterRegistry>>>,
) {
    let Some(client) = llm_client else { return };

    // Clone last_request under the lock, then release.
    let mut request = {
        let s = lock_state(state);
        if let Some(req) = &s.last_request {
            req.clone()
        } else {
            drop(s);
            let Some(config) = loaded_config else { return };
            if let Some(req) = rebuild_request_from_disk(character, data_dir, config) {
                // Persist the rebuilt request so keepalive pings can use it;
                // otherwise pings silently no-op after daemon restart until
                // the next user message.
                let mut s = lock_state(state);
                cache_last_request(&mut s, character, req.clone());
                drop(s);
                req
            } else {
                info!(
                    character,
                    "Heartbeat: skipping tick (no prior conversation)"
                );
                return;
            }
        }
    };

    // Clear the stale request ID from the previous user message —
    // reusing it across heartbeat iterations can confuse OpenRouter's
    // routing/dedup and cause unexpected cache misses.
    request.rid = None;
    request.forensic_character = Some(character.to_owned());

    let Some(lc) = loaded_config else { return };

    apply_heartbeat_model_override(&mut request, lc, character);

    // Build the dynamic heartbeat prompt.
    let character_data_dir = character_data_dir(data_dir, character);
    if let Err(e) = crate::memory::deferred_edits::ensure_active_prompt_snapshot(
        &character_data_dir,
        &lc.dirs.config,
        character,
    ) {
        warn!(character, error = %e, "Heartbeat: failed to prepare active prompt snapshot");
    }
    let user_name = lc.app.defaults.resolve_display_name();
    let default_interval_secs = lc
        .app
        .behavior
        .autonomy
        .heartbeat
        .fallback_heartbeat_interval
        .as_secs();
    let default_interval_str = if default_interval_secs >= 3600 && default_interval_secs % 3600 == 0
    {
        let h = default_interval_secs / 3600;
        if h == 1 {
            "1 hour".to_string()
        } else {
            format!("{h} hours")
        }
    } else {
        format!("{} minutes", default_interval_secs / 60)
    };
    let heartbeat_instructions =
        load_heartbeat_instructions(&character_data_dir).replace("{user}", &user_name);
    let heartbeat_prompt = build_heartbeat_prompt(&user_name, &default_interval_str);

    // Pin the heartbeat instructions + prompt at a fixed slot in
    // `request.messages` via `push_inline_system`. The heartbeat tool
    // loop below pushes `assistant` + `user(tool_result)` after this,
    // so the system entry's index must not depend on tail length. (The
    // removed `system_suffix` affordance re-expanded at the current tail
    // every `generate()` call and busted Anthropic's content-addressed
    // prefix cache across iterations — see
    // [`LlmRequest::push_inline_system`].)
    //
    // The cached chat prefix is left untouched — the inline system
    // entry sits AFTER chat's messages, so subsequent chat calls
    // reusing `last_request`'s prefix never see this text either.
    request.push_inline_system(format!("{heartbeat_instructions}\n\n{heartbeat_prompt}"));
    // Heartbeat ticks fire on a slow cadence; route the payload log to
    // the long-retention tier so reflection traces survive past chat's
    // 3-day prune.
    request.retain_long = true;

    // NOTE: set_next_wake is in the base tool set (tools/basic.rs), so the
    // tools array is identical between normal messages and heartbeat ticks.
    // This prevents cache prefix invalidation. Instructions for using
    // set_next_wake are in the heartbeat prompt.

    let tool_ctx = build_tool_context(character, data_dir, client, lc);
    let tool_ctx = Arc::new(HeartbeatToolContext {
        inner: tool_ctx,
        state: state.clone(),
    });
    let max_normal_iterations = lc.app.behavior.autonomy.heartbeat.max_tool_rounds;
    let wrap_up_grace = lc.app.behavior.autonomy.heartbeat.wrap_up_grace_rounds;
    let total_iterations = max_normal_iterations.saturating_add(wrap_up_grace);

    info!(
        character,
        max_iterations = max_normal_iterations,
        wrap_up_grace,
        "Heartbeat: executing tool loop tick"
    );

    // Collect <sendMessage> content across iterations (last-wins).
    let mut send_message_text: Option<String> = None;
    let mut cache_warmed = false;

    let loop_deadline = std::time::Instant::now() + HEARTBEAT_LOOP_DEADLINE;
    let mut wrap_up_nudged = false;

    for iteration in 0..total_iterations {
        let deadline_reached = std::time::Instant::now() >= loop_deadline;
        let normal_cap_reached = iteration >= max_normal_iterations;

        if (deadline_reached || normal_cap_reached) && !wrap_up_nudged {
            if wrap_up_grace == 0 {
                warn!(
                    character,
                    iteration,
                    deadline_reached,
                    normal_cap_reached,
                    "Heartbeat: tool budget reached, no wrap-up grace configured"
                );
                break;
            }
            warn!(
                character,
                iteration,
                deadline_reached,
                normal_cap_reached,
                wrap_up_grace,
                "Heartbeat: tool budget reached, nudging wrap-up"
            );
            append_wrap_up_nudge(&mut request);
            wrap_up_nudged = true;
            {
                let mut s = lock_state(state);
                s.heartbeat_log.push(
                    HeartbeatEventKind::ToolUse,
                    "Wrap-up nudge: budget reached, model asked to summarize".to_string(),
                );
            }
        } else if deadline_reached && wrap_up_nudged {
            warn!(
                character,
                iteration, "Heartbeat: deadline tripped during wrap-up grace, breaking"
            );
            break;
        }

        let call_type = if iteration == 0 {
            CallType::Heartbeat
        } else {
            CallType::HeartbeatToolLoop
        };

        let (resp, fallback_events) = match client
            .generate_with_config_fallback(&mut request, lc, call_type, character, false)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                error!(character, error = %e, iteration, "Heartbeat: LLM call failed");
                break;
            }
        };
        if !fallback_events.is_empty() {
            let mut s = lock_state(state);
            push_provider_fallback_events(&mut s, HeartbeatEventKind::ToolUse, &fallback_events);
            s.mark_dirty();
        }
        cache_warmed = true;

        info!(
            character,
            iteration,
            finish_reason = %resp.finish_reason,
            input_tokens = resp.usage.input_tokens,
            output_tokens = resp.usage.output_tokens,
            cache_read = resp.usage.cache_read_tokens,
            "Heartbeat: LLM response"
        );

        // Log text blocks.
        for block in &resp.content_blocks {
            if let ContentBlock::Text { text } = block {
                if !text.trim().is_empty() {
                    let preview: String = text.chars().take(200).collect();
                    info!(character, iteration, content = %preview, "Heartbeat: thought");
                }
            }
        }

        // Check for <sendMessage> in this response (last-wins).
        let text = resp.extract_text();
        if let Some(msg) = extract_send_message(&text) {
            send_message_text = Some(msg);
        }

        // Build assistant message from content blocks (filter unsigned thinking)
        // and push it before any exit path. Every successful generate() must
        // land in the ephemeral heartbeat history before any exit path, keeping
        // later tool-loop requests well formed.
        //
        // Uses content_block_to_api_json (Anthropic path) — heartbeat always
        // uses Anthropic models. ZAI would need content_block_to_json.
        let assistant_content: Vec<serde_json::Value> = resp
            .content_blocks
            .iter()
            .filter_map(crate::content_util::content_block_to_api_json)
            .collect();
        if !assistant_content.is_empty() {
            request.messages.push(json!({
                "role": "assistant",
                "content": assistant_content,
            }));
        }

        // Extract tool uses.
        let tool_uses = crate::content_util::extract_tool_uses(&resp.content_blocks);

        // If no tool use or finish_reason != "tool_use", we're done.
        if tool_uses.is_empty() || resp.finish_reason != "tool_use" {
            break;
        }

        // Dispatch each tool, collect results.
        let mut tool_results: Vec<serde_json::Value> = Vec::new();

        for (id, name, input) in &tool_uses {
            let input_str = serde_json::to_string(input).unwrap_or_default();
            info!(
                character,
                iteration,
                tool = %name, tool_id = %id,
                input = %truncate_summary(&input_str, 200),
                "Heartbeat: executing tool"
            );

            // Intercept set_next_wake — handled inline, not dispatched.
            let (output_str, is_error) = if name.as_str() == "set_next_wake" {
                crate::content_util::dispatch_result_to_output(Ok(schedule_next_wake_in_state(
                    state.as_ref(),
                    input,
                )))
            } else {
                crate::content_util::dispatch_result_to_output(
                    tool_system::dispatch_tool(name, input.clone(), tool_ctx.as_ref()).await,
                )
            };

            info!(
                character,
                iteration,
                tool = %name, is_error,
                output = %truncate_summary(&output_str, 200),
                "Heartbeat: tool result"
            );

            tool_results.push(crate::content_util::build_tool_result_json(
                id,
                &output_str,
                is_error,
            ));

            // Log to ring buffer (skip set_next_wake — already logged above).
            if name.as_str() != "set_next_wake" {
                let mut s = lock_state(state);
                s.heartbeat_log.push(
                    HeartbeatEventKind::ToolUse,
                    format!("Tool: {name} → {}", truncate_summary(&output_str, 80)),
                );
            }
        }

        // Append tool results as user message.
        request.messages.push(json!({
            "role": "user",
            "content": tool_results,
        }));
    }

    // -- Cache warmed: the tick itself was a cache-warming LLM call -----------
    if cache_warmed {
        let mut s = lock_state(state);
        s.cache_keepalive.on_cache_warmed(Instant::now());
        // Mirror schedule to keepalive (character may have called set_next_wake).
        if let Some(wake) = s.heartbeat.next_wake() {
            s.cache_keepalive.set_next_wake(Some(wake));
        }
    }

    // -- Persist <sendMessage> if present --------------------------------------
    if let Some(user_msg) = send_message_text {
        info!(character, msg = %truncate_summary(&user_msg, 200), "Heartbeat: sending message to user");

        let content_blocks = vec![ContentBlock::Text {
            text: user_msg.clone(),
        }];
        let content = derive_content_from_blocks(&content_blocks);
        let msg = Message {
            msg_id: format!("m_{}", uuid::Uuid::new_v4()),
            role: Role::Assistant,
            content,
            images: vec![],
            content_blocks,
            alt_index: None,
            alt_count: None,
            alternatives: vec![],
            provider_key: request.provider_key.clone(),
            timestamp: chrono::Local::now().to_rfc3339(),
        };

        // Persist via the engine lock to avoid racing with the handler's
        // MessageStore writes (atomic temp+rename). The engine's append_message
        // also calls broadcast_history(), so clients are notified automatically.
        if let Some(reg) = registry {
            // Acquire engine_arc under registry lock, then drop it before
            // locking the engine — matches handler's lock ordering and avoids
            // holding the registry during disk I/O.
            let engine_arc = {
                let mut r = reg.lock().await;
                r.get_or_create(character)
            };
            match engine_arc {
                Ok(engine_arc) => {
                    let mut engine = engine_arc.lock().await;
                    if let Err(e) = engine.append_message(msg.clone()) {
                        error!(character, error = %e, "Failed to persist autonomous message via engine");
                    } else if let Some(tx) = push_tx {
                        let _ = tx.send(ServerMessage::NewMessage(
                            shore_protocol::server_msg::NewMessage {
                                revision: engine.current_revision(),
                                character: Some(character.to_string()),
                                origin: Some(shore_protocol::server_msg::MessageOrigin::Autonomous),
                                message: msg.clone(),
                            },
                        ));
                    }
                }
                Err(e) => {
                    error!(character, error = %e, "Failed to get engine for autonomous message");
                }
            }
        } else {
            error!(
                character,
                "No registry available, autonomous message not persisted"
            );
        }
        if let Some(n) = notifier {
            n.notify(
                NotificationEvent::AutonomousMessage,
                &format!("Shore — {character}"),
                &msg.content,
            );
        }

        let mut s = lock_state(state);
        let preview: String = msg.content.chars().take(80).collect();
        s.heartbeat_log.push(
            HeartbeatEventKind::MessageSent,
            format!("Autonomous message sent: {preview}"),
        );
        s.mark_dirty();
    } else {
        let mut s = lock_state(state);
        s.heartbeat_log.push(
            HeartbeatEventKind::MessageSkipped,
            "Tick completed — no message sent".to_string(),
        );
    }
}

// ---------------------------------------------------------------------------
// Tool context builder for heartbeat ticks
// ---------------------------------------------------------------------------

/// Build a SharedToolContext for heartbeat ticks.
///
/// Uses the same ingredients as the handler (LlmClient, LoadedConfig, data_dir)
/// but resolves models with heartbeat-specific fallbacks. All tools work —
/// workspace/memory files, images, and web. The only gap is AutonomyManager (the
/// heatmap tool degrades gracefully via the trait default).
fn build_tool_context(
    character: &str,
    data_dir: &Path,
    client: &LedgerClient,
    config: &LoadedConfig,
) -> SharedToolContext {
    let char_dir = character_data_dir(data_dir, character);

    let image_gen_config = resolve_image_gen_config(
        config.app.defaults.image_generation.as_deref(),
        &config.models.image_generation,
    )
    .ok();
    let embedder = resolve_embedder(
        config.app.defaults.embedding.as_deref(),
        &config.models.embedding,
        client.inner().http_client(),
    )
    .map_err(|e| {
        debug!(character, error = %e, "Heartbeat: embedder unavailable; semantic memory retrieval disabled");
    })
    .ok();

    debug!(
        character,
        has_image_gen = image_gen_config.is_some(),
        "Heartbeat: tool context built"
    );

    SharedToolContext {
        image_dir: char_dir.join("images").to_string_lossy().into_owned(),
        llm_client: client.inner().clone(),
        image_gen_config,
        search_config: config.app.behavior.tool_use.search.clone(),
        character_name: character.to_string(),
        workspace_dir: character_workspace_dir(&config.dirs.config, character)
            .to_string_lossy()
            .into_owned(),
        markdown_store: crate::memory::markdown_store::MarkdownMemoryStore::open_sync(
            character_memory_dir(&config.dirs.config, character),
        )
        .ok(),
        memory_retrieval_config: config.app.memory.retrieval.clone(),
        embedder,
        memory_index_path: crate::memory::workspace_index::index_path(
            &config.dirs.cache,
            character,
        ),
        config_dir: config.dirs.config.to_string_lossy().into_owned(),
        character_data_dir: char_dir.to_string_lossy().into_owned(),
    }
}

/// Extract text between XML-style tags. Returns the last match (last-wins).
fn extract_tag(content: &str, start_tag: &str, end_tag: &str) -> Option<String> {
    let mut result = None;
    let mut search_from = 0;
    while let Some(start_pos) = content[search_from..].find(start_tag) {
        let abs_start = search_from + start_pos + start_tag.len();
        if let Some(end_pos) = content[abs_start..].find(end_tag) {
            let inner = content[abs_start..abs_start + end_pos].trim();
            if !inner.is_empty() {
                result = Some(inner.to_string());
            }
            search_from = abs_start + end_pos + end_tag.len();
        } else {
            break;
        }
    }
    result
}

/// Extract text between `<sendMessage>` and `</sendMessage>` tags (last-wins).
fn extract_send_message(content: &str) -> Option<String> {
    extract_tag(content, "<sendMessage>", "</sendMessage>")
}

const WRAP_UP_NUDGE_TEXT: &str = "[System nudge: heartbeat tool-use budget reached. Wrap up now — \
if you have unfinished work, edit HEARTBEAT.md so future-you can pick it up where you left off. \
Then either send a final <sendMessage> or respond HEARTBEAT_OK and stop.]";

/// Append the wrap-up nudge text to the request. The nudge has to land on the
/// last user message (Anthropic rejects two consecutive user turns), so this
/// folds it into the trailing tool_results message when one exists, and only
/// pushes a fresh user message when the request happens to end on an assistant
/// turn or is otherwise empty.
fn append_wrap_up_nudge(request: &mut LlmRequest) {
    let block = json!({"type": "text", "text": WRAP_UP_NUDGE_TEXT});
    if let Some(last) = request.messages.last_mut() {
        if last.get("role").and_then(|r| r.as_str()) == Some("user") {
            match last.get_mut("content") {
                Some(serde_json::Value::Array(arr)) => {
                    arr.push(block);
                    return;
                }
                Some(serde_json::Value::String(existing)) => {
                    let combined = format!("{existing}\n\n{WRAP_UP_NUDGE_TEXT}");
                    last["content"] = json!(combined);
                    return;
                }
                _ => {}
            }
        }
    }
    request.messages.push(json!({
        "role": "user",
        "content": WRAP_UP_NUDGE_TEXT,
    }));
}

// ---------------------------------------------------------------------------
// Dormant ping executor
// ---------------------------------------------------------------------------

struct DormantPingUsage {
    input_tokens: u64,
    cache_read_tokens: u64,
}

enum DormantPingOutcome {
    Success {
        usage: DormantPingUsage,
        fallback_events: Vec<CredentialFallbackEvent>,
    },
    Failed(String),
    Skipped(String),
}

/// Build a keepalive ping request from the most recent real request.
///
/// The ping MUST be byte-identical to the cached request in every field that
/// participates in the prompt cache prefix (tools, system, model, and the
/// original message sequence) — any divergence forces a cache write at 2.0×
/// instead of a cache read at 0.1×, defeating the entire keepalive subsystem.
///
/// The only permitted differences:
/// - `max_tokens = 1` (we don't want generation, just a cache touch)
/// - `rid = None` (don't reuse a stale request ID)
/// - `forensic_character` set for logging
/// - one extra user message appended (Anthropic requires conversations to end
///   with a user turn; the cloned request ends on an assistant message)
fn build_keepalive_ping(req: &LlmRequest, character: &str) -> LlmRequest {
    let mut ping = req.clone();
    ping.max_tokens = 1;
    ping.rid = None;
    ping.forensic_character = Some(character.to_owned());
    ping.messages.push(serde_json::json!({
        "role": "user",
        "content": "."
    }));
    ping
}

/// Send a minimal API call (max_tokens=1) to keep the prompt cache warm
/// while the character is dormant (no user activity).
///
/// Returns a structured outcome so the scheduler only advances the keepalive
/// deadline after a confirmed cache-warming call, and records skipped/failed
/// attempts in the heartbeat log.
async fn execute_dormant_ping(
    character: &str,
    state: &Arc<Mutex<AutonomyState>>,
    data_dir: &Path,
    llm_client: Option<&LedgerClient>,
    loaded_config: Option<&LoadedConfig>,
) -> DormantPingOutcome {
    let Some(client) = llm_client else {
        return DormantPingOutcome::Skipped("no LLM client available".to_string());
    };

    let mut request = {
        let s = lock_state(state);
        if let Some(req) = &s.last_request {
            build_keepalive_ping(req, character)
        } else {
            drop(s);
            let Some(config) = loaded_config else {
                debug!(character, "Dormant ping: no cached request, skipping");
                return DormantPingOutcome::Skipped(
                    "no cached request and no loaded config for rebuild".to_string(),
                );
            };
            if let Some(req) = rebuild_request_from_disk(character, data_dir, config) {
                let mut s = lock_state(state);
                cache_last_request(&mut s, character, req.clone());
                drop(s);
                build_keepalive_ping(&req, character)
            } else {
                debug!(
                    character,
                    "Dormant ping: failed to rebuild request, skipping"
                );
                return DormantPingOutcome::Skipped("no cached or rebuildable request".to_string());
            }
        }
    };
    let generate_result = match loaded_config {
        Some(config) => {
            client
                .generate_with_config_fallback(
                    &mut request,
                    config,
                    CallType::Keepalive,
                    character,
                    false,
                )
                .await
        }
        None => client
            .generate(&request, CallType::Keepalive, character, false)
            .await
            .map(|resp| (resp, Vec::new())),
    };

    match generate_result {
        Ok((resp, fallback_events)) => {
            info!(
                character,
                cache_read = resp.usage.cache_read_tokens,
                input_tokens = resp.usage.input_tokens,
                "Dormant ping: cache refreshed"
            );
            DormantPingOutcome::Success {
                usage: DormantPingUsage {
                    input_tokens: resp.usage.input_tokens,
                    cache_read_tokens: resp.usage.cache_read_tokens,
                },
                fallback_events,
            }
        }
        Err(e) => {
            error!(character, error = %e, "Dormant ping failed");
            DormantPingOutcome::Failed(e.to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    fn test_config() -> AutonomyConfig {
        AutonomyConfig::default()
    }

    fn test_message(role: Role) -> Message {
        Message {
            msg_id: format!("m_{}", uuid::Uuid::new_v4()),
            role,
            content: "hello".into(),
            images: vec![],
            content_blocks: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
            alt_index: None,
            alt_count: None,
            alternatives: vec![],
            provider_key: None,
            timestamp: chrono::Local::now().to_rfc3339(),
        }
    }

    fn test_manager(data_dir: &Path) -> AutonomyManager {
        let (_tx, rx) = tokio::sync::watch::channel(());
        AutonomyManager::new(
            test_config(),
            Default::default(),
            data_dir.to_path_buf(),
            rx,
        )
    }

    #[test]
    fn history_between_turns_only_after_assistant_message() {
        assert!(!history_is_between_turns(&[]));
        assert!(!history_is_between_turns(&[test_message(Role::User)]));
        assert!(history_is_between_turns(&[
            test_message(Role::User),
            test_message(Role::Assistant)
        ]));
    }

    fn empty_request() -> LlmRequest {
        LlmRequest {
            sdk: shore_config::models::Sdk::Anthropic,
            model: "test".into(),
            api_key: "k".into(),
            api_key_name: None,
            base_url: None,
            messages: vec![],
            system: None,
            tools: None,
            max_tokens: 1024,
            temperature: None,
            top_p: None,
            provider_options: None,
            provider_key: None,
            rid: None,
            forensic_character: None,
            retain_long: false,
        }
    }

    #[test]
    fn wrap_up_nudge_folds_into_trailing_tool_results() {
        let mut req = empty_request();
        req.messages.push(json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "tu_1", "content": "ok"}
            ],
        }));
        append_wrap_up_nudge(&mut req);
        assert_eq!(
            req.messages.len(),
            1,
            "must not introduce a second user turn"
        );
        let content = req.messages[0]["content"]
            .as_array()
            .expect("array content");
        assert_eq!(content.len(), 2);
        assert_eq!(content[1]["type"], "text");
        assert!(
            content[1]["text"]
                .as_str()
                .unwrap()
                .contains("HEARTBEAT.md")
        );
    }

    #[test]
    fn wrap_up_nudge_folds_into_string_user_content() {
        let mut req = empty_request();
        req.messages.push(json!({"role": "user", "content": "hi"}));
        append_wrap_up_nudge(&mut req);
        assert_eq!(req.messages.len(), 1);
        let s = req.messages[0]["content"].as_str().expect("string content");
        assert!(s.starts_with("hi"));
        assert!(s.contains("HEARTBEAT.md"));
    }

    #[test]
    fn wrap_up_nudge_pushes_after_assistant_turn() {
        let mut req = empty_request();
        req.messages.push(json!({"role": "user", "content": "hi"}));
        req.messages
            .push(json!({"role": "assistant", "content": "bye"}));
        append_wrap_up_nudge(&mut req);
        assert_eq!(req.messages.len(), 3);
        assert_eq!(req.messages[2]["role"], "user");
        assert!(
            req.messages[2]["content"]
                .as_str()
                .unwrap()
                .contains("HEARTBEAT.md")
        );
    }

    #[test]
    fn wrap_up_nudge_pushes_when_request_is_empty() {
        let mut req = empty_request();
        append_wrap_up_nudge(&mut req);
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0]["role"], "user");
    }

    // -- ensure_state ---------------------------------------------------------

    #[test]
    fn ensure_state_creates_on_first_call() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mgr = rt.block_on(async { test_manager(tmp.path()) });

        rt.block_on(async {
            mgr.ensure_state("alice", None);
            assert!(mgr.states.contains_key("alice"));
        });
    }

    #[test]
    fn ensure_state_idempotent() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mgr = rt.block_on(async { test_manager(tmp.path()) });

        rt.block_on(async {
            mgr.ensure_state("alice", None);
            mgr.ensure_state("alice", None);
            assert_eq!(mgr.states.len(), 1);
        });
    }

    #[test]
    fn ensure_state_recovers_from_poisoned_handles_mutex() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let (tx, rx) = tokio::sync::watch::channel(());
        let mgr = AutonomyManager::new(
            test_config(),
            Default::default(),
            tmp.path().to_path_buf(),
            rx,
        );

        let result = catch_unwind(AssertUnwindSafe(|| {
            let _guard = mgr.handles.lock().unwrap();
            panic!("poison autonomy handles");
        }));
        assert!(result.is_err());

        rt.block_on(async {
            assert!(mgr.ensure_state("alice", None));
        });
        assert!(mgr.states.contains_key("alice"));

        drop(tx);
        rt.block_on(async {
            mgr.shutdown().await;
        });
    }

    // -- notify ---------------------------------------------------------------

    #[test]
    fn notify_without_state_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let (_tx, rx) = tokio::sync::watch::channel(());
        let mgr = AutonomyManager::new(
            test_config(),
            Default::default(),
            tmp.path().to_path_buf(),
            rx,
        );
        // Should not panic.
        mgr.notify_user_message("nobody", 0);
        mgr.notify_assistant_message("nobody", 0);
    }

    // -- status ---------------------------------------------------------------

    #[test]
    fn status_returns_none_for_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let (_tx, rx) = tokio::sync::watch::channel(());
        let mgr = AutonomyManager::new(
            test_config(),
            Default::default(),
            tmp.path().to_path_buf(),
            rx,
        );
        assert!(mgr.status("nobody").is_none());
    }

    #[test]
    fn status_returns_some_after_ensure() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();

        rt.block_on(async {
            let mgr = test_manager(tmp.path());
            mgr.ensure_state("alice", None);
            let status = mgr.status("alice").unwrap();
            assert_eq!(status.heartbeat_state, "Active");
            assert_eq!(status.ticks_without_user, 0);
        });
    }

    #[test]
    fn status_reports_dormant_after_force_dormant() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();

        rt.block_on(async {
            let mgr = test_manager(tmp.path());
            mgr.ensure_state("alice", None);
            assert!(mgr.heartbeat_set_dormant("alice"));

            let status = mgr.status("alice").unwrap();
            assert_eq!(status.heartbeat_state, "Dormant");
        });
    }

    #[test]
    fn status_reports_dormant_after_silent_duration() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();

        rt.block_on(async {
            let mgr = test_manager(tmp.path());
            mgr.ensure_state("alice", None);
            mgr.with_state("alice", |s| {
                let now = Instant::now();
                s.heartbeat
                    .on_user_message(now - Duration::from_secs(3 * 24 * 60 * 60));
            });

            let status = mgr.status("alice").unwrap();
            assert_eq!(status.heartbeat_state, "Dormant");
        });
    }

    // -- backfill -------------------------------------------------------------

    #[test]
    fn ensure_state_returns_true_then_false() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mgr = rt.block_on(async { test_manager(tmp.path()) });

        rt.block_on(async {
            assert!(mgr.ensure_state("alice", None));
            assert!(!mgr.ensure_state("alice", None));
        });
    }

    #[test]
    fn backfill_activity_updates_message_count() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mgr = rt.block_on(async { test_manager(tmp.path()) });

        rt.block_on(async {
            mgr.ensure_state("alice", None);

            let timestamps = vec![
                chrono::NaiveDate::from_ymd_opt(2026, 3, 20)
                    .unwrap()
                    .and_hms_opt(10, 0, 0)
                    .unwrap(),
                chrono::NaiveDate::from_ymd_opt(2026, 3, 21)
                    .unwrap()
                    .and_hms_opt(14, 0, 0)
                    .unwrap(),
                chrono::NaiveDate::from_ymd_opt(2026, 3, 22)
                    .unwrap()
                    .and_hms_opt(9, 0, 0)
                    .unwrap(),
            ];
            mgr.backfill_activity("alice", &timestamps);

            let (_stats, count) = mgr.activity_stats("alice").unwrap();
            assert_eq!(count, 3);
        });
    }

    #[test]
    fn backfill_seeds_last_user_at_from_recent_history() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mgr = rt.block_on(async { test_manager(tmp.path()) });

        rt.block_on(async {
            mgr.ensure_state("alice", None);
            // Heartbeat starts with no user activity.
            mgr.with_state("alice", |s| assert!(s.heartbeat.last_user_at().is_none()));

            // Most recent backfilled user turn is ~2 minutes ago.
            let now_local = chrono::Local::now().naive_local();
            let timestamps = vec![
                now_local - chrono::Duration::minutes(30),
                now_local - chrono::Duration::minutes(2),
            ];
            mgr.backfill_activity("alice", &timestamps);

            // last_user_at is now seeded and reflects the recent (~2min) turn,
            // so a short inactivity window would NOT be satisfied.
            mgr.with_state("alice", |s| {
                let last = s.heartbeat.last_user_at().expect("seeded");
                let elapsed = Instant::now().duration_since(last);
                assert!(elapsed < Duration::from_secs(5 * 60));
                assert!(elapsed >= Duration::from_secs(60));
            });
        });
    }

    // -- persistence ----------------------------------------------------------

    #[test]
    fn save_and_restore_state() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        std::fs::create_dir_all(data_dir.join("alice")).unwrap();

        // Create and save.
        let mut state = AutonomyState {
            heartbeat: HeartbeatClock::with_config(&Default::default()),
            cache_keepalive: CacheKeepalive::new(),
            activity: ActivityTracker::new(),
            heartbeat_log: HeartbeatLog::new(),
            paused: false,
            dirty: true,
            last_compaction_activity: Instant::now(),
            compaction_triggered: false,
            active_turn_count: 0,
            compaction_pending: false,
            last_request: None,
            next_dream_attempt_at: None,
            dream_failure_count: 0,
        };
        save_state(data_dir, "alice", &mut state);
        assert!(!state.dirty);

        // Verify file exists.
        assert!(state_path(data_dir, "alice").exists());

        // Restore.
        let persisted = load_state(data_dir, "alice").unwrap();
        assert_eq!(persisted.version, STATE_VERSION);
        assert_eq!(persisted.ticks_without_user, 0);
        // next_wake_at should be None (clock was fresh, no deadline set).
        assert!(persisted.next_wake_at.is_none());
    }

    #[test]
    fn restore_state_recovers_ticks_and_timestamps() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        std::fs::create_dir_all(data_dir.join("alice")).unwrap();

        let persisted = PersistedState {
            version: STATE_VERSION,
            ticks_without_user: 5,
            next_wake_at: Some("2026-04-08T20:00:00+00:00".into()),
            last_user_at: Some("2026-04-08T14:00:00+00:00".into()),
        };
        let json = serde_json::to_string(&persisted).unwrap();
        std::fs::write(state_path(data_dir, "alice"), json).unwrap();

        let loaded = load_state(data_dir, "alice").unwrap();
        assert_eq!(loaded.ticks_without_user, 5);
        assert!(loaded.next_wake_at.is_some());

        // Test the full restore path: verify Instant conversion doesn't panic.
        let mut clock = HeartbeatClock::with_config(&Default::default());
        restore_from_persisted(&loaded, &mut clock);
        assert_eq!(clock.ticks_without_user(), 5);
    }

    #[tokio::test]
    async fn tick_character_runs_without_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config();
        let state = Arc::new(Mutex::new(AutonomyState {
            heartbeat: HeartbeatClock::with_config(&Default::default()),
            cache_keepalive: CacheKeepalive::new(),
            activity: ActivityTracker::new(),
            heartbeat_log: HeartbeatLog::new(),
            paused: false,
            dirty: false,
            last_compaction_activity: Instant::now(),
            compaction_triggered: false,
            active_turn_count: 0,
            compaction_pending: false,
            last_request: None,
            next_dream_attempt_at: None,
            dream_failure_count: 0,
        }));

        {
            let mut s = lock_state(&state);
            s.heartbeat.on_user_message(Instant::now());
            s.activity.record_message();
        }

        let tick_ctx = TickContext {
            state,
            config: Arc::new(config),
            compaction: Arc::new(Default::default()),
            data_dir: tmp.path().to_path_buf(),
            llm_client: None,
            push_tx: None,
            loaded_config: None,
            notifier: None,
            registry: None,
        };
        tick_character("alice", &tick_ctx).await;
    }

    #[tokio::test]
    async fn shutdown_recovers_from_poisoned_handles_mutex() {
        let tmp = tempfile::tempdir().unwrap();
        let (_tx, rx) = tokio::sync::watch::channel(());
        let mgr = AutonomyManager::new(
            test_config(),
            Default::default(),
            tmp.path().to_path_buf(),
            rx,
        );

        {
            let mut handles = mgr.handles.lock().unwrap();
            handles.push(tokio::spawn(async {}));
        }

        let result = catch_unwind(AssertUnwindSafe(|| {
            let _guard = mgr.handles.lock().unwrap();
            panic!("poison autonomy shutdown handles");
        }));
        assert!(result.is_err());

        mgr.shutdown().await;
        assert!(lock_or_recover("autonomy task handle list", &mgr.handles).is_empty());
    }

    #[test]
    fn extract_send_message_parses() {
        assert_eq!(
            extract_send_message("thinking...<sendMessage>Hey there!</sendMessage>...done"),
            Some("Hey there!".into())
        );
        assert_eq!(extract_send_message("no tags here"), None);
        assert_eq!(extract_send_message("<sendMessage></sendMessage>"), None);
    }

    // -- state resilience -----------------------------------------------------

    #[test]
    fn load_state_corrupt_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        let char_dir = data_dir.join("alice");
        std::fs::create_dir_all(&char_dir).unwrap();

        // Write garbage bytes.
        std::fs::write(state_path(data_dir, "alice"), b"not valid json {{{{").unwrap();

        let loaded = load_state(data_dir, "alice");
        assert!(loaded.is_none(), "Corrupt state file should return None");
    }

    #[test]
    fn load_state_future_version_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        let char_dir = data_dir.join("alice");
        std::fs::create_dir_all(&char_dir).unwrap();

        // Write valid JSON but with a future version number.
        let future = serde_json::json!({
            "version": 99,
            "ticks_without_user": 0,
        });
        std::fs::write(state_path(data_dir, "alice"), future.to_string()).unwrap();

        let loaded = load_state(data_dir, "alice");
        assert!(
            loaded.is_none(),
            "Future version should return None (migration path)"
        );
    }

    #[test]
    fn restore_from_persisted_sets_clock_state() {
        let persisted = PersistedState {
            version: STATE_VERSION,
            ticks_without_user: 7,
            next_wake_at: None,
            last_user_at: None,
        };
        let mut clock = HeartbeatClock::with_config(&Default::default());
        restore_from_persisted(&persisted, &mut clock);
        assert_eq!(clock.ticks_without_user(), 7);
    }

    // -- extract_tag / sendMessage tests -------------------------------------

    #[test]
    fn extract_send_message_last_wins() {
        let content = "<sendMessage>first</sendMessage> stuff <sendMessage>second</sendMessage>";
        assert_eq!(extract_send_message(content), Some("second".into()));
    }

    #[test]
    fn extract_tag_handles_nested_text() {
        let content = "<sendMessage>Hey <b>bold</b> text</sendMessage>";
        assert_eq!(
            extract_send_message(content),
            Some("Hey <b>bold</b> text".into())
        );
    }

    // -- keepalive integration tests ------------------------------------------
    // These test the seam between tick_character, execute_dormant_ping, and
    // on_cache_warmed — the exact boundary where the phantom ping bug lived.

    /// Helper: build a TickContext with no LLM client (pings always fail).
    fn tick_ctx_no_llm(state: Arc<Mutex<AutonomyState>>, data_dir: &Path) -> TickContext {
        TickContext {
            state,
            config: Arc::new(test_config()),
            compaction: Arc::new(Default::default()),
            data_dir: data_dir.to_path_buf(),
            llm_client: None,
            push_tx: None,
            loaded_config: None,
            notifier: None,
            registry: None,
        }
    }

    #[tokio::test]
    async fn failed_ping_does_not_advance_timer() {
        // The phantom ping bug: execute_dormant_ping returns early (no
        // LLM client / no last_request), but on_cache_warmed was called
        // unconditionally, resetting the timer for another keepalive interval.
        // After the fix, the timer must stay on a short retry path instead
        // of being reset for another 55 minutes.
        let tmp = tempfile::tempdir().unwrap();
        let now = Instant::now();

        let mut ka = CacheKeepalive::new();
        // Simulate: cache was warmed 59+ minutes ago, wake is set.
        ka.on_cache_warmed(now - Duration::from_secs(60 * 60));
        ka.set_next_wake(Some(now + Duration::from_secs(3600)));

        // Precondition: keepalive is due right now.
        assert_eq!(ka.tick(now), CacheKeepaliveAction::Ping);
        // Reset — tick() didn't advance, so re-prime for the actual test.
        ka.on_cache_warmed(now - Duration::from_secs(60 * 60));

        let state = Arc::new(Mutex::new(AutonomyState {
            heartbeat: HeartbeatClock::with_config(&Default::default()),
            cache_keepalive: ka,
            activity: ActivityTracker::new(),
            heartbeat_log: HeartbeatLog::new(),
            paused: false,
            dirty: false,
            last_compaction_activity: now,
            compaction_triggered: false,
            active_turn_count: 0,
            compaction_pending: false,
            last_request: None, // <-- no request → ping will be skipped
            next_dream_attempt_at: None,
            dream_failure_count: 0,
        }));

        let ctx = tick_ctx_no_llm(state.clone(), tmp.path());
        tick_character("test", &ctx).await;

        // After the tick: the keepalive should not fire immediately, but
        // should retry shortly rather than waiting a full keepalive interval.
        let mut s = lock_state(&state);
        let immediate = s.cache_keepalive.tick(Instant::now());
        assert_eq!(immediate, CacheKeepaliveAction::None);
        let action = s
            .cache_keepalive
            .tick(Instant::now() + Duration::from_secs(31));
        assert_eq!(
            action,
            CacheKeepaliveAction::Ping,
            "Failed ping must retry after short backoff"
        );
    }

    #[tokio::test]
    async fn successful_ping_advances_timer() {
        // Counterpart: after on_cache_warmed is called (simulating a
        // successful ping), the next tick should NOT return Ping until
        // 55 minutes later.
        let now = Instant::now();
        let mut ka = CacheKeepalive::new();
        ka.on_cache_warmed(now - Duration::from_secs(60 * 60));
        ka.set_next_wake(Some(now + Duration::from_secs(3600)));

        // Ping is due.
        assert_eq!(ka.tick(now), CacheKeepaliveAction::Ping);
        // Caller confirms success.
        ka.on_cache_warmed(now);

        // Immediately after: should NOT be due (55 min away).
        assert_eq!(
            ka.tick(now + Duration::from_secs(30)),
            CacheKeepaliveAction::None
        );
        // 55 minutes later: should fire again.
        assert_eq!(
            ka.tick(now + Duration::from_secs(55 * 60)),
            CacheKeepaliveAction::Ping
        );
    }

    #[tokio::test]
    async fn compaction_keeps_keepalive_deadline() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = test_manager(tmp.path());
        mgr.ensure_state("alice", None);

        let now = Instant::now();
        mgr.with_state("alice", |s| {
            s.cache_keepalive
                .on_cache_warmed(now - Duration::from_secs(60 * 60));
            s.cache_keepalive
                .set_next_wake(Some(now + Duration::from_secs(3600)));
            s.last_request = Some(empty_request());
        });

        mgr.notify_compaction_complete("alice", 2);

        let (action, request_cleared) = mgr
            .with_state("alice", |s| {
                (s.cache_keepalive.tick(now), s.last_request.is_none())
            })
            .unwrap();
        assert_eq!(action, CacheKeepaliveAction::Ping);
        assert!(request_cleared);

        mgr.shutdown().await;
    }

    #[test]
    fn wrap_up_nudge_preserves_existing_message_prefix() {
        let mut request = empty_request();
        request.system = Some(json!([{"type": "text", "text": "stable system"}]));
        request.tools = Some(vec![json!({"name": "read", "input_schema": {}})]);
        request.messages = vec![
            json!({"role": "user", "content": "cached user"}),
            json!({"role": "assistant", "content": "cached assistant"}),
        ];
        let original_messages = request.messages.clone();
        let original_system = request.system.clone();
        let original_tools = request.tools.clone();

        append_wrap_up_nudge(&mut request);

        assert_eq!(
            &request.messages[..original_messages.len()],
            original_messages.as_slice(),
            "wrap-up nudge must append after the cached prefix"
        );
        assert_eq!(request.messages.len(), original_messages.len() + 1);
        assert_eq!(
            request.system, original_system,
            "wrap-up nudge must not mutate system prefix"
        );
        assert_eq!(
            request.tools, original_tools,
            "wrap-up nudge must not mutate tools"
        );
    }

    #[test]
    fn startup_with_restored_wake_primes_keepalive() {
        // After daemon restart, if the heartbeat clock had a next_wake
        // restored from persistence, the keepalive timer must be primed
        // so pings start immediately — not wait for the first user message.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        std::fs::create_dir_all(data_dir.join("alice")).unwrap();

        // Save persisted state with a next_wake_at in the future.
        let wake_time = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
        let persisted = PersistedState {
            version: STATE_VERSION,
            ticks_without_user: 1,
            next_wake_at: Some(wake_time),
            last_user_at: Some(chrono::Utc::now().to_rfc3339()),
        };
        let json = serde_json::to_string_pretty(&persisted).unwrap();
        std::fs::write(state_path(data_dir, "alice"), json).unwrap();

        let mgr = rt.block_on(async { test_manager(data_dir) });
        rt.block_on(async {
            mgr.ensure_state("alice", None);
        });

        // The keepalive should be primed: after 55 minutes, tick should
        // return Ping (not None).
        let state = mgr.states.get("alice").unwrap();
        let mut s = lock_state(&state);
        let future = Instant::now() + Duration::from_secs(55 * 60);
        let action = s.cache_keepalive.tick(future);
        assert_eq!(
            action,
            CacheKeepaliveAction::Ping,
            "Keepalive must be primed on startup when next_wake is restored"
        );
    }

    // -- cache prefix stability -----------------------------------------------

    /// The heartbeat tick must NOT add tools (like `set_next_wake`) to the
    /// request's tools array.  The Anthropic cache prefix order is
    /// tools → system → messages.  Changing the tools array invalidates the
    /// ENTIRE cache prefix — system AND messages.  Every heartbeat tick
    /// with a different tools array pays full input price (20× expected).
    ///
    /// The fix is to keep `set_next_wake` in the normal tool list and
    /// intercept it during heartbeat execution.
    #[test]
    fn heartbeat_must_not_mutate_tools_array() {
        // Simulate what execute_heartbeat_tick does: clone last_request,
        // then check if tools are modified.
        let original_tools: Vec<serde_json::Value> = vec![
            json!({"name": "check_time", "input_schema": {}}),
            json!({"name": "search_history", "input_schema": {}}),
        ];

        let request = LlmRequest {
            sdk: shore_config::models::Sdk::Anthropic,
            model: "test".into(),
            api_key: "key".into(),
            api_key_name: None,
            base_url: None,
            messages: vec![json!({"role": "user", "content": "hello"})],
            system: Some(json!([{"type": "text", "text": "system prompt"}])),
            tools: Some(original_tools.clone()),
            max_tokens: 4096,
            temperature: None,
            top_p: None,
            provider_options: None,
            provider_key: None,
            rid: None,
            forensic_character: None,
            retain_long: false,
        };

        // set_next_wake is now in the base tool set (tools/basic.rs),
        // so execute_heartbeat_tick no longer pushes it at call time.
        // This test documents the invariant: the tools array must be
        // identical to the original conversation's tools to preserve
        // the cache prefix.
        assert_eq!(
            request.tools.as_ref().unwrap().len(),
            original_tools.len(),
            "Heartbeat must not add tools to the request. \
             Adding set_next_wake changes the tools prefix, which invalidates \
             the ENTIRE Anthropic cache (tools → system → messages). \
             Use an XML tag like <sendMessage> instead."
        );
    }

    #[test]
    fn rebuild_request_from_disk_strips_prior_thinking_when_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("data");
        let config_dir = tmp.path().join("config");
        let cache_dir = tmp.path().join("cache");
        let runtime_dir = tmp.path().join("runtime");
        let character_dir = data_dir.join("alice");
        std::fs::create_dir_all(&character_dir).unwrap();

        let mut store =
            crate::engine::messages::MessageStore::new(character_dir.join("active.jsonl"));
        store.append(test_message(Role::User)).unwrap();
        store
            .append(Message {
                msg_id: "assistant-with-thinking".into(),
                role: Role::Assistant,
                content: "answer".into(),
                images: vec![],
                content_blocks: vec![
                    ContentBlock::Thinking {
                        thinking: "private chain".into(),
                        signature: Some("sig".into()),
                    },
                    ContentBlock::Text {
                        text: "answer".into(),
                    },
                ],
                alt_index: None,
                alt_count: None,
                alternatives: vec![],
                provider_key: None,
                timestamp: chrono::Local::now().to_rfc3339(),
            })
            .unwrap();

        let api_key_env = "REBUILD_REQUEST_STRIP_THINKING_ANTHROPIC";
        std::env::set_var(api_key_env, "test-secret");
        let chat_toml = format!(
            r#"
[anthropic.sonnet]
model_id = "claude-sonnet-test"
api_key_env = "{api_key_env}"
"#
        );
        let chat: toml::Table = chat_toml.parse().unwrap();
        let catalog =
            shore_config::models::ModelCatalog::from_sections(Some(&chat), None, None, None)
                .unwrap();

        let mut app = shore_config::app::AppConfig::default();
        app.behavior.tool_use.enabled = false;
        app.memory.thinking.preserve_prior_turns = false;
        let config = shore_config::LoadedConfig::new_for_test(
            app,
            catalog,
            shore_config::ShoreDirs {
                config: config_dir,
                data: data_dir.clone(),
                runtime: runtime_dir,
                cache: cache_dir,
            },
        );

        let request = rebuild_request_from_disk("alice", &data_dir, &config).unwrap();
        let assistant = request
            .messages
            .iter()
            .find(|msg| msg.get("role").and_then(serde_json::Value::as_str) == Some("assistant"))
            .expect("rebuilt request should include assistant history");
        let blocks = assistant
            .get("content")
            .and_then(serde_json::Value::as_array)
            .expect("assistant content should be structured");

        assert!(
            blocks.iter().all(
                |block| block.get("type").and_then(serde_json::Value::as_str) != Some("thinking")
            ),
            "heartbeat rebuild must honor preserve_prior_turns=false"
        );
        assert!(
            blocks
                .iter()
                .any(|block| block.get("type").and_then(serde_json::Value::as_str) == Some("text")),
            "non-thinking assistant content must remain"
        );

        std::env::remove_var(api_key_env);
    }

    // -- heartbeat model override ------------------------------------------

    fn minimal_request(model_id: &str) -> LlmRequest {
        LlmRequest {
            sdk: shore_config::models::Sdk::Anthropic,
            model: model_id.into(),
            api_key: "chat-key".into(),
            api_key_name: None,
            base_url: None,
            messages: vec![json!({"role": "user", "content": "hi"})],
            system: Some(json!([{"type": "text", "text": "sys"}])),
            tools: Some(vec![json!({"name": "check_time", "input_schema": {}})]),
            max_tokens: 4096,
            temperature: None,
            top_p: None,
            provider_options: None,
            provider_key: None,
            rid: None,
            forensic_character: None,
            retain_long: false,
        }
    }

    fn loaded_config_with_two_chat_models(
        heartbeat: Option<&str>,
        chat_env: &str,
        heartbeat_env: &str,
    ) -> shore_config::LoadedConfig {
        let chat_toml = format!(
            r#"
[anthropic.sonnet]
model_id = "claude-sonnet-chat"
api_key_env = "{chat_env}"

[anthropic.slowthink]
model_id = "claude-opus-slowthink"
api_key_env = "{heartbeat_env}"
"#
        );
        let chat: toml::Table = chat_toml.parse().unwrap();
        let catalog =
            shore_config::models::ModelCatalog::from_sections(Some(&chat), None, None, None)
                .unwrap();

        let mut app = shore_config::app::AppConfig::default();
        app.defaults.background.heartbeat = heartbeat.map(str::to_string);

        let tmp = tempfile::tempdir().unwrap();
        shore_config::LoadedConfig::new_for_test(
            app,
            catalog,
            shore_config::ShoreDirs {
                config: tmp.path().to_path_buf(),
                data: tmp.path().to_path_buf(),
                runtime: tmp.path().to_path_buf(),
                cache: tmp.path().to_path_buf(),
            },
        )
    }

    /// Regression: `defaults.heartbeat` was silently ignored on the warm
    /// path because `execute_heartbeat_tick` reused the cached chat-turn
    /// request without rewriting the model.
    /// Regression: `defaults.heartbeat` was silently ignored on the warm
    /// path because `execute_heartbeat_tick` reused the cached chat-turn
    /// request without rewriting the model.
    #[test]
    fn heartbeat_override_swaps_model_when_set() {
        let chat_env = "HEARTBEAT_OVERRIDE_SWAP_CHAT";
        let int_env = "HEARTBEAT_OVERRIDE_SWAP_INT";
        std::env::set_var(chat_env, "chat-secret");
        std::env::set_var(int_env, "slowthink-secret");

        let config = loaded_config_with_two_chat_models(Some("slowthink"), chat_env, int_env);
        let mut request = minimal_request("claude-sonnet-chat");
        let original_messages = request.messages.clone();
        let original_system = request.system.clone();
        let original_tools = request.tools.clone();

        let applied = apply_heartbeat_model_override(&mut request, &config, "alice");

        assert!(applied, "override should have been applied");
        assert_eq!(request.model, "claude-opus-slowthink");
        assert_eq!(request.api_key, "slowthink-secret");
        assert_eq!(
            request.messages, original_messages,
            "heartbeat model override must preserve existing messages"
        );
        assert_eq!(
            request.system, original_system,
            "heartbeat model override must preserve system prefix"
        );
        assert_eq!(
            request.tools, original_tools,
            "heartbeat model override must preserve tool definitions"
        );

        std::env::remove_var(chat_env);
        std::env::remove_var(int_env);
    }

    #[test]
    fn heartbeat_override_is_noop_when_unset() {
        let config = loaded_config_with_two_chat_models(
            None,
            "HEARTBEAT_OVERRIDE_UNSET_CHAT",
            "HEARTBEAT_OVERRIDE_UNSET_INT",
        );
        let mut request = minimal_request("claude-sonnet-chat");

        let applied = apply_heartbeat_model_override(&mut request, &config, "alice");

        assert!(!applied);
        assert_eq!(request.model, "claude-sonnet-chat");
    }

    #[test]
    fn heartbeat_override_is_noop_when_model_matches() {
        let chat_env = "HEARTBEAT_OVERRIDE_MATCH_CHAT";
        let int_env = "HEARTBEAT_OVERRIDE_MATCH_INT";
        std::env::set_var(chat_env, "chat-secret");
        std::env::set_var(int_env, "slowthink-secret");

        let config = loaded_config_with_two_chat_models(Some("slowthink"), chat_env, int_env);
        let mut request = minimal_request("claude-opus-slowthink");

        let applied = apply_heartbeat_model_override(&mut request, &config, "alice");

        assert!(!applied);
        assert_eq!(request.model, "claude-opus-slowthink");

        std::env::remove_var(chat_env);
        std::env::remove_var(int_env);
    }

    /// Configuring `max_turns < min_turns` should disable compaction or
    /// be rejected, since the max_turns trigger can never fire when
    /// `active_turn_count >= max_turns && active_turn_count >= min_turns`
    /// requires both conditions simultaneously.
    #[test]
    fn compaction_disabled_when_max_turns_less_than_min_turns() {
        let config = AutonomyConfig::default();
        let compaction = CompactionConfig {
            enabled: true,
            min_turns: 12,
            max_turns: 8,
            keep_recent_turns: 3,
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let (_tx, rx) = tokio::sync::watch::channel(());
        let mgr = AutonomyManager::new(config, compaction, tmp.path().to_path_buf(), rx);

        // Compaction should be disabled because max_turns < min_turns
        // makes the max_turns trigger dead code.
        assert!(
            !mgr.compaction.enabled,
            "Compaction should be disabled when max_turns ({}) < min_turns ({})",
            8, 12,
        );
    }

    #[test]
    fn dream_inactivity_gate_respects_minimum_inactive_time() {
        let cfg = DreamingConfig {
            minimum_inactive_time: shore_config::ConfigDuration::from_secs(300),
            ..Default::default()
        };
        let now = Instant::now();

        // No config: never satisfied.
        assert!(!dream_inactivity_satisfied(None, Some(now), now));

        // No prior user message: no timer to enforce, allow.
        assert!(dream_inactivity_satisfied(Some(&cfg), None, now));

        // User active 2min ago, 5min window: not satisfied.
        let two_min_ago = now - Duration::from_secs(120);
        assert!(!dream_inactivity_satisfied(
            Some(&cfg),
            Some(two_min_ago),
            now
        ));

        // User active 6min ago: satisfied.
        let six_min_ago = now - Duration::from_secs(360);
        assert!(dream_inactivity_satisfied(
            Some(&cfg),
            Some(six_min_ago),
            now
        ));
    }

    // -- inline compaction tests -----------------------------------------------

    #[test]
    fn should_compact_now_fires_on_max_turns() {
        let compaction = CompactionConfig {
            enabled: true,
            min_turns: 8,
            max_turns: 16,
            keep_recent_turns: 2,
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let (_tx, rx) = tokio::sync::watch::channel(());
        let mgr =
            AutonomyManager::new(Default::default(), compaction, tmp.path().to_path_buf(), rx);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async { mgr.ensure_state("alice", None::<u64>) });

        // Below max_turns: should not compact.
        assert!(!mgr.should_compact_now("alice", 15, 0));
        // At max_turns: should compact.
        assert!(mgr.should_compact_now("alice", 16, 0));
        // After compaction, compaction_triggered is set so tick won't double-fire.
        let triggered = mgr.with_state("alice", |s| s.compaction_triggered).unwrap();
        assert!(
            triggered,
            "compaction_triggered should be set after should_compact_now"
        );
    }

    #[test]
    fn should_compact_now_respects_idle_pending_flag() {
        let compaction = CompactionConfig {
            enabled: true,
            min_turns: 8,
            max_turns: 34,
            keep_recent_turns: 2,
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let (_tx, rx) = tokio::sync::watch::channel(());
        let mgr =
            AutonomyManager::new(Default::default(), compaction, tmp.path().to_path_buf(), rx);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async { mgr.ensure_state("alice", None::<u64>) });

        // Below max_turns and no pending flag: should not compact.
        assert!(!mgr.should_compact_now("alice", 20, 0));

        // Simulate idle trigger setting the pending flag.
        mgr.with_state("alice", |s| {
            s.compaction_pending = true;
        });

        // Now should_compact_now should return true (and clear the flag).
        assert!(mgr.should_compact_now("alice", 20, 0));
        // Flag should be cleared.
        let pending = mgr.with_state("alice", |s| s.compaction_pending).unwrap();
        assert!(!pending, "compaction_pending should be cleared after take");
    }

    #[test]
    fn should_compact_now_disabled_when_config_disabled() {
        let compaction = CompactionConfig {
            enabled: false,
            min_turns: 8,
            max_turns: 16,
            keep_recent_turns: 2,
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let (_tx, rx) = tokio::sync::watch::channel(());
        let mgr =
            AutonomyManager::new(Default::default(), compaction, tmp.path().to_path_buf(), rx);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async { mgr.ensure_state("alice", None::<u64>) });

        // Even above max_turns, disabled config means no compaction.
        assert!(!mgr.should_compact_now("alice", 100, 0));
    }

    #[test]
    fn should_compact_now_fires_on_max_context_tokens() {
        // Disable the turn-count trigger (max_turns well above anything we
        // pass) so the token trigger is what decides.
        let compaction = CompactionConfig {
            enabled: true,
            min_turns: 8,
            max_turns: 10_000,
            max_context_tokens: 30_000,
            keep_recent_turns: 2,
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let (_tx, rx) = tokio::sync::watch::channel(());
        let mgr =
            AutonomyManager::new(Default::default(), compaction, tmp.path().to_path_buf(), rx);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async { mgr.ensure_state("alice", None::<u64>) });

        // Below threshold: no trigger.
        assert!(!mgr.should_compact_now("alice", 10, 29_999));
        // Below min_turns floor even though tokens exceed: no trigger.
        assert!(!mgr.should_compact_now("alice", 5, 50_000));
        // At threshold and above min_turns: trigger.
        assert!(mgr.should_compact_now("alice", 10, 30_000));
        // Flag was set.
        let triggered = mgr.with_state("alice", |s| s.compaction_triggered).unwrap();
        assert!(triggered);
    }

    #[test]
    fn should_compact_now_ignores_context_tokens_when_disabled() {
        // max_context_tokens = 0 disables the token trigger entirely.
        let compaction = CompactionConfig {
            enabled: true,
            min_turns: 8,
            max_turns: 10_000,
            max_context_tokens: 0,
            keep_recent_turns: 2,
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let (_tx, rx) = tokio::sync::watch::channel(());
        let mgr =
            AutonomyManager::new(Default::default(), compaction, tmp.path().to_path_buf(), rx);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async { mgr.ensure_state("alice", None::<u64>) });

        // Huge context, past min_turns — must not trigger when disabled.
        assert!(!mgr.should_compact_now("alice", 100, 1_000_000));
    }

    #[test]
    fn notify_compaction_complete_resets_flags() {
        let compaction = CompactionConfig {
            enabled: true,
            min_turns: 8,
            max_turns: 16,
            keep_recent_turns: 2,
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let (_tx, rx) = tokio::sync::watch::channel(());
        let mgr =
            AutonomyManager::new(Default::default(), compaction, tmp.path().to_path_buf(), rx);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async { mgr.ensure_state("alice", None::<u64>) });

        // Trigger compaction.
        assert!(mgr.should_compact_now("alice", 16, 0));

        // Simulate compaction complete.
        mgr.notify_compaction_complete("alice", 4);

        // Flags should be reset: compaction_triggered and compaction_pending cleared.
        let (triggered, pending, turn_count) = mgr
            .with_state("alice", |s| {
                (
                    s.compaction_triggered,
                    s.compaction_pending,
                    s.active_turn_count,
                )
            })
            .unwrap();
        assert!(
            !triggered,
            "compaction_triggered should be reset after completion"
        );
        assert!(
            !pending,
            "compaction_pending should be reset after completion"
        );
        assert_eq!(
            turn_count, 4,
            "active_turn_count should be updated to retained count"
        );

        // Should be able to trigger again.
        assert!(mgr.should_compact_now("alice", 16, 0));
    }

    #[tokio::test]
    async fn tick_sets_compaction_pending_on_idle_trigger() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = test_config();
        config.enabled = true;
        let compaction = CompactionConfig {
            enabled: true,
            min_turns: 4,
            max_turns: 20,
            keep_recent_turns: 2,
            idle_trigger: shore_config::ConfigDuration::from_secs(1),
            ..Default::default()
        };

        let state = Arc::new(Mutex::new(AutonomyState {
            heartbeat: HeartbeatClock::with_config(&Default::default()),
            cache_keepalive: CacheKeepalive::new(),
            activity: ActivityTracker::new(),
            heartbeat_log: HeartbeatLog::new(),
            paused: false,
            dirty: false,
            last_compaction_activity: Instant::now() - Duration::from_secs(10),
            compaction_triggered: false,
            active_turn_count: 8,
            compaction_pending: false,
            last_request: None,
            next_dream_attempt_at: None,
            dream_failure_count: 0,
        }));

        let tick_ctx = TickContext {
            state: state.clone(),
            config: Arc::new(config),
            compaction: Arc::new(compaction),
            data_dir: tmp.path().to_path_buf(),
            llm_client: None,
            push_tx: None,
            loaded_config: None,
            notifier: None,
            registry: None,
        };

        tick_character("alice", &tick_ctx).await;

        // After tick with idle_trigger=1s and 10s idle, the pending flag should be set.
        let s = lock_state(&state);
        assert!(
            s.compaction_pending,
            "compaction_pending should be set after idle trigger fires in tick"
        );
        assert!(
            s.compaction_triggered,
            "compaction_triggered should prevent double-fire"
        );
    }

    /// The cache-prefix invariant: a keepalive ping must be byte-identical
    /// to the cached request in every field that participates in Anthropic's
    /// prompt-cache prefix (tools, system, model, message prefix). Any
    /// divergence flips a 0.1× cache read into a 2.0× cache write — a
    /// 20× cost increase that defeats the entire keepalive subsystem.
    ///
    /// Regression guard: this exact bug was fixed in commit addada6 and
    /// silently re-introduced two months later in cea94c0.
    #[test]
    fn keepalive_ping_preserves_cache_prefix() {
        let mut original = empty_request();
        original.model = "claude-sonnet-4-6".into();
        original.system = Some(json!([{"type": "text", "text": "you are a character"}]));
        original.tools = Some(vec![
            json!({"name": "memory", "description": "x", "input_schema": {}}),
            json!({"name": "schedule", "description": "y", "input_schema": {}}),
        ]);
        original
            .messages
            .push(json!({"role": "user", "content": "hello"}));
        original
            .messages
            .push(json!({"role": "assistant", "content": "hi back"}));

        let ping = build_keepalive_ping(&original, "alice");

        assert_eq!(
            ping.tools, original.tools,
            "tools must be preserved — stripping them invalidates the cache prefix"
        );
        assert_eq!(
            ping.system, original.system,
            "system prompt must be preserved"
        );
        assert_eq!(ping.model, original.model, "model must be preserved");
        assert_eq!(
            &ping.messages[..original.messages.len()],
            &original.messages[..],
            "the original message sequence must be preserved as a prefix"
        );

        // Documented diffs:
        assert_eq!(ping.max_tokens, 1, "ping must request only 1 token");
        assert_eq!(
            ping.messages.len(),
            original.messages.len() + 1,
            "ping appends exactly one user turn"
        );
        let last = ping.messages.last().unwrap();
        assert_eq!(last["role"], "user", "ping must end with a user turn");
        assert_eq!(ping.rid, None, "ping must not reuse the cached request ID");
        assert_eq!(
            ping.forensic_character.as_deref(),
            Some("alice"),
            "ping must carry the character name for forensics"
        );
    }
}
