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
    next_tick_at: Option<Instant>,
    last_user_ts: Option<Instant>,
    /// Cache TTL in seconds (from provider config). When set, the effective
    /// tick interval becomes `min(interval_secs, cache_ttl - 60)` so that
    /// every tick also refreshes the prompt cache.
    cache_refresh_interval_secs: Option<u64>,
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
            self.schedule_next(Instant::now());
        }
    }

    /// Set the cache TTL so the clock can compute an effective interval that
    /// keeps the prompt cache warm. Pass `None` for non-Anthropic providers.
    pub fn set_cache_refresh_interval(&mut self, cache_ttl_secs: Option<u64>) {
        self.cache_refresh_interval_secs =
            cache_ttl_secs.map(|ttl| ttl.saturating_sub(60).max(60));
    }

    /// The actual interval used for scheduling — the minimum of the configured
    /// interiority interval and the cache refresh interval (if set).
    pub fn effective_interval_secs(&self) -> u64 {
        match self.cache_refresh_interval_secs {
            Some(refresh) => self.interval_secs.min(refresh),
            None => self.interval_secs,
        }
    }

    /// Main tick method. Call on each autonomy tick interval (~30s).
    /// Returns what the manager should do.
    pub fn tick(&mut self, now: Instant) -> InteriorityAction {
        if self.paused {
            return InteriorityAction::None;
        }

        // When dormant, still track the timer for cache refresh pings.
        if self.state == InteriorityState::Dormant {
            return self.tick_dormant(now);
        }

        // First tick: schedule and return.
        if self.next_tick_at.is_none() {
            self.schedule_next(now);
            return InteriorityAction::None;
        }

        let next = self.next_tick_at.unwrap();
        if now < next {
            return InteriorityAction::None;
        }

        // Tick fires.
        self.ticks_without_user += 1;

        if self.ticks_without_user > self.max_idle_ticks {
            self.state = InteriorityState::Dormant;
            // Schedule the dormant timer immediately so it starts ticking.
            self.schedule_next(now);
            return InteriorityAction::None;
        }

        self.schedule_next(now);
        InteriorityAction::RunTick
    }

    /// Call when a user message arrives.
    pub fn on_user_message(&mut self, now: Instant) {
        self.ticks_without_user = 0;
        self.last_user_ts = Some(now);

        if self.state == InteriorityState::Dormant {
            self.state = InteriorityState::Active;
            self.schedule_next(now);
        }
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

    /// Dormant tick: if a cache refresh interval is configured, fire bare
    /// pings on the timer to keep the cache warm. Otherwise return None.
    fn tick_dormant(&mut self, now: Instant) -> InteriorityAction {
        if self.cache_refresh_interval_secs.is_none() {
            return InteriorityAction::None;
        }

        if self.next_tick_at.is_none() {
            self.schedule_next(now);
            return InteriorityAction::None;
        }

        let next = self.next_tick_at.unwrap();
        if now < next {
            return InteriorityAction::None;
        }

        self.schedule_next(now);
        InteriorityAction::RunDormantPing
    }

    fn schedule_next(&mut self, now: Instant) {
        let base = self.effective_interval_secs() as f64;
        let jitter_range = base * self.jitter_factor;
        let jitter: f64 = rand::thread_rng().gen_range(-jitter_range..=jitter_range);
        let secs = (base + jitter).max(60.0); // minimum 60s
        self.next_tick_at = Some(now + Duration::from_secs_f64(secs));
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

    // -- effective interval tests ----------------------------------------

    #[test]
    fn effective_interval_no_cache() {
        let clock = clock_with_interval(3600, 3);
        assert_eq!(clock.effective_interval_secs(), 3600);
    }

    #[test]
    fn effective_interval_with_cache_shorter() {
        let mut clock = clock_with_interval(7200, 3);
        clock.set_cache_refresh_interval(Some(3600)); // 1h TTL → 3540s refresh
        assert_eq!(clock.effective_interval_secs(), 3540);
    }

    #[test]
    fn effective_interval_with_cache_longer() {
        let mut clock = clock_with_interval(1800, 3); // 30 min interiority
        clock.set_cache_refresh_interval(Some(7200)); // 2h TTL → 7140s refresh
        // Interiority interval is shorter, so it wins.
        assert_eq!(clock.effective_interval_secs(), 1800);
    }

    #[test]
    fn effective_interval_cache_floor() {
        let mut clock = clock_with_interval(3600, 3);
        // Very short TTL — saturating_sub(60) floors at 60.
        clock.set_cache_refresh_interval(Some(30));
        assert_eq!(clock.effective_interval_secs(), 60);
    }

    // -- dormant ping tests ----------------------------------------------

    #[test]
    fn dormant_with_cache_fires_ping() {
        let mut clock = clock_with_interval(60, 1);
        clock.set_cache_refresh_interval(Some(120)); // 2min TTL → 60s refresh
        let mut now = Instant::now();

        // Go active → tick → dormant.
        clock.tick(now); // schedule
        now += Duration::from_secs(61);
        clock.tick(now); // tick 1 (RunTick)
        now += Duration::from_secs(61);
        clock.tick(now); // → dormant
        assert_eq!(clock.state(), InteriorityState::Dormant);

        // Dormant tick should schedule and then fire a ping.
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
    fn dormant_ping_respects_effective_interval() {
        let mut clock = clock_with_interval(60, 1);
        clock.set_cache_refresh_interval(Some(120)); // effective = 60s
        let mut now = Instant::now();

        // Push to dormant.
        clock.tick(now);
        now += Duration::from_secs(61);
        clock.tick(now);
        now += Duration::from_secs(61);
        clock.tick(now); // dormant

        // First dormant tick schedules.
        now += Duration::from_secs(10);
        assert_eq!(clock.tick(now), InteriorityAction::None); // just scheduled

        // Not enough time elapsed.
        now += Duration::from_secs(30);
        assert_eq!(clock.tick(now), InteriorityAction::None);

        // After full interval, fires.
        now += Duration::from_secs(40);
        assert_eq!(clock.tick(now), InteriorityAction::RunDormantPing);
    }
}
