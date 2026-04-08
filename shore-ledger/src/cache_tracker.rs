//! Per-character Anthropic cache warm/cold state machine.

use chrono::{DateTime, Utc};
use tracing::debug;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheState {
    Cold,
    Warm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anomaly {
    UnexpectedRead,
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
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub call_type: String,
}

#[derive(Debug, Clone)]
pub struct ObservationResult {
    pub state: CacheState,
    pub anomaly: Option<Anomaly>,
}

pub struct CacheTracker {
    state: CacheState,
    last_ts: Option<DateTime<Utc>>,
    last_model: Option<String>,
    last_thinking: Option<bool>,
    last_cache_read: u32,
    ttl_secs: u64,
    /// True when the cache was Warm and just transitioned to Cold via TTL
    /// expiry. The next non-keepalive call in this state triggers a
    /// `KeepaliveMiss` anomaly — the keepalive system should have prevented
    /// the cold start.
    ttl_expired_since_warm: bool,
}

impl CacheTracker {
    pub fn new() -> Self {
        Self {
            state: CacheState::Cold,
            last_ts: None,
            last_model: None,
            last_thinking: None,
            last_cache_read: 0,
            ttl_secs: 3600,
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

    pub fn last_cache_read(&self) -> u32 {
        self.last_cache_read
    }

    pub fn reconstruct(
        last_ts_str: &str,
        last_model: &str,
        last_thinking: bool,
        last_cache_read: u32,
        ttl_secs: u64,
    ) -> Self {
        let parsed = DateTime::parse_from_rfc3339(last_ts_str)
            .map(|dt| dt.with_timezone(&Utc));

        let state = match &parsed {
            Ok(ts) => {
                let elapsed = Utc::now().signed_duration_since(*ts);
                if elapsed.num_seconds() < ttl_secs as i64 && last_cache_read > 0 {
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
            last_model: Some(last_model.to_string()),
            last_thinking: Some(last_thinking),
            last_cache_read,
            ttl_secs,
            ttl_expired_since_warm: false,
        }
    }

    pub fn observe(&mut self, obs: &Observation) -> ObservationResult {
        let obs_ts = DateTime::parse_from_rfc3339(&obs.ts)
            .map(|dt| dt.with_timezone(&Utc))
            .ok();

        // 1. Compaction always transitions to Cold (deliberate, not a keepalive failure)
        if obs.call_type == "compaction" {
            self.state = CacheState::Cold;
            self.last_cache_read = 0;
            self.ttl_expired_since_warm = false;
            self.update_metadata(obs_ts, &obs.model, obs.thinking_enabled);
            return ObservationResult {
                state: self.state,
                anomaly: None,
            };
        }

        // 2. TTL expiry: Warm → Cold
        if self.state == CacheState::Warm {
            if let (Some(last), Some(now)) = (self.last_ts, obs_ts) {
                let elapsed = now.signed_duration_since(last);
                if elapsed.num_seconds() > self.ttl_secs as i64 {
                    self.state = CacheState::Cold;
                    self.last_cache_read = 0;
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
                    self.ttl_expired_since_warm = false;
                }
            }
        }

        // 5. Evaluate against expected behavior
        let mut anomaly = match self.state {
            CacheState::Warm => {
                if obs.cache_read_tokens >= self.last_cache_read {
                    None // OK, stay Warm
                } else {
                    Some(Anomaly::UnexpectedWrite)
                }
            }
            CacheState::Cold => {
                if obs.cache_read_tokens > 0 {
                    Some(Anomaly::UnexpectedRead)
                } else if obs.cache_write_tokens > 0 {
                    self.state = CacheState::Warm;
                    None
                } else {
                    None // Cold, no read, no write — stay Cold
                }
            }
        };

        // 5b. Keepalive miss detection: cache expired from Warm → Cold and the
        // next call is NOT a keepalive. This means the keepalive system failed
        // to bridge the gap — a cold start that should have been prevented.
        if self.ttl_expired_since_warm {
            if obs.call_type == "keepalive" {
                // Keepalive arrived (possibly late, but it tried). Not an anomaly.
                self.ttl_expired_since_warm = false;
            } else {
                // A non-keepalive call is the first after TTL expiry → keepalive missed.
                if anomaly.is_none() {
                    anomaly = Some(Anomaly::KeepaliveMiss);
                }
                self.ttl_expired_since_warm = false;
            }
        }

        // 6. Update internal state
        self.last_cache_read = obs.cache_read_tokens;
        self.update_metadata(obs_ts, &obs.model, obs.thinking_enabled);

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

    fn update_metadata(
        &mut self,
        ts: Option<DateTime<Utc>>,
        model: &str,
        thinking: bool,
    ) {
        self.last_ts = ts;
        self.last_model = Some(model.to_string());
        self.last_thinking = Some(thinking);
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
        tracker.observe(&Observation {
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
        tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        tracker.observe(&Observation {
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
    fn cold_anomaly_on_unexpected_cache_read() {
        let mut tracker = CacheTracker::new();
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 500,
            cache_write_tokens: 0,
            call_type: "message".into(),
        });
        assert_eq!(result.anomaly, Some(Anomaly::UnexpectedRead));
    }

    #[test]
    fn compaction_transitions_to_cold() {
        let mut tracker = CacheTracker::new();
        tracker.observe(&Observation {
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
        tracker.observe(&Observation {
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
        tracker.observe(&Observation {
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
        tracker.observe(&Observation {
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
        let recent_ts = Utc::now()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let tracker = CacheTracker::reconstruct(
            &recent_ts,
            "claude-opus-4-6",
            true,
            500,
            3600,
        );
        assert_eq!(tracker.state(), CacheState::Warm);
        assert_eq!(tracker.last_cache_read(), 500);
    }

    #[test]
    fn reconstruct_cold_when_no_cache_read() {
        let recent_ts = Utc::now()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let tracker = CacheTracker::reconstruct(
            &recent_ts,
            "claude-opus-4-6",
            true,
            0,
            3600,
        );
        assert_eq!(tracker.state(), CacheState::Cold);
    }

    #[test]
    fn reconstruct_cold_when_ttl_expired() {
        let tracker = CacheTracker::reconstruct(
            "2020-01-01T00:00:00Z",
            "claude-opus-4-6",
            true,
            500,
            3600,
        );
        assert_eq!(tracker.state(), CacheState::Cold);
    }

    // -- keepalive miss detection -------------------------------------------

    #[test]
    fn keepalive_miss_when_ttl_expires_and_next_call_is_not_keepalive() {
        let mut tracker = CacheTracker::with_ttl_secs(60);
        // Warm up.
        tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        assert_eq!(tracker.state(), CacheState::Warm);

        // 2 minutes later — TTL expired. Next call is interiority, not keepalive.
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:02:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "interiority".into(),
        });
        assert_eq!(result.anomaly, Some(Anomaly::KeepaliveMiss));
    }

    #[test]
    fn no_keepalive_miss_when_keepalive_arrives_after_ttl() {
        let mut tracker = CacheTracker::with_ttl_secs(60);
        tracker.observe(&Observation {
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
        tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });

        // Compaction deliberately clears the cache — not a keepalive failure.
        tracker.observe(&Observation {
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
            call_type: "interiority".into(),
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
            call_type: "interiority".into(),
        });
        assert!(result.anomaly.is_none());
    }
}
