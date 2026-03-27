//! AutonomyManager — per-character scheduler state with background tick tasks.
//!
//! Each character gets its own tokio task that ticks the heartbeat and cache
//! keepalive schedulers on a fixed interval. State is persisted to
//! `{data_dir}/{character}/autonomy_state.json` and restored on startup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{NaiveDateTime, Timelike, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use shore_protocol::server_msg::{CacheWarning, NewMessage, ServerMessage};
use shore_protocol::types::{Message, Role};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use super::activity::{ActivityTracker, HourClassification};
use super::cache_keepalive::{
    CacheKeepaliveConfig, CacheKeepaliveScheduler, KeepaliveAction,
};
use super::heartbeat::{
    self, HeartbeatAction, HeartbeatScheduler, HeartbeatState, ProbeResult,
};
use super::timing::{compute_tau, TauParams};
use super::{AutonomyStatus, HeartbeatEventKind, HeartbeatLog};
use crate::notifications::{NotificationEvent, NotificationService};
use shore_config::app::AutonomyConfig;
use shore_config::LoadedConfig;
use shore_llm_client::types::LlmRequest;
use shore_llm_client::LlmClient;

// ---------------------------------------------------------------------------
// Per-character state
// ---------------------------------------------------------------------------

/// All autonomy state for a single character.
pub struct AutonomyState {
    pub heartbeat: HeartbeatScheduler,
    pub cache_keepalive: CacheKeepaliveScheduler,
    pub activity: ActivityTracker,
    /// Ring buffer of heartbeat events for `shore log --heartbeat`.
    pub heartbeat_log: HeartbeatLog,
    /// Whether state has changed since last save.
    dirty: bool,
    /// Last message activity timestamp for compaction idle trigger.
    last_compaction_activity: Instant,
    /// Whether compaction was already triggered for this idle period.
    compaction_triggered: bool,
    /// Current number of messages in active.jsonl (updated on each message notification).
    active_message_count: usize,
    /// Cached last LLM request for cache keepalive pings.
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

const STATE_VERSION: u32 = 1;
const STATE_FILENAME: &str = "autonomy_state.json";

#[derive(Serialize, Deserialize)]
struct PersistedState {
    version: u32,
    heartbeat_state: String,
    unanswered_count: u32,
    /// ISO 8601 wall-clock time for Deferred state.
    deferred_fire_at: Option<String>,
    deferred_reasoning: Option<String>,
    cache_ping_count: u32,
    cache_estimated_tokens: u32,
}

fn state_path(data_dir: &Path, character: &str) -> PathBuf {
    data_dir.join(character).join(STATE_FILENAME)
}

fn save_state(data_dir: &Path, character: &str, state: &mut AutonomyState) {
    if !state.dirty {
        return;
    }

    let (hb_state_str, fire_at, reasoning) = match state.heartbeat.state() {
        HeartbeatState::Session => ("Session".into(), None, None),
        HeartbeatState::PostSessionProbe => ("PostSessionProbe".into(), None, None),
        HeartbeatState::Deferred { fire_at, reasoning } => (
            "Deferred".into(),
            Some(fire_at.to_string()),
            Some(reasoning.clone()),
        ),
        HeartbeatState::SocialNeed => ("SocialNeed".into(), None, None),
        HeartbeatState::Dormant => ("Dormant".into(), None, None),
    };

    let persisted = PersistedState {
        version: STATE_VERSION,
        heartbeat_state: hb_state_str,
        unanswered_count: state.heartbeat.unanswered_count(),
        deferred_fire_at: fire_at,
        deferred_reasoning: reasoning,
        cache_ping_count: state.cache_keepalive.ping_count(),
        cache_estimated_tokens: 0, // not exposed via accessor; reset on restart
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
                "Ignoring autonomy state with unknown version"
            );
            None
        }
        Err(e) => {
            warn!(character, error = %e, "Failed to parse autonomy state");
            None
        }
    }
}

