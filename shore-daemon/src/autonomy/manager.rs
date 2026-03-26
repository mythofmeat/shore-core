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
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::activity::{ActivityTracker, HourClassification};
use super::cache_keepalive::{
    CacheKeepaliveConfig, CacheKeepaliveScheduler, KeepaliveAction,
};
use super::heartbeat::{HeartbeatAction, HeartbeatScheduler, HeartbeatState};
use super::timing::{compute_tau, TauParams};
use super::AutonomyStatus;
use crate::config::app::AutonomyConfig;

// ---------------------------------------------------------------------------
// Per-character state
// ---------------------------------------------------------------------------

/// All autonomy state for a single character.
pub struct AutonomyState {
    pub heartbeat: HeartbeatScheduler,
    pub cache_keepalive: CacheKeepaliveScheduler,
    pub activity: ActivityTracker,
    /// Whether state has changed since last save.
    dirty: bool,
    /// Last message activity timestamp for compaction idle trigger.
    last_compaction_activity: Instant,
    /// Whether compaction was already triggered for this idle period.
    compaction_triggered: bool,
    /// Current number of messages in active.jsonl (updated on each message notification).
    active_message_count: usize,
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
        };
        (mgr, compaction_rx)
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
            dirty: false,
            last_compaction_activity: Instant::now(),
            compaction_triggered: false,
            active_message_count: 0,
        }));

        states.insert(character.to_string(), state.clone());

        // Spawn per-character tick task.
        let name = character.to_string();
        let config = self.config.clone();
        let data_dir = self.data_dir.clone();
        let shutdown_rx = self.shutdown_rx.clone();
        let compaction_tx = self.compaction_tx.clone();

        let handle = tokio::spawn(async move {
            character_tick_loop(name, state, config, data_dir, shutdown_rx, compaction_tx).await;
        });

        self.handles.lock().unwrap().push(handle);
    }

    // -- event notifications from the message handler -------------------------

    /// Call after a user message is appended.
    pub fn notify_user_message(&self, character: &str, message_count: usize) {
        let states = self.states.lock().unwrap();
        if let Some(state) = states.get(character) {
            let mut s = state.lock().unwrap();
            let now = Instant::now();
            s.heartbeat.on_user_message(now);
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

    /// Update the cache keepalive config for a character (e.g. on model switch).
    pub fn update_keepalive_config(&self, character: &str, config: CacheKeepaliveConfig) {
        let states = self.states.lock().unwrap();
        if let Some(state) = states.get(character) {
            let mut s = state.lock().unwrap();
            s.cache_keepalive.update_config(config);
        }
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
            social_need_bar: if dormant_threshold > 0 {
                state.heartbeat.unanswered_count() as f64 / dormant_threshold as f64
            } else {
                0.0
            },
            tau,
            cache_keepalive_state: format!("{:?}", state.cache_keepalive.state()),
            cache_keepalive_pings: state.cache_keepalive.ping_count(),
        })
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
                tick_character(&character, &state, &config, &data_dir, &compaction_tx);
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
fn tick_character(
    character: &str,
    state: &Arc<Mutex<AutonomyState>>,
    config: &AutonomyConfig,
    data_dir: &Path,
    compaction_tx: &mpsc::Sender<String>,
) {
    let now = Instant::now();
    let wall_now = Utc::now().naive_utc();
    let mut rng = rand::thread_rng();

    let mut s = state.lock().unwrap();

    // -- heartbeat --------------------------------------------------------
    if config.enabled {
        // Clone stats to release the mutable borrow on activity before
        // accessing heartbeat.
        let stats = s.activity.stats().clone();
        let tau_params = TauParams {
            reciprocated: s.heartbeat.unanswered_count() == 0,
            hour_class: current_hour_class(stats.hour_classifications),
            personality: config.personality,
        };
        let random_value: f64 = rng.gen();
        let hb_action = s.heartbeat.tick(&stats, &tau_params, now, wall_now, random_value);

        match hb_action {
            HeartbeatAction::None => {}
            HeartbeatAction::GenerateProbe {
                idle_secs,
                current_time,
            } => {
                info!(
                    character = %character,
                    idle_secs,
                    current_time = %current_time,
                    "Heartbeat: post-session probe triggered"
                );
                s.mark_dirty();
                // TODO: invoke LLM for probe, feed response to
                // heartbeat.handle_probe_response()
            }
            HeartbeatAction::GenerateDeferredMessage { reasoning } => {
                info!(
                    character = %character,
                    reasoning = %reasoning,
                    "Heartbeat: deferred message triggered"
                );
                s.mark_dirty();
                // TODO: invoke LLM to generate and push message
            }
            HeartbeatAction::GenerateSocialNeedMessage { anomaly_context } => {
                info!(
                    character = %character,
                    anomaly_context,
                    "Heartbeat: social-need message triggered"
                );
                s.mark_dirty();
                // TODO: invoke LLM to generate and push message
            }
        }
    }

    // -- cache keepalive --------------------------------------------------
    let ka_action = s.cache_keepalive.tick(now);
    match ka_action {
        KeepaliveAction::None => {}
        KeepaliveAction::SendPing => {
            info!(
                character = %character,
                ping_count = s.cache_keepalive.ping_count(),
                "Cache keepalive: ping requested"
            );
            s.mark_dirty();
            // TODO: send minimal API call (max_tokens=1) and feed
            // response to cache_keepalive.on_ping_response()
        }
        KeepaliveAction::EmitCacheWarning {
            expected_tokens,
            message,
        } => {
            info!(
                character = %character,
                expected_tokens,
                message = %message,
                "Cache keepalive: cache miss warning"
            );
            s.mark_dirty();
            // TODO: push CacheWarning to SWP clients
        }
    }

    // -- compaction triggers ---------------------------------------------
    if config.enabled && !s.compaction_triggered {
        let min_total = config.compaction.min_messages + config.compaction.keep_recent;

        // Force compaction when max_messages is reached.
        if config.compaction.max_messages > 0
            && s.active_message_count >= config.compaction.max_messages
            && s.active_message_count >= min_total
        {
            s.compaction_triggered = true;
            info!(
                character = %character,
                message_count = s.active_message_count,
                max_messages = config.compaction.max_messages,
                "Compaction: max messages trigger fired"
            );
            let _ = compaction_tx.try_send(character.to_string());
        }
        // Idle trigger: only if we have enough messages.
        else if s.active_message_count >= min_total {
            let idle_secs = now.duration_since(s.last_compaction_activity).as_secs();
            let threshold_secs = config.compaction.idle_trigger_minutes as u64 * 60;
            if threshold_secs > 0 && idle_secs >= threshold_secs {
                s.compaction_triggered = true;
                info!(
                    character = %character,
                    idle_secs,
                    threshold_secs,
                    message_count = s.active_message_count,
                    "Compaction: idle trigger fired"
                );
                let _ = compaction_tx.try_send(character.to_string());
            }
        }
    }

    // -- persist if dirty -------------------------------------------------
    save_state(data_dir, character, &mut s);
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
            dirty: true,
            last_compaction_activity: Instant::now(),
            compaction_triggered: false,
            active_message_count: 0,
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

    #[test]
    fn tick_character_runs_without_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config();
        let (compaction_tx, _compaction_rx) = mpsc::channel(16);
        let state = Arc::new(Mutex::new(AutonomyState {
            heartbeat: HeartbeatScheduler::new(),
            cache_keepalive: CacheKeepaliveScheduler::new(CacheKeepaliveConfig::default()),
            activity: ActivityTracker::new(),
            dirty: false,
            last_compaction_activity: Instant::now(),
            compaction_triggered: false,
            active_message_count: 0,
        }));

        // Need at least one message for heartbeat to have timestamps.
        {
            let mut s = state.lock().unwrap();
            s.heartbeat.on_user_message(Instant::now());
            s.activity.record_message();
        }

        tick_character("alice", &state, &config, tmp.path(), &compaction_tx);
    }
}
