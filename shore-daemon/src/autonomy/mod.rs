pub mod activity;
pub mod cache_keepalive;
pub mod heartbeat;
pub mod time_parse;
pub mod timing;

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
