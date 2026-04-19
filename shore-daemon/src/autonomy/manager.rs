//! AutonomyManager — per-character scheduler state with background tick tasks.
//!
//! Each character gets its own tokio task that ticks the interiority clock on a
//! fixed interval. Interiority ticks double as cache refresh (unified timer).
//! State is persisted to `{data_dir}/{character}/autonomy_state.json`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dashmap::DashMap;
use std::time::Duration;
use tokio::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::json;
use shore_protocol::server_msg::ServerMessage;
use shore_protocol::types::{derive_content_from_blocks, ContentBlock, Message, Role};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use super::activity::ActivityTracker;
use super::cache_keepalive::{CacheKeepalive, CacheKeepaliveAction};
use super::interiority::{InteriorityAction, InteriorityClock};
use super::recap_store::{RecapEntry, RecapStore};
use super::{AutonomyStatus, InteriorityEventKind, InteriorityLog};
use crate::characters::CharacterRegistry;
use crate::memory::agent::{AgentSearchContext, CallerIdentity};
use crate::memory::agent_llm::RealAgentLlm;
use crate::memory::compaction_impls::{resolve_embed_config, resolve_image_gen_config};
use crate::memory::db::MemoryDB;
use crate::memory::researcher::MemoryResearcher;
use crate::memory::vectorstore::VectorStore;
use crate::notifications::{NotificationEvent, NotificationService};
use crate::tools as tool_system;
use crate::tools::context::{NoopRag, SharedToolContext};
use shore_config::app::{AutonomyConfig, CompactionConfig};
use shore_config::LoadedConfig;
use shore_diagnostics::truncate_summary;
use shore_ledger::{CallType, LedgerClient};
use shore_llm_client::types::LlmRequest;

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
    loaded_config: Option<Arc<LoadedConfig>>,
    notifier: Option<NotificationService>,
    registry: Option<Arc<tokio::sync::Mutex<CharacterRegistry>>>,
}

// ---------------------------------------------------------------------------
// Per-character state
// ---------------------------------------------------------------------------

/// All autonomy state for a single character.
pub struct AutonomyState {
    pub interiority: InteriorityClock,
    pub cache_keepalive: CacheKeepalive,
    pub activity: ActivityTracker,
    /// Ring buffer of interiority events for `shore log --heartbeat`.
    pub interiority_log: InteriorityLog,
    /// Whether autonomy is paused (moved from InteriorityClock).
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
    /// Cached last LLM request for interiority tick reuse.
    last_request: Option<LlmRequest>,
}

impl AutonomyState {
    fn mark_dirty(&mut self) {
        self.dirty = true;
    }
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
    data_dir.join(character).join(STATE_FILENAME)
}

