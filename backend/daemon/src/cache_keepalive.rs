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

/// Standalone subsystem that keeps a model's prompt cache warm during quiet
/// stretches. It is deliberately decoupled from the heartbeat: it does **not**
/// observe the next scheduled wake, the dormancy guard, or any heartbeat config.
/// It observes exactly three things:
///
/// - `set_interval`: the active model's `cache_keepalive` cadence (`None` = off).
/// - `on_cache_warmed`: a *real* LLM call that ran on the **same model** whose
///   cache we keep warm (a foreground reply, or a heartbeat/background tick that
///   happens to use that model) — resets both the ping timer and the idle
///   clock. A call on a *different* model (e.g. a heartbeat pinned to a cheap
///   background model) does NOT warm this model's prompt cache, so it is
///   ignored: counting it would push the ping out while the real cache silently
///   expires, turning every ping into a full cache recreation.
/// - `on_cache_invalidated`: the cached prefix is known unusable (e.g. the
///   model switched and its prefix is cold).
///
/// **Two independent knobs govern it:**
/// - **interval** (per-model `cache_keepalive`): how *often* to ping. Anthropic
///   defaults to `55m`; every other sdk defaults to off. The interval is a
///   literal cadence, unrelated to the Anthropic-only `cache_ttl` wire setting.
/// - **max_idle** (global `[behavior.autonomy].cache_keepalive_max`, default
///   12h): the longest stretch *since the last real activity* over which we keep
///   pinging. Once it elapses, pinging stops until the user returns. This is the
///   user-presence / cost ceiling — keyed to the last real message, NOT to the
///   last ping (otherwise each ping would reset the clock and it would never
///   expire).
#[derive(Debug)]
pub struct CacheKeepalive {
    /// Active model's ping cadence. `None` means keepalive is off.
    interval: Option<Duration>,
    /// The model whose prompt cache this keepalive keeps warm — i.e. the model
    /// the ping itself runs on (the foreground chat model). Set from each cached
    /// request via [`set_interval`]. A warm only counts when it ran on this same
    /// model; a background tick on a different model does not refresh this
    /// cache. `None` until the first request is cached.
    ///
    /// [`set_interval`]: CacheKeepalive::set_interval
    target_model: Option<String>,
    /// Global upper bound on time since the last real activity to keep pinging.
    max_idle: Duration,
    /// Next time a keepalive ping should fire.
    next_ping_at: Option<Instant>,
    /// Last *real* cache-warming activity (user message / heartbeat). Keepalive
    /// pings do NOT update this — it anchors the `max_idle` cutoff.
    last_active_at: Option<Instant>,
    /// Consecutive failed ping attempts. Used for retry backoff.
    failure_count: u32,
}

fn retry_delay(failure_count: u32) -> Duration {
    let exponent = failure_count.saturating_sub(1).min(5);
    let secs = 30_u64.saturating_mul(1_u64 << exponent);
    Duration::from_secs(secs.min(15 * 60))
}

impl CacheKeepalive {
    /// Create a keepalive with the global idle ceiling. The per-model interval
    /// starts unset (off) until [`set_interval`] is called from the first cached
    /// request.
    ///
    /// [`set_interval`]: CacheKeepalive::set_interval
    pub fn new(max_idle: Duration) -> Self {
        Self {
            interval: None,
            target_model: None,
            max_idle,
            next_ping_at: None,
            last_active_at: None,
            failure_count: 0,
        }
    }

    /// Update the active model's ping cadence (`None` = keepalive off), e.g. when
    /// a request is cached or the user switches models. Reschedules the ping
    /// timer to fire one interval after the last real activity. Disabling clears
    /// any pending ping.
    ///
    /// The schedule anchors strictly on `last_active_at`: if no real call has
    /// warmed a prefix yet (`None`, e.g. at startup or right after
    /// [`on_cache_invalidated`]), no ping is armed until the next
    /// [`on_cache_warmed`]. This keeps the invariant that we never ping a cold
    /// cache. `now` is unused for arming but retained for signature symmetry with
    /// the other timer mutators.
    ///
    /// [`on_cache_invalidated`]: CacheKeepalive::on_cache_invalidated
    /// [`on_cache_warmed`]: CacheKeepalive::on_cache_warmed
    pub fn set_interval(&mut self, interval: Option<Duration>, model: &str, _now: Instant) {
        // Record the model the keepalive ping runs on. Warms are gated on this:
        // only a call on this model actually refreshes the cache we maintain.
        if self.target_model.as_deref() != Some(model) {
            self.target_model = Some(model.to_owned());
        }
        let changed = self.interval != interval;
        self.interval = interval;
        match interval {
            None => self.next_ping_at = None,
            Some(iv) => {
                if self.next_ping_at.is_none() || changed {
                    self.next_ping_at = self
                        .last_active_at
                        .and_then(|anchor| anchor.checked_add(iv));
                }
            }
        }
    }

