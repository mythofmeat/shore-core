//! AutonomyManager — per-character scheduler state with background tick tasks.
//!
//! Each character gets its own tokio task that ticks the interiority clock on a
//! fixed interval. Interiority ticks double as cache refresh (unified timer).
//! State is persisted to `{data_dir}/{character}/autonomy_state.json`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dashmap::DashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;
use shore_protocol::server_msg::{NewMessage, ServerMessage};
use shore_protocol::types::{derive_content_from_blocks, ContentBlock, Message, Role};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use super::activity::ActivityTracker;
use super::cache_keepalive::{CacheKeepalive, CacheKeepaliveAction};
use super::interiority::{InteriorityAction, InteriorityClock};
use super::recap_store::{RecapEntry, RecapStore};
use super::{AutonomyStatus, InteriorityEventKind, InteriorityLog};
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

// ---------------------------------------------------------------------------
// Tick context — shared state for the per-character autonomy loop
// ---------------------------------------------------------------------------

/// Shared context passed to the per-character tick loop.
struct TickContext {
    state: Arc<Mutex<AutonomyState>>,
    config: Arc<AutonomyConfig>,
    compaction: Arc<CompactionConfig>,
    data_dir: PathBuf,
    compaction_tx: mpsc::Sender<String>,
    llm_client: Option<LedgerClient>,
    push_tx: Option<broadcast::Sender<ServerMessage>>,
    loaded_config: Option<Arc<LoadedConfig>>,
    notifier: Option<NotificationService>,
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
    /// Set after compaction completes — signals the handler to reload the engine.
    needs_engine_reload: bool,
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

/// Convert a `std::time::Instant` to an RFC3339 wall-clock string.
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
    let next_wake = persisted.next_wake_at.as_deref().and_then(rfc3339_to_instant);
    let last_user = persisted.last_user_at.as_deref().and_then(rfc3339_to_instant);
    interiority.restore(persisted.ticks_without_user, next_wake, last_user);
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
    /// Channel for sending compaction trigger signals (character name).
    compaction_tx: mpsc::Sender<String>,
    /// LLM client for interiority ticks and cache keepalive pings.
    llm_client: Option<LedgerClient>,
    /// Broadcast sender for pushing autonomous messages to SWP clients.
    push_tx: Option<broadcast::Sender<ServerMessage>>,
    /// Full config for model resolution in autonomous actions.
    loaded_config: Option<Arc<LoadedConfig>>,
    /// Push notification service for autonomous events.
    notifier: Option<NotificationService>,
}

impl AutonomyManager {
    pub fn new(
        config: AutonomyConfig,
        mut compaction: CompactionConfig,
        data_dir: PathBuf,
        shutdown_rx: tokio::sync::watch::Receiver<()>,
    ) -> (Self, mpsc::Receiver<String>) {
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
        }

