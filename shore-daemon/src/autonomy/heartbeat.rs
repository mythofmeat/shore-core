//! 5-state heartbeat scheduler for autonomous character outreach.
//!
//! States: Session → PostSessionProbe → Deferred / SocialNeed → Dormant.
//! See §13.1 of ARCHITECTURE.md for the full spec.

use chrono::{Duration, NaiveDateTime};
use std::time::{Duration as StdDuration, Instant};

use super::activity::{ActivityStats, ANOMALY_Z_SCORE, SESSION_GAP};
use super::time_parse::{parse_time_expression, TimeParseResult};
use super::timing::{
    compute_tau, roll_probability, roll_succeeds, TauParams, MAX_DEFERRAL_HOURS,
    SOCIAL_NEED_CHECK_SECS, SOCIAL_NEED_JITTER,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Minimum session duration (seconds) before a post-session probe is eligible.
pub const SESSION_PROBE_FLOOR: u64 = 180;

/// Default consecutive unanswered autonomous messages before going dormant.
pub const DORMANT_THRESHOLD: u32 = 1;

// ---------------------------------------------------------------------------
// Prompt templates
// ---------------------------------------------------------------------------

/// Post-session probe template. Placeholders: `{idle_duration}`, `{current_time}`.
pub const PROMPT_POST_SESSION: &str = "\
The user has been idle for {idle_duration}. It is currently {current_time}.\n\
\n\
Would you like to reach out to them later? If so, suggest a specific time \
(e.g., \"8:30 PM\", \"in 3 hours\", \"tomorrow morning\").\n\
If you'd rather not reach out, say so.\n\
\n\
Your response will NOT be shown to the user.";

/// Deferred follow-up template. Placeholders: `{reasoning}`, `{current_time}`.
pub const PROMPT_DEFERRED: &str = "\
Earlier, when the user went idle, you decided: \"{reasoning}\"\n\
\n\
It's now {current_time} — the time you chose to reach out.\n\
Write a natural message to the user.";

/// Social-need template. Placeholder: `{anomaly_context}`.
pub const PROMPT_SOCIAL_NEED: &str = "\
You haven't heard from the user in a while.\n\
{anomaly_context}\n\
If you'd like, write a natural message to reach out.";

// ---------------------------------------------------------------------------
// Prompt rendering helpers
// ---------------------------------------------------------------------------

/// Render the post-session probe prompt.
pub fn render_post_session(idle_secs: u64, current_time: &NaiveDateTime) -> String {
    PROMPT_POST_SESSION
        .replace("{idle_duration}", &format_duration(idle_secs))
        .replace(
            "{current_time}",
            &current_time.format("%I:%M %p").to_string(),
        )
}

/// Render the deferred follow-up prompt.
pub fn render_deferred(reasoning: &str, current_time: &NaiveDateTime) -> String {
    PROMPT_DEFERRED
        .replace("{reasoning}", reasoning)
        .replace(
            "{current_time}",
            &current_time.format("%I:%M %p").to_string(),
        )
}

/// Render the social-need prompt.
pub fn render_social_need(anomaly_context: bool) -> String {
    let context = if anomaly_context {
        "Their absence is unusual based on your conversation patterns."
    } else {
        ""
    };
    PROMPT_SOCIAL_NEED.replace("{anomaly_context}", context)
}

fn format_duration(secs: u64) -> String {
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

// ---------------------------------------------------------------------------
// State types
// ---------------------------------------------------------------------------

/// The five states of the heartbeat scheduler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatState {
    /// User is actively chatting (or scheduler just started).
    Session,
    /// One-shot probe after session idle — awaiting LLM response.
    PostSessionProbe,
    /// Character chose a time; waiting for timer to fire.
    Deferred {
        fire_at: NaiveDateTime,
        reasoning: String,
    },
    /// Spontaneous probabilistic outreach.
    SocialNeed,
    /// Too many unanswered messages — silent until user returns.
    Dormant,
}

/// Action returned by [`HeartbeatScheduler::tick`] telling the caller what to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatAction {
    /// Nothing to do this tick.
    None,
    /// Generate a post-session probe via LLM. Caller should pass the response
    /// to [`HeartbeatScheduler::handle_probe_response`].
    GenerateProbe {
        idle_secs: u64,
        current_time: NaiveDateTime,
    },
    /// Timer fired — generate the deferred follow-up message and push as
    /// `NewMessage` to SWP clients.
    GenerateDeferredMessage { reasoning: String },
    /// Social-need roll succeeded — generate a spontaneous message and push
    /// as `NewMessage` to SWP clients.
    GenerateSocialNeedMessage { anomaly_context: bool },
}

