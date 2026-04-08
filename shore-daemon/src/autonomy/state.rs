//! Persistence and state types for per-character autonomy.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tracing::{debug, error, warn};

use super::activity::ActivityTracker;
use super::interiority::{InteriorityClock, InteriorityState};
use super::InteriorityLog;
use shore_llm_client::types::LlmRequest;

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
    pub(super) dirty: bool,
    /// Last message activity timestamp for compaction idle trigger.
    pub(super) last_compaction_activity: Instant,
    /// Whether compaction was already triggered for this idle period.
    pub(super) compaction_triggered: bool,
    /// Current number of messages in active.jsonl (updated on each message notification).
    pub(super) active_turn_count: usize,
    /// Set after compaction completes — signals the handler to reload the engine.
    pub(super) needs_engine_reload: bool,
    /// Cached last LLM request for interiority tick reuse.
    pub(super) last_request: Option<LlmRequest>,
}

impl AutonomyState {
    pub(super) fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

pub(super) const STATE_VERSION: u32 = 3;
pub(super) const STATE_FILENAME: &str = "autonomy_state.json";

#[derive(Serialize, Deserialize)]
pub(super) struct PersistedState {
    pub(super) version: u32,
    pub(super) interiority_state: String,
    pub(super) ticks_without_user: u32,
}

pub(super) fn state_path(data_dir: &Path, character: &str) -> PathBuf {
    data_dir.join(character).join(STATE_FILENAME)
}

pub(super) fn save_state(data_dir: &Path, character: &str, state: &mut AutonomyState) {
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

pub(super) fn load_state(data_dir: &Path, character: &str) -> Option<PersistedState> {
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

pub(super) fn restore_interiority(persisted: &PersistedState) -> (InteriorityState, u32) {
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

/// Lock the per-character autonomy state, recovering from mutex poisoning
/// instead of panicking. A poisoned mutex means a previous holder panicked,
/// but the state inside is still usable — letting the tick loop die would be
/// worse (no more keepalive, no more interiority, permanent silent failure).
pub(super) fn lock_state(m: &Mutex<AutonomyState>) -> std::sync::MutexGuard<'_, AutonomyState> {
    m.lock().unwrap_or_else(|poisoned| {
        error!("Autonomy state mutex was poisoned, recovering");
        poisoned.into_inner()
    })
}
