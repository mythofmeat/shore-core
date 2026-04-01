//! Interiority clock — periodic "turns to self" replacing the heartbeat FSM.
//!
//! Simple timer with jitter and dormancy counter. Each tick gives the character
//! a full agentic turn with its existing tools plus scratchpad. The character
//! decides what to do — write notes, research things, or optionally message
//! the user via `<sendMessage>` tags.

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
    RunTick,
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

    /// Main tick method. Call on each autonomy tick interval (~30s).
    /// Returns what the manager should do.
    pub fn tick(&mut self, now: Instant) -> InteriorityAction {
        if self.paused || self.state == InteriorityState::Dormant {
            return InteriorityAction::None;
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

    /// Snap the next scheduled tick forward to `deadline` if it falls after
    /// the deadline but within the jitter range. This lets an interiority tick
    /// serve as a cache keepalive, avoiding a redundant ping.
    pub fn snap_to_deadline(&mut self, deadline: Instant) {
        let Some(next) = self.next_tick_at else { return };
        if next <= deadline {
            return; // Already fires before deadline — no snap needed.
        }
        // Only snap if the pull-forward distance is within the jitter range.
        // Beyond that, let keepalive handle it independently.
        let max_snap = Duration::from_secs_f64(self.interval_secs as f64 * self.jitter_factor);
        if next.duration_since(deadline) <= max_snap {
            self.next_tick_at = Some(deadline);
        }
    }

    fn schedule_next(&mut self, now: Instant) {
        let base = self.interval_secs as f64;
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
    fn dormant_returns_none() {
        let mut clock = clock_with_interval(60, 3);
        clock.restore(InteriorityState::Dormant, 5);
        let now = Instant::now();
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

    // -- snap_to_deadline tests -------------------------------------------

    #[test]
    fn snap_when_after_deadline_within_jitter() {
        // interval=100, jitter=0.25 → max_snap=25s
        let config = InteriorityConfig {
            enabled: true,
            interval_secs: 100,
            jitter_factor: 0.25,
            max_idle_ticks: 3,
            max_tool_rounds: 3,
        };
        let mut clock = InteriorityClock::with_config(&config);
        let now = Instant::now();
        clock.tick(now); // schedule next (jitter=0 in with_config? no, jitter_factor is set)

        // Force next_tick_at to a known value.
        let deadline = now + Duration::from_secs(90);
        clock.next_tick_at = Some(deadline + Duration::from_secs(20)); // 20s after deadline, within 25s max_snap

        clock.snap_to_deadline(deadline);
        assert_eq!(clock.next_tick_at, Some(deadline));
    }

    #[test]
    fn no_snap_when_after_deadline_beyond_jitter() {
        let config = InteriorityConfig {
            enabled: true,
            interval_secs: 100,
            jitter_factor: 0.25,
            max_idle_ticks: 3,
            max_tool_rounds: 3,
        };
        let mut clock = InteriorityClock::with_config(&config);
        let now = Instant::now();

        let deadline = now + Duration::from_secs(90);
        let original = deadline + Duration::from_secs(30); // 30s after deadline, beyond 25s max_snap
        clock.next_tick_at = Some(original);

        clock.snap_to_deadline(deadline);
        assert_eq!(clock.next_tick_at, Some(original)); // unchanged
    }

    #[test]
    fn no_snap_when_before_deadline() {
        let config = InteriorityConfig {
            enabled: true,
            interval_secs: 100,
            jitter_factor: 0.25,
            max_idle_ticks: 3,
            max_tool_rounds: 3,
        };
        let mut clock = InteriorityClock::with_config(&config);
        let now = Instant::now();

        let deadline = now + Duration::from_secs(90);
        let original = deadline - Duration::from_secs(10); // before deadline
        clock.next_tick_at = Some(original);

        clock.snap_to_deadline(deadline);
        assert_eq!(clock.next_tick_at, Some(original)); // unchanged
    }

    #[test]
    fn no_snap_when_no_tick_scheduled() {
        let config = InteriorityConfig {
            enabled: true,
            interval_secs: 100,
            jitter_factor: 0.25,
            max_idle_ticks: 3,
            max_tool_rounds: 3,
        };
        let mut clock = InteriorityClock::with_config(&config);
        let now = Instant::now();
        let deadline = now + Duration::from_secs(90);

        clock.snap_to_deadline(deadline); // next_tick_at is None
        assert_eq!(clock.next_tick_at, None);
    }
}