/// Result of handling a probe response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeResult {
    /// Character chose a time; now in Deferred state.
    Deferred(NaiveDateTime),
    /// Character declined; now in SocialNeed state.
    Declined,
}

// ---------------------------------------------------------------------------
// HeartbeatScheduler
// ---------------------------------------------------------------------------

pub struct HeartbeatScheduler {
    state: HeartbeatState,
    paused: bool,
    /// Configurable dormant threshold (overrides DORMANT_THRESHOLD constant).
    dormant_threshold: u32,
    /// Idle gap (seconds) before session → probe transition.
    session_gap_secs: u64,
    /// Minimum session duration (seconds) before a probe can fire.
    session_probe_floor_secs: u64,
    last_user_ts: Option<Instant>,
    last_assistant_ts: Option<Instant>,
    /// When the current Session period began (for session_probe_floor_secs).
    session_start: Option<Instant>,
    /// Consecutive autonomous messages without a user reply.
    unanswered_count: u32,
    /// Last time a social-need probability check was performed.
    last_social_check: Option<Instant>,
    /// Next scheduled social-need check time (with jitter applied).
    next_social_check_at: Option<Instant>,
    /// Cumulative probability of no successful roll since entering SocialNeed.
    /// Social need bar = 1.0 - cumulative_no_hit.
    cumulative_no_hit: f64,
}

impl HeartbeatScheduler {
    pub fn new() -> Self {
        Self::with_config(DORMANT_THRESHOLD, SESSION_GAP, SESSION_PROBE_FLOOR)
    }

    /// Create a scheduler with a custom dormant threshold (uses default gap/floor).
    pub fn with_threshold(dormant_threshold: u32) -> Self {
        Self::with_config(dormant_threshold, SESSION_GAP, SESSION_PROBE_FLOOR)
    }

    /// Create a scheduler with full config control over timing.
    pub fn with_config(dormant_threshold: u32, session_gap_secs: u64, session_probe_floor_secs: u64) -> Self {
        Self {
            state: HeartbeatState::Session,
            paused: false,
            dormant_threshold,
            session_gap_secs,
            session_probe_floor_secs,
            last_user_ts: None,
            last_assistant_ts: None,
            session_start: None,
            unanswered_count: 0,
            last_social_check: None,
            next_social_check_at: None,
            cumulative_no_hit: 1.0,
        }
    }

    // -- accessors ------------------------------------------------------------