    /// Called after ANY *real* LLM call involving the cached prompt — user
    /// message or heartbeat tick (NOT a keepalive ping). Resets the idle clock
    /// and schedules the next ping one interval out.
    pub fn on_cache_warmed(&mut self, model: &str, now: Instant) {
        // Only a call on the model we keep warm actually refreshed the cache.
        // A heartbeat/background tick on a *different* model leaves this model's
        // prompt cache untouched — counting it would reschedule the ping past
        // the cache's own TTL, so the next ping lands cold and pays a full cache
        // recreation. Reject only when a target is already established and the
        // models differ; before any target exists (the first foreground turn,
        // before its request is cached) the warm bootstraps the clock.
        if self.target_model.as_deref().is_some_and(|t| t != model) {
            return;
        }
        self.last_active_at = Some(now);
        self.failure_count = 0;
        self.next_ping_at = self.interval.and_then(|iv| now.checked_add(iv));
    }

    /// Called after a keepalive ping is confirmed sent. Advances the ping timer
    /// one interval from `now` WITHOUT touching the idle clock (so `max_idle`
    /// keeps counting from the last real message).
    pub fn on_ping_succeeded(&mut self, now: Instant) {
        self.failure_count = 0;
        self.next_ping_at = self.interval.and_then(|iv| now.checked_add(iv));
    }

    /// Called when the cached prompt prefix is known to be unusable (e.g. the
    /// active model changed and its prefix is cold). Pauses pinging until the
    /// next real call warms a new prefix.
    ///
    /// Ordinary compaction should not call this: the conversation tail changes,
    /// but stable pinned system sections are still worth keeping warm.
    pub fn on_cache_invalidated(&mut self) {
        self.next_ping_at = None;
        // Clear the activity anchor too: the warmed prefix is gone, so a later
        // `set_interval` must NOT re-arm off the stale timestamp. Pinging only
        // resumes once a real call re-warms via `on_cache_warmed`.
        self.last_active_at = None;
        self.failure_count = 0;
    }

    /// Called by the autonomy loop on each tick.
    ///
    /// Returns `Ping` iff a ping is due (`next_ping_at` set and reached) and the
    /// character is still within the `max_idle` window since its last real
    /// activity. Past `max_idle`, pinging stops (the user is presumed away) until
    /// real activity resumes.
    ///
    /// Does NOT advance `next_ping_at` — the caller must call
    /// [`on_ping_succeeded`] after a successful ping, or [`on_ping_failed`] to
    /// schedule a short retry backoff.
    ///
    /// [`on_ping_succeeded`]: CacheKeepalive::on_ping_succeeded
    /// [`on_ping_failed`]: CacheKeepalive::on_ping_failed
    pub fn tick(&mut self, now: Instant) -> CacheKeepaliveAction {
        let Some(ping_at) = self.next_ping_at else {
            return CacheKeepaliveAction::None;
        };
        if now < ping_at {
            return CacheKeepaliveAction::None;
        }

        // Stop pinging once we've gone `max_idle` without a real message — the
        // user is presumed away and keeping the cache warm is no longer worth
        // the spend. Counted from the last real activity, never from a ping.
        if let Some(last) = self.last_active_at {
            if now.duration_since(last) >= self.max_idle {
                self.next_ping_at = None;
                return CacheKeepaliveAction::None;
            }
        }

        CacheKeepaliveAction::Ping
    }

