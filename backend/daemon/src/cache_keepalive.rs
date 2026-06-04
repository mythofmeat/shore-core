use std::time::Duration;
use tokio::time::Instant;

/// Action returned by [`CacheKeepalive::tick`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheKeepaliveAction {
    /// No action needed.
    None,
    /// Fire a bare keepalive ping to refresh the prompt cache.
    Ping,
}

/// Standalone subsystem that keeps the Anthropic 1h prompt cache warm during
/// quiet stretches — but only when doing so is economically justified.
///
/// Has zero knowledge of autonomy/heartbeat state. It observes two signals:
/// - `on_cache_warmed`: any LLM call that touches the cached prompt
/// - `set_next_wake`: the character's next scheduled wake time
///
/// **The math (1h Anthropic tier):**
/// Cache write = 2.0× input, cache read = 0.1×. Cold-wake penalty = 1.9N,
/// one keepalive ping = 0.1N. Break-even at ~19 pings → 19 hours.
/// We use 18h as the threshold (slight headroom).
#[derive(Debug)]
pub struct CacheKeepalive {
    /// Next time a keepalive ping should fire.
    next_ping_at: Option<Instant>,
    /// The next scheduled wake, supplied by the heartbeat subsystem.
    next_wake_at: Option<Instant>,
    /// Consecutive failed ping attempts. Used for retry backoff.
    failure_count: u32,
}

/// Break-even point: if the gap to next wake exceeds this, pings cost more
/// than the cold-start savings.
const KEEPALIVE_BREAKEVEN: Duration = Duration::from_hours(18);
const DEFAULT_KEEPALIVE_INTERVAL: Duration = Duration::from_mins(55);

/// Ping interval: 55 minutes — 5 minutes of headroom before the 60-minute
/// cache TTL expires. If the first attempt fails, retries use short
/// exponential backoff so transient errors get another chance without turning
/// provider/budget outages into a 10-second failure loop.
/// Economics: one early ping costs ~0.1N tokens; a cache miss costs ~1.9N.
/// The 5-minute insurance is worth ~$0.01 to avoid a ~$0.20 cold-start.
///
/// Override with `SHORE_KEEPALIVE_INTERVAL_SECS` env var for testing.
fn ping_interval() -> Duration {
    match std::env::var("SHORE_KEEPALIVE_INTERVAL_SECS") {
        Ok(s) => {
            let secs = s.parse().unwrap_or(DEFAULT_KEEPALIVE_INTERVAL.as_secs());
            Duration::from_secs(secs)
        }
        Err(_) => DEFAULT_KEEPALIVE_INTERVAL,
    }
}

fn retry_delay(failure_count: u32) -> Duration {
    let exponent = failure_count.saturating_sub(1).min(5);
    let secs = 30_u64.saturating_mul(1_u64 << exponent);
    Duration::from_secs(secs.min(15 * 60))
}

impl CacheKeepalive {
    pub fn new() -> Self {
        Self {
            next_ping_at: None,
            next_wake_at: None,
            failure_count: 0,
        }
    }

    /// Called after ANY LLM call involving the cached prompt — user message,
    /// assistant response, heartbeat tick, or keepalive ping itself.
    /// Resets the internal ping deadline.
    pub fn on_cache_warmed(&mut self, now: Instant) {
        self.next_ping_at = now.checked_add(ping_interval());
        self.failure_count = 0;
    }

    /// Called when the cached prompt prefix is known to be unusable.
    ///
    /// Ordinary compaction should not call this: the conversation tail changes,
    /// but stable pinned system sections are still worth keeping warm.
    pub fn on_cache_invalidated(&mut self) {
        self.next_ping_at = None;
        self.failure_count = 0;
    }

    /// Mirror of the scheduled heartbeat wake. Called whenever
    /// `next_wake_at` changes (including being cleared on guard trip).
    pub fn set_next_wake(&mut self, at: Option<Instant>) {
        self.next_wake_at = at;
        if at.is_none() {
            // Guard tripped — stop pinging.
            self.next_ping_at = None;
            self.failure_count = 0;
        }
    }