    pub fn state(&self) -> &HeartbeatState {
        &self.state
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn unanswered_count(&self) -> u32 {
        self.unanswered_count
    }

    /// Cumulative probability that at least one social-need roll has succeeded
    /// since entering SocialNeed state. Ranges from 0.0 (just entered) to ~1.0.
    pub fn social_need_bar(&self) -> f64 {
        1.0 - self.cumulative_no_hit
    }

    // -- control --------------------------------------------------------------

    /// Toggle pause/resume. Returns the new paused state.
    pub fn toggle_pause(&mut self) -> bool {
        self.paused = !self.paused;
        self.paused
    }

    /// Explicitly set the pause state. Returns the new state.
    pub fn set_paused(&mut self, paused: bool) -> bool {
        self.paused = paused;
        self.paused
    }

    /// Restore persisted state (heartbeat state and unanswered count).
    pub fn restore(&mut self, state: HeartbeatState, unanswered_count: u32) {
        self.state = state;
        self.unanswered_count = unanswered_count;
        self.cumulative_no_hit = 1.0;
        self.next_social_check_at = None;
    }

    /// Returns the dormant threshold.
    pub fn dormant_threshold(&self) -> u32 {
        self.dormant_threshold
    }

    // -- event handlers -------------------------------------------------------

    /// Call when the user sends a message. Resets unanswered count and
    /// returns to Session state from any other state.
    pub fn on_user_message(&mut self, now: Instant) {
        self.last_user_ts = Some(now);
        self.unanswered_count = 0;
        self.cumulative_no_hit = 1.0;
        self.next_social_check_at = None;

        if !matches!(self.state, HeartbeatState::Session) {
            self.state = HeartbeatState::Session;
            self.session_start = Some(now);
            self.last_social_check = None;
        } else if self.session_start.is_none() {
            // First message in initial Session state.
            self.session_start = Some(now);
        }
    }

    /// Call when the assistant sends a (non-autonomous) message.
    pub fn on_assistant_message(&mut self, now: Instant) {
        self.last_assistant_ts = Some(now);
    }

    // -- main tick ------------------------------------------------------------

    /// Advance the state machine by one tick.
    ///
    /// * `stats` — latest activity statistics.
    /// * `params` — τ modulation parameters for social-need rolls.
    /// * `now` — monotonic instant (for elapsed-time calculations).
    /// * `wall_now` — wall-clock time (for deferred timer comparison).
    /// * `random_value` — value in `[0, 1)` for probability rolls.
    pub fn tick(
        &mut self,
        stats: &ActivityStats,
        params: &TauParams,
        now: Instant,
        wall_now: NaiveDateTime,
        random_value: f64,
    ) -> HeartbeatAction {
        if self.paused {
            return HeartbeatAction::None;
        }

        // Snapshot the state kind to avoid borrow conflict with &mut self methods.
        let is_session = matches!(self.state, HeartbeatState::Session);
        let is_deferred = matches!(self.state, HeartbeatState::Deferred { .. });
        let is_social = matches!(self.state, HeartbeatState::SocialNeed);

        if is_session {
            self.tick_session(now, wall_now)
        } else if is_deferred {
            self.tick_deferred(now, wall_now)
        } else if is_social {
            self.tick_social_need(stats, params, now, random_value)
        } else {
            // PostSessionProbe (waiting for handle_probe_response) or Dormant.
            HeartbeatAction::None
        }
    }

    // -- probe response -------------------------------------------------------

    /// Handle the character's response to a post-session probe.
    ///
    /// Parses a time expression from `response`. If a valid time is found,
    /// transitions to Deferred (capped at MAX_DEFERRAL_HOURS). Otherwise
    /// transitions to SocialNeed.
    pub fn handle_probe_response(
        &mut self,
        response: &str,
        now: Instant,
        wall_now: NaiveDateTime,
    ) -> ProbeResult {
        match parse_time_expression(response, wall_now) {
            TimeParseResult::Time(mut target) => {
                let max_time =
                    wall_now + Duration::hours(MAX_DEFERRAL_HOURS as i64);
                if target > max_time {
                    target = max_time;
                }
                self.state = HeartbeatState::Deferred {
                    fire_at: target,
                    reasoning: response.to_string(),
                };
                ProbeResult::Deferred(target)
            }
            TimeParseResult::Declined => {
                self.state = HeartbeatState::SocialNeed;
                self.last_social_check = Some(now);
                self.cumulative_no_hit = 1.0;
                self.next_social_check_at = Some(now + StdDuration::from_secs_f64(SOCIAL_NEED_CHECK_SECS));
                ProbeResult::Declined
            }
        }
    }

    // -- tick sub-handlers ----------------------------------------------------

    fn tick_session(&mut self, now: Instant, wall_now: NaiveDateTime) -> HeartbeatAction {
        let last_activity = match (self.last_user_ts, self.last_assistant_ts) {
            (Some(u), Some(a)) => Some(u.max(a)),
            (Some(u), None) => Some(u),
            (None, Some(a)) => Some(a),
            (None, None) => None,
        };

        let idle_secs = match last_activity {
            Some(last) => now.duration_since(last).as_secs(),
            None => return HeartbeatAction::None,
        };

        let session_duration = self
            .session_start
            .map(|s| now.duration_since(s).as_secs())
            .unwrap_or(0);

        if idle_secs >= self.session_gap_secs && session_duration >= self.session_probe_floor_secs {
            self.state = HeartbeatState::PostSessionProbe;
            return HeartbeatAction::GenerateProbe {
                idle_secs,
                current_time: wall_now,
            };
        }

        HeartbeatAction::None
    }

    fn tick_deferred(&mut self, now: Instant, wall_now: NaiveDateTime) -> HeartbeatAction {
        let (fire_at, reasoning) =
            if let HeartbeatState::Deferred { fire_at, reasoning } = &self.state {
                (*fire_at, reasoning.clone())
            } else {
                return HeartbeatAction::None;
            };

        if wall_now >= fire_at {
            self.unanswered_count += 1;
            self.state = HeartbeatState::SocialNeed;
            self.last_social_check = Some(now);
            self.cumulative_no_hit = 1.0;
            self.next_social_check_at = Some(now + StdDuration::from_secs_f64(SOCIAL_NEED_CHECK_SECS));
            return HeartbeatAction::GenerateDeferredMessage { reasoning };
        }

        HeartbeatAction::None
    }

    fn tick_social_need(
        &mut self,
        stats: &ActivityStats,
        params: &TauParams,
        now: Instant,
        random_value: f64,
    ) -> HeartbeatAction {
        // Check dormant threshold before rolling.
        if self.unanswered_count >= self.dormant_threshold {
            self.state = HeartbeatState::Dormant;
            return HeartbeatAction::None;
        }

        // Initialize check schedule if not set (e.g., after state restore).
        if self.next_social_check_at.is_none() {
            self.next_social_check_at = Some(now + StdDuration::from_secs_f64(SOCIAL_NEED_CHECK_SECS));
            self.last_social_check = Some(now);
            return HeartbeatAction::None;
        }

        // Wait for the jittered check interval to elapse.
        if self.next_social_check_at.is_some_and(|next| now < next) {
            return HeartbeatAction::None;
        }

        let elapsed = self
            .last_social_check
            .map_or(SOCIAL_NEED_CHECK_SECS, |lc| now.duration_since(lc).as_secs_f64());

        let tau = compute_tau(stats, params);
        let prob = roll_probability(elapsed, tau);

        // Update cumulative miss probability.
        self.cumulative_no_hit *= 1.0 - prob;

        // Schedule next jittered check: base interval × (1 ± jitter).
        let jitter_factor = 1.0 + (random_value * 2.0 - 1.0) * SOCIAL_NEED_JITTER;
        let next_interval = SOCIAL_NEED_CHECK_SECS * jitter_factor;
        self.next_social_check_at = Some(now + StdDuration::from_secs_f64(next_interval));
        self.last_social_check = Some(now);

        if roll_succeeds(prob, random_value) {
            self.unanswered_count += 1;
            let anomaly = stats
                .anomaly_z_score
                .is_some_and(|z| z >= ANOMALY_Z_SCORE);
            return HeartbeatAction::GenerateSocialNeedMessage {
                anomaly_context: anomaly,
            };
        }

        HeartbeatAction::None
    }
}

impl Default for HeartbeatScheduler {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autonomy::activity::HourClassification;
    use std::time::Duration as StdDuration;

