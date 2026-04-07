//! Interiority clock — periodic "turns to self" with unified cache refresh.
//!
//! Simple timer with jitter and dormancy counter. Each tick gives the character
//! a single agentic turn backed by a rolling journal. When dormant, ticks
//! degrade to bare `max_tokens=1` pings to keep the prompt cache warm.

use std::time::{Duration, Instant};

use rand::Rng;

use shore_config::app::InteriorityConfig;

// ---------------------------------------------------------------------------
// State & action enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteriorityState {
    Active,
    Dormant,
}

impl std::fmt::Display for InteriorityState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "Active"),
            Self::Dormant => write!(f, "Dormant"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteriorityAction {
    None,
    /// Fire a full interiority tick (journal-backed, one LLM call with tools).
    RunTick,
    /// Fire a bare cache-refresh ping (max_tokens=1, no journal, no tools).
    /// Only returned when dormant with a cache TTL configured.
    RunDormantPing,
}

// ---------------------------------------------------------------------------
// InteriorityClock
// ---------------------------------------------------------------------------

pub struct InteriorityClock {
    state: InteriorityState,
    paused: bool,
    interval_secs: u64,
    jitter_factor: f64,
    max_idle_ticks: u32,
    ticks_without_user: u32,
    /// Next full interiority tick deadline.
    next_tick_at: Option<Instant>,
    /// Next cache refresh deadline (bare ping). Only used when
    /// `cache_refresh_interval_secs` is set.
    next_cache_ping_at: Option<Instant>,
    last_user_ts: Option<Instant>,
    /// Cache TTL in seconds (from provider config). When set, bare cache
    /// refresh pings fire on their own cadence between interiority ticks.
    cache_refresh_interval_secs: Option<u64>,
}

impl Default for InteriorityClock {
    fn default() -> Self {
        Self::new()
    }
}

impl InteriorityClock {
    pub fn new() -> Self {
        Self {
            state: InteriorityState::Active,
            paused: false,
            interval_secs: 3600,
            jitter_factor: 0.25,
            max_idle_ticks: 3,
            ticks_without_user: 0,
            next_tick_at: None,
            next_cache_ping_at: None,
            last_user_ts: None,
            cache_refresh_interval_secs: None,
        }
    }

    pub fn with_config(config: &InteriorityConfig) -> Self {
        Self {
            state: InteriorityState::Active,
            paused: false,
            interval_secs: config.interval_secs,
            jitter_factor: config.jitter_factor,
            max_idle_ticks: config.max_idle_ticks,
            ticks_without_user: 0,
            next_tick_at: None,
            next_cache_ping_at: None,
            last_user_ts: None,
            cache_refresh_interval_secs: None,
        }
    }

    pub fn state(&self) -> InteriorityState {
        self.state
    }

    pub fn ticks_without_user(&self) -> u32 {
        self.ticks_without_user
    }

    pub fn max_idle_ticks(&self) -> u32 {
        self.max_idle_ticks
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        if !paused && self.next_tick_at.is_none() {
            let now = Instant::now();
            self.schedule_next_tick(now);
            self.schedule_next_cache_ping(now);
        }
    }

    /// Set the cache TTL so the clock fires bare refresh pings between
    /// interiority ticks. Pass `None` for non-Anthropic providers.
    pub fn set_cache_refresh_interval(&mut self, cache_ttl_secs: Option<u64>) {
        self.cache_refresh_interval_secs = cache_ttl_secs.map(|ttl| ttl.saturating_sub(60).max(60));
    }

    /// The effective interiority tick interval (user-configured).
    pub fn effective_interval_secs(&self) -> u64 {
        self.interval_secs
    }

    /// The cache refresh interval, if configured.
    pub fn cache_refresh_interval(&self) -> Option<u64> {
        self.cache_refresh_interval_secs
    }

    /// Main tick method. Call on each autonomy tick interval (~30s).
    /// Returns what the manager should do.
    pub fn tick(&mut self, now: Instant) -> InteriorityAction {
        if self.paused {
            return InteriorityAction::None;
        }

        // When dormant, only fire cache refresh pings.
        if self.state == InteriorityState::Dormant {
            return self.tick_dormant(now);
        }

        // First tick: schedule both timers and return.
        if self.next_tick_at.is_none() {
            self.schedule_next_tick(now);
            self.schedule_next_cache_ping(now);
            return InteriorityAction::None;
        }

        // Check interiority deadline first (takes priority over cache ping).
        let next = self.next_tick_at.unwrap();
        if now >= next {
            self.ticks_without_user += 1;

            if self.ticks_without_user > self.max_idle_ticks {
                self.state = InteriorityState::Dormant;
                self.next_tick_at = None;
                // Schedule the dormant cache ping timer.
                self.schedule_next_cache_ping(now);
                return InteriorityAction::None;
            }

            // Full tick fires — also resets the cache ping timer since
            // the full LLM call refreshes the cache.
            self.schedule_next_tick(now);
            self.schedule_next_cache_ping(now);
            return InteriorityAction::RunTick;
        }

        // Interiority not due yet — check cache refresh deadline.
        if let Some(next_ping) = self.next_cache_ping_at {
            if now >= next_ping {
                self.schedule_next_cache_ping(now);
                return InteriorityAction::RunDormantPing;
            }
        }

        InteriorityAction::None
    }

    /// Call when a user message arrives.
    pub fn on_user_message(&mut self, now: Instant) {
        self.ticks_without_user = 0;
        self.last_user_ts = Some(now);

        if self.state == InteriorityState::Dormant {
            self.state = InteriorityState::Active;
        }

        // Always reschedule the tick from now — the user is actively present,
        // so the next tick should fire a full interval after the last message,
        // not at whatever deadline was set before the conversation started.
        self.schedule_next_tick(now);
        self.schedule_next_cache_ping(now);
    }

    /// Call when an assistant message is generated (optional tracking).
    pub fn on_assistant_message(&mut self, _now: Instant) {
        // Currently a no-op — ticks_without_user only resets on user messages.
    }

    /// Restore state from persistence.
    pub fn restore(&mut self, state: InteriorityState, ticks_without_user: u32) {
        self.state = state;
        self.ticks_without_user = ticks_without_user;
    }

    // -- internal ---------------------------------------------------------

    /// Dormant tick: only fire cache refresh pings if configured.
    fn tick_dormant(&mut self, now: Instant) -> InteriorityAction {
        if self.cache_refresh_interval_secs.is_none() {
            return InteriorityAction::None;
        }

        if self.next_cache_ping_at.is_none() {
            self.schedule_next_cache_ping(now);
            return InteriorityAction::None;
        }

        let next = self.next_cache_ping_at.unwrap();
        if now < next {
            return InteriorityAction::None;
        }

        self.schedule_next_cache_ping(now);
        InteriorityAction::RunDormantPing
    }

    fn schedule_next_tick(&mut self, now: Instant) {
        let base = self.interval_secs as f64;
        let jitter_range = base * self.jitter_factor;
        let jitter: f64 = rand::thread_rng().gen_range(-jitter_range..=jitter_range);
        let secs = (base + jitter).max(60.0); // minimum 60s
        self.next_tick_at = Some(now + Duration::from_secs_f64(secs));
    }

    fn schedule_next_cache_ping(&mut self, now: Instant) {
        if let Some(refresh) = self.cache_refresh_interval_secs {
            let base = refresh as f64;
            // Use a fraction of the clock's jitter factor for cache pings
            // (smaller jitter since timing matters more for cache TTL).
            let jitter_pct = self.jitter_factor * 0.2; // e.g. 25% → 5%
            let jitter_range = base * jitter_pct;
            let jitter: f64 = if jitter_range > 0.0 {
                rand::thread_rng().gen_range(-jitter_range..=jitter_range)
            } else {
                0.0
            };
            let secs = (base + jitter).max(30.0);
            self.next_cache_ping_at = Some(now + Duration::from_secs_f64(secs));
        } else {
            self.next_cache_ping_at = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn clock_with_interval(secs: u64, max_idle: u32) -> InteriorityClock {
        let config = InteriorityConfig {
            enabled: true,
            interval_secs: secs,
            jitter_factor: 0.0, // no jitter for deterministic tests
            max_idle_ticks: max_idle,
            max_tool_rounds: 3,
        };
        InteriorityClock::with_config(&config)
    }

    #[test]
    fn first_tick_schedules_only() {
        let mut clock = clock_with_interval(60, 3);
        let now = Instant::now();
        assert_eq!(clock.tick(now), InteriorityAction::None);
        assert!(clock.next_tick_at.is_some());
    }

    #[test]
    fn tick_fires_after_interval() {
        let mut clock = clock_with_interval(60, 3);
        let now = Instant::now();
        clock.tick(now); // schedule
        let after = now + Duration::from_secs(61);
        assert_eq!(clock.tick(after), InteriorityAction::RunTick);
        assert_eq!(clock.ticks_without_user, 1);
    }

    #[test]
    fn dormancy_after_max_idle() {
        let mut clock = clock_with_interval(60, 2);
        let mut now = Instant::now();
        clock.tick(now); // schedule

        // Tick 1
        now += Duration::from_secs(61);
        assert_eq!(clock.tick(now), InteriorityAction::RunTick);

        // Tick 2
        now += Duration::from_secs(61);
        assert_eq!(clock.tick(now), InteriorityAction::RunTick);

        // Tick 3 → dormant (ticks_without_user=3 > max_idle=2)
        now += Duration::from_secs(61);
        assert_eq!(clock.tick(now), InteriorityAction::None);
        assert_eq!(clock.state(), InteriorityState::Dormant);
    }

    #[test]
    fn user_message_wakes_from_dormant() {
        let mut clock = clock_with_interval(60, 1);
        let mut now = Instant::now();
        clock.tick(now);

        now += Duration::from_secs(61);
        clock.tick(now); // tick 1

        now += Duration::from_secs(61);
        clock.tick(now); // dormant
        assert_eq!(clock.state(), InteriorityState::Dormant);

        now += Duration::from_secs(10);
        clock.on_user_message(now);
        assert_eq!(clock.state(), InteriorityState::Active);
        assert_eq!(clock.ticks_without_user, 0);
    }

    #[test]
    fn paused_returns_none() {
        let mut clock = clock_with_interval(60, 3);
        clock.set_paused(true);
        let now = Instant::now();
        assert_eq!(clock.tick(now), InteriorityAction::None);
    }

    #[test]
    fn dormant_without_cache_returns_none() {
        let mut clock = clock_with_interval(60, 3);
        clock.restore(InteriorityState::Dormant, 5);
        let now = Instant::now();
        // No cache_refresh_interval → dormant returns None (no cache to keep warm)
        assert_eq!(clock.tick(now), InteriorityAction::None);
    }

    #[test]
    fn user_message_resets_counter() {
        let mut clock = clock_with_interval(60, 3);
        let mut now = Instant::now();
        clock.tick(now);

        now += Duration::from_secs(61);
        clock.tick(now); // ticks_without_user = 1

        now += Duration::from_secs(10);
        clock.on_user_message(now);
        assert_eq!(clock.ticks_without_user, 0);
    }

    #[test]
    fn user_message_reschedules_tick_while_active() {
        // Regression: tick must NOT fire mid-conversation. A user message
        // arriving before the deadline should push the deadline forward.
        let mut clock = clock_with_interval(60, 3);
        let mut now = Instant::now();
        clock.tick(now); // schedule first tick at t+60

        // At t+50 user sends a message — reschedules tick to t+110.
        now += Duration::from_secs(50);
        clock.on_user_message(now);

        // At t+65 (past the *original* deadline) no tick should fire.
        now += Duration::from_secs(15);
        assert_eq!(clock.tick(now), InteriorityAction::None);

        // At t+111 (past the *rescheduled* deadline) the tick fires.
        now += Duration::from_secs(46);
        assert_eq!(clock.tick(now), InteriorityAction::RunTick);
    }

    // -- effective interval tests ----------------------------------------

    #[test]
    fn effective_interval_always_returns_configured() {
        let clock = clock_with_interval(3600, 3);
        assert_eq!(clock.effective_interval_secs(), 3600);
    }

    #[test]
    fn effective_interval_unaffected_by_cache() {
        let mut clock = clock_with_interval(7200, 3);
        clock.set_cache_refresh_interval(Some(3600));
        // effective_interval_secs returns interiority interval, not cache.
        assert_eq!(clock.effective_interval_secs(), 7200);
        // Cache refresh is tracked separately.
        assert_eq!(clock.cache_refresh_interval(), Some(3540));
    }

    #[test]
    fn cache_refresh_interval_floor() {
        let mut clock = clock_with_interval(3600, 3);
        // Very short TTL — saturating_sub(60) floors at 60.
        clock.set_cache_refresh_interval(Some(30));
        assert_eq!(clock.cache_refresh_interval(), Some(60));
    }

    // -- active cache ping tests -----------------------------------------

    #[test]
    fn active_cache_ping_fires_between_ticks() {
        // Interiority = 600s, cache refresh = 60s (from 120s TTL).
        // Between interiority ticks, cache pings should fire.
        let mut clock = clock_with_interval(600, 3);
        clock.set_cache_refresh_interval(Some(120)); // 120-60 = 60s refresh
        let mut now = Instant::now();

        clock.tick(now); // schedule both timers
        assert!(clock.next_tick_at.is_some());
        assert!(clock.next_cache_ping_at.is_some());

        // After 61s: cache ping fires, but interiority tick hasn't.
        now += Duration::from_secs(61);
        assert_eq!(clock.tick(now), InteriorityAction::RunDormantPing);
        assert_eq!(clock.state(), InteriorityState::Active);

        // After another 61s: another cache ping.
        now += Duration::from_secs(61);
        assert_eq!(clock.tick(now), InteriorityAction::RunDormantPing);

        // After full interiority interval: full tick fires.
        now += Duration::from_secs(500);
        assert_eq!(clock.tick(now), InteriorityAction::RunTick);
    }

    #[test]
    fn no_cache_ping_without_config() {
        let mut clock = clock_with_interval(600, 3);
        // No cache refresh configured.
        let mut now = Instant::now();

        clock.tick(now); // schedule
        assert!(clock.next_cache_ping_at.is_none());

        // Between ticks, nothing fires.
        now += Duration::from_secs(300);
        assert_eq!(clock.tick(now), InteriorityAction::None);

        // Full tick fires at interval.
        now += Duration::from_secs(301);
        assert_eq!(clock.tick(now), InteriorityAction::RunTick);
    }

    #[test]
    fn full_tick_resets_cache_timer() {
        let mut clock = clock_with_interval(60, 3);
        clock.set_cache_refresh_interval(Some(120)); // 60s refresh
        let mut now = Instant::now();

        clock.tick(now); // schedule

        // Full tick fires.
        now += Duration::from_secs(61);
        assert_eq!(clock.tick(now), InteriorityAction::RunTick);

        // Cache timer was reset by the full tick, so no ping yet.
        now += Duration::from_secs(30);
        assert_eq!(clock.tick(now), InteriorityAction::None);
    }

    // -- dormant ping tests ----------------------------------------------

    #[test]
    fn dormant_with_cache_fires_ping() {
        let mut clock = clock_with_interval(60, 1);
        clock.set_cache_refresh_interval(Some(120)); // 60s refresh
        let mut now = Instant::now();

        // Go active → tick → dormant.
        clock.tick(now); // schedule
        now += Duration::from_secs(61);
        clock.tick(now); // tick 1 (RunTick)
        now += Duration::from_secs(61);
        clock.tick(now); // → dormant
        assert_eq!(clock.state(), InteriorityState::Dormant);

        // Dormant tick should fire a cache ping.
        now += Duration::from_secs(61);
        assert_eq!(clock.tick(now), InteriorityAction::RunDormantPing);
    }

    #[test]
    fn dormant_without_cache_no_ping() {
        let mut clock = clock_with_interval(60, 1);
        // No cache_refresh_interval set.
        let mut now = Instant::now();

        clock.tick(now);
        now += Duration::from_secs(61);
        clock.tick(now);
        now += Duration::from_secs(61);
        clock.tick(now); // → dormant
        assert_eq!(clock.state(), InteriorityState::Dormant);

        now += Duration::from_secs(61);
        assert_eq!(clock.tick(now), InteriorityAction::None);
    }

    #[test]
    fn dormant_ping_respects_cache_interval() {
        let mut clock = clock_with_interval(60, 1);
        clock.set_cache_refresh_interval(Some(120)); // 60s refresh
        let mut now = Instant::now();

        // Push to dormant.
        clock.tick(now);
        now += Duration::from_secs(61);
        clock.tick(now);
        now += Duration::from_secs(61);
        clock.tick(now); // dormant

        // Dormant schedules cache ping timer on entry.
        // Not enough time for cache ping yet.
        now += Duration::from_secs(30);
        assert_eq!(clock.tick(now), InteriorityAction::None);

        // After full cache interval, fires.
        now += Duration::from_secs(40);
        assert_eq!(clock.tick(now), InteriorityAction::RunDormantPing);
    }

    // -- real-world config simulation ------------------------------------

    #[test]
    fn simulate_real_config_2h_interiority_1h_cache() {
        // Simulates: interval_secs=7200, cache_ttl="1h" (3600s → 3540s refresh).
        // No jitter for deterministic assertions.
        let mut clock = clock_with_interval(7200, 8);
        clock.set_cache_refresh_interval(Some(3600)); // 3600-60 = 3540s stored
        let mut now = Instant::now();

        clock.tick(now); // t=0: schedule both timers

        // Collect all actions over a 5-hour window (18000s).
        let mut ticks = Vec::new();
        let mut pings = Vec::new();
        let step = 30; // check every 30s like the real tick loop
        for elapsed in (step..=18000).step_by(step) {
            now += Duration::from_secs(step as u64);
            match clock.tick(now) {
                InteriorityAction::RunTick => ticks.push(elapsed),
                InteriorityAction::RunDormantPing => pings.push(elapsed),
                InteriorityAction::None => {}
            }
        }

        // Interiority ticks: should fire at ~7200, ~14400.
        assert_eq!(
            ticks.len(),
            2,
            "Expected 2 interiority ticks in 5h, got: {ticks:?}"
        );
        assert!(
            ticks[0] >= 7170 && ticks[0] <= 7230,
            "First tick at {}",
            ticks[0]
        );
        assert!(
            ticks[1] >= 14370 && ticks[1] <= 14430,
            "Second tick at {}",
            ticks[1]
        );

        // Cache pings: fire at ~3540s intervals. Between each 7200s interiority
        // tick there are 2 pings (~3540 and ~7080). Full tick resets cache timer.
        // Over 18000s: 2 (before tick1) + 2 (before tick2) + 1 (after tick2) = 5.
        assert!(
            pings.len() >= 4 && pings.len() <= 6,
            "Expected 4-6 cache pings in 5h, got {}: {pings:?}",
            pings.len()
        );

        // First cache ping should be around 3540s.
        assert!(
            pings[0] >= 3510 && pings[0] <= 3570,
            "First cache ping at {}",
            pings[0]
        );

        // No cache pings should be too close to an interiority tick
        // (within 30s) since the full tick resets the cache timer.
        for &ping_time in &pings {
            for &tick_time in &ticks {
                let gap = (ping_time as i64 - tick_time as i64).unsigned_abs() as usize;
                assert!(
                    gap >= step,
                    "Cache ping at {ping_time}s too close to tick at {tick_time}s (gap={gap}s)"
                );
            }
        }
    }

    #[test]
    fn simulate_dormancy_and_wake_cycle() {
        // interval=7200s, max_idle=8, cache_ttl=1h.
        // User goes away: 8 full ticks → dormant → pings only → user returns.
        let mut clock = clock_with_interval(7200, 8);
        clock.set_cache_refresh_interval(Some(3600));
        let mut now = Instant::now();

        clock.tick(now); // schedule

        // Fast-forward through 8 full ticks (16+ hours).
        for _ in 0..8 {
            now += Duration::from_secs(7201);
            // Skip cache pings between ticks.
            while clock.tick(now) != InteriorityAction::RunTick {
                now += Duration::from_secs(30);
            }
        }
        assert_eq!(clock.ticks_without_user(), 8);

        // 9th tick interval → goes dormant.
        now += Duration::from_secs(7201);
        loop {
            let action = clock.tick(now);
            if action == InteriorityAction::None && clock.state() == InteriorityState::Dormant {
                break;
            }
            now += Duration::from_secs(30);
        }
        assert_eq!(clock.state(), InteriorityState::Dormant);

        // While dormant, only cache pings fire.
        let mut dormant_pings = 0;
        for _ in 0..200 {
            now += Duration::from_secs(30);
            match clock.tick(now) {
                InteriorityAction::RunDormantPing => dormant_pings += 1,
                InteriorityAction::RunTick => panic!("Should not get RunTick while dormant"),
                InteriorityAction::None => {}
            }
        }
        // 200 * 30s = 6000s. With 3540s cache interval, expect 1-2 pings.
        assert!(
            dormant_pings >= 1 && dormant_pings <= 2,
            "Expected 1-2 dormant pings in 6000s, got {dormant_pings}"
        );

        // User returns → wake.
        clock.on_user_message(now);
        assert_eq!(clock.state(), InteriorityState::Active);
        assert_eq!(clock.ticks_without_user(), 0);

        // Next tick should be a full interiority tick after the interval.
        now += Duration::from_secs(7201);
        // Skip any cache pings.
        loop {
            let action = clock.tick(now);
            if action == InteriorityAction::RunTick {
                break;
            }
            assert_ne!(
                action,
                InteriorityAction::None,
                "Expected a tick or ping, not None after 7201s"
            );
            now += Duration::from_secs(30);
        }
    }
}
