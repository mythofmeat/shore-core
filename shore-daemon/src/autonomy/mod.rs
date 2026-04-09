pub mod activity;
pub mod cache_keepalive;
pub mod interiority;
pub mod manager;
pub mod recap_store;

use std::collections::VecDeque;

use serde::Serialize;

/// Snapshot of autonomy subsystem state for the `status` command.
#[derive(Debug, Clone, Serialize)]
pub struct AutonomyStatus {
    /// Whether autonomy is paused.
    pub paused: bool,
    /// Current interiority state label.
    pub interiority_state: String,
    /// Consecutive interiority ticks without a user message.
    pub ticks_without_user: u32,
    /// Max idle ticks before going dormant.
    pub dormant_after_interiority_turns: u32,
    /// Effective tick interval in seconds (min of interiority + cache refresh).
    pub effective_interval_secs: u64,
}

// ---------------------------------------------------------------------------
// Interiority event log
// ---------------------------------------------------------------------------

/// Maximum number of interiority events to keep in the ring buffer.
const INTERIORITY_LOG_CAPACITY: usize = 100;

/// A single interiority event recorded by the autonomy manager.
#[derive(Debug, Clone, Serialize)]
pub struct InteriorityEvent {
    /// ISO 8601 timestamp.
    pub timestamp: String,
    /// Event kind label.
    pub kind: InteriorityEventKind,
    /// Human-readable description.
    pub detail: String,
}

/// Categorised interiority event types.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InteriorityEventKind {
    /// An interiority tick fired.
    TickFired,
    /// Autonomous message was generated and sent.
    MessageSent,
    /// Character chose not to message the user.
    MessageSkipped,
    /// Tool use during interiority tick.
    ToolUse,
    /// Entered dormant state (max idle ticks reached).
    Dormant,
    /// Woke from dormant (user returned).
    Wake,
    /// Interiority tick was killed by the timeout guard.
    Timeout,
    /// Dormant bare ping sent to keep cache warm.
    DormantPing,
}

impl std::fmt::Display for InteriorityEventKind {
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
        }
    }
}

/// Ring buffer of interiority events.
#[derive(Debug, Clone)]
pub struct InteriorityLog {
    events: VecDeque<InteriorityEvent>,
}

impl Default for InteriorityLog {
    fn default() -> Self {
        Self::new()
    }
}

impl InteriorityLog {
    pub fn new() -> Self {
        Self {
            events: VecDeque::with_capacity(INTERIORITY_LOG_CAPACITY),
        }
    }

    pub fn push(&mut self, kind: InteriorityEventKind, detail: impl Into<String>) {
        if self.events.len() >= INTERIORITY_LOG_CAPACITY {
            self.events.pop_front();
        }
        self.events.push_back(InteriorityEvent {
            timestamp: chrono::Local::now().to_rfc3339(),
            kind,
            detail: detail.into(),
        });
    }

    /// Return recent events, most recent last. `limit` caps the count.
    pub fn recent(&self, limit: usize) -> Vec<&InteriorityEvent> {
        let start = self.events.len().saturating_sub(limit);
        self.events.range(start..).collect()
    }
}

// ---------------------------------------------------------------------------
// Cache TTL parsing (shared between handler and interiority)
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