    /// Called when a keepalive ping fails or is skipped. Retries with a short
    /// exponential backoff so transient failures still get another chance before
    /// the cache goes cold, while budget/provider outages don't hammer the
    /// account every scheduler tick.
    pub fn on_ping_failed(&mut self, now: Instant) {
        self.failure_count = self.failure_count.saturating_add(1);
        self.next_ping_at = now.checked_add(retry_delay(self.failure_count));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The model whose cache the keepalive maintains in these tests.
    const MODEL: &str = "opus";
    /// A different model — a warm reported on this must NOT count.
    const OTHER_MODEL: &str = "glm";

    fn hours(h: u64) -> Duration {
        Duration::from_secs(h.saturating_mul(3600))
    }

    fn minutes(m: u64) -> Duration {
        Duration::from_secs(m.saturating_mul(60))
    }

    /// A keepalive with a 12h idle ceiling and a 55m interval already armed.
    fn armed(now: Instant) -> CacheKeepalive {
        let mut ka = CacheKeepalive::new(hours(12));
        ka.set_interval(Some(minutes(55)), MODEL, now);
        ka.on_cache_warmed(MODEL, now);
        ka
    }

    #[test]
    fn new_returns_no_action() {
        let mut ka = CacheKeepalive::new(hours(12));
        assert_eq!(ka.tick(Instant::now()), CacheKeepaliveAction::None);
    }

    #[test]
    fn off_interval_never_pings() {
        let now = Instant::now();
        let mut ka = CacheKeepalive::new(hours(12));
        // No interval set (keepalive off) — warming does not schedule a ping.
        ka.on_cache_warmed(MODEL, now);
        assert_eq!(ka.tick(now + hours(2)), CacheKeepaliveAction::None);
        // Explicitly off.
        ka.set_interval(None, MODEL, now);
        assert_eq!(ka.tick(now + hours(2)), CacheKeepaliveAction::None);
    }

    #[test]
    fn ping_fires_after_interval() {
        let now = Instant::now();
        let mut ka = armed(now);
        // Not due yet at 54 minutes (interval is 55min).
        assert_eq!(ka.tick(now + minutes(54)), CacheKeepaliveAction::None);
        // Due at 55 minutes.
        assert_eq!(ka.tick(now + minutes(55)), CacheKeepaliveAction::Ping);
    }

    #[test]
    fn ping_reschedules_after_confirm() {
        let now = Instant::now();
        let mut ka = armed(now);
        assert_eq!(ka.tick(now + minutes(55)), CacheKeepaliveAction::Ping);
        // Confirm the ping succeeded — advances from the ping time.
        ka.on_ping_succeeded(now + minutes(55));
        assert_eq!(ka.tick(now + minutes(109)), CacheKeepaliveAction::None);
        assert_eq!(ka.tick(now + minutes(110)), CacheKeepaliveAction::Ping);
    }

    #[test]
    fn ping_succeeded_does_not_reset_idle_clock() {
        // The max_idle cutoff counts from the last REAL activity, so repeated
        // pings must not push it back. With a 2h ceiling and 55m interval, the
        // third scheduled ping lands past the ceiling and must be suppressed.
        let now = Instant::now();
        let mut ka = CacheKeepalive::new(hours(2));
        ka.set_interval(Some(minutes(55)), MODEL, now);
        ka.on_cache_warmed(MODEL, now);

        // Ping 1 at 55m — within 2h.
        assert_eq!(ka.tick(now + minutes(55)), CacheKeepaliveAction::Ping);
        ka.on_ping_succeeded(now + minutes(55));
        // Ping 2 at 110m — within 2h.
        assert_eq!(ka.tick(now + minutes(110)), CacheKeepaliveAction::Ping);
        ka.on_ping_succeeded(now + minutes(110));
        // Ping 3 would be at 165m — past the 2h (120m) idle ceiling → stop.
        assert_eq!(ka.tick(now + minutes(165)), CacheKeepaliveAction::None);
        // Cleared, so later ticks also stay quiet until real activity resumes.
        assert_eq!(ka.tick(now + minutes(166)), CacheKeepaliveAction::None);
    }

    #[test]
    fn real_activity_resets_idle_clock_and_resumes() {
        let now = Instant::now();
        let mut ka = CacheKeepalive::new(hours(2));
        ka.set_interval(Some(minutes(55)), MODEL, now);
        ka.on_cache_warmed(MODEL, now);

        // Drift to just under the ceiling, then a real message arrives.
        ka.on_cache_warmed(MODEL, now + minutes(115));
        // The old 55m ping does not fire; the timer moved to 115+55=170m.
        assert_eq!(ka.tick(now + minutes(120)), CacheKeepaliveAction::None);
        // Fires at 170m, now measured against the fresh activity at 115m.
        assert_eq!(ka.tick(now + minutes(170)), CacheKeepaliveAction::Ping);
    }

    #[test]
    fn retry_backs_off_when_not_confirmed() {
        let now = Instant::now();
        let mut ka = armed(now);
        assert_eq!(ka.tick(now + minutes(55)), CacheKeepaliveAction::Ping);
        // Caller does NOT confirm (ping failed/skipped) — short backoff, not a
        // tight spin.
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
    fn cache_warm_resets_ping_deadline() {
        let now = Instant::now();
        let mut ka = armed(now);
        // A user message warms the cache at 30min.
        ka.on_cache_warmed(MODEL, now + minutes(30));
        // The old ping at 55min should NOT fire (deadline moved to 30+55=85).
        assert_eq!(ka.tick(now + minutes(55)), CacheKeepaliveAction::None);
        assert_eq!(ka.tick(now + minutes(85)), CacheKeepaliveAction::Ping);
    }

    #[test]
    fn warm_on_different_model_is_ignored() {
        // Regression: a heartbeat/background tick on a DIFFERENT model than the
        // keepalive target does not refresh the target's prompt cache, so it
        // must neither reschedule the ping nor reset the idle clock. Counting it
        // (the old bug) pushed the ping past the cache's TTL, turning every ping
        // into a full cache recreation.
        let now = Instant::now();
        let mut ka = armed(now);
        // Off-model "warm" at 30min must NOT move the deadline to 30+55=85m...
        ka.on_cache_warmed(OTHER_MODEL, now + minutes(30));
        // ...the original 55m ping (from the real warm at `now`) still stands.
        assert_eq!(ka.tick(now + minutes(55)), CacheKeepaliveAction::Ping);
    }

    #[test]
    fn warm_on_different_model_does_not_reset_idle_ceiling() {
        // The idle ceiling counts from the last REAL target warm. An off-model
        // tick must not push it back, or background noise would keep the cache
        // warm forever while the user is away.
        let now = Instant::now();
        let mut ka = CacheKeepalive::new(hours(2));
        ka.set_interval(Some(minutes(55)), MODEL, now);
        ka.on_cache_warmed(MODEL, now);

        // An off-model tick at 90min does not reset the 2h ceiling anchored at `now`.
        ka.on_cache_warmed(OTHER_MODEL, now + minutes(90));
        ka.on_ping_succeeded(now + minutes(55));
        ka.on_ping_succeeded(now + minutes(110));
        // Ping 3 at 165m is past the 2h ceiling (still measured from `now`) → stop.
        assert_eq!(ka.tick(now + minutes(165)), CacheKeepaliveAction::None);
    }

    #[test]
    fn invalidation_pauses_and_warm_resumes() {
        let now = Instant::now();
        let mut ka = armed(now);
        // The cached prefix becomes unusable (e.g. model switch).
        ka.on_cache_invalidated();
        assert_eq!(ka.tick(now + minutes(55)), CacheKeepaliveAction::None);
        // Next real call warms a new prefix — pings resume.
        ka.on_cache_warmed(MODEL, now + hours(1));
        assert_eq!(
            ka.tick(now + hours(1) + minutes(55)),
            CacheKeepaliveAction::Ping
        );
    }

    #[test]
    fn disabling_interval_stops_pinging() {
        let now = Instant::now();
        let mut ka = armed(now);
        // User switches to a model with keepalive off.
        ka.set_interval(None, MODEL, now + minutes(10));
        assert_eq!(ka.tick(now + minutes(55)), CacheKeepaliveAction::None);
        assert_eq!(ka.tick(now + hours(3)), CacheKeepaliveAction::None);
    }

    #[test]
    fn switching_interval_reschedules_from_last_activity() {
        let now = Instant::now();
        let mut ka = armed(now);
        // Switch to a 6h cadence at 30min; anchored to last activity (now),
        // the next ping moves to 6h.
        ka.set_interval(Some(hours(6)), MODEL, now + minutes(30));
        assert_eq!(ka.tick(now + minutes(55)), CacheKeepaliveAction::None);
        assert_eq!(ka.tick(now + hours(6)), CacheKeepaliveAction::Ping);
    }

    #[test]
    fn set_interval_after_invalidation_does_not_arm_cold_cache() {
        // After invalidation, a model-switch `set_interval` must NOT re-arm off
        // the stale activity timestamp: pinging a cold prefix is exactly what
        // invalidation exists to prevent. Only a real warm resumes pinging.
        let now = Instant::now();
        let mut ka = armed(now);
        ka.on_cache_invalidated();

        // New request cached for the switched model, but nothing has warmed its
        // prefix yet → no ping armed, even far in the future.
        ka.set_interval(Some(minutes(55)), MODEL, now + minutes(5));
        assert_eq!(ka.tick(now + hours(2)), CacheKeepaliveAction::None);

        // A real call warms the new prefix → pinging resumes from there.
        ka.on_cache_warmed(MODEL, now + hours(1));
        assert_eq!(
            ka.tick(now + hours(1) + minutes(55)),
            CacheKeepaliveAction::Ping
        );
    }
}