        let (compaction_tx, compaction_rx) = mpsc::channel(16);
        let mgr = Self {
            states: Arc::new(DashMap::new()),
            handles: Arc::new(Mutex::new(Vec::new())),
            config: Arc::new(config),
            compaction: Arc::new(compaction),
            data_dir,
            shutdown_rx,
            compaction_tx,
            llm_client: None,
            push_tx: None,
            loaded_config: None,
            notifier: None,
        };
        (mgr, compaction_rx)
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
        // it to the keepalive so it can decide whether to bridge.
        if let Some(wake) = interiority.next_wake() {
            cache_keepalive.set_next_wake(Some(wake));
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
            needs_engine_reload: false,
            last_request: None,
        }));

        self.states.insert(character.to_string(), state.clone());

        // Spawn per-character tick task.
        let name = character.to_string();
        let config = autonomy_cfg;
        let compaction = self.compaction.clone();
        let data_dir = self.data_dir.clone();
        let shutdown_rx = self.shutdown_rx.clone();
        let compaction_tx = self.compaction_tx.clone();
        let llm_client = self.llm_client.clone();
        let push_tx = self.push_tx.clone();
        let loaded_config = effective_config
            .map(|c| Arc::new(c.clone()))
            .or_else(|| self.loaded_config.clone());
        let notifier = self.notifier.clone();

        let tick_ctx = TickContext {
            state,
            config,
            compaction,
            data_dir,
            compaction_tx,
            llm_client,
            push_tx,
            loaded_config,
            notifier,
        };
        let handle = tokio::spawn(async move {
            character_tick_loop(name, tick_ctx, shutdown_rx).await;
        });

        self.handles.lock().unwrap().push(handle);

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
    /// and signals the handler to reload the engine on the next message.
    pub fn notify_compaction_complete(&self, character: &str, new_turn_count: usize) {
        self.with_state(character, |s| {
            s.active_turn_count = new_turn_count;
            s.needs_engine_reload = true;
            // Invalidate the cached request — it contains the pre-compaction
            // conversation. The next interiority tick will rebuild from disk.
            s.last_request = None;
            // Keep compaction_triggered = true until engine reload acknowledges it.
            s.mark_dirty();
            info!(
                character = %character,
                new_turn_count,
                "Compaction complete — engine reload pending, last_request invalidated"
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

    /// Check if a character's engine needs reloading after compaction.
    /// Returns true (and clears the flag) if a reload is needed.
    pub fn take_needs_reload(&self, character: &str) -> bool {
        self.with_state(character, |s| {
            if s.needs_engine_reload {
                s.needs_engine_reload = false;
                // Compaction cycle complete — allow future compaction triggers.
                s.compaction_triggered = false;
                s.last_compaction_activity = Instant::now();
                info!(character, "Engine reload taken after compaction");
                return true;
            }
            false
        })
        .unwrap_or(false)
    }

    /// Explicitly set the paused state for a character. Returns the new state,
    /// or None if the character has no autonomy state.
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
            interiority_state: s.interiority.state().to_string(),
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
            let mut h = self.handles.lock().unwrap();
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
const TICK_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum wall-clock time for a single interiority tick (including all tool
/// rounds). If the tick exceeds this, the future is dropped and the tick loop
/// continues. Prevents a hung LLM call from killing keepalive permanently.
const INTERIORITY_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes

/// Lock the per-character autonomy state, recovering from mutex poisoning
/// instead of panicking. A poisoned mutex means a previous holder panicked,
/// but the state inside is still usable — letting the tick loop die would be
/// worse (no more keepalive, no more interiority, permanent silent failure).
fn lock_state(m: &Mutex<AutonomyState>) -> std::sync::MutexGuard<'_, AutonomyState> {
    m.lock().unwrap_or_else(|poisoned| {
        error!("Autonomy state mutex was poisoned, recovering");
        poisoned.into_inner()
    })
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
            state = %s.interiority.state(),
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

    if compaction_needed {
        if ctx.compaction_tx.try_send(character.to_string()).is_err() {
            warn!(character, "Compaction channel full, trigger dropped");
        }
    }

    // -- execute interiority action with timeout (async, outside lock) ----
    match int_action {
        InteriorityAction::None => {}
        InteriorityAction::RunTick => {
            {
                let mut s = lock_state(&ctx.state);
                s.interiority_log
                    .push(InteriorityEventKind::TickFired, "Interiority tick fired");
            }
            match tokio::time::timeout(
                INTERIORITY_TIMEOUT,
                execute_unified_tick(
                    character,
                    &ctx.state,
                    &ctx.data_dir,
                    ctx.llm_client.as_ref(),
                    ctx.push_tx.as_ref(),
                    ctx.loaded_config.as_deref(),
                    ctx.notifier.as_ref(),
                ),
            )
            .await
            {
                Ok(()) => {}
                Err(_) => {
                    error!(
                        character = %character,
                        timeout_secs = INTERIORITY_TIMEOUT.as_secs(),
                        "Interiority tick timed out, dropping to keep tick loop alive"
                    );
                    let mut s = lock_state(&ctx.state);
                    s.interiority_log.push(
                        InteriorityEventKind::Timeout,
                        format!("Tick timed out after {}s", INTERIORITY_TIMEOUT.as_secs()),
                    );
                    s.mark_dirty();
                }
            }
        }
    }

    // -- cache keepalive ping (async, outside lock) -------------------------
    if keepalive_action == CacheKeepaliveAction::Ping {
        execute_dormant_ping(character, &ctx.state, ctx.llm_client.as_ref()).await;
        // The ping itself warms the cache — update the keepalive deadline.
        let mut s = lock_state(&ctx.state);
        s.cache_keepalive.on_cache_warmed(Instant::now());
        s.interiority_log
            .push(InteriorityEventKind::DormantPing, "Cache keepalive ping");
    }

    // -- final persist (in case async actions dirtied state) ---------------
    {
        let mut s = lock_state(&ctx.state);
        save_state(&ctx.data_dir, character, &mut s);
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

/// Build the `{recent_thread_block}` from recap entries and the ring buffer.
///
/// Uses the most recent 1–3 recap entries since the last user message.
/// Falls back to ring-buffer tool-use summaries if no recaps exist.
fn build_recent_thread(
    log: &InteriorityLog,
    recap_store: &super::recap_store::RecapStore,
) -> String {
    // Try recap entries first (most recent 3).
    let recaps = recap_store.entries();
    if !recaps.is_empty() {
        let recent: Vec<_> = recaps.iter().rev().take(3).collect();
        let mut lines = vec!["Where you left off:".to_string()];
        for entry in recent.iter().rev() {
            lines.push(format!(" · {}", entry.recap));
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

    // Resolve model (same logic as handler: defaults.model → first_chat_model).
    let model_name = config.app.defaults.model.as_deref();
    let resolved = match model_name {
        Some(name) => config.models.find_model(name).ok()?,
        None => config.models.first_chat_model()?,
    };

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
        remember_image_enabled: tool_toggles.remember_image(),
        generate_image_enabled: tool_toggles.generate_image(),
        web_search_enabled: tool_toggles.web_search(),
        activity_heatmap_enabled: tool_toggles.activity_heatmap(),
        roll_dice_enabled: tool_toggles.roll_dice(),
        check_time_enabled: tool_toggles.check_time(),
    };

    let recap_path = char_dir.join("recaps.jsonl");
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
        recap_store_path: Some(&recap_path),
    });

    let (llm_messages, system) = crate::handler::build_llm_messages(&prompt_result, false);

    let tool_defs = if config.app.behavior.tool_use.enabled {
        let defs: Vec<serde_json::Value> = tool_system::available_tools(false, tool_toggles)
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters.clone(),
                })
            })
            .collect();
        Some(defs)
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

/// Execute a unified interiority tick: a real tool loop using non-streaming
/// generate() calls. Tool loop messages are ephemeral — only <sendMessage>
/// output persists to active.jsonl. All activity is logged to the ring buffer
/// for `shore log --heartbeat`.
async fn execute_unified_tick(
    character: &str,
    state: &Arc<Mutex<AutonomyState>>,
    data_dir: &Path,
    llm_client: Option<&LedgerClient>,
    push_tx: Option<&broadcast::Sender<ServerMessage>>,
    loaded_config: Option<&LoadedConfig>,
    notifier: Option<&NotificationService>,
) {
    let Some(client) = llm_client else { return };

    // Clone last_request under the lock, then release.
    let mut request = {
        let s = lock_state(state);
        match &s.last_request {
            Some(req) => req.clone(),
            None => {
                drop(s);
                let Some(config) = loaded_config else { return };
                match rebuild_request_from_disk(character, data_dir, config) {
                    Some(req) => req,
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

    let Some(lc) = loaded_config else { return };

    // Build the dynamic interiority prompt.
    let recap_path = data_dir.join(character).join("recaps.jsonl");
    let recap_store = RecapStore::load(&recap_path);
    let user_name = lc.app.defaults.resolve_display_name();
    let default_interval_secs = lc.app.behavior.autonomy.interiority.fallback_interiority_interval.as_secs();
    let default_interval_str = if default_interval_secs >= 3600 && default_interval_secs % 3600 == 0 {
        let h = default_interval_secs / 3600;
        if h == 1 { "1 hour".to_string() } else { format!("{h} hours") }
    } else {
        format!("{} minutes", default_interval_secs / 60)
    };
    let recent_thread = {
        let s = lock_state(state);
        build_recent_thread(&s.interiority_log, &recap_store)
    };
    let interiority_prompt = build_interiority_prompt(&recent_thread, &user_name, &default_interval_str);

    // Append the interiority prompt as a user message.
    request
        .messages
        .push(json!({"role": "user", "content": interiority_prompt}));

    // Inject the set_next_wake tool definition into the request.
    let set_next_wake_def = json!({
        "name": "set_next_wake",
        "description": "Schedule when you want to have your next private moment to think and use tools. Use this at the end of a tick to express your own sense of pacing.",
        "input_schema": {
            "type": "object",
            "properties": {
                "hours_from_now": {
                    "type": "number",
                    "description": "Hours until your next private moment (1.0 to 48.0; clamped if outside range)"
                },
                "reason": {
                    "type": "string",
                    "description": "A brief note to your future self about why you chose this timing"
                }
            },
            "required": ["hours_from_now", "reason"]
        }
    });
    if let Some(tools) = request.tools.as_mut() {
        tools.push(set_next_wake_def);
    } else {
        request.tools = Some(vec![set_next_wake_def]);
    }

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
    let active_path = data_dir.join(character).join("active.jsonl");
    let max_iterations = std::cmp::min(lc.app.behavior.tool_use.max_iterations, 6);

    info!(
        character,
        max_iterations,
        "Interiority: executing tool loop tick"
    );

    // Collect <sendMessage> and <recap> content across iterations (last-wins).
    let mut send_message_text: Option<String> = None;
    let mut recap_text: Option<String> = None;

    for iteration in 0..max_iterations {
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
                (format!("Scheduled next moment in {clamped:.1} hours."), false)
            } else {
                match tool_system::dispatch_tool(name, input.clone(), &tool_ctx).await {
                    Ok(value) => {
                        let s = if let Some(s) = value.as_str() {
                            s.to_string()
                        } else {
                            serde_json::to_string(&value).unwrap_or_default()
                        };
                        (s, false)
                    }
                    Err(e) => (e.to_string(), true),
                }
            };

            info!(
                character,
                iteration,
                tool = %name, is_error,
                output = %truncate_summary(&output_str, 200),
                "Interiority: tool result"
            );

            let mut result = json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": output_str,
            });
            if is_error {
                result["is_error"] = json!(true);
            }
            tool_results.push(result);

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
    }

    // -- Persist <recap> if present -----------------------------------------------
    if let Some(recap) = recap_text {
        info!(character, recap = %truncate_summary(&recap, 200), "Interiority: recap written");
        let entry = RecapEntry {
            timestamp: chrono::Local::now().fixed_offset(),
            tick_id: format!("tick_{}", uuid::Uuid::new_v4()),
            recap,
        };
        let mut store = RecapStore::load(&recap_path);
        if let Err(e) = store.append(entry) {
            warn!(character, error = %e, "Interiority: failed to persist recap");
        }
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

        if let Ok(line) = msg.serialize_for_storage() {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&active_path)
            {
                let _ = writeln!(f, "{line}");
            }
        }

        if let Some(tx) = push_tx {
            let _ = tx.send(ServerMessage::NewMessage(NewMessage {
                message: msg.clone(),
            }));
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
                .map(|vs| AgentSearchContext::new(vs, client.inner().clone(), embed_config))
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
        db,
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
        researcher_llm_val: researcher_model
            .as_ref()
            .map(|_| RealAgentLlm::new(client.clone(), character.to_string(), CallType::Researcher)),
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
async fn execute_dormant_ping(
    character: &str,
    state: &Arc<Mutex<AutonomyState>>,
    llm_client: Option<&LedgerClient>,
) {
    let Some(client) = llm_client else { return };

    let request = {
        let s = lock_state(state);
        match &s.last_request {
            Some(req) => {
                let mut ping = req.clone();
                ping.max_tokens = 1;
                ping
            }
            None => {
                debug!(character, "Dormant ping: no cached request, skipping");
                return;
            }
        }
    };

    match client.generate(&request, CallType::Keepalive, character, false).await {
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
        }
        Err(e) => {
            error!(character, error = %e, "Dormant ping failed");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> AutonomyConfig {
        AutonomyConfig::default()
    }

    fn test_manager(data_dir: &Path) -> AutonomyManager {
        let (_tx, rx) = tokio::sync::watch::channel(());
        let (mgr, _compaction_rx) = AutonomyManager::new(
            test_config(),
            Default::default(),
            data_dir.to_path_buf(),
            rx,
        );
        mgr
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

    // -- notify ---------------------------------------------------------------

    #[test]
    fn notify_without_state_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let (_tx, rx) = tokio::sync::watch::channel(());
        let (mgr, _compaction_rx) = AutonomyManager::new(
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
        let (mgr, _compaction_rx) = AutonomyManager::new(
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
            needs_engine_reload: false,
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
        let (compaction_tx, _compaction_rx) = mpsc::channel(16);
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
            needs_engine_reload: false,
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
            compaction_tx,
            llm_client: None,
            push_tx: None,
            loaded_config: None,
            notifier: None,
        };
        tick_character("alice", &tick_ctx).await;
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
}
