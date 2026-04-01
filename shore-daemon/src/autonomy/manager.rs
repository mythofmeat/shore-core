//! AutonomyManager — per-character scheduler state with background tick tasks.
//!
//! Each character gets its own tokio task that ticks the interiority clock and
//! cache keepalive scheduler on a fixed interval. State is persisted to
//! `{data_dir}/{character}/autonomy_state.json` and restored on startup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;
use shore_protocol::server_msg::{CacheWarning, NewMessage, ServerMessage};
use shore_protocol::types::{derive_content_from_blocks, ContentBlock, Message, Role};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use super::activity::ActivityTracker;
use super::cache_keepalive::{
    CacheKeepaliveConfig, CacheKeepaliveScheduler, KeepaliveAction, KeepaliveState,
};
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
use shore_llm_client::types::LlmRequest;
use shore_llm_client::LlmClient;

// ---------------------------------------------------------------------------
// Per-character state
// ---------------------------------------------------------------------------

/// All autonomy state for a single character.
pub struct AutonomyState {
    pub interiority: InteriorityClock,
    pub cache_keepalive: CacheKeepaliveScheduler,
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
    /// Cached last LLM request for cache keepalive pings.
    last_request: Option<LlmRequest>,
}

impl AutonomyState {
    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Snap the interiority tick to the cache keepalive deadline when close
    /// enough, so a single API call serves both purposes.
    fn coordinate_interiority_keepalive(&mut self) {
        if let Some(deadline) = self.cache_keepalive.next_deadline() {
            self.interiority.snap_to_deadline(deadline);
        }
    }
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

const STATE_VERSION: u32 = 2;
const STATE_FILENAME: &str = "autonomy_state.json";

#[derive(Serialize, Deserialize)]
struct PersistedState {
    version: u32,
    interiority_state: String,
    ticks_without_user: u32,
    cache_ping_count: u32,
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
        cache_ping_count: state.cache_keepalive.ping_count(),
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
            warn!(state = other, "Unknown interiority state, defaulting to Active");
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
    states: Arc<Mutex<HashMap<String, Arc<Mutex<AutonomyState>>>>>,
    handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
    config: Arc<AutonomyConfig>,
    compaction: Arc<CompactionConfig>,
    data_dir: PathBuf,
    shutdown_rx: tokio::sync::watch::Receiver<()>,
    /// Channel for sending compaction trigger signals (character name).
    compaction_tx: mpsc::Sender<String>,
    /// LLM client for interiority ticks and cache keepalive pings.
    llm_client: Option<LlmClient>,
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
            states: Arc::new(Mutex::new(HashMap::new())),
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
        llm_client: LlmClient,
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
    pub fn ensure_state(&self, character: &str, keepalive_config: CacheKeepaliveConfig) {
        self.ensure_state_with_config(character, keepalive_config, None);
    }