    // -- helpers --------------------------------------------------------------

    fn make_stats(
        engagement: f64,
        sessions_per_day: f64,
        sufficient: bool,
        anomaly: Option<f64>,
    ) -> ActivityStats {
        ActivityStats {
            engagement_score: engagement,
            consistency: 0.8,
            tempo_score: 0.7,
            session_count: 4,
            sessions_per_day,
            hour_histogram: [0.0; 24],
            hour_classifications: [HourClassification::Normal; 24],
            has_sufficient_data: sufficient,
            has_sufficient_heatmap: false,
            median_session_gap: Some(7200.0),
            anomaly_z_score: anomaly,
            computed_at: Instant::now(),
        }
    }

    fn default_params() -> TauParams {
        TauParams {
            reciprocated: true,
            hour_class: HourClassification::Normal,
            personality: 0.5,
        }
    }

    fn wall(hour: u32, min: u32) -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 3, 25)
            .unwrap()
            .and_hms_opt(hour, min, 0)
            .unwrap()
    }

    // -- Session → PostSessionProbe -------------------------------------------

    #[test]
    fn test_session_to_probe_on_idle() {
        let mut sched = HeartbeatScheduler::new();
        let t0 = Instant::now();

        // Record user message to start the session.
        sched.on_user_message(t0);
        // Record assistant response.
        sched.on_assistant_message(t0 + StdDuration::from_secs(5));

        // Tick before SESSION_GAP — should stay in Session.
        let stats = make_stats(0.8, 4.0, true, None);
        let action = sched.tick(
            &stats,
            &default_params(),
            t0 + StdDuration::from_secs(SESSION_GAP - 1),
            wall(14, 29),
            0.5,
        );
        assert_eq!(action, HeartbeatAction::None);
        assert!(matches!(sched.state(), HeartbeatState::Session));

        // Tick past SESSION_GAP from last activity (assistant at t0+5).
        // idle = (SESSION_GAP + 10) - 5 = SESSION_GAP + 5 ≥ SESSION_GAP ✓
        let action = sched.tick(
            &stats,
            &default_params(),
            t0 + StdDuration::from_secs(SESSION_GAP + 10),
            wall(14, 31),
            0.5,
        );
        assert!(matches!(action, HeartbeatAction::GenerateProbe { .. }));
        assert!(matches!(sched.state(), HeartbeatState::PostSessionProbe));
    }

    #[test]
    fn test_session_no_probe_without_messages() {
        let mut sched = HeartbeatScheduler::new();
        let t0 = Instant::now();
        let stats = make_stats(0.8, 4.0, true, None);

        // No messages recorded — should not probe.
        let action = sched.tick(
            &stats,
            &default_params(),
            t0 + StdDuration::from_secs(SESSION_GAP + 100),
            wall(15, 0),
            0.5,
        );
        assert_eq!(action, HeartbeatAction::None);
    }

    #[test]
    fn test_session_probe_floor_prevents_short_session() {
        let mut sched = HeartbeatScheduler::new();
        let t0 = Instant::now();

        // session_start will be set by on_user_message.
        sched.on_user_message(t0);

        // Advance by SESSION_GAP but session_start is only SESSION_PROBE_FLOOR - 1 ago.
        // This can happen if session_start is very close to the last activity.
        // In practice, idle >= SESSION_GAP implies session_duration >= SESSION_GAP,
        // so this is a redundancy check. We test it by manipulating session_start
        // to be recent while having an old last_user_ts.

        // Set a very early last_user_ts manually for testing.
        let early = t0 - StdDuration::from_secs(SESSION_GAP + 10);
        sched.last_user_ts = Some(early);
        // But session_start is recent (less than floor).
        sched.session_start = Some(t0);

        let stats = make_stats(0.8, 4.0, true, None);
        let action = sched.tick(
            &stats,
            &default_params(),
            t0 + StdDuration::from_secs(SESSION_PROBE_FLOOR - 1),
            wall(14, 2),
            0.5,
        );
        // idle from early user_ts is huge, but session_duration < floor.
        assert_eq!(action, HeartbeatAction::None);
        assert!(matches!(sched.state(), HeartbeatState::Session));
    }

    // -- PostSessionProbe → Deferred / SocialNeed -----------------------------

    #[test]
    fn test_probe_response_with_time_goes_to_deferred() {
        let mut sched = HeartbeatScheduler::new();
        sched.state = HeartbeatState::PostSessionProbe;
        let t0 = Instant::now();

        let result = sched.handle_probe_response("8:30 PM", t0, wall(14, 0));
        assert!(matches!(result, ProbeResult::Deferred(_)));
        assert!(matches!(sched.state(), HeartbeatState::Deferred { .. }));

        if let HeartbeatState::Deferred { fire_at, .. } = sched.state() {
            assert_eq!(*fire_at, wall(20, 30));
        }
    }

    #[test]
    fn test_probe_response_decline_goes_to_social_need() {
        let mut sched = HeartbeatScheduler::new();
        sched.state = HeartbeatState::PostSessionProbe;
        let t0 = Instant::now();

        let result = sched.handle_probe_response("No thanks, I'll wait", t0, wall(14, 0));
        assert_eq!(result, ProbeResult::Declined);
        assert!(matches!(sched.state(), HeartbeatState::SocialNeed));
    }

    #[test]
    fn test_probe_response_caps_deferral() {
        let mut sched = HeartbeatScheduler::new();
        sched.state = HeartbeatState::PostSessionProbe;
        let t0 = Instant::now();

        // "tomorrow morning" is ~19h from 14:00 — within MAX_DEFERRAL_HOURS (24h).
        let result = sched.handle_probe_response("tomorrow morning", t0, wall(14, 0));
        if let ProbeResult::Deferred(time) = result {
            let max = wall(14, 0) + Duration::hours(MAX_DEFERRAL_HOURS as i64);
            assert!(time <= max);
        }
    }

    // -- Deferred → fires → SocialNeed ----------------------------------------

    #[test]
    fn test_deferred_fires_on_time() {
        let mut sched = HeartbeatScheduler::new();
        let t0 = Instant::now();

        sched.state = HeartbeatState::Deferred {
            fire_at: wall(20, 30),
            reasoning: "I'll check in at 8:30 PM".to_string(),
        };

        let stats = make_stats(0.8, 4.0, true, None);

        // Before fire_at — no action.
        let action = sched.tick(
            &stats,
            &default_params(),
            t0 + StdDuration::from_secs(100),
            wall(20, 29),
            0.5,
        );
        assert_eq!(action, HeartbeatAction::None);

        // At/after fire_at — fires.
        let action = sched.tick(
            &stats,
            &default_params(),
            t0 + StdDuration::from_secs(200),
            wall(20, 30),
            0.5,
        );
        assert!(matches!(
            action,
            HeartbeatAction::GenerateDeferredMessage { .. }
        ));
        assert!(matches!(sched.state(), HeartbeatState::SocialNeed));
        assert_eq!(sched.unanswered_count(), 1);
    }

    // -- Deferred → user messages → Session -----------------------------------

    #[test]
    fn test_deferred_discarded_on_user_message() {
        let mut sched = HeartbeatScheduler::new();
        sched.state = HeartbeatState::Deferred {
            fire_at: wall(20, 30),
            reasoning: "checking in later".to_string(),
        };

        sched.on_user_message(Instant::now());
        assert!(matches!(sched.state(), HeartbeatState::Session));
    }

    // -- SocialNeed → message delivery ----------------------------------------

    #[test]
    fn test_social_need_roll_succeeds() {
        let mut sched = HeartbeatScheduler::new();
        let t0 = Instant::now();

        sched.state = HeartbeatState::SocialNeed;
        sched.last_social_check = Some(t0);
        sched.next_social_check_at = Some(t0); // Allow immediate check.

        let stats = make_stats(0.8, 4.0, true, None);

        // Use random_value = 0.0 (always succeeds) and large elapsed time.
        let action = sched.tick(
            &stats,
            &default_params(),
            t0 + StdDuration::from_secs(7200),
            wall(16, 0),
            0.0, // guaranteed success
        );
        assert!(matches!(
            action,
            HeartbeatAction::GenerateSocialNeedMessage {
                anomaly_context: false
            }
        ));
        assert_eq!(sched.unanswered_count(), 1);
    }

    #[test]
    fn test_social_need_roll_fails() {
        let mut sched = HeartbeatScheduler::new();
        let t0 = Instant::now();

        sched.state = HeartbeatState::SocialNeed;
        sched.last_social_check = Some(t0);
        sched.next_social_check_at = Some(t0); // Allow immediate check.

        let stats = make_stats(0.8, 4.0, true, None);

        // Elapsed = 1800s (one check interval), random_value = 0.999 → likely fails.
        let action = sched.tick(
            &stats,
            &default_params(),
            t0 + StdDuration::from_secs(1800),
            wall(14, 0),
            0.999,
        );
        assert_eq!(action, HeartbeatAction::None);
        assert_eq!(sched.unanswered_count(), 0);
        // Cumulative bar should have advanced.
        assert!(sched.social_need_bar() > 0.0);
    }

    #[test]
    fn test_social_need_anomaly_context() {
        let mut sched = HeartbeatScheduler::new();
        let t0 = Instant::now();

        sched.state = HeartbeatState::SocialNeed;
        sched.last_social_check = Some(t0);
        sched.next_social_check_at = Some(t0); // Allow immediate check.

        // z-score 2.0 > ANOMALY_Z_SCORE (1.5) → anomaly_context = true.
        let stats = make_stats(0.8, 4.0, true, Some(2.0));

        let action = sched.tick(
            &stats,
            &default_params(),
            t0 + StdDuration::from_secs(7200),
            wall(16, 0),
            0.0,
        );
        assert_eq!(
            action,
            HeartbeatAction::GenerateSocialNeedMessage {
                anomaly_context: true
            }
        );
    }

    // -- SocialNeed → Dormant -------------------------------------------------

    #[test]
    fn test_social_need_goes_dormant_at_threshold() {
        let mut sched = HeartbeatScheduler::new();
        let t0 = Instant::now();

        sched.state = HeartbeatState::SocialNeed;
        sched.last_social_check = Some(t0);
        sched.unanswered_count = DORMANT_THRESHOLD;

        let stats = make_stats(0.8, 4.0, true, None);

        let action = sched.tick(
            &stats,
            &default_params(),
            t0 + StdDuration::from_secs(100),
            wall(14, 0),
            0.0,
        );
        assert_eq!(action, HeartbeatAction::None);
        assert!(matches!(sched.state(), HeartbeatState::Dormant));
    }

    #[test]
    fn test_social_need_increments_toward_dormant() {
        let mut sched = HeartbeatScheduler::new();
        let t0 = Instant::now();

        sched.state = HeartbeatState::SocialNeed;
        sched.last_social_check = Some(t0);
        sched.next_social_check_at = Some(t0); // Allow immediate check.

        let stats = make_stats(0.8, 4.0, true, None);

        // Send DORMANT_THRESHOLD messages (all succeed).
        for i in 0..DORMANT_THRESHOLD {
            let elapsed = StdDuration::from_secs((i as u64 + 1) * 7200);
            // Ensure the check fires by setting next_social_check_at to the past.
            sched.next_social_check_at = Some(t0);
            let action = sched.tick(
                &stats,
                &default_params(),
                t0 + elapsed,
                wall(14, 0),
                0.0, // always succeed
            );
            if i < DORMANT_THRESHOLD {
                // Messages sent until threshold reached.
                if matches!(sched.state(), HeartbeatState::Dormant) {
                    break;
                }
                assert!(matches!(
                    action,
                    HeartbeatAction::GenerateSocialNeedMessage { .. }
                ));
            }
        }

        // After threshold, next tick should transition to Dormant.
        let action = sched.tick(
            &stats,
            &default_params(),
            t0 + StdDuration::from_secs(100_000),
            wall(14, 0),
            0.0,
        );
        assert_eq!(action, HeartbeatAction::None);
        assert!(matches!(sched.state(), HeartbeatState::Dormant));
    }

    // -- Social need check interval and cumulative bar --------------------------

    #[test]
    fn test_social_need_skips_before_interval() {
        let mut sched = HeartbeatScheduler::new();
        let t0 = Instant::now();

        sched.state = HeartbeatState::SocialNeed;
        sched.last_social_check = Some(t0);
        // Next check at t0 + 30 min.
        sched.next_social_check_at = Some(t0 + StdDuration::from_secs(1800));

        let stats = make_stats(0.8, 4.0, true, None);

        // Tick at t0 + 10 min — too early, should skip.
        let action = sched.tick(
            &stats,
            &default_params(),
            t0 + StdDuration::from_secs(600),
            wall(14, 10),
            0.0,
        );
        assert_eq!(action, HeartbeatAction::None);
        assert_eq!(sched.unanswered_count(), 0);
        // Bar should not have changed — no roll happened.
        assert!((sched.social_need_bar() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_social_need_cumulative_bar_grows() {
        let mut sched = HeartbeatScheduler::new();
        let t0 = Instant::now();

        sched.state = HeartbeatState::SocialNeed;
        sched.last_social_check = Some(t0);
        sched.next_social_check_at = Some(t0); // Allow immediate check.

        let stats = make_stats(0.8, 4.0, true, None);

        // First failed roll — bar should increase from 0.
        let action = sched.tick(
            &stats,
            &default_params(),
            t0 + StdDuration::from_secs(1800),
            wall(14, 30),
            0.999,
        );
        assert_eq!(action, HeartbeatAction::None);
        let bar_after_one = sched.social_need_bar();
        assert!(bar_after_one > 0.0, "bar should grow after a roll");

        // Second failed roll — bar should grow further.
        sched.next_social_check_at = Some(t0 + StdDuration::from_secs(1800));
        let action = sched.tick(
            &stats,
            &default_params(),
            t0 + StdDuration::from_secs(3600),
            wall(15, 0),
            0.999,
        );
        assert_eq!(action, HeartbeatAction::None);
        let bar_after_two = sched.social_need_bar();
        assert!(bar_after_two > bar_after_one, "bar should grow monotonically");
    }

    #[test]
    fn test_social_need_bar_resets_on_user_message() {
        let mut sched = HeartbeatScheduler::new();
        let t0 = Instant::now();

        sched.state = HeartbeatState::SocialNeed;
        sched.last_social_check = Some(t0);
        sched.next_social_check_at = Some(t0);
        sched.cumulative_no_hit = 0.5; // Simulate accumulated rolls.

        assert!((sched.social_need_bar() - 0.5).abs() < f64::EPSILON);

        sched.on_user_message(t0 + StdDuration::from_secs(100));
        assert!((sched.social_need_bar() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_social_need_jittered_interval() {
        let mut sched = HeartbeatScheduler::new();
        let t0 = Instant::now();

        sched.state = HeartbeatState::SocialNeed;
        sched.last_social_check = Some(t0);
        sched.next_social_check_at = Some(t0);

        let stats = make_stats(0.8, 4.0, true, None);

        // After a tick, next_social_check_at should be set to a jittered interval.
        sched.tick(
            &stats,
            &default_params(),
            t0 + StdDuration::from_secs(1800),
            wall(14, 30),
            0.75, // random_value for jitter and roll
        );

        let next = sched.next_social_check_at.unwrap();
        let tick_time = t0 + StdDuration::from_secs(1800);
        let interval = next.duration_since(tick_time).as_secs_f64();

        // Jitter factor = 1.0 + (0.75 * 2.0 - 1.0) * 0.5 = 1.0 + 0.25 = 1.25
        // Expected interval = 1800 * 1.25 = 2250s
        let expected = 1800.0 * 1.25;
        assert!(
            (interval - expected).abs() < 1.0,
            "expected interval ~{expected}s, got {interval}s"
        );
    }

    // -- Dormant → user message → Session -------------------------------------

    #[test]
    fn test_dormant_returns_to_session_on_user_message() {
        let mut sched = HeartbeatScheduler::new();
        sched.state = HeartbeatState::Dormant;
        sched.unanswered_count = DORMANT_THRESHOLD;

        sched.on_user_message(Instant::now());

        assert!(matches!(sched.state(), HeartbeatState::Session));
        assert_eq!(sched.unanswered_count(), 0);
    }

    #[test]
    fn test_dormant_tick_does_nothing() {
        let mut sched = HeartbeatScheduler::new();
        sched.state = HeartbeatState::Dormant;

        let stats = make_stats(0.8, 4.0, true, None);
        let action = sched.tick(
            &stats,
            &default_params(),
            Instant::now(),
            wall(14, 0),
            0.0,
        );
        assert_eq!(action, HeartbeatAction::None);
        assert!(matches!(sched.state(), HeartbeatState::Dormant));
    }

    // -- User message resets from any state -----------------------------------

    #[test]
    fn test_user_message_resets_from_post_session_probe() {
        let mut sched = HeartbeatScheduler::new();
        sched.state = HeartbeatState::PostSessionProbe;
        sched.on_user_message(Instant::now());
        assert!(matches!(sched.state(), HeartbeatState::Session));
    }

    #[test]
    fn test_user_message_resets_from_social_need() {
        let mut sched = HeartbeatScheduler::new();
        sched.state = HeartbeatState::SocialNeed;
        sched.unanswered_count = 3;
        sched.on_user_message(Instant::now());
        assert!(matches!(sched.state(), HeartbeatState::Session));
        assert_eq!(sched.unanswered_count(), 0);
    }

    // -- Pause / resume -------------------------------------------------------

    #[test]
    fn test_toggle_pause() {
        let mut sched = HeartbeatScheduler::new();
        assert!(!sched.is_paused());

        let paused = sched.toggle_pause();
        assert!(paused);
        assert!(sched.is_paused());

        let paused = sched.toggle_pause();
        assert!(!paused);
    }

    #[test]
    fn test_paused_tick_does_nothing() {
        let mut sched = HeartbeatScheduler::new();
        let t0 = Instant::now();

        sched.on_user_message(t0);
        sched.toggle_pause();

        let stats = make_stats(0.8, 4.0, true, None);
        let action = sched.tick(
            &stats,
            &default_params(),
            t0 + StdDuration::from_secs(SESSION_GAP + 100),
            wall(15, 0),
            0.5,
        );
        assert_eq!(action, HeartbeatAction::None);
        // State should NOT have changed.
        assert!(matches!(sched.state(), HeartbeatState::Session));
    }

    #[test]
    fn test_resume_after_pause() {
        let mut sched = HeartbeatScheduler::new();
        let t0 = Instant::now();

        sched.on_user_message(t0);
        sched.on_assistant_message(t0 + StdDuration::from_secs(5));
        sched.toggle_pause(); // pause

        let stats = make_stats(0.8, 4.0, true, None);

        // Paused: no transition.
        sched.tick(
            &stats,
            &default_params(),
            t0 + StdDuration::from_secs(SESSION_GAP + 10),
            wall(14, 31),
            0.5,
        );
        assert!(matches!(sched.state(), HeartbeatState::Session));

        // Resume — idle from assistant at t0+5 is SESSION_GAP+5 ≥ SESSION_GAP.
        sched.toggle_pause();
        let action = sched.tick(
            &stats,
            &default_params(),
            t0 + StdDuration::from_secs(SESSION_GAP + 10),
            wall(14, 31),
            0.5,
        );
        assert!(matches!(action, HeartbeatAction::GenerateProbe { .. }));
    }

    // -- Prompt rendering -----------------------------------------------------

    #[test]
    fn test_render_post_session() {
        let prompt = render_post_session(1860, &wall(14, 31));
        assert!(prompt.contains("31m"));
        assert!(prompt.contains("02:31 PM"));

        // Test with hours.
        let prompt_h = render_post_session(7320, &wall(16, 0));
        assert!(prompt_h.contains("2h 2m"));
    }

    #[test]
    fn test_render_deferred() {
        let prompt = render_deferred("I'll check in at 8:30 PM", &wall(20, 30));
        assert!(prompt.contains("I'll check in at 8:30 PM"));
        assert!(prompt.contains("08:30 PM"));
    }

    #[test]
    fn test_render_social_need_with_anomaly() {
        let prompt = render_social_need(true);
        assert!(prompt.contains("unusual"));
    }

    #[test]
    fn test_render_social_need_without_anomaly() {
        let prompt = render_social_need(false);
        assert!(!prompt.contains("unusual"));
    }

    // -- Format duration ------------------------------------------------------

    #[test]
    fn test_format_duration_hours() {
        assert_eq!(format_duration(3720), "1h 2m");
    }

    #[test]
    fn test_format_duration_minutes_only() {
        assert_eq!(format_duration(300), "5m");
    }
}
