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
/// Has zero knowledge of interiority state. It observes two signals:
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
    /// Mirror of the interiority clock's next_wake_at.
    next_wake_at: Option<Instant>,
}

/// Break-even point: if the gap to next wake exceeds this, pings cost more
/// than the cold-start savings.
const KEEPALIVE_BREAKEVEN: Duration = Duration::from_secs(18 * 3600); // 18h

/// Ping interval: 55 minutes — 5 minutes of headroom before 60-minute TTL.
const PING_INTERVAL: Duration = Duration::from_secs(55 * 60); // 55 min

impl CacheKeepalive {
    pub fn new() -> Self {
        Self {
            next_ping_at: None,
            next_wake_at: None,
        }
    }

    /// Called after ANY LLM call involving the cached prompt — user message,
    /// assistant response, interiority tick, or keepalive ping itself.
    /// Resets the internal ping deadline.
    pub fn on_cache_warmed(&mut self, now: Instant) {
        self.next_ping_at = Some(now + PING_INTERVAL);
    }

    /// Mirror of the interiority clock's schedule. Called whenever
    /// `next_wake_at` changes (including being cleared on guard trip).
    pub fn set_next_wake(&mut self, at: Option<Instant>) {
        self.next_wake_at = at;
        if at.is_none() {
            // Guard tripped — stop pinging.
            self.next_ping_at = None;
        }
    }

    /// Called by the autonomy loop on each ~30s tick.
    ///
    /// Returns `Ping` iff:
    /// 1. `next_ping_at` is set and `now >= next_ping_at`
    /// 2. `next_wake_at` is set
    /// 3. `next_wake_at - now < KEEPALIVE_BREAKEVEN`
    pub fn tick(&mut self, now: Instant) -> CacheKeepaliveAction {
        let ping_at = match self.next_ping_at {
            Some(t) if now >= t => t,
            _ => return CacheKeepaliveAction::None,
        };

        let wake_at = match self.next_wake_at {
            Some(t) => t,
            None => return CacheKeepaliveAction::None,
        };

        // Don't ping if the next wake is too far out — not worth the cost.
        if wake_at > now && wake_at.duration_since(now) >= KEEPALIVE_BREAKEVEN {
            self.next_ping_at = None;
            return CacheKeepaliveAction::None;
        }

        // Ping is due and economically justified. Reschedule.
        self.next_ping_at = Some(ping_at + PING_INTERVAL);
        CacheKeepaliveAction::Ping
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hours(h: u64) -> Duration {
        Duration::from_secs(h * 3600)
    }

    fn minutes(m: u64) -> Duration {
        Duration::from_secs(m * 60)
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

        // Not due yet at 58 minutes.
        assert_eq!(
            ka.tick(now + minutes(58)),
            CacheKeepaliveAction::None
        );

        // Due at 59 minutes.
        assert_eq!(
            ka.tick(now + minutes(59)),
            CacheKeepaliveAction::Ping
        );
    }

    #[test]
    fn ping_reschedules_after_firing() {
        let now = Instant::now();
        let mut ka = CacheKeepalive::new();

        ka.on_cache_warmed(now);
        ka.set_next_wake(Some(now + hours(4)));

        // First ping at 59min.
        assert_eq!(ka.tick(now + minutes(59)), CacheKeepaliveAction::Ping);
        // Not due at 59+58 = 117 minutes.
        assert_eq!(ka.tick(now + minutes(117)), CacheKeepaliveAction::None);
        // Due at 59+59 = 118 minutes.
        assert_eq!(ka.tick(now + minutes(118)), CacheKeepaliveAction::Ping);
    }

    #[test]
    fn no_ping_when_wake_exceeds_breakeven() {
        let now = Instant::now();
        let mut ka = CacheKeepalive::new();

        ka.on_cache_warmed(now);
        // Wake 30h out — beyond the 18h breakeven.
        ka.set_next_wake(Some(now + hours(30)));

        // Ping would be due at 59min, but the breakeven check kills it.
        assert_eq!(ka.tick(now + minutes(59)), CacheKeepaliveAction::None);
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

        // The old ping at 59min should NOT fire (deadline moved to 30+59=89).
        assert_eq!(ka.tick(now + minutes(59)), CacheKeepaliveAction::None);
        // Should fire at 89min.
        assert_eq!(ka.tick(now + minutes(89)), CacheKeepaliveAction::Ping);
    }
}
