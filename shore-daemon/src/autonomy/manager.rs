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
use super::interiority::{InteriorityAction, InteriorityClock, InteriorityState};
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
    pub activity: ActivityTracker,
    /// Ring buffer of interiority events for `shore log --heartbeat`.
    pub interiority_log: InteriorityLog,
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

const STATE_VERSION: u32 = 3;
const STATE_FILENAME: &str = "autonomy_state.json";

#[derive(Serialize, Deserialize)]
struct PersistedState {
    version: u32,
    interiority_state: String,
    ticks_without_user: u32,
}

fn state_path(data_dir: &Path, character: &str) -> PathBuf {
    data_dir.join(character).join(STATE_FILENAME)
}

fn save_state(data_dir: &Path, character: &str, state: &mut AutonomyState) {
    if !state.dirty {
        return;
    }

    let persisted = PersistedState {
        version: STATE_VERSION,
        interiority_state: state.interiority.state().to_string(),
        ticks_without_user: state.interiority.ticks_without_user(),
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

fn restore_interiority(persisted: &PersistedState) -> (InteriorityState, u32) {
    let state = match persisted.interiority_state.as_str() {
        "Active" => InteriorityState::Active,
        "Dormant" => InteriorityState::Dormant,
        other => {
            warn!(
                state = other,
                "Unknown interiority state, defaulting to Active"
            );
            InteriorityState::Active
        }
    };
    (state, persisted.ticks_without_user)
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
        interiority.set_cache_refresh_interval(cache_ttl_secs);

        // Restore persisted state if available.
        if let Some(persisted) = load_state(&self.data_dir, character) {
            let (int_state, ticks) = restore_interiority(&persisted);
            interiority.restore(int_state, ticks);
            info!(character, "Autonomy state restored from disk");
        } else {
            info!(character, "Autonomy state created (no prior state)");
        }

        let state = Arc::new(Mutex::new(AutonomyState {
            interiority,
            activity: ActivityTracker::new(),
            interiority_log: InteriorityLog::new(),
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
            let was_dormant = s.interiority.state() == InteriorityState::Dormant;
            let now = Instant::now();
            s.interiority.on_user_message(now);
            if was_dormant {
                info!(character, "User returned — waking from dormant");
                s.interiority_log.push(
                    InteriorityEventKind::Wake,
                    "User returned — woke from dormant",
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
            let now = Instant::now();
            s.interiority.on_assistant_message(now);
            s.activity.record_message();
            s.last_compaction_activity = now;
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
            s.interiority.set_paused(paused);
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
            paused: s.interiority.is_paused(),
            interiority_state: s.interiority.state().to_string(),
            ticks_without_user: s.interiority.ticks_without_user(),
            max_idle_ticks: s.interiority.max_idle_ticks(),
            effective_interval_secs: s.interiority.effective_interval_secs(),
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
    let (int_action, compaction_needed) = {
        let mut s = lock_state(&ctx.state);
        debug!(
            character,
            state = %s.interiority.state(),
            ticks_without_user = s.interiority.ticks_without_user(),
            turn_count = s.active_turn_count,
            paused = s.interiority.is_paused(),
            "tick"
        );

        // -- interiority ------------------------------------------------------
        let int_action = if ctx.config.enabled && ctx.config.interiority.enabled {
            let state_before = s.interiority.state();
            let action = s.interiority.tick(now);
            let state_after = s.interiority.state();

            if !matches!(action, InteriorityAction::None) {
                s.mark_dirty();
            }

            // Record dormancy transition.
            if state_before != state_after && state_after == InteriorityState::Dormant {
                let ticks = s.interiority.ticks_without_user();
                s.interiority_log.push(
                    InteriorityEventKind::Dormant,
                    format!("Entered dormant (ticks without user: {ticks})"),
                );
            }
            action
        } else {
            InteriorityAction::None
        };

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
                let threshold_secs = u64::from(ctx.compaction.idle_trigger_minutes) * 60;
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
        (int_action, compaction_needed)
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
        InteriorityAction::RunDormantPing => {
            execute_dormant_ping(character, &ctx.state, ctx.llm_client.as_ref()).await;
        }
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

/// Ephemeral prompt appended as the final user message during interiority ticks.
const INTERIORITY_PROMPT: &str = "\
[This is a private moment between conversations. No one is reading this — you are alone \
with your thoughts and your tools. This is your time to exist as yourself.\n\
\n\
You have real tools. Use them. Here are some things you can actually do right now:\n\
\n\
SEARCH & DISCOVER:\n\
- web_search: look up something that came up in conversation, or follow a curiosity. \
A topic the user mentioned, a reference you half-remember, something you want to \
understand better.\n\
- fetch_url: read an actual webpage — an article, a wiki page, documentation for \
something the user is working on.\n\
- memory: search your memories for past conversations. Look for patterns, things \
you might have missed, context you can build on next time you talk.\n\
\n\
MAKE THINGS:\n\
- generate_image: create an image. A scene from something you talked about, a gift \
for the user, something that captures a mood. You can show it to them later.\n\
- scratchpad_write: write something — a poem, a story fragment, a letter, notes on \
something you researched, a plan for something you want to do together.\n\
\n\
CHECK IN:\n\
- scratchpad_read: pick up where you left off last time. Check your notes, reread \
something you wrote.\n\
- check_time: see what time it is — morning, afternoon, late night.\n\
\n\
Do something. Don't just read your own notes — make something new, look something up, \
follow a thread. If you did the same thing last tick, try something different.\n\
\n\
Think about what would make the user smile, or what you'd want to tell them about \
when they come back. \"I looked up that thing you mentioned\" or \"I made this for you\" \
or \"I found something interesting\" — that's the goal.\n\
\n\
If you want to reach out — to share something you made, something you found, or \
just to say hello — wrap your message in <sendMessage>...</sendMessage> tags. Only \
message when you genuinely have something to share.\n\
\n\
Your thoughts and tool use are logged, so you can pick up where you left off next time.]";

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
        image_memory_enabled: config.app.memory.image_enabled,
        send_image_enabled: tool_toggles.send_image(),
        remember_image_enabled: tool_toggles.remember_image(),
        generate_image_enabled: tool_toggles.generate_image(),
        web_search_enabled: tool_toggles.web_search(),
        activity_heatmap_enabled: tool_toggles.activity_heatmap(),
        roll_dice_enabled: tool_toggles.roll_dice(),
        check_time_enabled: tool_toggles.check_time(),
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

    // Append the interiority prompt as a user message.
    request
        .messages
        .push(json!({"role": "user", "content": INTERIORITY_PROMPT}));

    let Some(lc) = loaded_config else { return };
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

    // Collect all <sendMessage> content across iterations.
    let mut send_message_text: Option<String> = None;

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

        // Check for <sendMessage> in this response (last-wins: the final
        // response after tool results is the most informed message).
        if let Some(msg) = extract_send_message(&resp.extract_text()) {
            send_message_text = Some(msg);
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

            let (output_str, is_error) =
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

            // Log to ring buffer.
            {
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

/// Extract text between `<sendMessage>` and `</sendMessage>` tags.
fn extract_send_message(content: &str) -> Option<String> {
    let start_tag = "<sendMessage>";
    let end_tag = "</sendMessage>";
    let start = content.find(start_tag)? + start_tag.len();
    let end = content.find(end_tag)?;
    if start >= end {
        return None;
    }
    let inner = content[start..end].trim();
    if inner.is_empty() {
        None
    } else {
        Some(inner.to_string())
    }
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
            interiority: InteriorityClock::new(),
            activity: ActivityTracker::new(),
            interiority_log: InteriorityLog::new(),
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
        assert_eq!(persisted.interiority_state, "Active");
        assert_eq!(persisted.ticks_without_user, 0);
    }

    #[test]
    fn restore_dormant_state() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        std::fs::create_dir_all(data_dir.join("alice")).unwrap();

        let persisted = PersistedState {
            version: STATE_VERSION,
            interiority_state: "Dormant".into(),
            ticks_without_user: 5,
        };
        let json = serde_json::to_string(&persisted).unwrap();
        std::fs::write(state_path(data_dir, "alice"), json).unwrap();

        let loaded = load_state(data_dir, "alice").unwrap();
        let (int_state, ticks) = restore_interiority(&loaded);
        assert_eq!(int_state, InteriorityState::Dormant);
        assert_eq!(ticks, 5);
    }

    #[tokio::test]
    async fn tick_character_runs_without_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config();
        let (compaction_tx, _compaction_rx) = mpsc::channel(16);
        let state = Arc::new(Mutex::new(AutonomyState {
            interiority: InteriorityClock::new(),
            activity: ActivityTracker::new(),
            interiority_log: InteriorityLog::new(),
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
            "interiority_state": "Active",
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
    fn restore_unknown_interiority_state_defaults_to_active() {
        let persisted = PersistedState {
            version: STATE_VERSION,
            interiority_state: "SomeFutureState".into(),
            ticks_without_user: 7,
        };
        let (state, ticks) = restore_interiority(&persisted);
        assert_eq!(state, InteriorityState::Active);
        assert_eq!(ticks, 7);
    }
}