fn restore_heartbeat(persisted: &PersistedState) -> (HeartbeatState, u32) {
    let wall_now = Utc::now().naive_utc();

    let state = match persisted.heartbeat_state.as_str() {
        "Session" => HeartbeatState::Session,
        "PostSessionProbe" => {
            // Don't block on a probe from a previous session.
            HeartbeatState::SocialNeed
        }
        "Deferred" => {
            match (&persisted.deferred_fire_at, &persisted.deferred_reasoning) {
                (Some(fire_at_str), Some(reasoning)) => {
                    match fire_at_str.parse::<NaiveDateTime>() {
                        Ok(fire_at) if fire_at > wall_now => HeartbeatState::Deferred {
                            fire_at,
                            reasoning: reasoning.clone(),
                        },
                        // Expired or parse error — move to SocialNeed.
                        _ => HeartbeatState::SocialNeed,
                    }
                }
                _ => HeartbeatState::SocialNeed,
            }
        }
        "SocialNeed" => HeartbeatState::SocialNeed,
        "Dormant" => HeartbeatState::Dormant,
        other => {
            warn!(state = other, "Unknown heartbeat state, defaulting to Session");
            HeartbeatState::Session
        }
    };

    (state, persisted.unanswered_count)
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
    data_dir: PathBuf,
    shutdown_rx: tokio::sync::watch::Receiver<()>,
    /// Channel for sending compaction trigger signals (character name).
    compaction_tx: mpsc::Sender<String>,
    /// LLM client for heartbeat probes and cache keepalive pings.
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
        data_dir: PathBuf,
        shutdown_rx: tokio::sync::watch::Receiver<()>,
    ) -> (Self, mpsc::Receiver<String>) {
        let (compaction_tx, compaction_rx) = mpsc::channel(16);
        let mgr = Self {
            states: Arc::new(Mutex::new(HashMap::new())),
            handles: Arc::new(Mutex::new(Vec::new())),
            config: Arc::new(config),
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
        let mut states = self.states.lock().unwrap();
        if states.contains_key(character) {
            return;
        }

        // Create scheduler state.
        let threshold = self.config.heartbeat.dormant_threshold;
        let mut heartbeat = HeartbeatScheduler::with_threshold(threshold);
        let mut cache_keepalive = CacheKeepaliveScheduler::new(keepalive_config);

        // Restore persisted state if available.
        if let Some(persisted) = load_state(&self.data_dir, character) {
            let (hb_state, unanswered) = restore_heartbeat(&persisted);
            heartbeat.restore(hb_state, unanswered);
            cache_keepalive.restore_counters(persisted.cache_ping_count, persisted.cache_estimated_tokens);
            info!(character, "Autonomy state restored from disk");
        } else {
            info!(character, "Autonomy state created (no prior state)");
        }

        let state = Arc::new(Mutex::new(AutonomyState {
            heartbeat,
            cache_keepalive,
            activity: ActivityTracker::new(),
            heartbeat_log: HeartbeatLog::new(),
            dirty: false,
            last_compaction_activity: Instant::now(),
            compaction_triggered: false,
            active_message_count: 0,
            last_request: None,
        }));

        states.insert(character.to_string(), state.clone());

        // Spawn per-character tick task.
        let name = character.to_string();
        let config = self.config.clone();
        let data_dir = self.data_dir.clone();
        let shutdown_rx = self.shutdown_rx.clone();
        let compaction_tx = self.compaction_tx.clone();
        let llm_client = self.llm_client.clone();
        let push_tx = self.push_tx.clone();
        let loaded_config = self.loaded_config.clone();
        let notifier = self.notifier.clone();

        let handle = tokio::spawn(async move {
            character_tick_loop(
                name,
                state,
                config,
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

    // -- event notifications from the message handler -------------------------

    /// Call after a user message is appended.
    pub fn notify_user_message(&self, character: &str, message_count: usize) {
        let states = self.states.lock().unwrap();
        if let Some(state) = states.get(character) {
            let mut s = state.lock().unwrap();
            let was_dormant = matches!(s.heartbeat.state(), HeartbeatState::Dormant);
            let now = Instant::now();
            s.heartbeat.on_user_message(now);
            if was_dormant {
                s.heartbeat_log.push(
                    HeartbeatEventKind::Wake,
                    "User returned — woke from dormant",
                );
            }
            s.activity.record_message();
            s.last_compaction_activity = now;
            s.compaction_triggered = false;
            s.active_message_count = message_count;
            s.mark_dirty();
        }
    }

    /// Call after an assistant message is appended.
    pub fn notify_assistant_message(&self, character: &str, message_count: usize) {
        let states = self.states.lock().unwrap();
        if let Some(state) = states.get(character) {
            let mut s = state.lock().unwrap();
            let now = Instant::now();
            s.heartbeat.on_assistant_message(now);
            s.activity.record_message();
            s.last_compaction_activity = now;
            s.compaction_triggered = false;
            s.active_message_count = message_count;
        }
    }

    /// Call after an LLM API response with cache usage info.
    pub fn notify_api_response(
        &self,
        character: &str,
        cache_read_tokens: u32,
        input_tokens: u32,
    ) {
        let states = self.states.lock().unwrap();
        if let Some(state) = states.get(character) {
            let mut s = state.lock().unwrap();
            let now = Instant::now();
            s.cache_keepalive
                .on_api_response(now, cache_read_tokens, input_tokens);
        }
    }

    /// Cache the last LLM request for keepalive ping reuse.
    pub fn notify_last_request(&self, character: &str, request: LlmRequest) {
        let states = self.states.lock().unwrap();
        if let Some(state) = states.get(character) {
            let mut s = state.lock().unwrap();
            s.last_request = Some(request);
        }
    }

    /// Update the cache keepalive config for a character (e.g. on model switch).
    pub fn update_keepalive_config(&self, character: &str, config: CacheKeepaliveConfig) {
        let states = self.states.lock().unwrap();
        if let Some(state) = states.get(character) {
            let mut s = state.lock().unwrap();
            s.cache_keepalive.update_config(config);
        }
    }

    /// Explicitly set the paused state for a character. Returns the new state,
    /// or None if the character has no autonomy state.
    pub fn set_paused(&self, character: &str, paused: bool) -> Option<bool> {
        let states = self.states.lock().unwrap();
        let state_arc = states.get(character)?;
        let mut s = state_arc.lock().unwrap();
        s.heartbeat.set_paused(paused);
        s.cache_keepalive.set_paused(paused);
        s.mark_dirty();
        Some(paused)
    }

    // -- activity stats --------------------------------------------------------

    /// Return a clone of the `ActivityStats` and message count for a character.
    ///
    /// Used by the activity heatmap tool.
    pub fn activity_stats(
        &self,
        character: &str,
    ) -> Option<(super::activity::ActivityStats, usize)> {
        let states = self.states.lock().unwrap();
        let state_arc = states.get(character)?;
        let mut state = state_arc.lock().unwrap();
        let stats = state.activity.stats().clone();
        let count = state.activity.message_count();
        Some((stats, count))
    }

    // -- status snapshot ------------------------------------------------------

    /// Build an `AutonomyStatus` snapshot for the status command.
    pub fn status(&self, character: &str) -> Option<AutonomyStatus> {
        let states = self.states.lock().unwrap();
        let state_arc = states.get(character)?;
        let mut state = state_arc.lock().unwrap();

        // Clone stats to release the mutable borrow on activity before
        // accessing heartbeat fields.
        let stats = state.activity.stats().clone();
        let tau_params = TauParams {
            reciprocated: state.heartbeat.unanswered_count() == 0,
            hour_class: current_hour_class(stats.hour_classifications),
            personality: self.config.personality,
        };
        let tau = compute_tau(&stats, &tau_params);
        let dormant_threshold = state.heartbeat.dormant_threshold();

        Some(AutonomyStatus {
            paused: state.heartbeat.is_paused(),
            heartbeat_state: format!("{:?}", state.heartbeat.state()),
            unanswered_count: state.heartbeat.unanswered_count(),
            dormant_threshold,
            social_need_bar: state.heartbeat.social_need_bar(),
            tau,
            cache_keepalive_state: format!("{:?}", state.cache_keepalive.state()),
            cache_keepalive_pings: state.cache_keepalive.ping_count(),
        })
    }

    /// Return recent heartbeat events for `shore log --heartbeat`.
    pub fn heartbeat_log(&self, character: &str, limit: usize) -> Vec<super::HeartbeatEvent> {
        let states = self.states.lock().unwrap();
        let Some(state_arc) = states.get(character) else {
            return vec![];
        };
        let state = state_arc.lock().unwrap();
        state.heartbeat_log.recent(limit).into_iter().cloned().collect()
    }

    // -- shutdown -------------------------------------------------------------

    /// Wait for all per-character tick tasks to finish.
    ///
    /// Call after the shutdown signal has been sent.
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
    data_dir: &Path,
    compaction_tx: &mpsc::Sender<String>,
    llm_client: Option<&LlmClient>,
    push_tx: Option<&broadcast::Sender<ServerMessage>>,
    loaded_config: Option<&LoadedConfig>,
    notifier: Option<&NotificationService>,
) {
    let now = Instant::now();
    let wall_now = Utc::now().naive_utc();

    // Collect actions under the lock, then release before any async work.
    let (hb_action, ka_action, compaction_needed) = {
        let mut s = state.lock().unwrap();

        // -- heartbeat --------------------------------------------------------
        let hb_action = if config.enabled && config.heartbeat.enabled {
            let state_before = format!("{:?}", s.heartbeat.state());
            let stats = s.activity.stats().clone();
            let tau_params = TauParams {
                reciprocated: s.heartbeat.unanswered_count() == 0,
                hour_class: current_hour_class(stats.hour_classifications),
                personality: config.personality,
            };
            let random_value: f64 = rand::thread_rng().gen();
            let action = s.heartbeat.tick(&stats, &tau_params, now, wall_now, random_value);
            if !matches!(action, HeartbeatAction::None) {
                s.mark_dirty();
            }
            // Record state transitions and dormant entry.
            let state_after = format!("{:?}", s.heartbeat.state());
            if state_before != state_after {
                if matches!(s.heartbeat.state(), HeartbeatState::Dormant) {
                    let count = s.heartbeat.unanswered_count();
                    s.heartbeat_log.push(
                        HeartbeatEventKind::Dormant,
                        format!("Entered dormant (unanswered: {count})"),
                    );
                } else {
                    s.heartbeat_log.push(
                        HeartbeatEventKind::StateChange,
                        format!("{state_before} → {state_after}"),
                    );
                }
            }
            action
        } else {
            HeartbeatAction::None
        };

        // -- cache keepalive --------------------------------------------------
        let ka_action = s.cache_keepalive.tick(now);
        if !matches!(ka_action, KeepaliveAction::None) {
            s.mark_dirty();
        }

        // -- compaction triggers ---------------------------------------------
        let mut compaction_needed = false;
        if config.enabled && config.compaction.enabled && !s.compaction_triggered {
            let min_total = config.compaction.min_messages + config.compaction.keep_recent;
            if config.compaction.max_messages > 0
                && s.active_message_count >= config.compaction.max_messages
                && s.active_message_count >= min_total
            {
                s.compaction_triggered = true;
                compaction_needed = true;
                info!(
                    character = %character,
                    message_count = s.active_message_count,
                    max_messages = config.compaction.max_messages,
                    "Compaction: max messages trigger fired"
                );
            } else if s.active_message_count >= min_total {
                let idle_secs = now.duration_since(s.last_compaction_activity).as_secs();
                let threshold_secs = config.compaction.idle_trigger_minutes as u64 * 60;
                if threshold_secs > 0 && idle_secs >= threshold_secs {
                    s.compaction_triggered = true;
                    compaction_needed = true;
                    info!(
                        character = %character,
                        idle_secs,
                        threshold_secs,
                        message_count = s.active_message_count,
                        "Compaction: idle trigger fired"
                    );
                }
            }
        }

        save_state(data_dir, character, &mut s);
        (hb_action, ka_action, compaction_needed)
    };

    if compaction_needed {
        let _ = compaction_tx.try_send(character.to_string());
    }

    // -- execute heartbeat actions (async, outside lock) -------------------
    match hb_action {
        HeartbeatAction::None => {}
        HeartbeatAction::GenerateProbe { idle_secs, current_time } => {
            {
                let mut s = state.lock().unwrap();
                s.heartbeat_log.push(
                    HeartbeatEventKind::ProbeTrigger,
                    format!("Post-session probe after {idle_secs}s idle"),
                );
            }
            info!(
                character = %character,
                idle_secs,
                current_time = %current_time,
                "Heartbeat: post-session probe triggered"
            );
            execute_probe(character, idle_secs, current_time, now, wall_now, state, data_dir, llm_client, loaded_config).await;
        }
        HeartbeatAction::GenerateDeferredMessage { reasoning } => {
            {
                let mut s = state.lock().unwrap();
                s.heartbeat_log.push(
                    HeartbeatEventKind::DeferredFire,
                    format!("Deferred timer fired: {reasoning}"),
                );
            }
            info!(
                character = %character,
                reasoning = %reasoning,
                "Heartbeat: deferred message triggered"
            );
            execute_autonomous_message(
                character, &heartbeat::render_deferred(&reasoning, &wall_now),
                state, data_dir, llm_client, push_tx, loaded_config, notifier,
            ).await;
        }
        HeartbeatAction::GenerateSocialNeedMessage { anomaly_context } => {
            {
                let mut s = state.lock().unwrap();
                s.heartbeat_log.push(
                    HeartbeatEventKind::SocialNeedFire,
                    if anomaly_context {
                        "Social-need message triggered (anomaly detected)"
                    } else {
                        "Social-need message triggered"
                    },
                );
            }
            info!(
                character = %character,
                anomaly_context,
                "Heartbeat: social-need message triggered"
            );
            execute_autonomous_message(
                character, &heartbeat::render_social_need(anomaly_context),
                state, data_dir, llm_client, push_tx, loaded_config, notifier,
            ).await;
        }
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
// Heartbeat action executors
// ---------------------------------------------------------------------------

/// Execute the post-session probe: ask the LLM if the character wants to reach out.
async fn execute_probe(
    character: &str,
    idle_secs: u64,
    current_time: NaiveDateTime,
    now: Instant,
    wall_now: NaiveDateTime,
    state: &Arc<Mutex<AutonomyState>>,
    data_dir: &Path,
    llm_client: Option<&LlmClient>,
    loaded_config: Option<&LoadedConfig>,
) {
    let Some(client) = llm_client else { return };
    let Some(config) = loaded_config else { return };

    let prompt = heartbeat::render_post_session(idle_secs, &current_time);
    let request = match build_autonomy_request(config, &prompt, 500) {
        Some(r) => r,
        None => {
            warn!(character, "Cannot execute probe: no model configured");
            return;
        }
    };

    match client.generate(&request, None).await {
        Ok(resp) => {
            let mut s = state.lock().unwrap();
            let result = s.heartbeat.handle_probe_response(&resp.content, now, wall_now);
            match &result {
                ProbeResult::Deferred(fire_at) => {
                    s.heartbeat_log.push(
                        HeartbeatEventKind::ProbeResult,
                        format!("Deferred to {fire_at}"),
                    );
                    info!(character, fire_at = %fire_at, "Probe: character deferred to later");
                }
                ProbeResult::Declined => {
                    s.heartbeat_log.push(
                        HeartbeatEventKind::ProbeResult,
                        "Declined to reach out",
                    );
                    info!(character, "Probe: character declined to reach out");
                }
            }
            s.mark_dirty();
            save_state(data_dir, character, &mut s);
        }
        Err(e) => {
            error!(character, error = %e, "Heartbeat probe LLM call failed");
        }
    }
}

/// Execute an autonomous message (deferred or social-need): generate text and push it.
async fn execute_autonomous_message(
    character: &str,
    prompt: &str,
    state: &Arc<Mutex<AutonomyState>>,
    data_dir: &Path,
    llm_client: Option<&LlmClient>,
    push_tx: Option<&broadcast::Sender<ServerMessage>>,
    loaded_config: Option<&LoadedConfig>,
    notifier: Option<&NotificationService>,
) {
    let Some(client) = llm_client else { return };
    let Some(config) = loaded_config else { return };

    let request = match build_autonomy_request(config, prompt, 1000) {
        Some(r) => r,
        None => {
            warn!(character, "Cannot execute autonomous message: no model configured");
            return;
        }
    };

    match client.generate(&request, None).await {
        Ok(resp) => {
            if resp.content.trim().is_empty() {
                {
                    let mut s = state.lock().unwrap();
                    s.heartbeat_log.push(
                        HeartbeatEventKind::MessageSkipped,
                        "Character chose not to respond",
                    );
                }
                info!(character, "Autonomous message: character chose not to respond");
                return;
            }

            // Append to conversation file directly (like compaction_task does).
            let msg = Message {
                msg_id: format!("m_{}", uuid::Uuid::new_v4()),
                role: Role::Assistant,
                content: resp.content,
                images: vec![],
                content_blocks: vec![],
                alt_index: None,
                alt_count: None,
                timestamp: chrono::Utc::now().to_rfc3339(),
            };

            let active_path = data_dir.join(character).join("active.jsonl");
            if let Ok(line) = serde_json::to_string(&msg) {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&active_path)
                {
                    let _ = writeln!(f, "{line}");
                }
            }

            // Broadcast to connected clients.
            if let Some(tx) = push_tx {
                let _ = tx.send(ServerMessage::NewMessage(NewMessage { message: msg.clone() }));
            }

            // Push notification.
            if let Some(n) = notifier {
                n.notify(
                    NotificationEvent::AutonomousMessage,
                    &format!("Shore — {character}"),
                    &msg.content,
                );
            }

            // Update heartbeat state.
            {
                let mut s = state.lock().unwrap();
                let now = Instant::now();
                s.heartbeat.on_assistant_message(now);
                s.activity.record_message();
                let preview: String = msg.content.chars().take(80).collect();
                s.heartbeat_log.push(
                    HeartbeatEventKind::MessageSent,
                    format!("Autonomous message sent: {preview}"),
                );
                s.mark_dirty();
                save_state(data_dir, character, &mut s);
            }
        }
        Err(e) => {
            error!(character, error = %e, "Autonomous message LLM call failed");
        }
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

    // Clone the last request from state (if available), with max_tokens=1.
    let request = {
        let s = state.lock().unwrap();
        info!(
            character = %character,
            ping_count = s.cache_keepalive.ping_count(),
            "Cache keepalive: sending ping"
        );
        match &s.last_request {
            Some(req) => {
                let mut ping = req.clone();
                ping.max_tokens = 1;
                ping.tools = None;
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
// Helpers
// ---------------------------------------------------------------------------

/// Build a minimal LLM request for autonomous actions using the default model.
fn build_autonomy_request(
    config: &LoadedConfig,
    prompt: &str,
    max_tokens: u32,
) -> Option<LlmRequest> {
    let model_name = config.app.defaults.model.as_deref()?;
    let resolved = config.models.find_model(model_name).ok()?;
    LlmClient::build_request(
        resolved,
        vec![json!({"role": "user", "content": prompt})],
        None,
        None,
        None,
    ).ok().map(|mut r| {
        r.max_tokens = max_tokens;
        r
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Get the HourClassification for the current UTC hour.
fn current_hour_class(classifications: [HourClassification; 24]) -> HourClassification {
    let hour = Utc::now().naive_utc().time().hour() as usize;
    classifications[hour]
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
        let (mgr, _compaction_rx) = AutonomyManager::new(test_config(), data_dir.to_path_buf(), rx);
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
        let (mgr, _compaction_rx) = AutonomyManager::new(test_config(), tmp.path().to_path_buf(), rx);
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
        let (mgr, _compaction_rx) = AutonomyManager::new(test_config(), tmp.path().to_path_buf(), rx);
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
            assert_eq!(status.heartbeat_state, "Session");
            assert_eq!(status.unanswered_count, 0);
        });
    }

    // -- persistence ----------------------------------------------------------

    #[test]
    fn save_and_restore_session_state() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        std::fs::create_dir_all(data_dir.join("alice")).unwrap();

        // Create and save.
        let mut state = AutonomyState {
            heartbeat: HeartbeatScheduler::new(),
            cache_keepalive: CacheKeepaliveScheduler::new(CacheKeepaliveConfig::default()),
            activity: ActivityTracker::new(),
            heartbeat_log: HeartbeatLog::new(),
            dirty: true,
            last_compaction_activity: Instant::now(),
            compaction_triggered: false,
            active_message_count: 0,
            last_request: None,
        };
        save_state(data_dir, "alice", &mut state);
        assert!(!state.dirty);

        // Verify file exists.
        assert!(state_path(data_dir, "alice").exists());

        // Restore.
        let persisted = load_state(data_dir, "alice").unwrap();
        assert_eq!(persisted.heartbeat_state, "Session");
        assert_eq!(persisted.unanswered_count, 0);
    }

    #[test]
    fn restore_dormant_state() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        std::fs::create_dir_all(data_dir.join("alice")).unwrap();

        let persisted = PersistedState {
            version: STATE_VERSION,
            heartbeat_state: "Dormant".into(),
            unanswered_count: 5,
            deferred_fire_at: None,
            deferred_reasoning: None,
            cache_ping_count: 3,
            cache_estimated_tokens: 0,
        };
        let json = serde_json::to_string(&persisted).unwrap();
        std::fs::write(state_path(data_dir, "alice"), json).unwrap();

        let loaded = load_state(data_dir, "alice").unwrap();
        let (hb_state, unanswered) = restore_heartbeat(&loaded);
        assert!(matches!(hb_state, HeartbeatState::Dormant));
        assert_eq!(unanswered, 5);
    }

    #[test]
    fn restore_expired_deferred_becomes_social_need() {
        let expired = "2020-01-01T00:00:00".to_string();

        let persisted = PersistedState {
            version: STATE_VERSION,
            heartbeat_state: "Deferred".into(),
            unanswered_count: 1,
            deferred_fire_at: Some(expired),
            deferred_reasoning: Some("testing".into()),
            cache_ping_count: 0,
            cache_estimated_tokens: 0,
        };

        let (hb_state, _) = restore_heartbeat(&persisted);
        assert!(matches!(hb_state, HeartbeatState::SocialNeed));
    }

    #[test]
    fn restore_post_session_probe_becomes_social_need() {
        let persisted = PersistedState {
            version: STATE_VERSION,
            heartbeat_state: "PostSessionProbe".into(),
            unanswered_count: 0,
            deferred_fire_at: None,
            deferred_reasoning: None,
            cache_ping_count: 0,
            cache_estimated_tokens: 0,
        };

        let (hb_state, _) = restore_heartbeat(&persisted);
        assert!(matches!(hb_state, HeartbeatState::SocialNeed));
    }

    #[tokio::test]
    async fn tick_character_runs_without_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config();
        let (compaction_tx, _compaction_rx) = mpsc::channel(16);
        let state = Arc::new(Mutex::new(AutonomyState {
            heartbeat: HeartbeatScheduler::new(),
            cache_keepalive: CacheKeepaliveScheduler::new(CacheKeepaliveConfig::default()),
            activity: ActivityTracker::new(),
            heartbeat_log: HeartbeatLog::new(),
            dirty: false,
            last_compaction_activity: Instant::now(),
            compaction_triggered: false,
            active_message_count: 0,
            last_request: None,
        }));

        // Need at least one message for heartbeat to have timestamps.
        {
            let mut s = state.lock().unwrap();
            s.heartbeat.on_user_message(Instant::now());
            s.activity.record_message();
        }

        tick_character("alice", &state, &config, tmp.path(), &compaction_tx, None, None, None, None).await;
    }
}
