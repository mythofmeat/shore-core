//! AutonomyManager — per-character scheduler state with background tick tasks.
//!
//! Each character gets its own tokio task that ticks the interiority clock on a
//! fixed interval. Interiority ticks double as cache refresh (unified timer).
//! State is persisted to `{data_dir}/{character}/autonomy_state.json`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use dashmap::DashMap;

use shore_protocol::server_msg::ServerMessage;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::activity::ActivityTracker;
use super::interiority::{InteriorityClock, InteriorityState};
use super::state::{load_state, lock_state, restore_interiority, AutonomyState};
use super::tick::{character_tick_loop, TickContext};
use super::{AutonomyStatus, InteriorityEventKind, InteriorityLog};
use crate::engine::ConversationEngine;
use crate::notifications::NotificationService;
use shore_config::app::{AutonomyConfig, CompactionConfig};
use shore_config::LoadedConfig;
use shore_ledger::LedgerClient;
use shore_llm_client::types::LlmRequest;

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
        self.ensure_state_with_config(character, cache_ttl_secs, None, None)
    }

    /// Like `ensure_state`, but accepts an optional per-character effective config
    /// that overrides the global config for model resolution and autonomy settings.
    /// `engine` must be provided for autonomous messages to be persisted safely
    /// through the engine lock rather than via raw file appends.
    pub fn ensure_state_with_config(
        &self,
        character: &str,
        cache_ttl_secs: Option<u64>,
        effective_config: Option<&LoadedConfig>,
        engine: Option<Arc<tokio::sync::Mutex<ConversationEngine>>>,
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
            engine,
            db: std::sync::Mutex::new(None),
            vs: std::sync::Mutex::new(None),
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use super::super::state::{
        save_state, load_state, state_path, restore_interiority,
        PersistedState, STATE_VERSION,
    };
    use super::super::tick::TickContext;

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
        use super::super::tick;

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
            engine: None,
            db: std::sync::Mutex::new(None),
            vs: std::sync::Mutex::new(None),
        };
        tick::tick_character_for_test("alice", &tick_ctx).await;
    }

    #[test]
    fn extract_send_message_parses() {
        use super::super::tick;

        assert_eq!(
            tick::extract_send_message_for_test("thinking...<sendMessage>Hey there!</sendMessage>...done"),
            Some("Hey there!".into())
        );
        assert_eq!(tick::extract_send_message_for_test("no tags here"), None);
        assert_eq!(tick::extract_send_message_for_test("<sendMessage></sendMessage>"), None);
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