    /// Like `ensure_state`, but accepts an optional per-character effective config
    /// that overrides the global config for model resolution and autonomy settings.
    pub fn ensure_state_with_config(
        &self,
        character: &str,
        keepalive_config: CacheKeepaliveConfig,
        effective_config: Option<&LoadedConfig>,
    ) {
        let mut states = self.states.lock().unwrap();
        if states.contains_key(character) {
            return;
        }

        // Use per-character autonomy config if available, otherwise global.
        let autonomy_cfg = effective_config
            .map(|c| Arc::new(c.app.behavior.autonomy.clone()))
            .unwrap_or_else(|| self.config.clone());

        // Create interiority clock with config values.
        let mut interiority = InteriorityClock::with_config(&autonomy_cfg.interiority);
        let cache_keepalive = CacheKeepaliveScheduler::new(keepalive_config);

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
            cache_keepalive,
            activity: ActivityTracker::new(),
            interiority_log: InteriorityLog::new(),
            dirty: false,
            last_compaction_activity: Instant::now(),
            compaction_triggered: false,
            active_turn_count: 0,
            needs_engine_reload: false,
            last_request: None,
        }));

        states.insert(character.to_string(), state.clone());

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

        let handle = tokio::spawn(async move {
            character_tick_loop(
                name,
                state,
                config,
                compaction,
                data_dir,
                shutdown_rx,
                compaction_tx,
                llm_client,
                push_tx,
                loaded_config,
                notifier,
            )
            .await;
        });

        self.handles.lock().unwrap().push(handle);
    }

    // -- state access helper ---------------------------------------------------

    /// Lock the states map, find the character's state, lock it, and call `f`.
    /// Returns `None` if the character has no autonomy state.
    fn with_state<R, F: FnOnce(&mut AutonomyState) -> R>(
        &self,
        character: &str,
        f: F,
    ) -> Option<R> {
        let states = self.states.lock().unwrap();
        let state = states.get(character)?;
        let mut s = state.lock().unwrap();
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
                s.interiority_log.push(
                    InteriorityEventKind::Wake,
                    "User returned — woke from dormant",
                );
            }
            s.activity.record_message();
            s.last_compaction_activity = now;
            s.active_turn_count = message_count;
            s.coordinate_interiority_keepalive();
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
            s.mark_dirty();
        });
    }

    /// Call after an LLM API response with cache usage info.
    pub fn notify_api_response(
        &self,
        character: &str,
        cache_read_tokens: u32,
        input_tokens: u32,
    ) {
        self.with_state(character, |s| {
            let now = Instant::now();
            s.cache_keepalive
                .on_api_response(now, cache_read_tokens, input_tokens);
            s.coordinate_interiority_keepalive();
        });
    }

    /// Cache the last LLM request for keepalive ping reuse.
    pub fn notify_last_request(&self, character: &str, request: LlmRequest) {
        self.with_state(character, |s| {
            s.last_request = Some(request);
        });
    }

    /// Call after compaction completes successfully. Updates the turn count
    /// and signals the handler to reload the engine on the next message.
    pub fn notify_compaction_complete(&self, character: &str, new_turn_count: usize) {
        self.with_state(character, |s| {
            s.active_turn_count = new_turn_count;
            s.needs_engine_reload = true;
            // Keep compaction_triggered = true until engine reload acknowledges it.
            s.mark_dirty();
            info!(
                character = %character,
                new_turn_count,
                "Compaction complete — engine reload pending"
            );
        });
    }

    /// Call after compaction fails. Resets the trigger so it can retry.
    pub fn notify_compaction_failed(&self, character: &str) {
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
                return true;
            }
            false
        })
        .unwrap_or(false)
    }


    /// Update the cache keepalive config for a character (e.g. on model switch).
    pub fn update_keepalive_config(&self, character: &str, config: CacheKeepaliveConfig) {
        self.with_state(character, |s| {
            s.cache_keepalive.update_config(config);
        });
    }

    /// Explicitly set the paused state for a character. Returns the new state,
    /// or None if the character has no autonomy state.
    pub fn set_paused(&self, character: &str, paused: bool) -> Option<bool> {
        self.with_state(character, |s| {
            s.interiority.set_paused(paused);
            s.cache_keepalive.set_paused(paused);
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
            cache_keepalive_state: format!("{:?}", s.cache_keepalive.state()),
            cache_keepalive_pings: s.cache_keepalive.ping_count(),
        })
    }

    /// Return recent interiority events for `shore log --heartbeat`.
    pub fn heartbeat_log(&self, character: &str, limit: usize) -> Vec<super::InteriorityEvent> {
        self.with_state(character, |s| {
            s.interiority_log.recent(limit).into_iter().cloned().collect()
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
        for handle in handles {
            let _ = handle.await;
        }
    }
}

// ---------------------------------------------------------------------------
// Per-character tick loop
// ---------------------------------------------------------------------------

/// Tick interval for each character's autonomy loop.
const TICK_INTERVAL: Duration = Duration::from_secs(30);

async fn character_tick_loop(
    character: String,
    state: Arc<Mutex<AutonomyState>>,
    config: Arc<AutonomyConfig>,
    compaction: Arc<CompactionConfig>,
    data_dir: PathBuf,
    mut shutdown_rx: tokio::sync::watch::Receiver<()>,
    compaction_tx: mpsc::Sender<String>,
    llm_client: Option<LlmClient>,
    push_tx: Option<broadcast::Sender<ServerMessage>>,
    loaded_config: Option<Arc<LoadedConfig>>,
    notifier: Option<NotificationService>,
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
                tick_character(
                    &character,
                    &state,
                    &config,
                    &compaction,
                    &data_dir,
                    &compaction_tx,
                    llm_client.as_ref(),
                    push_tx.as_ref(),
                    loaded_config.as_deref(),
                    notifier.as_ref(),
                ).await;
            }
            _ = shutdown_rx.changed() => {
                // Final save before shutdown.
                let mut s = state.lock().unwrap();
                s.mark_dirty();
                save_state(&data_dir, &character, &mut s);
                info!(character = %character, "Autonomy tick task shutting down");
                break;
            }
        }
    }
}

