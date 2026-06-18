//! Per-character Anthropic cache warm/cold state machine.

use crate::convert::u64_to_i64;
use chrono::{DateTime, Utc};
use tracing::debug;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheState {
    Cold,
    Warm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anomaly {
    UnexpectedWrite,
    /// The cache was Warm, TTL expired (→ Cold), and the next call was NOT a
    /// keepalive — meaning the keepalive system failed to bridge the gap.
    KeepaliveMiss,
}

#[derive(Debug, Clone)]
pub struct Observation {
    pub ts: String,
    pub model: String,
    pub thinking_enabled: bool,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub call_type: String,
}

#[derive(Debug, Clone)]
pub struct ObservationResult {
    pub state: CacheState,
    pub anomaly: Option<Anomaly>,
}

#[derive(Debug)]
pub struct CacheTracker {
    state: CacheState,
    last_ts: Option<DateTime<Utc>>,
    last_model: Option<String>,
    last_thinking: Option<bool>,
    last_call_type: Option<String>,
    last_cache_read: u64,
    last_tool_loop_kind: Option<String>,
    last_tool_loop_cache_read: u64,
    ttl_secs: u64,
    /// Longest idle stretch the keepalive subsystem keeps pinging over before it
    /// deliberately stops (mirrors `[behavior.autonomy].cache_keepalive_max`,
    /// default 12h). A cold start after a gap longer than this is the keepalive
    /// ceiling working as designed, not a `KeepaliveMiss`.
    max_idle_secs: u64,
    /// True when the cache was Warm and just transitioned to Cold via TTL
    /// expiry. The next non-keepalive call in this state triggers a
    /// `KeepaliveMiss` anomaly — the keepalive system should have prevented
    /// the cold start.
    ttl_expired_since_warm: bool,
}

/// Default keepalive idle ceiling in seconds (12h), mirroring the
/// `[behavior.autonomy].cache_keepalive_max` config default. Past this gap the
/// keepalive subsystem stops pinging, so a cold start is expected.
const DEFAULT_MAX_IDLE_SECS: u64 = 12 * 3600;

impl Default for CacheTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl CacheTracker {
    pub fn new() -> Self {
        Self {
            state: CacheState::Cold,
            last_ts: None,
            last_model: None,
            last_thinking: None,
            last_call_type: None,
            last_cache_read: 0,
            last_tool_loop_kind: None,
            last_tool_loop_cache_read: 0,
            ttl_secs: 3600,
            max_idle_secs: DEFAULT_MAX_IDLE_SECS,
            ttl_expired_since_warm: false,
        }
    }

    pub fn with_ttl_secs(ttl: u64) -> Self {
        Self {
            ttl_secs: ttl,
            ..Self::new()
        }
    }

    pub fn state(&self) -> CacheState {
        self.state
    }

    pub fn last_cache_read(&self) -> u64 {
        self.last_cache_read
    }

    pub fn reconstruct(
        last_ts_str: &str,
        last_model: &str,
        last_thinking: bool,
        last_cache_read: u64,
        ttl_secs: u64,
    ) -> Self {
        let parsed = DateTime::parse_from_rfc3339(last_ts_str).map(|dt| dt.with_timezone(&Utc));

        let state = match &parsed {
            Ok(ts) => {
                let elapsed = Utc::now().signed_duration_since(*ts);
                if elapsed.num_seconds() < u64_to_i64(ttl_secs) && last_cache_read > 0 {
                    CacheState::Warm
                } else {
                    CacheState::Cold
                }
            }
            Err(_) => CacheState::Cold,
        };

        Self {
            state,
            last_ts: parsed.ok(),
            last_model: Some(last_model.to_owned()),
            last_thinking: Some(last_thinking),
            last_call_type: None,
            last_cache_read,
            last_tool_loop_kind: None,
            last_tool_loop_cache_read: 0,
            ttl_secs,
            max_idle_secs: DEFAULT_MAX_IDLE_SECS,
            ttl_expired_since_warm: false,
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "classifies every observation type to update cache state and anomaly tracking"
    )]
    pub fn observe(&mut self, obs: &Observation) -> ObservationResult {
        let obs_ts = DateTime::parse_from_rfc3339(&obs.ts)
            .map(|dt| dt.with_timezone(&Utc))
            .ok();

        // 1. Compaction always transitions to Cold (deliberate, not a keepalive failure)
        if obs.call_type == "compaction" {
            self.state = CacheState::Cold;
            self.last_cache_read = 0;
            self.clear_tool_loop_baseline();
            self.ttl_expired_since_warm = false;
            self.update_metadata(obs_ts, &obs.model, obs.thinking_enabled);
            self.last_call_type = Some(obs.call_type.clone());
            return ObservationResult {
                state: self.state,
                anomaly: None,
            };
        }

        // 1b. Heartbeat and tool-loop calls operate on derived prefixes, so
        // their reads are not comparable to the normal message baseline.
        // Tool loops still have an invariant of their own: within a single
        // loop the cacheable prefix should advance monotonically through
        // completed tool_result messages.
        // Only normal `message` calls share the cacheable prefix the warm/cold
        // baseline tracks. Keepalive pings, heartbeats, subagents, and memory
        // queries each run on a *different* prefix, so their cache_read is not
        // comparable to the message baseline — comparing them produced false
        // `unexpected_write`s (e.g. a healthy keepalive reading a slightly
        // shorter prefix, or a subagent's unrelated prompt). Tool loops keep
        // their own short-lived baseline, handled separately below.
        let tool_loop_kind = tool_loop_kind(&obs.call_type);
        let skip_normal_cache_read_comparison =
            obs.call_type != "message" && tool_loop_kind.is_none();

        // 2. TTL expiry: Warm → Cold
        if self.state == CacheState::Warm {
            if let (Some(last), Some(now)) = (self.last_ts, obs_ts) {
                let elapsed = now.signed_duration_since(last);
                if elapsed.num_seconds() > u64_to_i64(self.ttl_secs) {
                    self.state = CacheState::Cold;
                    self.last_cache_read = 0;
                    self.clear_tool_loop_baseline();
                    self.ttl_expired_since_warm = true;
                }
            }
        }

        // 3. Model change: Warm → Cold (deliberate, not a keepalive failure)
        if self.state == CacheState::Warm {
            if let Some(ref last_model) = self.last_model {
                if *last_model != obs.model {
                    self.state = CacheState::Cold;
                    self.last_cache_read = 0;
                    self.clear_tool_loop_baseline();
                    self.ttl_expired_since_warm = false;
                }
            }
        }

        // 4. Thinking toggle: Warm → Cold (deliberate, not a keepalive failure)
        if self.state == CacheState::Warm {
            if let Some(last_thinking) = self.last_thinking {
                if last_thinking != obs.thinking_enabled {
                    self.state = CacheState::Cold;
                    self.last_cache_read = 0;
                    self.clear_tool_loop_baseline();
                    self.ttl_expired_since_warm = false;
                }
            }
        }

        // 5. Evaluate against expected behavior
        let mut anomaly = match self.state {
            CacheState::Warm => self.observe_warm_cache(obs, tool_loop_kind),
            CacheState::Cold => {
                if obs.cache_read_tokens > 0 || obs.cache_write_tokens > 0 {
                    self.state = CacheState::Warm;
                }
                None
            }
        };

        // 5b. Keepalive miss detection: cache expired from Warm → Cold and the
        // next call is NOT a keepalive. This means the keepalive system failed
        // to bridge the gap — a cold start that should have been prevented.
        if self.ttl_expired_since_warm {
            self.ttl_expired_since_warm = false;
            if obs.call_type != "keepalive" {
                // A non-keepalive call is the first after TTL expiry. This is
                // only a miss if keepalive *should* have been pinging. Past the
                // idle ceiling the keepalive subsystem deliberately stops (user
                // presumed away), so a cold start beyond that gap is expected,
                // not a failure. With no usable timestamps we can't prove a
                // deliberate stop, so we keep the stricter behavior and flag.
                let within_keepalive_window = match (self.last_ts, obs_ts) {
                    (Some(last), Some(now)) => {
                        now.signed_duration_since(last).num_seconds()
                            <= u64_to_i64(self.max_idle_secs)
                    }
                    _ => true,
                };
                if anomaly.is_none() && within_keepalive_window {
                    anomaly = Some(Anomaly::KeepaliveMiss);
                }
            }
            // else: keepalive arrived (possibly late, but it tried). Not an anomaly.
        }

        // 6. Update internal state — only update cache_read baseline from
        // normal message calls, not heartbeat/tool_loop (different prefix).
        if let Some(kind) = tool_loop_kind {
            if anomaly.is_none() {
                self.last_tool_loop_kind = Some(kind.to_owned());
                self.last_tool_loop_cache_read = obs.cache_read_tokens;
            } else {
                self.clear_tool_loop_baseline();
            }
        } else if !skip_normal_cache_read_comparison {
            self.last_cache_read = obs.cache_read_tokens;
            self.clear_tool_loop_baseline();
        } else if obs.call_type == "heartbeat" {
            self.clear_tool_loop_baseline();
        } else {
            // Comparison skipped for a non-heartbeat call: leave the baseline
            // untouched.
        }
        self.update_metadata(obs_ts, &obs.model, obs.thinking_enabled);
        self.last_call_type = Some(obs.call_type.clone());

        debug!(
            call_type = obs.call_type,
            state = ?self.state,
            anomaly = ?anomaly,
            cache_read_tokens = obs.cache_read_tokens,
            cache_write_tokens = obs.cache_write_tokens,
            "Cache state observed"
        );

        ObservationResult {
            state: self.state,
            anomaly,
        }
    }

    fn update_metadata(&mut self, ts: Option<DateTime<Utc>>, model: &str, thinking: bool) {
        self.last_ts = ts;
        self.last_model = Some(model.to_owned());
        self.last_thinking = Some(thinking);
    }

    fn observe_warm_cache(
        &mut self,
        obs: &Observation,
        tool_loop_kind: Option<&'static str>,
    ) -> Option<Anomaly> {
        if let Some(kind) = tool_loop_kind {
            let continued_loop = self.last_tool_loop_kind.as_deref() == Some(kind)
                && self.last_call_type.as_deref() == Some(obs.call_type.as_str());
            let dropped_within_loop =
                continued_loop && obs.cache_read_tokens < self.last_tool_loop_cache_read;
            let cold_write_after_warm_message = !continued_loop
                && self.last_cache_read > 0
                && obs.cache_read_tokens == 0
                && obs.cache_write_tokens > 0;

            if dropped_within_loop || cold_write_after_warm_message {
                self.state = CacheState::Cold;
                self.last_cache_read = 0;
                return Some(Anomaly::UnexpectedWrite);
            }

            return None;
        }

        // Only `message` calls are comparable to the message baseline. Every
        // other call type reaching here (heartbeat, keepalive, subagent,
        // memory_query) runs a different prefix and must not fire an
        // `unexpected_write` from a read that looks "smaller" than the baseline.
        if obs.call_type != "message" || obs.cache_read_tokens >= self.last_cache_read {
            None
        } else {
            self.state = CacheState::Cold;
            self.last_cache_read = 0;
            Some(Anomaly::UnexpectedWrite)
        }
    }

    fn clear_tool_loop_baseline(&mut self) {
        self.last_tool_loop_kind = None;
        self.last_tool_loop_cache_read = 0;
    }
}

fn tool_loop_kind(call_type: &str) -> Option<&'static str> {
    match call_type {
        "tool_loop" => Some("tool_loop"),
        "heartbeat_tool_loop" => Some("heartbeat_tool_loop"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_cold() {
        let tracker = CacheTracker::new();
        assert_eq!(tracker.state(), CacheState::Cold);
    }

    #[test]
    fn cold_to_warm_on_cache_write() {
        let mut tracker = CacheTracker::new();
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        assert_eq!(tracker.state(), CacheState::Warm);
        assert!(result.anomaly.is_none());
    }

    #[test]
    fn warm_stays_warm_on_increasing_cache_read() {
        let mut tracker = CacheTracker::new();
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:30Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 500,
            cache_write_tokens: 50,
            call_type: "message".into(),
        });
        assert_eq!(tracker.state(), CacheState::Warm);
        assert!(result.anomaly.is_none());
    }

    #[test]
    fn warm_anomaly_on_cache_read_decrease() {
        let mut tracker = CacheTracker::new();
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:30Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 500,
            cache_write_tokens: 50,
            call_type: "message".into(),
        });
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:01:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 200,
            cache_write_tokens: 400,
            call_type: "message".into(),
        });
        assert_eq!(result.anomaly, Some(Anomaly::UnexpectedWrite));
    }

    #[test]
    fn cold_to_warm_on_cache_read() {
        let mut tracker = CacheTracker::new();
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 500,
            cache_write_tokens: 0,
            call_type: "message".into(),
        });
        assert!(result.anomaly.is_none());
        assert_eq!(
            tracker.state(),
            CacheState::Warm,
            "Cold + cache_read > 0 must transition to Warm"
        );
    }

    #[test]
    fn cold_to_warm_no_anomaly_on_subsequent_calls() {
        let mut tracker = CacheTracker::new();
        let r1 = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 500,
            cache_write_tokens: 100,
            call_type: "message".into(),
        });
        assert!(r1.anomaly.is_none());
        assert_eq!(tracker.state(), CacheState::Warm);

        let r2 = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:30Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 600,
            cache_write_tokens: 50,
            call_type: "message".into(),
        });
        assert!(r2.anomaly.is_none());
        assert_eq!(tracker.state(), CacheState::Warm);
    }

    #[test]
    fn compaction_transitions_to_cold() {
        let mut tracker = CacheTracker::new();
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        assert_eq!(tracker.state(), CacheState::Warm);
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:01:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 300,
            call_type: "compaction".into(),
        });
        assert_eq!(tracker.state(), CacheState::Cold);
        assert!(result.anomaly.is_none());
    }

    #[test]
    fn model_change_transitions_to_cold() {
        let mut tracker = CacheTracker::new();
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:01:00Z".into(),
            model: "claude-sonnet-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        // Model changed → cold → write → warm. No anomaly.
        assert_eq!(tracker.state(), CacheState::Warm);
        assert!(result.anomaly.is_none());
    }

    #[test]
    fn thinking_toggle_transitions_to_cold() {
        let mut tracker = CacheTracker::new();
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:01:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: false,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        assert_eq!(tracker.state(), CacheState::Warm);
        assert!(result.anomaly.is_none());
    }

    #[test]
    fn ttl_expiry_transitions_to_cold_with_keepalive_miss() {
        let mut tracker = CacheTracker::with_ttl_secs(60);
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        // 2 minutes later → TTL expired, non-keepalive call → keepalive miss
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:02:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        assert_eq!(tracker.state(), CacheState::Warm); // cold → write → warm
        assert_eq!(result.anomaly, Some(Anomaly::KeepaliveMiss));
    }

    #[test]
    fn reconstruct_warm_within_ttl() {
        let recent_ts = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let tracker = CacheTracker::reconstruct(&recent_ts, "claude-opus-4-6", true, 500, 3600);
        assert_eq!(tracker.state(), CacheState::Warm);
        assert_eq!(tracker.last_cache_read(), 500);
    }

    #[test]
    fn reconstruct_cold_when_no_cache_read() {
        let recent_ts = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let tracker = CacheTracker::reconstruct(&recent_ts, "claude-opus-4-6", true, 0, 3600);
        assert_eq!(tracker.state(), CacheState::Cold);
    }

    #[test]
    fn reconstruct_cold_when_ttl_expired() {
        let tracker =
            CacheTracker::reconstruct("2020-01-01T00:00:00Z", "claude-opus-4-6", true, 500, 3600);
        assert_eq!(tracker.state(), CacheState::Cold);
    }

    // -- keepalive miss detection -------------------------------------------

    #[test]
    fn keepalive_miss_when_ttl_expires_and_next_call_is_not_keepalive() {
        let mut tracker = CacheTracker::with_ttl_secs(60);
        // Warm up.
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        assert_eq!(tracker.state(), CacheState::Warm);

        // 2 minutes later — TTL expired. Next call is heartbeat, not keepalive.
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:02:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "heartbeat".into(),
        });
        assert_eq!(result.anomaly, Some(Anomaly::KeepaliveMiss));
    }

    #[test]
    fn no_keepalive_miss_when_keepalive_arrives_after_ttl() {
        let mut tracker = CacheTracker::with_ttl_secs(60);
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });

        // TTL expired, but next call IS a keepalive — system is working.
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:02:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "keepalive".into(),
        });
        assert!(result.anomaly.is_none());
    }

    #[test]
    fn no_keepalive_miss_on_compaction_cold() {
        let mut tracker = CacheTracker::with_ttl_secs(60);
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });

        // Compaction deliberately clears the cache — not a keepalive failure.
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:30Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 300,
            call_type: "compaction".into(),
        });

        // Next call after compaction should not flag keepalive miss.
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:02:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "heartbeat".into(),
        });
        assert!(result.anomaly.is_none());
    }

    #[test]
    fn no_keepalive_miss_when_cold_from_start() {
        // Tracker starts Cold — TTL never expired from Warm. No miss.
        let mut tracker = CacheTracker::with_ttl_secs(60);
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "heartbeat".into(),
        });
        assert!(result.anomaly.is_none());
    }

    /// When the tracker is Warm and observes an UnexpectedWrite (cache_read
    /// drops), it should transition to Cold — the cache was invalidated.
    /// Currently it stays Warm, causing subsequent observations to have
    /// incorrect state.
    #[test]
    fn unexpected_write_transitions_warm_to_cold() {
        let mut tracker = CacheTracker::new();
        // Warm up.
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        assert_eq!(tracker.state(), CacheState::Warm);

        // Now observe: cache_read dropped (cache was invalidated).
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 500,
            cache_write_tokens: 0,
            call_type: "message".into(),
        });
        // Set baseline: last_cache_read = 500.

        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:01:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 100, // dropped from 500 → UnexpectedWrite
            cache_write_tokens: 400,
            call_type: "message".into(),
        });

        assert_eq!(
            result.anomaly,
            Some(Anomaly::UnexpectedWrite),
            "Should detect UnexpectedWrite"
        );
        assert_eq!(
            tracker.state(),
            CacheState::Cold,
            "UnexpectedWrite should transition Warm → Cold"
        );
    }

    #[test]
    fn keepalive_with_smaller_read_is_not_unexpected_write() {
        // A keepalive ping runs on a slightly shorter prefix than the last
        // message, so its cache_read is legitimately below the message baseline.
        // That must not be flagged as an UnexpectedWrite.
        let mut tracker = CacheTracker::new();
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 42_400,
            cache_write_tokens: 0,
            call_type: "message".into(),
        });
        assert_eq!(tracker.state(), CacheState::Warm);

        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:10:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 42_368,
            cache_write_tokens: 4,
            call_type: "keepalive".into(),
        });
        assert!(result.anomaly.is_none());
        // The message baseline is untouched by the keepalive.
        assert_eq!(tracker.last_cache_read(), 42_400);
    }

    #[test]
    fn subagent_with_smaller_read_is_not_unexpected_write() {
        // A subagent runs an entirely separate prompt; its cache_read is not
        // comparable to the main message baseline.
        let mut tracker = CacheTracker::new();
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 40_000,
            cache_write_tokens: 0,
            call_type: "message".into(),
        });

        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:10Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 512,
            cache_write_tokens: 0,
            call_type: "subagent".into(),
        });
        assert!(result.anomaly.is_none());
        assert_eq!(tracker.last_cache_read(), 40_000);
    }

    #[test]
    fn no_keepalive_miss_when_idle_exceeds_ceiling() {
        // Gap longer than max_idle: the keepalive subsystem deliberately stopped
        // pinging, so the cold start is expected, not a miss.
        let mut tracker = CacheTracker::with_ttl_secs(3600);
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T00:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        assert_eq!(tracker.state(), CacheState::Warm);

        // 13h later — past the 12h ceiling. A cold message is expected.
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T13:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 12_653,
            call_type: "message".into(),
        });
        assert!(result.anomaly.is_none());
    }

    #[test]
    fn keepalive_miss_still_fires_within_ceiling() {
        // Gap past the TTL but within the keepalive ceiling: keepalive should
        // have pinged and didn't → still a genuine miss.
        let mut tracker = CacheTracker::with_ttl_secs(3600);
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T00:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });

        let result = tracker.observe(&Observation {
            ts: "2026-04-05T02:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 10_783,
            call_type: "message".into(),
        });
        assert_eq!(result.anomaly, Some(Anomaly::KeepaliveMiss));
    }

    #[test]
    fn first_tool_loop_zero_read_after_warm_message_is_unexpected_write() {
        let mut tracker = CacheTracker::new();
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 20_000,
            cache_write_tokens: 0,
            call_type: "message".into(),
        });
        assert_eq!(tracker.state(), CacheState::Warm);

        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:10Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 21_000,
            call_type: "tool_loop".into(),
        });

        assert_eq!(result.anomaly, Some(Anomaly::UnexpectedWrite));
        assert_eq!(tracker.state(), CacheState::Cold);
    }

    #[test]
    fn consecutive_tool_loop_cache_drop_is_unexpected_write() {
        let mut tracker = CacheTracker::new();
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 20_000,
            cache_write_tokens: 0,
            call_type: "message".into(),
        });
        assert_eq!(tracker.state(), CacheState::Warm);

        let first_loop = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:10Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 20_500,
            cache_write_tokens: 500,
            call_type: "tool_loop".into(),
        });
        assert!(first_loop.anomaly.is_none());

        let second_loop = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:20Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 8_000,
            cache_write_tokens: 12_000,
            call_type: "tool_loop".into(),
        });

        assert_eq!(second_loop.anomaly, Some(Anomaly::UnexpectedWrite));
        assert_eq!(tracker.state(), CacheState::Cold);
    }

    #[test]
    fn tool_loop_does_not_replace_normal_message_baseline() {
        let mut tracker = CacheTracker::new();
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 20_000,
            cache_write_tokens: 0,
            call_type: "message".into(),
        });
        _ = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:10Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 20_500,
            cache_write_tokens: 500,
            call_type: "tool_loop".into(),
        });

        let final_message = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:30Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 20_100,
            cache_write_tokens: 0,
            call_type: "message".into(),
        });

        assert!(
            final_message.anomaly.is_none(),
            "normal message comparison should use the pre-loop message baseline"
        );
        assert_eq!(tracker.last_cache_read(), 20_100);
    }
}
