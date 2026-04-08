//! Interiority clock — deadline holder with abandonment guard.
//!
//! The character schedules its own next wake via `set_next_wake`. The clock
//! holds that deadline and fires `RunTick` when it passes. An abandonment
//! guard stops ticking when the user has been absent too long.

use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use shore_config::app::InteriorityConfig;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Minimum interval a character can schedule (1 hour).
pub const MIN_WAKE_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Maximum interval a character can schedule (48 hours).
pub const MAX_WAKE_INTERVAL: Duration = Duration::from_secs(48 * 60 * 60);

// ---------------------------------------------------------------------------
// Action enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteriorityAction {
    /// Nothing to do this tick.
    None,
    /// Fire a full interiority tick (journal-backed, LLM call with tools).
    RunTick,
}

// ---------------------------------------------------------------------------
// InteriorityClock
// ---------------------------------------------------------------------------

/// Deadline holder with abandonment guard.
///
/// The character drives its own cadence via `schedule()`. The clock's job is
/// to hold that deadline, apply bounds, and stop ticking when the user has
/// been gone too long.
pub struct InteriorityClock {
    /// Next scheduled wake time. `None` means no wake is scheduled (first
    /// boot, or guard has tripped).
    next_wake_at: Option<Instant>,

    /// Last time a wake was scheduled or fired. Used for the default-interval
    /// fallback when the character doesn't call set_next_wake.
    last_anchor: Instant,

    // -- abandonment guard --------------------------------------------------

    /// Consecutive interiority ticks that fired without a user message.
    ticks_without_user: u32,

    /// Last time a user message arrived. Used for the wall-clock leg of the
    /// abandonment guard.
    last_user_at: Option<Instant>,

    // -- config -------------------------------------------------------------

    /// Fallback interval when the character doesn't call set_next_wake.
    default_interval: Duration,

    /// Max consecutive ticks without user before the guard stops ticking.
    max_idle_ticks: u32,

    /// Max wall-clock duration without user before the guard stops ticking.
    max_silent_duration: Duration,
}

impl InteriorityClock {
    pub fn with_config(config: &InteriorityConfig) -> Self {
        Self {
            next_wake_at: None,
            last_anchor: Instant::now(),
            ticks_without_user: 0,
            last_user_at: None,
            default_interval: Duration::from_secs(config.interval_secs),
            max_idle_ticks: config.max_idle_ticks,
            max_silent_duration: Duration::from_secs(config.max_silent_secs),
        }
    }

    // -- accessors ----------------------------------------------------------

    pub fn next_wake(&self) -> Option<Instant> {
        self.next_wake_at
    }

    pub fn ticks_without_user(&self) -> u32 {
        self.ticks_without_user
    }

    pub fn max_idle_ticks(&self) -> u32 {
        self.max_idle_ticks
    }

    pub fn last_user_at(&self) -> Option<Instant> {
        self.last_user_at
    }

    pub fn default_interval(&self) -> Duration {
        self.default_interval
    }

    /// Human-readable state label for status display and logging.
    pub fn state(&self) -> &str {
        if self.next_wake_at.is_none() && self.ticks_without_user >= self.max_idle_ticks {
            "Abandoned"
        } else {
            "Active"
        }
    }

    // -- core ---------------------------------------------------------------

    /// Called by the autonomy loop on each ~30s tick.
    ///
    /// Semantics:
    /// 1. If `next_wake_at` is None → set to `last_anchor + default_interval`, return None.
    /// 2. If `now < next_wake_at` → return None.
    /// 3. Deadline passed — check abandonment guard. If tripped, clear
    ///    `next_wake_at` and return None.
    /// 4. Guard passes → increment counter, clear deadline, update anchor,
    ///    return RunTick.
    pub fn tick(&mut self, now: Instant) -> InteriorityAction {
        // Step 1: bootstrap if no deadline set.
        if self.next_wake_at.is_none() {
            let target = self.last_anchor + self.default_interval;
            self.next_wake_at = Some(target);
            debug!(
                default_interval_secs = self.default_interval.as_secs(),
                "InteriorityClock: no deadline set, scheduling default"
            );
            return InteriorityAction::None;
        }

        // Step 2: not due yet.
        let wake_at = self.next_wake_at.unwrap();
        if now < wake_at {
            return InteriorityAction::None;
        }

        // Step 3: deadline passed — check abandonment guard.
        if self.ticks_without_user >= self.max_idle_ticks {
            // Tick-count guard.
            info!(
                ticks_without_user = self.ticks_without_user,
                max_idle_ticks = self.max_idle_ticks,
                "InteriorityClock: abandonment guard tripped (tick count)"
            );
            self.next_wake_at = None;
            return InteriorityAction::None;
        }
        if let Some(last_user) = self.last_user_at {
            if now.duration_since(last_user) >= self.max_silent_duration {
                info!(
                    silent_secs = now.duration_since(last_user).as_secs(),
                    max_silent_secs = self.max_silent_duration.as_secs(),
                    "InteriorityClock: abandonment guard tripped (silent duration)"
                );
                self.next_wake_at = None;
                return InteriorityAction::None;
            }
        }

        // Step 4: guard passes — fire the tick.
        self.ticks_without_user += 1;
        self.next_wake_at = None;
        self.last_anchor = now;
        debug!(
            ticks_without_user = self.ticks_without_user,
            "InteriorityClock: tick firing"
        );
        InteriorityAction::RunTick
    }