    /// Called by the autonomy loop on each ~30s tick.
    ///
    /// Returns `Ping` iff:
    /// 1. `next_ping_at` is set and `now >= next_ping_at`
    /// 2. `next_wake_at` is set
    /// 3. `next_wake_at - now < KEEPALIVE_BREAKEVEN`
    ///
    /// Does NOT advance `next_ping_at` — the caller must call
    /// `on_cache_warmed` after a successful ping, or `on_ping_failed`
    /// to schedule a short retry backoff.
    pub fn tick(&mut self, now: Instant) -> CacheKeepaliveAction {
        let _ping_at = match self.next_ping_at {
            Some(t) if now >= t => t,
            _ => return CacheKeepaliveAction::None,
        };

        let Some(wake_at) = self.next_wake_at else {
            return CacheKeepaliveAction::None;
        };

        // Don't ping if the next wake is too far out — not worth the cost.
        if wake_at > now && wake_at.duration_since(now) >= KEEPALIVE_BREAKEVEN {
            self.next_ping_at = None;
            return CacheKeepaliveAction::None;
        }

        // Ping is due and economically justified.
        // Do NOT advance next_ping_at here — caller must confirm the ping
        // actually succeeded via on_cache_warmed(), or call on_ping_failed()
        // to schedule a short retry backoff.
        CacheKeepaliveAction::Ping
    }

    /// Called when a keepalive ping fails or is skipped. Retries with a short
    /// exponential backoff so transient failures still get another chance
    /// before the cache goes cold, while budget/provider outages don't hammer
    /// the account every scheduler tick.
    pub fn on_ping_failed(&mut self, now: Instant) {
        self.failure_count = self.failure_count.saturating_add(1);
        self.next_ping_at = now.checked_add(retry_delay(self.failure_count));
    }
}

impl Default for CacheKeepalive {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hours(h: u64) -> Duration {
        Duration::from_secs(h.saturating_mul(3600))
    }

    fn minutes(m: u64) -> Duration {
        Duration::from_secs(m.saturating_mul(60))
    }

    #[test]
    fn new_returns_no_action() {
        let mut ka = CacheKeepalive::new();
        assert_eq!(ka.tick(Instant::now()), CacheKeepaliveAction::None);
    }

    #[test]
    fn ping_fires_after_interval() {
        let now = Instant::now();
        let mut ka = CacheKeepalive::new();

        // Warm the cache and set a wake 4h out.
        ka.on_cache_warmed(now);
        ka.set_next_wake(Some(now + hours(4)));

        // Not due yet at 54 minutes (interval is 55min).
        assert_eq!(ka.tick(now + minutes(54)), CacheKeepaliveAction::None);

        // Due at 55 minutes.
        assert_eq!(ka.tick(now + minutes(55)), CacheKeepaliveAction::Ping);
    }

    #[test]
    fn ping_reschedules_after_caller_confirms() {
        let now = Instant::now();
        let mut ka = CacheKeepalive::new();

        ka.on_cache_warmed(now);
        ka.set_next_wake(Some(now + hours(4)));

        // First ping at 55min.
        assert_eq!(ka.tick(now + minutes(55)), CacheKeepaliveAction::Ping);
        // Caller confirms the ping succeeded.
        ka.on_cache_warmed(now + minutes(55));

        // Not due at 55+54 = 109 minutes.
        assert_eq!(ka.tick(now + minutes(109)), CacheKeepaliveAction::None);
        // Due at 55+55 = 110 minutes.
        assert_eq!(ka.tick(now + minutes(110)), CacheKeepaliveAction::Ping);
    }