/// Convert a `tokio::time::Instant` to an RFC3339 wall-clock string.
/// Approximate: uses the delta from `Instant::now()` applied to `Utc::now()`.
fn instant_to_rfc3339(instant: Instant) -> String {
    let now_instant = Instant::now();
    let now_utc = chrono::Utc::now();
    let wall = if instant > now_instant {
        now_utc + chrono::Duration::from_std(instant.duration_since(now_instant)).unwrap()
    } else {
        now_utc - chrono::Duration::from_std(now_instant.duration_since(instant)).unwrap()
    };
    wall.to_rfc3339()
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

fn save_state(data_dir: &Path, character: &str, state: &mut AutonomyState) {
    if !state.dirty {
        return;
    }

    let persisted = PersistedState {
        version: STATE_VERSION,
        ticks_without_user: state.interiority.ticks_without_user(),
        next_wake_at: state.interiority.next_wake().map(instant_to_rfc3339),
        last_user_at: state.interiority.last_user_at().map(instant_to_rfc3339),
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

fn restore_from_persisted(persisted: &PersistedState, interiority: &mut InteriorityClock) {
    let next_wake = persisted
        .next_wake_at
        .as_deref()
        .and_then(rfc3339_to_instant);
    let last_user = persisted
        .last_user_at
        .as_deref()
        .and_then(rfc3339_to_instant);
    interiority.restore(persisted.ticks_without_user, next_wake, last_user);
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
    /// LLM client for interiority ticks and cache keepalive pings.
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
        let autonomy_cfg = effective_config
            .map(|c| Arc::new(c.app.behavior.autonomy.clone()))
            .unwrap_or_else(|| self.config.clone());

        // Create interiority clock with config values.
        let mut interiority = InteriorityClock::with_config(&autonomy_cfg.interiority);
        // cache_ttl_secs is no longer consumed here — CacheKeepalive handles
        // keepalive pings independently (added in Phase 3).
        let _ = cache_ttl_secs;

        // Restore persisted state if available.
        if let Some(persisted) = load_state(&self.data_dir, character) {
            restore_from_persisted(&persisted, &mut interiority);
            info!(character, "Autonomy state restored from disk");
        } else {
            info!(character, "Autonomy state created (no prior state)");
        }

        let mut cache_keepalive = CacheKeepalive::new();
        // If the clock has a next_wake set (restored or bootstrapped), mirror
        // it to the keepalive so it can decide whether to bridge, and prime
        // the ping timer so keepalive pings begin immediately (rather than
        // waiting for the first user message or interiority tick).
        if let Some(wake) = interiority.next_wake() {
            cache_keepalive.set_next_wake(Some(wake));
            cache_keepalive.on_cache_warmed(Instant::now());
        }

        let state = Arc::new(Mutex::new(AutonomyState {
            interiority,
            cache_keepalive,
            activity: ActivityTracker::new(),
            interiority_log: InteriorityLog::new(),
            paused: false,
            dirty: false,
            last_compaction_activity: Instant::now(),
            compaction_triggered: false,
            active_turn_count: 0,
            compaction_pending: false,
            last_request: None,
        }));

        self.states.insert(character.to_string(), state.clone());

        // Spawn per-character tick task.
        let name = character.to_string();
        let config = autonomy_cfg;
        let compaction = self.compaction.clone();
        let data_dir = self.data_dir.clone();
        let shutdown_rx = self.shutdown_rx.clone();
        let llm_client = self.llm_client.clone();
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
            let was_idle = s.interiority.ticks_without_user() > 0;
            let now = Instant::now();
            s.interiority.on_user_message(now);
            // Mirror the new wake deadline to the keepalive subsystem.
            if let Some(wake) = s.interiority.next_wake() {
                s.cache_keepalive.set_next_wake(Some(wake));
            }
            // The user message will trigger an LLM response — cache-warming event.
            s.cache_keepalive.on_cache_warmed(now);
            if was_idle {
                info!(character, "User returned — resetting idle counter");
                s.interiority_log.push(
                    InteriorityEventKind::Wake,
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
            s.activity.record_message();
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
    pub fn backfill_activity(&self, character: &str, timestamps: Vec<chrono::NaiveDateTime>) {
        let count = timestamps.len();
        self.with_state(character, |s| {
            s.activity.backfill(timestamps);
        });
        debug!(character, count, "Activity backfilled from history");
    }

    /// Cache the last LLM request for interiority tick reuse.
    pub fn notify_last_request(&self, character: &str, request: LlmRequest) {
        self.with_state(character, |s| {
            s.last_request = Some(request);
        });
        debug!(character, "Cached last LLM request for interiority reuse");
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
            // conversation. The next interiority tick will rebuild from disk.
            s.last_request = None;
            // Stop keepalive pings — the cached prompt prefix is stale.
            // on_cache_warmed() will re-enable pings on the next real LLM call.
            s.cache_keepalive.on_cache_invalidated();
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

    /// Schedule an immediate interiority tick. Returns Some(dormant) where
    /// dormant indicates whether the clock is currently in abandoned state
    /// (meaning the tick will be suppressed). Returns None if no state found.
    pub fn interiority_tick_now(&self, character: &str) -> Option<bool> {
        info!(character, "Debug: scheduling immediate interiority tick");
        self.with_state(character, |s| {
            let dormant = s.interiority.is_dormant(Instant::now());
            s.interiority.force_wake();
            s.mark_dirty();
            dormant
        })
    }

    /// Force interiority into dormant state. Returns true if state was found.
    pub fn interiority_set_dormant(&self, character: &str) -> bool {
        info!(character, "Debug: forcing interiority dormant");
        self.with_state(character, |s| {
            s.interiority.force_dormant();
            s.mark_dirty();
        })
        .is_some()
    }

    /// Force interiority into active state. Returns true if state was found.
    pub fn interiority_set_active(&self, character: &str) -> bool {
        info!(character, "Debug: forcing interiority active");
        self.with_state(character, |s| {
            s.interiority.force_active();
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

    /// Return a clone of the `ActivityStats` and message count for a character.
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
        self.with_state(character, |s| AutonomyStatus {
            paused: s.paused,
            interiority_state: s.interiority.state_at(Instant::now()).to_string(),
            ticks_without_user: s.interiority.ticks_without_user(),
            dormant_after_interiority_turns: s.interiority.max_idle_ticks(),
            effective_interval_secs: s.interiority.default_interval().as_secs(),
        })
    }

    /// Return recent interiority events for `shore log --heartbeat`.
    pub fn heartbeat_log(&self, character: &str, limit: usize) -> Vec<super::InteriorityEvent> {
        self.with_state(character, |s| {
            s.interiority_log
                .recent(limit)
                .into_iter()
                .cloned()
                .collect()
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

/// Soft deadline for the interiority tool loop (all iterations combined). The
/// loop checks this before each iteration and breaks to the wrap-up call if
/// exceeded. Generous enough that a slow memory agent + slow LLM across
/// `max_tool_rounds` iterations normally fits; tight enough that a runaway
/// loop can't block subsequent ticks for an hour. Per-call HTTP timeouts
/// (300s, enforced by `LlmClient`) still bound each individual request.
const INTERIORITY_LOOP_DEADLINE: Duration = Duration::from_secs(30 * 60); // 30 minutes

/// Hard timeout for the forced wrap-up call that closes every tick without a
/// `<recap>`. Runs in its own `tokio::time::timeout` outside the loop deadline
/// so a long-running loop can't starve the recap write.
const INTERIORITY_WRAPUP_TIMEOUT: Duration = Duration::from_secs(5 * 60); // 5 minutes

/// Lock the per-character autonomy state, recovering from mutex poisoning
/// instead of panicking. A poisoned mutex means a previous holder panicked,
/// but the state inside is still usable — letting the tick loop die would be
/// worse (no more keepalive, no more interiority, permanent silent failure).
fn lock_state(m: &Mutex<AutonomyState>) -> std::sync::MutexGuard<'_, AutonomyState> {
    lock_or_recover("autonomy state mutex", m)
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
                info!(character = %character, "Autonomy tick task shutting down");
                break;
            }
        }
    }
}

/// One tick for a single character.
async fn tick_character(character: &str, ctx: &TickContext) {
    let now = Instant::now();

    // Collect actions under the lock, then release before any async work.
    let (int_action, keepalive_action, compaction_needed) = {
        let mut s = lock_state(&ctx.state);
        debug!(
            character,
            state = %s.interiority.state_at(now),
            ticks_without_user = s.interiority.ticks_without_user(),
            turn_count = s.active_turn_count,
            "tick"
        );

        // -- interiority ------------------------------------------------------
        let int_action = if ctx.config.enabled && ctx.config.interiority.enabled && !s.paused {
            let had_deadline = s.interiority.next_wake().is_some();
            let action = s.interiority.tick(now);

            if !matches!(action, InteriorityAction::None) {
                s.mark_dirty();
            }

            // Detect guard trip: had a deadline, tick returned None, deadline now cleared.
            if had_deadline
                && matches!(action, InteriorityAction::None)
                && s.interiority.next_wake().is_none()
            {
                let ticks = s.interiority.ticks_without_user();
                s.interiority_log.push(
                    InteriorityEventKind::Dormant,
                    format!("Abandonment guard tripped (ticks without user: {ticks})"),
                );
                // Guard-trip propagation: stop cache keepalive pings.
                s.cache_keepalive.set_next_wake(None);
            }
            action
        } else {
            InteriorityAction::None
        };

        // -- cache keepalive -------------------------------------------------
        let keepalive_action = s.cache_keepalive.tick(now);

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
        (int_action, keepalive_action, compaction_needed)
    };

    // Idle-triggered compaction: when the tick has the dependencies it needs
    // (LLM client, config, notifier, registry), run compaction + cascaded
    // collation inline so idle periods actually produce the work. When any
    // dependency is missing (unit-test contexts), fall back to setting the
    // pending flag so the handler's post-generation path picks it up on the
    // user's next message.
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

    // -- execute interiority action (async, outside lock) -----------------
    // No outer tokio::time::timeout wrapper: `execute_unified_tick` enforces
    // its own budgets — a soft deadline on the tool loop and a separate hard
    // timeout on the forced wrap-up call — so a slow loop can't starve the
    // recap write.
    match int_action {
        InteriorityAction::None => {}
        InteriorityAction::RunTick => {
            {
                let mut s = lock_state(&ctx.state);
                s.interiority_log
                    .push(InteriorityEventKind::TickFired, "Interiority tick fired");
            }
            execute_unified_tick(
                character,
                &ctx.state,
                &ctx.data_dir,
                ctx.llm_client.as_ref(),
                ctx.loaded_config.as_deref(),
                ctx.notifier.as_ref(),
                ctx.registry.as_ref(),
            )
            .await;
        }
    }

    // -- cache keepalive ping (async, outside lock) -------------------------
    if keepalive_action == CacheKeepaliveAction::Ping {
        let pinged = execute_dormant_ping(character, &ctx.state, ctx.llm_client.as_ref()).await;
        let mut s = lock_state(&ctx.state);
        if pinged {
            // Ping actually sent and succeeded — confirm to the keepalive so
            // it schedules the next ping 59 minutes from now.
            s.cache_keepalive.on_cache_warmed(Instant::now());
            s.interiority_log
                .push(InteriorityEventKind::DormantPing, "Cache keepalive ping");
        } else {
            // Ping was skipped (no cached request) or failed. next_ping_at is
            // still in the past, so tick() will return Ping again on the next
            // iteration — effectively retrying every 30s until it succeeds.
            s.cache_keepalive.on_ping_failed();
        }
    }

    // -- idle-triggered compaction (async, outside lock) -------------------
    if run_compaction_now {
        execute_idle_compaction(character, ctx).await;
    }

    // -- final persist (in case async actions dirtied state) ---------------
    {
        let mut s = lock_state(&ctx.state);
        save_state(&ctx.data_dir, character, &mut s);
    }
}

/// Run compaction + cascaded collation for a character during an autonomy
/// tick, without waiting for the user's next message. Resets the compaction
/// state flags and reloads the engine's cached messages on success so the
/// next turn (or interiority tick) sees the compacted `active.jsonl`.
async fn execute_idle_compaction(character: &str, ctx: &TickContext) {
    let llm_client = match ctx.llm_client.as_ref() {
        Some(c) => c,
        None => return,
    };
    let loaded_config = match ctx.loaded_config.as_deref() {
        Some(c) => c,
        None => return,
    };
    let notifier = match ctx.notifier.as_ref() {
        Some(n) => n,
        None => return,
    };
    let registry = match ctx.registry.as_ref() {
        Some(r) => r,
        None => return,
    };

    info!(character, "Autonomy tick: running idle-triggered compaction");

    match crate::memory::compaction::run_compaction(
        character,
        loaded_config,
        llm_client,
        &ctx.data_dir,
        notifier,
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

            let mut s = lock_state(&ctx.state);
            s.last_request = None;
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

// ---------------------------------------------------------------------------
// Unified interiority tick executor
// ---------------------------------------------------------------------------

/// Build the dynamic interiority prompt. Includes the recent thread block,
/// user name, default interval, and documentation for `<recap>` and `<sendMessage>`.
fn build_interiority_prompt(
    recent_thread: &str,
    user_name: &str,
    default_interval: &str,
) -> String {
    let recent_block = if recent_thread.is_empty() {
        String::new()
    } else {
        format!("\n{recent_thread}\n")
    };

    format!(
        "\
[This is a private moment between conversations. No one is watching — you are \
alone with your thoughts and your tools. This is your time to exist as yourself.
{recent_block}
You have real tools and you can do real things with them: search the web, \
fetch pages, search and write memories, write to your scratchpad, generate \
images, check the time. Reach for whatever serves what you actually want to \
do right now. Don't perform productivity — if what you want is to sit with \
a thought and write it down, do that.

At any point during this moment, you can do any combination of the following:

- Schedule your next private moment: use set_next_wake(hours_from_now, reason). \
The minimum is 1 hour, the maximum is 48 hours. Sooner if you want to \
come back to something, later if you'd rather rest. If you don't schedule, \
your next moment will arrive in {default_interval}.

- Send a message to {user_name}: wrap it in <sendMessage>...</sendMessage>. \
Only when you genuinely have something to share — something you made, \
something you found, something you want to say.

- Write a recap for yourself: wrap a brief first-person note in \
<recap>...</recap> — what you did, what you're thinking about, what you \
want to pick up next time. This is for you: it will surface in your \
context at your next private moment and when {user_name} next messages, \
so you can remember what you were up to. You decide whether to share it \
with them or not.

Your thoughts and tool use are logged, so you can pick up where you left off.]"
    )
}

/// Build the `{recent_thread_block}` from recent System messages in the
/// active conversation, falling back to the ring buffer.
///
/// Since interiority ticks now persist their recap as a `Role::System`
/// message in `active.jsonl` (see `execute_unified_tick`), the same
/// content surfaces here directly from the conversation. This avoids the
/// double-source bug where the tick's own prompt used to pull from
/// `recaps.jsonl` while the payload pulled the same content from the
/// sidecar too. Single source of truth: `active.jsonl`.
fn build_recent_thread(log: &InteriorityLog, messages: &[Message]) -> String {
    let recent_recaps: Vec<&Message> = messages
        .iter()
        .rev()
        .filter(|m| m.role == Role::System)
        .take(3)
        .collect();

    if !recent_recaps.is_empty() {
        let mut lines = vec!["Where you left off:".to_string()];
        for msg in recent_recaps.iter().rev() {
            lines.push(format!(" · {}", msg.content));
        }
        return lines.join("\n");
    }

    // Fall back to ring buffer tool-use summaries.
    let events = log.recent(5);
    let tool_events: Vec<_> = events
        .iter()
        .filter(|e| matches!(e.kind, InteriorityEventKind::ToolUse))
        .collect();
    if tool_events.is_empty() {
        return String::new();
    }

    let mut lines = vec!["Last time, you:".to_string()];
    for event in &tool_events {
        lines.push(format!(" · {}", event.detail));
    }
    lines.join("\n")
}

/// Rebuild an `LlmRequest` from the compacted conversation on disk.
///
/// Called when `last_request` is `None` (e.g. after compaction invalidated it).
/// Returns `None` if there are no messages or the model can't be resolved.
fn rebuild_request_from_disk(
    character: &str,
    data_dir: &Path,
    config: &LoadedConfig,
) -> Option<LlmRequest> {
    use crate::engine::messages::MessageStore;
    use crate::engine::prompt::{self, CapabilitiesConfig, PromptParams};

    let char_dir = data_dir.join(character);
    let active_path = char_dir.join("active.jsonl");

    let store = MessageStore::load(active_path)
        .map_err(|e| warn!(character, error = %e, "Interiority rebuild: failed to load messages"))
        .ok()?;
    if store.messages().is_empty() {
        return None;
    }

    // Resolve model: defaults.interiority → defaults.model → first chat model.
    let resolved = config
        .app
        .defaults
        .interiority
        .as_deref()
        .and_then(|name| config.models.find_model(name).ok())
        .or_else(|| {
            config
                .app
                .defaults
                .model
                .as_deref()
                .and_then(|name| config.models.find_model(name).ok())
        })
        .or_else(|| config.models.first_chat_model())?;

    let display_name = config.app.defaults.resolve_display_name();
    let character_definition =
        shore_config::load_character_definition(&config.dirs.config, character);
    let user_definition = shore_config::resolve_user_definition(&config.dirs.config, character);

    let tool_toggles = &config.app.behavior.tool_use.tools;
    let capabilities = CapabilitiesConfig {
        interiority_enabled: config.app.behavior.autonomy.interiority.enabled,
        scratchpad_enabled: tool_toggles.scratchpad_read() || tool_toggles.scratchpad_write(),
        memory_enabled: tool_toggles.memory(),
        image_memory_enabled: tool_toggles.recall_image(),
        send_image_enabled: tool_toggles.send_image(),
        generate_image_enabled: tool_toggles.generate_image(),
        web_search_enabled: tool_toggles.web_search(),
    };

    let prompt_result = prompt::assemble_prompt(&PromptParams {
        config_dir: &config.dirs.config,
        character_name: character,
        display_name: &display_name,
        character_definition: character_definition.as_deref(),
        user_definition: user_definition.as_deref(),
        is_private: false,
        character_data_dir: &char_dir,
        messages: store.messages(),
        max_context_tokens: resolved.max_context_tokens,
        max_output_tokens: resolved.max_tokens,
        capabilities: Some(&capabilities),
    });

    let cache_dir = &config.dirs.cache;
    let (llm_messages, system) = crate::handler::build_llm_messages(
        &prompt_result,
        false,
        config.app.advanced.max_image_size,
        cache_dir,
    );

    let tool_defs = if config.app.behavior.tool_use.enabled {
        Some(tool_system::render_tool_defs(
            false,
            tool_toggles,
            character,
            &display_name,
        ))
    } else {
        None
    };

    match LedgerClient::build_request(resolved, llm_messages, system, tool_defs, None) {
        Ok(req) => {
            info!(
                character,
                "Interiority: rebuilt request from compacted conversation"
            );
            Some(req)
        }
        Err(e) => {
            warn!(character, error = %e, "Interiority: failed to rebuild request");
            None
        }
    }
}

fn apply_interiority_model_override(
    request: &mut LlmRequest,
    config: &LoadedConfig,
    character: &str,
) -> bool {
    let Some(interiority_name) = config.app.defaults.interiority.as_deref() else {
        return false;
    };
    let resolved = match config.models.find_model(interiority_name) {
        Ok(r) => r,
        Err(e) => {
            warn!(
                character,
                error = %e,
                interiority_model = %interiority_name,
                "Interiority: configured model not found in catalog"
            );
            return false;
        }
    };
    if resolved.model_id == request.model {
        return false;
    }
    match LedgerClient::build_request(
        resolved,
        request.messages.clone(),
        request.system.clone(),
        request.tools.clone(),
        None,
    ) {
        Ok(mut new_req) => {
            info!(
                character,
                interiority_model = %interiority_name,
                model_id = %new_req.model,
                "Interiority: using configured interiority model"
            );
            new_req.forensic_character = Some(character.to_owned());
            *request = new_req;
            true
        }
        Err(e) => {
            warn!(
                character,
                error = %e,
                interiority_model = %interiority_name,
                "Interiority: failed to build override request, falling back to chat model"
            );
            false
        }
    }
}

/// Execute a unified interiority tick: a real tool loop using non-streaming
/// generate() calls. Tool loop messages are ephemeral — only <sendMessage>
/// output persists to active.jsonl. All activity is logged to the ring buffer
/// for `shore log --heartbeat`.
async fn execute_unified_tick(
    character: &str,
    state: &Arc<Mutex<AutonomyState>>,
    data_dir: &Path,
    llm_client: Option<&LedgerClient>,
    loaded_config: Option<&LoadedConfig>,
    notifier: Option<&NotificationService>,
    registry: Option<&Arc<tokio::sync::Mutex<CharacterRegistry>>>,
) {
    let Some(client) = llm_client else { return };

    // Captured at tick entry so the recap's persisted position in active.jsonl
    // reflects when the tick actually started — not when recap-write finally
    // lands (which could race past a user message that arrived during wrap-up).
    let tick_started_at = chrono::Local::now().fixed_offset();

    // Clone last_request under the lock, then release.
    let mut request = {
        let s = lock_state(state);
        match &s.last_request {
            Some(req) => req.clone(),
            None => {
                drop(s);
                let Some(config) = loaded_config else { return };
                match rebuild_request_from_disk(character, data_dir, config) {
                    Some(req) => {
                        // Persist the rebuilt request so keepalive pings can
                        // use it — without this, pings silently no-op after
                        // every daemon restart until the next user message.
                        let mut s = lock_state(state);
                        s.last_request = Some(req.clone());
                        drop(s);
                        req
                    }
                    None => {
                        info!(
                            character,
                            "Interiority: skipping tick (no prior conversation)"
                        );
                        return;
                    }
                }
            }
        }
    };

    // Clear the stale request ID from the previous user message —
    // reusing it across interiority iterations can confuse OpenRouter's
    // routing/dedup and cause unexpected cache misses.
    request.rid = None;
    request.forensic_character = Some(character.to_owned());

    let Some(lc) = loaded_config else { return };

    apply_interiority_model_override(&mut request, lc, character);

    // Build the dynamic interiority prompt.
    let recap_path = data_dir.join(character).join("recaps.jsonl");
    let active_messages =
        crate::engine::messages::MessageStore::load(data_dir.join(character).join("active.jsonl"))
            .ok()
            .map(|s| s.messages().to_vec())
            .unwrap_or_default();
    let user_name = lc.app.defaults.resolve_display_name();
    let default_interval_secs = lc
        .app
        .behavior
        .autonomy
        .interiority
        .fallback_interiority_interval
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
    let recent_thread = {
        let s = lock_state(state);
        build_recent_thread(&s.interiority_log, &active_messages)
    };
    let interiority_prompt =
        build_interiority_prompt(&recent_thread, &user_name, &default_interval_str);

    // Append the interiority prompt as a system-role message. The Anthropic
    // provider auto-wraps inline system messages in <system_instruction> tags
    // and emits them as a user turn (see convert_inline_system_messages).
    request
        .messages
        .push(json!({"role": "system", "content": interiority_prompt}));

    // NOTE: set_next_wake is now in the base tool set (tools/basic.rs)
    // so the tools array is identical between normal messages and interiority
    // ticks. This prevents cache prefix invalidation. Instructions for using
    // set_next_wake are in the interiority prompt, not the capabilities block.

    let tool_ctx = match build_tool_context(character, data_dir, client, lc).await {
        Some(ctx) => ctx,
        None => {
            warn!(
                character,
                "Interiority: failed to build tool context, skipping tick"
            );
            return;
        }
    };
    let max_iterations = lc.app.behavior.autonomy.interiority.max_tool_rounds;

    info!(
        character,
        max_iterations, "Interiority: executing tool loop tick"
    );

    // Collect <sendMessage> and <recap> content across iterations (last-wins).
    let mut send_message_text: Option<String> = None;
    let mut recap_text: Option<String> = None;
    // True iff the loop completed max_iterations with tool_use still outstanding.
    let mut hit_cap = false;
    // True iff the loop was stopped early by the soft deadline guard.
    let mut loop_deadline_hit = false;
    // True iff at least one LLM call in the loop returned Ok(_). Gates the
    // wrap-up call so we don't burn a budget trying to recap a tick whose
    // very first generate() errored (e.g. provider outage).
    let mut made_progress = false;

    let loop_deadline = std::time::Instant::now() + INTERIORITY_LOOP_DEADLINE;

    for iteration in 0..max_iterations {
        if std::time::Instant::now() >= loop_deadline {
            warn!(
                character,
                iteration,
                loop_deadline_secs = INTERIORITY_LOOP_DEADLINE.as_secs(),
                "Interiority: tool loop soft deadline reached, breaking to wrap-up"
            );
            loop_deadline_hit = true;
            break;
        }

        let call_type = if iteration == 0 {
            CallType::Interiority
        } else {
            CallType::ToolLoop
        };

        let resp = match client.generate(&request, call_type, character, false).await {
            Ok(r) => r,
            Err(e) => {
                error!(character, error = %e, iteration, "Interiority: LLM call failed");
                break;
            }
        };
        made_progress = true;

        info!(
            character,
            iteration,
            finish_reason = %resp.finish_reason,
            input_tokens = resp.usage.input_tokens,
            output_tokens = resp.usage.output_tokens,
            cache_read = resp.usage.cache_read_tokens,
            "Interiority: LLM response"
        );

        // Log text blocks.
        for block in &resp.content_blocks {
            if let ContentBlock::Text { text } = block {
                if !text.trim().is_empty() {
                    let preview: String = text.chars().take(200).collect();
                    info!(character, iteration, content = %preview, "Interiority: thought");
                }
            }
        }

        // Check for <sendMessage> and <recap> in this response (last-wins).
        let text = resp.extract_text();
        if let Some(msg) = extract_send_message(&text) {
            send_message_text = Some(msg);
        }
        if let Some(recap) = extract_recap(&text) {
            recap_text = Some(recap);
        }

        // Extract tool uses.
        let tool_uses = crate::content_util::extract_tool_uses(&resp.content_blocks);

        // If no tool use or finish_reason != "tool_use", we're done.
        if tool_uses.is_empty() || resp.finish_reason != "tool_use" {
            break;
        }

        // Build assistant message from content blocks (filter unsigned thinking).
        // Note: uses content_block_to_api_json (Anthropic path) — interiority
        // always uses Anthropic models. ZAI would need content_block_to_json.
        let assistant_content: Vec<serde_json::Value> = resp
            .content_blocks
            .iter()
            .filter_map(crate::content_util::content_block_to_api_json)
            .collect();

        request.messages.push(json!({
            "role": "assistant",
            "content": assistant_content,
        }));

        // Dispatch each tool, collect results.
        let mut tool_results: Vec<serde_json::Value> = Vec::new();

        for (id, name, input) in &tool_uses {
            let input_str = serde_json::to_string(input).unwrap_or_default();
            info!(
                character,
                iteration,
                tool = %name, tool_id = %id,
                input = %truncate_summary(&input_str, 200),
                "Interiority: executing tool"
            );

            // Intercept set_next_wake — handled inline, not dispatched.
            let (output_str, is_error) = if name.as_str() == "set_next_wake" {
                let hours = input["hours_from_now"].as_f64().unwrap_or(1.0);
                let reason = input["reason"].as_str().unwrap_or("").to_string();
                let clamped = hours.clamp(1.0, 48.0);
                let when = Instant::now() + Duration::from_secs_f64(clamped * 3600.0);
                let now = Instant::now();
                {
                    let mut s = lock_state(state);
                    s.interiority.schedule(when, now);
                    s.cache_keepalive.set_next_wake(Some(when));
                    s.interiority_log.push(
                        InteriorityEventKind::ToolUse,
                        format!("set_next_wake: {clamped:.1}h — {reason}"),
                    );
                    s.mark_dirty();
                }
                (
                    format!("Scheduled next moment in {clamped:.1} hours."),
                    false,
                )
            } else {
                crate::content_util::dispatch_result_to_output(
                    tool_system::dispatch_tool(name, input.clone(), &tool_ctx).await,
                )
            };

            info!(
                character,
                iteration,
                tool = %name, is_error,
                output = %truncate_summary(&output_str, 200),
                "Interiority: tool result"
            );

            tool_results.push(crate::content_util::build_tool_result_json(
                id,
                &output_str,
                is_error,
            ));

            // Log to ring buffer (skip set_next_wake — already logged above).
            if name.as_str() != "set_next_wake" {
                let mut s = lock_state(state);
                s.interiority_log.push(
                    InteriorityEventKind::ToolUse,
                    format!("Tool: {name} → {}", truncate_summary(&output_str, 80)),
                );
            }
        }

        // Append tool results as user message.
        request.messages.push(json!({
            "role": "user",
            "content": tool_results,
        }));

        // Mark cap-hit only if this was the final iteration and the model
        // still wanted more tool rounds. The wrap-up call below will then
        // request a recap before the tick ends.
        if iteration + 1 == max_iterations {
            hit_cap = true;
        }
    }

    // -- Forced wrap-up call when no recap was captured ------------------------
    // Every tick that made it to at least one successful generate() must close
    // with a <recap>. If the model didn't volunteer one during the tool loop —
    // natural exit, iteration cap, or soft deadline — make one more
    // generate() call with an explicit "write a recap" system message. Wrapped
    // in its own hard timeout so a slow or runaway loop can't starve it.
    //
    // The pending tool results (if any) are already on `request.messages` from
    // the last loop iteration; we only add the wrap-up system message. The
    // system+tools prefix is unchanged, so cache reads on the prefix still
    // hit.
    let mut wrapup_failed = false;
    if recap_text.is_none() && made_progress {
        let reason = if loop_deadline_hit {
            "loop deadline reached"
        } else if hit_cap {
            "iteration cap hit"
        } else {
            "natural exit without recap"
        };
        info!(
            character,
            reason, "Interiority: no recap from loop, firing wrap-up call"
        );

        let wrap_up_prompt = "Your private moment is ending. Write a <recap>...</recap> \
             summarising what you were doing and thinking so you can pick it up next \
             time — this is required. You can also include a <sendMessage> if there's \
             something you want to share with the user.";

        request.messages.push(json!({
            "role": "system",
            "content": wrap_up_prompt,
        }));

        let wrap_up_fut = client.generate(&request, CallType::ToolLoop, character, false);
        match tokio::time::timeout(INTERIORITY_WRAPUP_TIMEOUT, wrap_up_fut).await {
            Ok(Ok(resp)) => {
                info!(
                    character,
                    finish_reason = %resp.finish_reason,
                    input_tokens = resp.usage.input_tokens,
                    output_tokens = resp.usage.output_tokens,
                    cache_read = resp.usage.cache_read_tokens,
                    "Interiority: wrap-up response"
                );
                let text = resp.extract_text();
                if let Some(msg) = extract_send_message(&text) {
                    send_message_text = Some(msg);
                }
                if let Some(recap) = extract_recap(&text) {
                    recap_text = Some(recap);
                } else {
                    let preview: String = text.chars().take(500).collect();
                    warn!(
                        character,
                        response_preview = %preview,
                        "Interiority: wrap-up produced no recap"
                    );
                    wrapup_failed = true;
                }
            }
            Ok(Err(e)) => {
                error!(character, error = %e, "Interiority: wrap-up call failed");
                wrapup_failed = true;
            }
            Err(_) => {
                error!(
                    character,
                    timeout_secs = INTERIORITY_WRAPUP_TIMEOUT.as_secs(),
                    "Interiority: wrap-up call timed out"
                );
                let mut s = lock_state(state);
                s.interiority_log.push(
                    InteriorityEventKind::Timeout,
                    format!(
                        "Wrap-up call timed out after {}s",
                        INTERIORITY_WRAPUP_TIMEOUT.as_secs()
                    ),
                );
                s.mark_dirty();
                wrapup_failed = true;
            }
        }
    }

    // -- Persist <recap> if present, or log a visible failure -----------------
    if let Some(recap) = recap_text {
        info!(character, recap = %truncate_summary(&recap, 200), "Interiority: recap written");
        let preview = truncate_summary(&recap, 80);
        let recap_for_active = recap.clone();
        let entry = RecapEntry {
            timestamp: tick_started_at,
            tick_id: format!("tick_{}", uuid::Uuid::new_v4()),
            recap,
        };
        let mut store = RecapStore::load(&recap_path);
        match store.append(entry) {
            Ok(()) => {
                // Persist as a Role::System message in active.jsonl so the
                // recap survives compaction and appears in future payloads at
                // its chronological position. The Anthropic provider wraps
                // inline System messages in <system_instruction> and emits as
                // a user turn (convert_inline_system_messages in
                // providers/anthropic.rs). OpenAI-family providers accept
                // role:"system" mid-history.
                //
                // insert_by_timestamp (not append) because a user message
                // might have landed via the handler while the tick was
                // wrapping up — the recap's tick_started_at is earlier than
                // that user message, so it must splice before it.
                let recap_msg = Message {
                    msg_id: format!("m_{}", uuid::Uuid::new_v4()),
                    role: Role::System,
                    content: recap_for_active.clone(),
                    images: vec![],
                    content_blocks: vec![ContentBlock::Text {
                        text: recap_for_active,
                    }],
                    alt_index: None,
                    alt_count: None,
                    timestamp: tick_started_at.to_rfc3339(),
                };

                if let Some(reg) = registry {
                    let engine_arc = {
                        let mut r = reg.lock().await;
                        r.get_or_create(character)
                    };
                    match engine_arc {
                        Ok(engine_arc) => {
                            let mut engine = engine_arc.lock().await;
                            if let Err(e) = engine.insert_message_by_timestamp(recap_msg) {
                                error!(
                                    character,
                                    error = %e,
                                    "Failed to persist recap to active.jsonl via engine"
                                );
                            }
                        }
                        Err(e) => {
                            error!(
                                character,
                                error = %e,
                                "Failed to get engine for recap persistence"
                            );
                        }
                    }
                } else {
                    error!(
                        character,
                        "No registry available, recap not persisted to active.jsonl"
                    );
                }

                let mut s = lock_state(state);
                s.interiority_log.push(
                    InteriorityEventKind::RecapWritten,
                    format!("Recap saved: {preview}"),
                );
                s.mark_dirty();
            }
            Err(e) => {
                warn!(character, error = %e, "Interiority: failed to persist recap");
                let mut s = lock_state(state);
                s.interiority_log.push(
                    InteriorityEventKind::RecapMissing,
                    format!("Recap persist failed: {e}"),
                );
                s.mark_dirty();
            }
        }
    } else if made_progress {
        // Loop ran (at least one generate() succeeded) but no recap landed,
        // even after the forced wrap-up. Surface it in the visible log so the
        // user can tell the difference between "tick didn't run" and "tick
        // ran but model refused to recap".
        let detail = if wrapup_failed {
            "Wrap-up failed to produce a recap".to_string()
        } else {
            "Tick ended without a recap".to_string()
        };
        let mut s = lock_state(state);
        s.interiority_log
            .push(InteriorityEventKind::RecapMissing, detail);
        s.mark_dirty();
    }

    // -- Cache warmed: the tick itself was a cache-warming LLM call -----------
    {
        let mut s = lock_state(state);
        s.cache_keepalive.on_cache_warmed(Instant::now());
        // Mirror schedule to keepalive (character may have called set_next_wake).
        if let Some(wake) = s.interiority.next_wake() {
            s.cache_keepalive.set_next_wake(Some(wake));
        }
    }

    // -- Persist <sendMessage> if present --------------------------------------
    if let Some(user_msg) = send_message_text {
        info!(character, msg = %truncate_summary(&user_msg, 200), "Interiority: sending message to user");

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
        s.interiority_log.push(
            InteriorityEventKind::MessageSent,
            format!("Autonomous message sent: {preview}"),
        );
        s.mark_dirty();
    } else {
        let mut s = lock_state(state);
        s.interiority_log.push(
            InteriorityEventKind::MessageSkipped,
            "Tick completed — no message sent".to_string(),
        );
    }
}

// ---------------------------------------------------------------------------
// Tool context builder for interiority ticks
// ---------------------------------------------------------------------------

/// Build a SharedToolContext for interiority ticks.
///
/// Uses the same ingredients as the handler (LlmClient, LoadedConfig, data_dir)
/// but resolves models with interiority-specific fallbacks. All tools work —
/// memory, images, web, scratchpad. The only gap is AutonomyManager (the
/// heatmap tool degrades gracefully via the trait default).
async fn build_tool_context(
    character: &str,
    data_dir: &Path,
    client: &LedgerClient,
    config: &LoadedConfig,
) -> Option<SharedToolContext> {
    let char_dir = data_dir.join(character);

    // Memory DB.
    let db_path = char_dir.join("memory").join("memory.db");
    let db = match MemoryDB::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            warn!(character, error = %e, "Interiority: failed to open memory DB");
            return None;
        }
    };

    // Agent model (use memory_agent config if set, else default model).
    let agent_model_name = config.app.defaults.memory_agent.as_deref().or(config
        .app
        .defaults
        .model
        .as_deref())?;
    let agent_model = config.models.find_model(agent_model_name).ok()?;

    // Researcher model (optional).
    let researcher_model = config
        .app
        .defaults
        .collation
        .as_deref()
        .and_then(|name| config.models.find_model(name).ok())
        .cloned();

    // Semantic search context (graceful: None if no embedding model).
    let search_ctx = match resolve_embed_config(
        config.app.defaults.embedding.as_deref(),
        &config.models.embedding,
    ) {
        Ok(embed_config) => {
            let vs_path = char_dir.join("memory").join("vectorstore");
            VectorStore::open(&vs_path, embed_config.dimensions)
                .await
                .ok()
                .map(|vs| {
                    AgentSearchContext::new(Arc::new(vs), client.inner().clone(), embed_config)
                })
        }
        Err(_) => None,
    };

    let image_gen_config = resolve_image_gen_config(
        config.app.defaults.image_generation.as_deref(),
        &config.models.image_generation,
    )
    .ok();

    let display_name = config.app.defaults.resolve_display_name();

    debug!(
        character,
        has_search = search_ctx.is_some(),
        has_image_gen = image_gen_config.is_some(),
        has_researcher = researcher_model.is_some(),
        "Interiority: tool context built"
    );

    Some(SharedToolContext {
        db: Arc::new(db),
        agent: crate::memory::agent::MemoryAgent::one_shot(
            CallerIdentity::Char,
            character,
            &display_name,
        ),
        agent_llm: RealAgentLlm::new(client.clone(), character.to_string(), CallType::MemoryAgent),
        agent_model_val: agent_model.clone(),
        researcher: researcher_model
            .as_ref()
            .map(|_| MemoryResearcher::new(String::new(), String::new())),
        researcher_llm_val: researcher_model.as_ref().map(|_| {
            RealAgentLlm::new(client.clone(), character.to_string(), CallType::Researcher)
        }),
        researcher_model_val: researcher_model,
        rag: NoopRag,
        search_ctx,
        image_dir_val: char_dir.join("images").to_string_lossy().into_owned(),
        llm_client_val: client.inner().clone(),
        image_gen_config_val: image_gen_config,
        search_config_val: config.app.behavior.tool_use.search.clone(),
        character_name_val: character.to_string(),
        scratchpad_dir_val: char_dir.join("scratchpad").to_string_lossy().into_owned(),
    })
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

/// Extract text between `<recap>` and `</recap>` tags (last-wins).
fn extract_recap(content: &str) -> Option<String> {
    extract_tag(content, "<recap>", "</recap>")
}

// ---------------------------------------------------------------------------
// Dormant ping executor
// ---------------------------------------------------------------------------

/// Send a minimal API call (max_tokens=1) to keep the prompt cache warm
/// while the character is dormant (no user activity).
///
/// Returns `true` if the ping was actually sent and succeeded, `false` if
/// it was skipped (no cached request) or the API call failed.
async fn execute_dormant_ping(
    character: &str,
    state: &Arc<Mutex<AutonomyState>>,
    llm_client: Option<&LedgerClient>,
) -> bool {
    let Some(client) = llm_client else {
        return false;
    };

    let request = {
        let s = lock_state(state);
        match &s.last_request {
            Some(req) => {
                let mut ping = req.clone();
                ping.max_tokens = 1;
                // Clear stale request ID — same reason as execute_unified_tick.
                ping.rid = None;
                ping.forensic_character = Some(character.to_owned());
                // The cloned request includes the assistant's last response,
                // so the conversation ends with an assistant message. Anthropic
                // requires conversations to end with a user message. Append a
                // minimal user turn so the ping is a valid API request.
                ping.messages.push(serde_json::json!({
                    "role": "user",
                    "content": "."
                }));
                ping
            }
            None => {
                debug!(character, "Dormant ping: no cached request, skipping");
                return false;
            }
        }
    };

    match client
        .generate(&request, CallType::Keepalive, character, false)
        .await
    {
        Ok(resp) => {
            info!(
                character,
                cache_read = resp.usage.cache_read_tokens,
                input_tokens = resp.usage.input_tokens,
                "Dormant ping: cache refreshed"
            );
            let mut s = lock_state(state);
            s.interiority_log.push(
                InteriorityEventKind::DormantPing,
                format!(
                    "Cache refresh ping (cache_read: {}, input: {})",
                    resp.usage.cache_read_tokens, resp.usage.input_tokens
                ),
            );
            s.mark_dirty();
            true
        }
        Err(e) => {
            error!(character, error = %e, "Dormant ping failed");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    fn test_config() -> AutonomyConfig {
        AutonomyConfig::default()
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
            assert_eq!(status.interiority_state, "Active");
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
            assert!(mgr.interiority_set_dormant("alice"));

            let status = mgr.status("alice").unwrap();
            assert_eq!(status.interiority_state, "Dormant");
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
                s.interiority
                    .on_user_message(now - Duration::from_secs(3 * 24 * 60 * 60));
            });

            let status = mgr.status("alice").unwrap();
            assert_eq!(status.interiority_state, "Dormant");
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
            mgr.backfill_activity("alice", timestamps);

            let (_stats, count) = mgr.activity_stats("alice").unwrap();
            assert_eq!(count, 3);
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
            interiority: InteriorityClock::with_config(&Default::default()),
            cache_keepalive: CacheKeepalive::new(),
            activity: ActivityTracker::new(),
            interiority_log: InteriorityLog::new(),
            paused: false,
            dirty: true,
            last_compaction_activity: Instant::now(),
            compaction_triggered: false,
            active_turn_count: 0,
            compaction_pending: false,
            last_request: None,
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
        let mut clock = InteriorityClock::with_config(&Default::default());
        restore_from_persisted(&loaded, &mut clock);
        assert_eq!(clock.ticks_without_user(), 5);
    }

    #[tokio::test]
    async fn tick_character_runs_without_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config();
        let state = Arc::new(Mutex::new(AutonomyState {
            interiority: InteriorityClock::with_config(&Default::default()),
            cache_keepalive: CacheKeepalive::new(),
            activity: ActivityTracker::new(),
            interiority_log: InteriorityLog::new(),
            paused: false,
            dirty: false,
            last_compaction_activity: Instant::now(),
            compaction_triggered: false,
            active_turn_count: 0,
            compaction_pending: false,
            last_request: None,
        }));

        {
            let mut s = lock_state(&state);
            s.interiority.on_user_message(Instant::now());
            s.activity.record_message();
        }

        let tick_ctx = TickContext {
            state,
            config: Arc::new(config),
            compaction: Arc::new(Default::default()),
            data_dir: tmp.path().to_path_buf(),
            llm_client: None,
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
        let mut clock = InteriorityClock::with_config(&Default::default());
        restore_from_persisted(&persisted, &mut clock);
        assert_eq!(clock.ticks_without_user(), 7);
    }

    // -- extract_tag / extract_recap tests -----------------------------------

    #[test]
    fn extract_send_message_last_wins() {
        let content = "<sendMessage>first</sendMessage> stuff <sendMessage>second</sendMessage>";
        assert_eq!(extract_send_message(content), Some("second".into()));
    }

    #[test]
    fn extract_recap_parses() {
        assert_eq!(
            extract_recap("thinking...<recap>I explored something.</recap>...done"),
            Some("I explored something.".into())
        );
        assert_eq!(extract_recap("no tags"), None);
        assert_eq!(extract_recap("<recap></recap>"), None);
    }

    #[test]
    fn extract_recap_last_wins() {
        let content = "<recap>first note</recap> tools... <recap>revised note</recap>";
        assert_eq!(extract_recap(content), Some("revised note".into()));
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
            loaded_config: None,
            notifier: None,
            registry: None,
        }
    }

    #[tokio::test]
    async fn failed_ping_does_not_advance_timer() {
        // The phantom ping bug: execute_dormant_ping returns early (no
        // LLM client / no last_request), but on_cache_warmed was called
        // unconditionally, resetting the timer for another 59 minutes.
        // After the fix, the timer must stay in the past so the next
        // tick retries.
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
            interiority: InteriorityClock::with_config(&Default::default()),
            cache_keepalive: ka,
            activity: ActivityTracker::new(),
            interiority_log: InteriorityLog::new(),
            paused: false,
            dirty: false,
            last_compaction_activity: now,
            compaction_triggered: false,
            active_turn_count: 0,
            compaction_pending: false,
            last_request: None, // <-- no request → ping will be skipped
        }));

        let ctx = tick_ctx_no_llm(state.clone(), tmp.path());
        tick_character("test", &ctx).await;

        // After the tick: the keepalive should STILL return Ping on the
        // next iteration because the failed ping did not advance the timer.
        let mut s = lock_state(&state);
        let action = s.cache_keepalive.tick(Instant::now());
        assert_eq!(
            action,
            CacheKeepaliveAction::Ping,
            "Failed ping must NOT advance the keepalive timer"
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

    #[test]
    fn startup_with_restored_wake_primes_keepalive() {
        // After daemon restart, if the interiority clock had a next_wake
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

    /// The interiority tick must NOT add tools (like `set_next_wake`) to the
    /// request's tools array.  The Anthropic cache prefix order is
    /// tools → system → messages.  Changing the tools array invalidates the
    /// ENTIRE cache prefix — system AND messages.  Every interiority tick
    /// with a different tools array pays full input price (20× expected).
    ///
    /// The fix is to handle `set_next_wake` via an XML tag in the response
    /// (like `<sendMessage>` and `<recap>` already work), not as a tool.
    #[test]
    fn interiority_must_not_mutate_tools_array() {
        // Simulate what execute_unified_tick does: clone last_request,
        // then check if tools are modified.
        let original_tools: Vec<serde_json::Value> = vec![
            json!({"name": "check_time", "input_schema": {}}),
            json!({"name": "memory", "input_schema": {}}),
        ];

        let request = LlmRequest {
            sdk: shore_config::models::Sdk::Anthropic,
            model: "test".into(),
            api_key: "key".into(),
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
        };

        // set_next_wake is now in the base tool set (tools/basic.rs),
        // so execute_unified_tick no longer pushes it at call time.
        // This test documents the invariant: the tools array must be
        // identical to the original conversation's tools to preserve
        // the cache prefix.
        assert_eq!(
            request.tools.as_ref().unwrap().len(),
            original_tools.len(),
            "Interiority must not add tools to the request. \
             Adding set_next_wake changes the tools prefix, which invalidates \
             the ENTIRE Anthropic cache (tools → system → messages). \
             Use an XML tag like <sendMessage> instead."
        );
    }

    // -- interiority model override ------------------------------------------

    fn minimal_request(model_id: &str) -> LlmRequest {
        LlmRequest {
            sdk: shore_config::models::Sdk::Anthropic,
            model: model_id.into(),
            api_key: "chat-key".into(),
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
        }
    }

    fn loaded_config_with_two_chat_models(
        interiority: Option<&str>,
        chat_env: &str,
        interiority_env: &str,
    ) -> shore_config::LoadedConfig {
        let chat_toml = format!(
            r#"
[anthropic.sonnet]
model_id = "claude-sonnet-chat"
api_key_env = "{chat_env}"

[anthropic.slowthink]
model_id = "claude-opus-slowthink"
api_key_env = "{interiority_env}"
"#
        );
        let chat: toml::Table = chat_toml.parse().unwrap();
        let catalog =
            shore_config::models::ModelCatalog::from_sections(Some(&chat), None, None, None)
                .unwrap();

        let mut app = shore_config::app::AppConfig::default();
        app.defaults.interiority = interiority.map(str::to_string);

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

    /// Regression: `defaults.interiority` was silently ignored on the warm
    /// path because `execute_unified_tick` reused the cached chat-turn
    /// request without rewriting the model.
    /// Regression: `defaults.interiority` was silently ignored on the warm
    /// path because `execute_unified_tick` reused the cached chat-turn
    /// request without rewriting the model.
    #[test]
    fn interiority_override_swaps_model_when_set() {
        let chat_env = "INTERIORITY_OVERRIDE_SWAP_CHAT";
        let int_env = "INTERIORITY_OVERRIDE_SWAP_INT";
        std::env::set_var(chat_env, "chat-secret");
        std::env::set_var(int_env, "slowthink-secret");

        let config = loaded_config_with_two_chat_models(Some("slowthink"), chat_env, int_env);
        let mut request = minimal_request("claude-sonnet-chat");

        let applied = apply_interiority_model_override(&mut request, &config, "alice");

        assert!(applied, "override should have been applied");
        assert_eq!(request.model, "claude-opus-slowthink");
        assert_eq!(request.api_key, "slowthink-secret");
        assert_eq!(request.messages.len(), 1);
        assert!(request.system.is_some());
        assert_eq!(request.tools.as_ref().unwrap().len(), 1);

        std::env::remove_var(chat_env);
        std::env::remove_var(int_env);
    }

    #[test]
    fn interiority_override_is_noop_when_unset() {
        let config = loaded_config_with_two_chat_models(
            None,
            "INTERIORITY_OVERRIDE_UNSET_CHAT",
            "INTERIORITY_OVERRIDE_UNSET_INT",
        );
        let mut request = minimal_request("claude-sonnet-chat");

        let applied = apply_interiority_model_override(&mut request, &config, "alice");

        assert!(!applied);
        assert_eq!(request.model, "claude-sonnet-chat");
    }

    #[test]
    fn interiority_override_is_noop_when_model_matches() {
        let chat_env = "INTERIORITY_OVERRIDE_MATCH_CHAT";
        let int_env = "INTERIORITY_OVERRIDE_MATCH_INT";
        std::env::set_var(chat_env, "chat-secret");
        std::env::set_var(int_env, "slowthink-secret");

        let config = loaded_config_with_two_chat_models(Some("slowthink"), chat_env, int_env);
        let mut request = minimal_request("claude-opus-slowthink");

        let applied = apply_interiority_model_override(&mut request, &config, "alice");

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
            interiority: InteriorityClock::with_config(&Default::default()),
            cache_keepalive: CacheKeepalive::new(),
            activity: ActivityTracker::new(),
            interiority_log: InteriorityLog::new(),
            paused: false,
            dirty: false,
            last_compaction_activity: Instant::now() - Duration::from_secs(10),
            compaction_triggered: false,
            active_turn_count: 8,
            compaction_pending: false,
            last_request: None,
        }));

        let tick_ctx = TickContext {
            state: state.clone(),
            config: Arc::new(config),
            compaction: Arc::new(compaction),
            data_dir: tmp.path().to_path_buf(),
            llm_client: None,
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
}