    /// Called when the character invokes `set_next_wake` during a tick.
    ///
    /// Bounds: `MIN_WAKE_INTERVAL <= (when - now) <= MAX_WAKE_INTERVAL`.
    /// Out-of-range values are clamped (with a warning logged) rather than
    /// rejected, so a misbehaving character can never silently disable
    /// interiority.
    pub fn schedule(&mut self, when: Instant, now: Instant) {
        let delta = when.saturating_duration_since(now);
        let clamped = delta.clamp(MIN_WAKE_INTERVAL, MAX_WAKE_INTERVAL);

        if clamped != delta {
            warn!(
                requested_secs = delta.as_secs(),
                clamped_secs = clamped.as_secs(),
                "InteriorityClock: set_next_wake clamped to bounds"
            );
        }

        let target = now + clamped;
        self.next_wake_at = Some(target);
        self.last_anchor = now;
        debug!(
            wake_in_secs = clamped.as_secs(),
            "InteriorityClock: character scheduled next wake"
        );
    }

    /// Called when a user message arrives.
    ///
    /// Semantics:
    /// 1. Reset `ticks_without_user = 0`.
    /// 2. Set `last_user_at = Some(now)`.
    /// 3. `next_wake_at = max(next_wake_at, Some(now + MIN_WAKE_INTERVAL))`.
    ///    If `next_wake_at` was None (first message, or abandoned), this
    ///    bootstraps the cycle. If the character had scheduled further out,
    ///    the schedule is preserved.
    pub fn on_user_message(&mut self, now: Instant) {
        self.ticks_without_user = 0;
        self.last_user_at = Some(now);

        let min_wake = now + MIN_WAKE_INTERVAL;
        self.next_wake_at = Some(match self.next_wake_at {
            Some(existing) if existing > min_wake => existing,
            _ => min_wake,
        });

        debug!(
            wake_in_secs = self.next_wake_at.unwrap().duration_since(now).as_secs(),
            "InteriorityClock: user message, deadline set"
        );
    }

