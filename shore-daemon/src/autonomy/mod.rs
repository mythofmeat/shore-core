pub mod activity;
pub mod cache_keepalive;
pub mod heartbeat;
pub mod manager;
pub mod time_parse;
pub mod timing;

use std::collections::VecDeque;

use serde::Serialize;

/// Snapshot of autonomy subsystem state for the `status` command.
#[derive(Debug, Clone, Serialize)]
pub struct AutonomyStatus {
    /// Whether autonomy is paused.
    pub paused: bool,
    /// Current heartbeat state label.
    pub heartbeat_state: String,
    /// Consecutive unanswered autonomous messages.
    pub unanswered_count: u32,
    /// Dormant threshold (max unanswered before going dormant).
    pub dormant_threshold: u32,
    /// Social need bar level (0.0–1.0): unanswered / dormant_threshold.
    pub social_need_bar: f64,
    /// Current τ value (seconds) for social-need probability rolls.
    pub tau: f64,
    /// Current cache keepalive state label.
    pub cache_keepalive_state: String,
    /// Number of cache keepalive pings sent.
    pub cache_keepalive_pings: u32,
}

// ---------------------------------------------------------------------------
// Heartbeat event log
// ---------------------------------------------------------------------------

/// Maximum number of heartbeat events to keep in the ring buffer.
const HEARTBEAT_LOG_CAPACITY: usize = 100;

/// A single heartbeat event recorded by the autonomy manager.
#[derive(Debug, Clone, Serialize)]
pub struct HeartbeatEvent {
    /// ISO 8601 timestamp.
    pub timestamp: String,
    /// Event kind label.
    pub kind: HeartbeatEventKind,
    /// Human-readable description.
    pub detail: String,
}

/// Categorised heartbeat event types.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatEventKind {
    /// State machine transitioned to a new state.
    StateChange,
    /// Post-session probe was triggered.
    ProbeTrigger,
    /// Post-session probe result (deferred / declined).
    ProbeResult,
    /// Deferred timer fired.
    DeferredFire,
    /// Social-need roll triggered a message.
    SocialNeedFire,
    /// Autonomous message was generated and sent.
    MessageSent,
    /// Character chose not to respond.
    MessageSkipped,
    /// Entered dormant state (unanswered threshold reached).
    Dormant,
    /// Woke from dormant (user returned).
    Wake,
}

impl std::fmt::Display for HeartbeatEventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StateChange => write!(f, "state_change"),
            Self::ProbeTrigger => write!(f, "probe_trigger"),
            Self::ProbeResult => write!(f, "probe_result"),
            Self::DeferredFire => write!(f, "deferred_fire"),
            Self::SocialNeedFire => write!(f, "social_need_fire"),
            Self::MessageSent => write!(f, "message_sent"),
            Self::MessageSkipped => write!(f, "message_skipped"),
            Self::Dormant => write!(f, "dormant"),
            Self::Wake => write!(f, "wake"),
        }
    }
}

/// Ring buffer of heartbeat events.
#[derive(Debug, Clone)]
pub struct HeartbeatLog {
    events: VecDeque<HeartbeatEvent>,
}

impl HeartbeatLog {
    pub fn new() -> Self {
        Self {
            events: VecDeque::with_capacity(HEARTBEAT_LOG_CAPACITY),
        }
    }

    pub fn push(&mut self, kind: HeartbeatEventKind, detail: impl Into<String>) {
        if self.events.len() >= HEARTBEAT_LOG_CAPACITY {
            self.events.pop_front();
        }
        self.events.push_back(HeartbeatEvent {
            timestamp: chrono::Utc::now().to_rfc3339(),
            kind,
            detail: detail.into(),
        });
    }

    /// Return recent events, most recent last. `limit` caps the count.
    pub fn recent(&self, limit: usize) -> Vec<&HeartbeatEvent> {
        let start = self.events.len().saturating_sub(limit);
        self.events.range(start..).collect()
    }
}