/// One tick for a single character.
async fn tick_character(
    character: &str,
    state: &Arc<Mutex<AutonomyState>>,
    config: &AutonomyConfig,
    compaction: &CompactionConfig,
    data_dir: &Path,
    compaction_tx: &mpsc::Sender<String>,
    llm_client: Option<&LlmClient>,
    push_tx: Option<&broadcast::Sender<ServerMessage>>,
    loaded_config: Option<&LoadedConfig>,
    notifier: Option<&NotificationService>,
) {
    let now = Instant::now();

    // Collect actions under the lock, then release before any async work.
    let (int_action, ka_action, compaction_needed) = {
        let mut s = state.lock().unwrap();

        // -- interiority ------------------------------------------------------
        let int_action = if config.enabled && config.interiority.enabled {
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

        // -- coordinate interiority → keepalive --------------------------------
        s.coordinate_interiority_keepalive();

        // -- cache keepalive --------------------------------------------------
        let ka_action = s.cache_keepalive.tick(now);
        if !matches!(ka_action, KeepaliveAction::None) {
            s.mark_dirty();
        }

        // -- compaction triggers ---------------------------------------------
        let mut compaction_needed = false;
        if config.enabled && compaction.enabled && !s.compaction_triggered {
            if compaction.max_turns > 0
                && s.active_turn_count >= compaction.max_turns
                && s.active_turn_count >= compaction.min_turns
            {
                s.compaction_triggered = true;
                compaction_needed = true;
                info!(
                    character = %character,
                    turn_count = s.active_turn_count,
                    max_turns = compaction.max_turns,
                    "Compaction: max turns trigger fired"
                );
            } else if s.active_turn_count >= compaction.min_turns {
                let idle_secs = now.duration_since(s.last_compaction_activity).as_secs();
                let threshold_secs = compaction.idle_trigger_minutes as u64 * 60;
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

        save_state(data_dir, character, &mut s);
        (int_action, ka_action, compaction_needed)
    };

    if compaction_needed {
        let _ = compaction_tx.try_send(character.to_string());
    }

    // -- execute interiority tick (async, outside lock) -------------------
    if matches!(int_action, InteriorityAction::RunTick) {
        {
            let mut s = state.lock().unwrap();
            s.interiority_log.push(
                InteriorityEventKind::TickFired,
                "Interiority tick fired",
            );
        }
        execute_interiority_tick(
            character, state, config, data_dir, llm_client, push_tx, loaded_config, notifier,
        ).await;
    }

    // -- execute keepalive actions (async, outside lock) -------------------
    match ka_action {
        KeepaliveAction::None => {}
        KeepaliveAction::SendPing => {
            execute_keepalive_ping(character, state, llm_client).await;
        }
        KeepaliveAction::EmitCacheWarning { expected_tokens, message } => {
            info!(
                character = %character,
                expected_tokens,
                message = %message,
                "Cache keepalive: cache miss warning"
            );
            if let Some(tx) = push_tx {
                let _ = tx.send(ServerMessage::CacheWarning(CacheWarning {
                    expected_tokens,
                    message: message.clone(),
                }));
            }
            if let Some(n) = notifier {
                n.notify(
                    NotificationEvent::CacheWarning,
                    &format!("Shore — {character}"),
                    &message,
                );
            }
        }
    }

    // -- final persist (in case async actions dirtied state) ---------------
    {
        let mut s = state.lock().unwrap();
        save_state(data_dir, character, &mut s);
    }
}

// ---------------------------------------------------------------------------
// Interiority tick executor
// ---------------------------------------------------------------------------

/// Ephemeral prompt appended as the final user message during interiority ticks.
const INTERIORITY_PROMPT: &str = "\
[This is a private moment. You are not in conversation — no one is watching. \
Use your tools (memory, scratchpad, web) to think, plan, reflect, or research. \
If you want to message the user, wrap your message in <sendMessage>...</sendMessage> tags. \
Otherwise, work silently.]";

/// Execute a full interiority tick: clone last_request, append interiority prompt,
/// run a tool loop, optionally message the user.
async fn execute_interiority_tick(
    character: &str,
    state: &Arc<Mutex<AutonomyState>>,
    config: &AutonomyConfig,
    data_dir: &Path,
    llm_client: Option<&LlmClient>,
    push_tx: Option<&broadcast::Sender<ServerMessage>>,
    loaded_config: Option<&LoadedConfig>,
    notifier: Option<&NotificationService>,
) {
    let Some(client) = llm_client else { return };

    // Clone the last conversation request to preserve the cache prefix
    // (system prompt, tool definitions, provider_options are all inherited).
    let mut request = {
        let s = state.lock().unwrap();
        match &s.last_request {
            Some(req) => req.clone(),
            None => {
                info!(character, "Interiority: skipping tick (no prior conversation)");
                return;
            }
        }
    };

    // Append the interiority prompt as a new user message.
    request.messages.push(json!({"role": "user", "content": INTERIORITY_PROMPT}));
    request.max_tokens = 1000;

    let max_rounds = config.interiority.max_tool_rounds;
    let Some(lc) = loaded_config else { return };
    let tool_ctx = match build_tool_context(
        character, data_dir, client, lc,
    ).await {
        Some(ctx) => ctx,
        None => {
            warn!(character, "Interiority: failed to build tool context, skipping tick");
            return;
        }
    };
    let active_path = data_dir.join(character).join("active.jsonl");

    info!(character, max_rounds, "Interiority: executing tick");

    // -- Tool loop: generate → dispatch tools → generate again ----------------
    let mut final_content = String::new();
    let mut final_blocks: Vec<ContentBlock> = Vec::new();

    for round in 0..=max_rounds {
        let resp = match client.generate(&request, None).await {
            Ok(r) => r,
            Err(e) => {
                error!(character, error = %e, round, "Interiority: LLM call failed");
                return;
            }
        };

        // Update cache keepalive — this API call keeps the cache warm.
        {
            let mut s = state.lock().unwrap();
            let now = Instant::now();
            s.cache_keepalive.on_api_response(
                now,
                resp.usage.cache_read_tokens,
                resp.usage.input_tokens,
            );
        }

        // Log the response.
        info!(
            character,
            round,
            finish_reason = %resp.finish_reason,
            input_tokens = resp.usage.input_tokens,
            output_tokens = resp.usage.output_tokens,
            cache_read = resp.usage.cache_read_tokens,
            "Interiority: LLM response"
        );
        if !resp.content.is_empty() {
            let preview: String = resp.content.chars().take(200).collect();
            info!(character, round, content = %preview, "Interiority: response text");
        }

        final_content = resp.content.clone();
        final_blocks = resp.content_blocks.clone();

        // If no tool use, we're done.
        if resp.finish_reason != "tool_use" {
            break;
        }

        // Bail if we've exhausted tool rounds.
        if round >= max_rounds {
            warn!(character, max_rounds, "Interiority: hit max tool rounds");
            let mut s = state.lock().unwrap();
            s.interiority_log.push(
                InteriorityEventKind::ToolUse,
                format!("Hit max tool rounds ({max_rounds})"),
            );
            break;
        }

        // Build assistant message from content blocks for the next request.
        let assistant_content: Vec<serde_json::Value> = resp.content_blocks.iter()
            .filter_map(crate::content_util::content_block_to_api_json)
            .collect();

        request.messages.push(json!({"role": "assistant", "content": assistant_content}));

        // Execute each tool call and collect results.
        let mut tool_results: Vec<serde_json::Value> = Vec::new();
        for block in &resp.content_blocks {
            if let ContentBlock::ToolUse { id, name, input } = block {
                info!(
                    character, round,
                    tool = %name, tool_id = %id,
                    input = %serde_json::to_string(input).unwrap_or_default(),
                    "Interiority: executing tool"
                );

                let (output_str, is_error) = match tool_system::dispatch_tool(name, input.clone(), &tool_ctx).await {
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
                    character, round,
                    tool = %name, is_error,
                    output = %truncate_log(&output_str, 200),
                    "Interiority: tool result"
                );

                {
                    let mut s = state.lock().unwrap();
                    s.interiority_log.push(
                        InteriorityEventKind::ToolUse,
                        format!("Tool: {name} → {}", truncate_log(&output_str, 80)),
                    );
                }

                let mut result_block = json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": output_str,
                });
                if is_error {
                    result_block["is_error"] = json!(true);
                }
                tool_results.push(result_block);
            }
        }

        // Append tool results as user message.
        request.messages.push(json!({"role": "user", "content": tool_results}));
    }

    // -- Extract <sendMessage> and handle result --------------------------------

    let send_message = extract_send_message(&final_content);

    if let Some(user_msg) = send_message {
        info!(character, msg = %truncate_log(&user_msg, 200), "Interiority: sending message to user");

        let content_blocks = vec![ContentBlock::Text { text: user_msg.clone() }];
        let content = derive_content_from_blocks(&content_blocks);
        let msg = Message {
            msg_id: format!("m_{}", uuid::Uuid::new_v4()),
            role: Role::Assistant,
            content,
            images: vec![],
            content_blocks,
            alt_index: None,
            alt_count: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
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
            let _ = tx.send(ServerMessage::NewMessage(NewMessage { message: msg.clone() }));
        }
        if let Some(n) = notifier {
            n.notify(
                NotificationEvent::AutonomousMessage,
                &format!("Shore — {character}"),
                &msg.content,
            );
        }

        let mut s = state.lock().unwrap();
        let preview: String = msg.content.chars().take(80).collect();
        s.interiority_log.push(
            InteriorityEventKind::MessageSent,
            format!("Autonomous message sent: {preview}"),
        );
        s.mark_dirty();
    } else {
        // Log thinking even when no message sent — useful for prompt tuning.
        let thinking: Vec<&str> = final_blocks.iter().filter_map(|b| {
            if let ContentBlock::Text { text } = b { Some(text.as_str()) } else { None }
        }).collect();
        let summary = if thinking.is_empty() {
            "Tick completed — no message, no text output".to_string()
        } else {
            format!("Tick completed silently: {}", truncate_log(&thinking.join(" "), 150))
        };
        info!(character, summary = %summary, "Interiority: tick complete (silent)");
        let mut s = state.lock().unwrap();
        s.interiority_log.push(InteriorityEventKind::MessageSkipped, summary);
    }
}

/// Truncate a string for log output.
fn truncate_log(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}…")
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
    client: &LlmClient,
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
    let agent_model_name = config.app.defaults.memory_agent.as_deref()
        .or(config.app.defaults.model.as_deref())?;
    let agent_model = config.models.find_model(agent_model_name).ok()?;

    // Researcher model (optional).
    let researcher_model = config.app.defaults.collation.as_deref()
        .and_then(|name| config.models.find_model(name).ok())
        .cloned();

    // Semantic search context (graceful: None if no embedding model).
    let search_ctx = match resolve_embed_config(
        config.app.defaults.embedding.as_deref(),
        &config.models.embedding,
    ) {
        Ok(embed_config) => {
            let vs_path = char_dir.join("memory").join("vectorstore");
            VectorStore::open(&vs_path, embed_config.dimensions).await
                .ok()
                .map(|vs| AgentSearchContext::new(vs, client.clone(), embed_config))
        }
        Err(_) => None,
    };

    let image_gen_config = resolve_image_gen_config(
        config.app.defaults.image_generation.as_deref(),
        &config.models.image_generation,
    ).ok();

    let display_name = config.app.defaults.resolve_display_name();

    Some(SharedToolContext {
        db,
        agent: crate::memory::agent::MemoryAgent::one_shot(
            CallerIdentity::Char, character, &display_name,
        ),
        agent_llm: RealAgentLlm::new(client.clone()),
        agent_model_val: agent_model.clone(),
        researcher: researcher_model.as_ref().map(|_| MemoryResearcher::new(String::new(), String::new())),
        researcher_llm_val: researcher_model.as_ref().map(|_| RealAgentLlm::new(client.clone())),
        researcher_model_val: researcher_model,
        rag: NoopRag,
        search_ctx,
        image_dir_val: char_dir.join("images").to_string_lossy().into_owned(),
        llm_client_val: client.clone(),
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
// Cache keepalive executor
// ---------------------------------------------------------------------------

/// Send a minimal API call (max_tokens=1) to refresh the prompt cache.
async fn execute_keepalive_ping(
    character: &str,
    state: &Arc<Mutex<AutonomyState>>,
    llm_client: Option<&LlmClient>,
) {
    let Some(client) = llm_client else { return };

    // Re-check state under the lock before sending.
    let request = {
        let s = state.lock().unwrap();
        if !matches!(s.cache_keepalive.state(), &KeepaliveState::Pinging) {
            debug!(character, "Cache keepalive: state changed since tick, skipping ping");
            return;
        }
        info!(
            character = %character,
            ping_count = s.cache_keepalive.ping_count(),
            "Cache keepalive: sending ping"
        );
        match &s.last_request {
            Some(req) => {
                let mut ping = req.clone();
                ping.max_tokens = 1;
                Some(ping)
            }
            None => None,
        }
    };

    let Some(request) = request else {
        debug!(character, "Cache keepalive: no cached request, skipping ping");
        return;
    };

    match client.generate(&request, None).await {
        Ok(resp) => {
            let mut s = state.lock().unwrap();
            let now = Instant::now();
            let action = s.cache_keepalive.on_ping_response(now, resp.usage.cache_read_tokens);
            match action {
                KeepaliveAction::EmitCacheWarning { expected_tokens, message } => {
                    warn!(character, expected_tokens, %message, "Cache keepalive: miss after ping");
                }
                _ => {
                    debug!(
                        character,
                        cache_read = resp.usage.cache_read_tokens,
                        "Cache keepalive: ping successful"
                    );
                }
            }
            s.mark_dirty();
        }
        Err(e) => {
            error!(character, error = %e, "Cache keepalive ping failed");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autonomy::cache_keepalive::CacheKeepaliveConfig;

    fn test_config() -> AutonomyConfig {
        AutonomyConfig::default()
    }

    fn test_keepalive_config() -> CacheKeepaliveConfig {
        CacheKeepaliveConfig::default()
    }

    fn test_manager(data_dir: &Path) -> AutonomyManager {
        let (_tx, rx) = tokio::sync::watch::channel(());
        let (mgr, _compaction_rx) = AutonomyManager::new(test_config(), Default::default(), data_dir.to_path_buf(), rx);
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
            mgr.ensure_state("alice", test_keepalive_config());
            let states = mgr.states.lock().unwrap();
            assert!(states.contains_key("alice"));
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
            mgr.ensure_state("alice", test_keepalive_config());
            mgr.ensure_state("alice", test_keepalive_config());
            let states = mgr.states.lock().unwrap();
            assert_eq!(states.len(), 1);
        });
    }

    // -- notify ---------------------------------------------------------------

    #[test]
    fn notify_without_state_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let (_tx, rx) = tokio::sync::watch::channel(());
        let (mgr, _compaction_rx) = AutonomyManager::new(test_config(), Default::default(), tmp.path().to_path_buf(), rx);
        // Should not panic.
        mgr.notify_user_message("nobody", 0);
        mgr.notify_assistant_message("nobody", 0);
        mgr.notify_api_response("nobody", 100, 200);
    }

    // -- status ---------------------------------------------------------------

    #[test]
    fn status_returns_none_for_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let (_tx, rx) = tokio::sync::watch::channel(());
        let (mgr, _compaction_rx) = AutonomyManager::new(test_config(), Default::default(), tmp.path().to_path_buf(), rx);
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
            mgr.ensure_state("alice", test_keepalive_config());
            let status = mgr.status("alice").unwrap();
            assert_eq!(status.interiority_state, "Active");
            assert_eq!(status.ticks_without_user, 0);
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
            cache_keepalive: CacheKeepaliveScheduler::new(CacheKeepaliveConfig::default()),
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
            cache_ping_count: 3,
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
            cache_keepalive: CacheKeepaliveScheduler::new(CacheKeepaliveConfig::default()),
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
            let mut s = state.lock().unwrap();
            s.interiority.on_user_message(Instant::now());
            s.activity.record_message();
        }

        tick_character("alice", &state, &config, &Default::default(), tmp.path(), &compaction_tx, None, None, None, None).await;
    }

    #[test]
    fn extract_send_message_parses() {
        assert_eq!(
            extract_send_message("thinking...<sendMessage>Hey there!</sendMessage>...done"),
            Some("Hey there!".into())
        );
        assert_eq!(extract_send_message("no tags here"), None);
        assert_eq!(
            extract_send_message("<sendMessage></sendMessage>"),
            None
        );
    }
}