    /// Restore state from persistence (daemon restart).
    pub fn restore(
        &mut self,
        ticks_without_user: u32,
        next_wake_at: Option<Instant>,
        last_user_at: Option<Instant>,
    ) {
        debug!(
            ticks_without_user,
            has_wake = next_wake_at.is_some(),
            has_user = last_user_at.is_some(),
            "InteriorityClock: state restored from persistence"
        );
        self.ticks_without_user = ticks_without_user;
        if let Some(wake) = next_wake_at {
            self.next_wake_at = Some(wake);
            self.last_anchor = wake;
        }
        if let Some(user) = last_user_at {
            self.last_user_at = Some(user);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(interval_secs: u64, max_idle: u32) -> InteriorityClock {
        let config = InteriorityConfig {
            enabled: true,
            interval_secs,
            max_idle_ticks: max_idle,
            max_silent_secs: 172800, // 48h
            max_tool_rounds: 3,
        };
        InteriorityClock::with_config(&config)
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    // -- basic lifecycle ----------------------------------------------------

    #[test]
    fn first_tick_bootstraps_deadline() {
        let mut c = clock(60, 3);
        let now = Instant::now();
        assert_eq!(c.tick(now), InteriorityAction::None);
        assert!(c.next_wake_at.is_some());
    }

    #[test]
    fn tick_fires_after_default_interval() {
        let mut c = clock(60, 3);
        let now = Instant::now();
        c.tick(now); // bootstrap
        assert_eq!(c.tick(now + secs(61)), InteriorityAction::RunTick);
        assert_eq!(c.ticks_without_user, 1);
    }

    #[test]
    fn tick_does_not_fire_before_deadline() {
        let mut c = clock(60, 3);
        let now = Instant::now();
        c.tick(now); // bootstrap
        assert_eq!(c.tick(now + secs(30)), InteriorityAction::None);
    }

    #[test]
    fn after_tick_fires_next_bootstrap_applies() {
        // After RunTick, next_wake_at is None. The next 30s poll should
        // re-bootstrap with default_interval from the new anchor.
        let mut c = clock(60, 3);
        let now = Instant::now();
        c.tick(now); // bootstrap
        let t1 = now + secs(61);
        assert_eq!(c.tick(t1), InteriorityAction::RunTick);
        // next_wake_at is now None; next tick re-bootstraps.
        assert_eq!(c.tick(t1 + secs(1)), InteriorityAction::None);
        // Fires again after another full interval from anchor.
        assert_eq!(c.tick(t1 + secs(61)), InteriorityAction::RunTick);
    }

    // -- abandonment guard: tick count --------------------------------------

    #[test]
    fn guard_trips_after_max_idle_ticks() {
        let mut c = clock(60, 2);
        let mut now = Instant::now();

        c.tick(now); // bootstrap

        // Tick 1.
        now += secs(61);
        assert_eq!(c.tick(now), InteriorityAction::RunTick);

        // Tick 2.
        now += secs(61);
        c.tick(now); // bootstrap
        now += secs(61);
        assert_eq!(c.tick(now), InteriorityAction::RunTick);

        // ticks_without_user is now 2 == max_idle. Next deadline: guard trips.
        now += secs(61);
        c.tick(now); // bootstrap
        now += secs(61);
        assert_eq!(c.tick(now), InteriorityAction::None);
        assert!(c.next_wake_at.is_none());
    }

    #[test]
    fn guard_does_not_trip_if_user_active() {
        let mut c = clock(60, 2);
        let mut now = Instant::now();

        c.tick(now); // bootstrap

        now += secs(61);
        assert_eq!(c.tick(now), InteriorityAction::RunTick); // tick 1

        // User resets the counter.
        now += secs(10);
        c.on_user_message(now);
        assert_eq!(c.ticks_without_user, 0);

        // Next tick fires normally.
        now += secs(3601);
        assert_eq!(c.tick(now), InteriorityAction::RunTick);
        assert_eq!(c.ticks_without_user, 1);
    }

    // -- abandonment guard: silent duration ---------------------------------

    #[test]
    fn guard_trips_on_silent_duration() {
        let mut c = clock(3600, 100); // high tick count so it doesn't trip first
        c.max_silent_duration = secs(7200); // 2h for test speed
        let now = Instant::now();

        // Simulate: user sent a message, then silence.
        c.on_user_message(now);

        // Fast-forward past the first tick (1h).
        let t1 = now + secs(3601);
        assert_eq!(c.tick(t1), InteriorityAction::RunTick);

        // Bootstrap next deadline.
        let t2 = t1 + secs(1);
        c.tick(t2);

        // At 2h+1s past user message → silent guard trips.
        let t3 = now + secs(7201);
        assert_eq!(c.tick(t3), InteriorityAction::None);
        assert!(c.next_wake_at.is_none());
    }

    // -- schedule() ---------------------------------------------------------

    #[test]
    fn schedule_sets_deadline() {
        let mut c = clock(3600, 3);
        let now = Instant::now();
        c.on_user_message(now);

        // Character schedules 4h out.
        c.schedule(now + Duration::from_secs(4 * 3600), now);
        assert!(c.next_wake_at.is_some());

        // Should not fire at 3h.
        assert_eq!(c.tick(now + secs(3 * 3600)), InteriorityAction::None);
        // Should fire at 4h+1s.
        assert_eq!(c.tick(now + secs(4 * 3600 + 1)), InteriorityAction::RunTick);
    }

    #[test]
    fn schedule_clamps_below_minimum() {
        let mut c = clock(3600, 3);
        let now = Instant::now();
        c.on_user_message(now);

        // Try to schedule 10 minutes out — clamped to 1h.
        c.schedule(now + secs(600), now);
        let wake = c.next_wake_at.unwrap();
        let delta = wake.duration_since(now);
        assert_eq!(delta, MIN_WAKE_INTERVAL);
    }

    #[test]
    fn schedule_clamps_above_maximum() {
        let mut c = clock(3600, 3);
        let now = Instant::now();
        c.on_user_message(now);

        // Try to schedule 72h out — clamped to 48h.
        c.schedule(now + secs(72 * 3600), now);
        let wake = c.next_wake_at.unwrap();
        let delta = wake.duration_since(now);
        assert_eq!(delta, MAX_WAKE_INTERVAL);
    }

    // -- on_user_message() --------------------------------------------------

    #[test]
    fn user_message_resets_counter() {
        let mut c = clock(60, 3);
        let mut now = Instant::now();
        c.tick(now);
        now += secs(61);
        c.tick(now); // ticks_without_user = 1
        assert_eq!(c.ticks_without_user, 1);

        c.on_user_message(now);
        assert_eq!(c.ticks_without_user, 0);
    }

    #[test]
    fn user_message_preserves_further_schedule() {
        let mut c = clock(3600, 3);
        let now = Instant::now();

        // Character scheduled 6h out.
        c.schedule(now + secs(6 * 3600), now);
        let original = c.next_wake_at.unwrap();

        // User message at t+30min. The 6h schedule is further out than
        // now + MIN_WAKE (1h), so it should be preserved.
        c.on_user_message(now + secs(1800));
        assert_eq!(c.next_wake_at.unwrap(), original);
    }

    #[test]
    fn user_message_pushes_imminent_deadline() {
        let mut c = clock(60, 3);
        let now = Instant::now();
        c.tick(now); // bootstrap: deadline at now + 60s

        // User message at t+50s. The existing deadline (now+60) is only 10s
        // away, which is less than MIN_WAKE (1h), so on_user_message pushes
        // it to now+50 + 1h.
        let msg_time = now + secs(50);
        c.on_user_message(msg_time);
        let expected_min = msg_time + MIN_WAKE_INTERVAL;
        assert_eq!(c.next_wake_at.unwrap(), expected_min);
    }

    #[test]
    fn user_message_bootstraps_from_none() {
        let mut c = clock(3600, 3);
        let now = Instant::now();
        // next_wake_at is None (fresh clock, no tick yet).
        assert!(c.next_wake_at.is_none());

        c.on_user_message(now);
        // Should have set next_wake_at to now + MIN_WAKE.
        assert_eq!(c.next_wake_at.unwrap(), now + MIN_WAKE_INTERVAL);
    }

    #[test]
    fn user_message_wakes_from_abandoned() {
        let mut c = clock(60, 1);
        let mut now = Instant::now();
        c.tick(now); // bootstrap
        now += secs(61);
        c.tick(now); // tick 1

        // Bootstrap and trip the guard.
        now += secs(61);
        c.tick(now); // bootstrap
        now += secs(61);
        assert_eq!(c.tick(now), InteriorityAction::None); // guard trips
        assert!(c.next_wake_at.is_none());

        // User returns.
        now += secs(100);
        c.on_user_message(now);
        assert_eq!(c.ticks_without_user, 0);
        assert!(c.next_wake_at.is_some());
    }

    // -- restore() ----------------------------------------------------------

    #[test]
    fn restore_with_future_wake() {
        let mut c = clock(3600, 3);
        let now = Instant::now();
        let future = now + secs(7200);

        c.restore(2, Some(future), Some(now));
        assert_eq!(c.ticks_without_user, 2);
        assert_eq!(c.next_wake_at, Some(future));
        assert_eq!(c.last_user_at, Some(now));
    }

    #[test]
    fn restore_with_past_wake_fires_immediately() {
        let mut c = clock(3600, 3);
        let now = Instant::now();
        let past = now - secs(100);

        c.restore(1, Some(past), Some(now));
        // Deadline is in the past → tick() fires immediately.
        assert_eq!(c.tick(now), InteriorityAction::RunTick);
    }

    // -- state() label ------------------------------------------------------

    #[test]
    fn state_label_active_when_healthy() {
        let c = clock(3600, 3);
        assert_eq!(c.state(), "Active");
    }

    #[test]
    fn state_label_abandoned_when_guard_tripped() {
        let mut c = clock(60, 1);
        let mut now = Instant::now();
        c.tick(now); // bootstrap
        now += secs(61);
        c.tick(now); // tick 1
        now += secs(61);
        c.tick(now); // bootstrap
        now += secs(61);
        c.tick(now); // guard trips
        assert_eq!(c.state(), "Abandoned");
    }
}
