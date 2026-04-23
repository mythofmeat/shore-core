pub mod activity;
pub mod heartbeat;
pub mod manager;

use std::collections::VecDeque;

use serde::Serialize;

/// Snapshot of autonomy subsystem state for the `status` command.
#[derive(Debug, Clone, Serialize)]
pub struct AutonomyStatus {
    /// Whether autonomy is paused.
    pub paused: bool,
    /// Current heartbeat state label.
    pub heartbeat_state: String,
    /// Consecutive heartbeat ticks without a user message.
    pub ticks_without_user: u32,
    /// Max idle ticks before going dormant.
    pub dormant_after_heartbeat_turns: u32,
    /// Effective heartbeat tick interval in seconds.
    pub effective_interval_secs: u64,
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
    /// A heartbeat tick fired.
    TickFired,
    /// Autonomous message was generated and sent.
    MessageSent,
    /// Character chose not to message the user.
    MessageSkipped,
    /// Tool use during heartbeat tick.
    ToolUse,
    /// Entered dormant state (max idle ticks reached).
    Dormant,
    /// Woke from dormant (user returned).
    Wake,
    /// Heartbeat tick was killed by the timeout guard.
    Timeout,
    /// Dormant bare ping sent to keep cache warm.
    DormantPing,
    /// A `<recap>` was captured from the tick and persisted to markdown daily notes.
    RecapWritten,
    /// Tick finished without a recap, even after the forced wrap-up call.
    RecapMissing,
}

impl std::fmt::Display for HeartbeatEventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TickFired => write!(f, "tick_fired"),
            Self::MessageSent => write!(f, "message_sent"),
            Self::MessageSkipped => write!(f, "message_skipped"),
            Self::ToolUse => write!(f, "tool_use"),
            Self::Dormant => write!(f, "dormant"),
            Self::Wake => write!(f, "wake"),
            Self::Timeout => write!(f, "timeout"),
            Self::DormantPing => write!(f, "dormant_ping"),
            Self::RecapWritten => write!(f, "recap_written"),
            Self::RecapMissing => write!(f, "recap_missing"),
        }
    }
}

/// Ring buffer of heartbeat events.
#[derive(Debug, Clone)]
pub struct HeartbeatLog {
    events: VecDeque<HeartbeatEvent>,
}

impl Default for HeartbeatLog {
    fn default() -> Self {
        Self::new()
    }
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
            timestamp: chrono::Local::now().to_rfc3339(),
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

// ---------------------------------------------------------------------------
// Cache TTL parsing (shared between handler and heartbeat)
// ---------------------------------------------------------------------------

/// Parse a `cache_ttl` duration string (e.g. `"1h"`, `"5m"`) into seconds.
pub fn parse_cache_ttl_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(h) = s.strip_suffix('h') {
        h.parse::<u64>().ok().map(|v| v * 3600)
    } else if let Some(m) = s.strip_suffix('m') {
        m.parse::<u64>().ok().map(|v| v * 60)
    } else {
        None
    }
}