    #[test]
    fn ping_retries_when_not_confirmed() {
        let now = Instant::now();
        let mut ka = CacheKeepalive::new();

        ka.on_cache_warmed(now);
        ka.set_next_wake(Some(now + hours(4)));

        // Ping fires at 55min.
        assert_eq!(ka.tick(now + minutes(55)), CacheKeepaliveAction::Ping);
        // Caller does NOT confirm (ping failed/skipped).
        // Retry is delayed briefly rather than spinning every scheduler tick.
        ka.on_ping_failed(now + minutes(55));
        assert_eq!(
            ka.tick(now + minutes(55) + Duration::from_secs(29)),
            CacheKeepaliveAction::None
        );
        assert_eq!(
            ka.tick(now + minutes(55) + Duration::from_secs(30)),
            CacheKeepaliveAction::Ping
        );
    }

    #[test]
    fn no_ping_when_wake_exceeds_breakeven() {
        let now = Instant::now();
        let mut ka = CacheKeepalive::new();

        ka.on_cache_warmed(now);
        // Wake 30h out — beyond the 18h breakeven.
        ka.set_next_wake(Some(now + hours(30)));

        // Ping would be due at 55min, but the breakeven check kills it.
        assert_eq!(ka.tick(now + minutes(55)), CacheKeepaliveAction::None);
        // next_ping_at was cleared, so future ticks also return None.
        assert_eq!(ka.tick(now + hours(2)), CacheKeepaliveAction::None);
    }

    #[test]
    fn no_ping_when_no_wake_set() {
        let now = Instant::now();
        let mut ka = CacheKeepalive::new();

        ka.on_cache_warmed(now);
        // No set_next_wake — should not ping.
        assert_eq!(ka.tick(now + hours(1)), CacheKeepaliveAction::None);
    }

    #[test]
    fn guard_trip_clears_pings() {
        let now = Instant::now();
        let mut ka = CacheKeepalive::new();

        ka.on_cache_warmed(now);
        ka.set_next_wake(Some(now + hours(4)));

        // Guard trips — clock clears next_wake_at.
        ka.set_next_wake(None);

        // Even though ping_at is due, no wake means no ping.
        assert_eq!(ka.tick(now + hours(1)), CacheKeepaliveAction::None);
    }

    #[test]
    fn cache_warm_resets_ping_deadline() {
        let now = Instant::now();
        let mut ka = CacheKeepalive::new();

        ka.on_cache_warmed(now);
        ka.set_next_wake(Some(now + hours(4)));

        // Simulate a user message warming the cache at 30min.
        ka.on_cache_warmed(now + minutes(30));

        // The old ping at 55min should NOT fire (deadline moved to 30+55=85).
        assert_eq!(ka.tick(now + minutes(55)), CacheKeepaliveAction::None);
        // Should fire at 85min.
        assert_eq!(ka.tick(now + minutes(85)), CacheKeepaliveAction::Ping);
    }

    #[test]
    fn no_ping_when_wake_exceeds_breakeven_after_retry() {
        // Regression: after tick() stopped advancing next_ping_at,
        // the breakeven check must still clear it.
        let now = Instant::now();
        let mut ka = CacheKeepalive::new();

        ka.on_cache_warmed(now);
        ka.set_next_wake(Some(now + hours(30)));

        assert_eq!(ka.tick(now + minutes(55)), CacheKeepaliveAction::None);
        // next_ping_at was cleared by the breakeven check.
        assert_eq!(ka.tick(now + minutes(56)), CacheKeepaliveAction::None);
    }

    #[test]
    fn compaction_pauses_and_warm_resumes() {
        let now = Instant::now();
        let mut ka = CacheKeepalive::new();

        ka.on_cache_warmed(now);
        ka.set_next_wake(Some(now + hours(4)));

        // Compaction completes — invalidates the cached prefix.
        ka.on_cache_invalidated();

        // Ping would be due at 55min, but invalidation cleared it.
        assert_eq!(ka.tick(now + minutes(55)), CacheKeepaliveAction::None);

        // Next real LLM call warms the cache — pings resume.
        ka.on_cache_warmed(now + hours(1));
        assert_eq!(
            ka.tick(now + hours(1) + minutes(55)),
            CacheKeepaliveAction::Ping
        );
    }
}
