//! Human-like reply delay.
//!
//! When enabled (`[behavior.response_delay]`), the daemon holds a brief pause
//! before the character's reply begins streaming, so chatting feels less like
//! talking to an instant oracle. The pause:
//!
//! - scales with how long the user was silent before their message — rapid
//!   back-and-forth gets a short delay, a message after a long gap gets a
//!   longer one (capped),
//! - is jittered so the exact arrival time is never predictable,
//! - is bounded by a `min` floor and a hard `max` ceiling.
//!
//! This module is the pure curve: [`compute_delay`] takes the preceding gap,
//! the resolved [`ResponseDelayParams`], and an RNG, and returns the duration
//! to wait. It has no I/O so it is exhaustively unit-testable with a seeded
//! RNG; the manager owns enable-state, the pending-reply deadline, and the
//! activity lookup that feeds `last_gap`.

use std::time::Duration;

use rand::Rng;
use shore_config::app::ResponseDelayConfig;

/// Resolved, sanitized inputs to [`compute_delay`]. Built from
/// [`ResponseDelayConfig`] via [`ResponseDelayParams::from_config`].
#[derive(Debug, Clone, PartialEq)]
pub struct ResponseDelayParams {
    /// Whether the delay applies at all.
    pub enabled: bool,
    /// Shortest delay ever returned when enabled.
    pub min: Duration,
    /// Hard ceiling on the delay.
    pub max: Duration,
    /// Fraction of the preceding gap used as the base delay.
    pub scale: f64,
    /// Jitter spread as a fraction in `[0, 1]`.
    pub jitter: f64,
}

impl ResponseDelayParams {
    /// Build params from config, sanitizing values so [`compute_delay`] can
    /// never panic even if a config was constructed directly (config-load
    /// validation already rejects these for on-disk configs, see
    /// [`ResponseDelayConfig::validate`]). `min` is capped at `max` so the
    /// final `clamp` is always well-ordered.
    pub fn from_config(cfg: &ResponseDelayConfig) -> Self {
        let max = cfg.max.as_duration();
        let min = cfg.min.as_duration().min(max);
        let scale = if cfg.scale.is_finite() && cfg.scale >= 0.0 {
            cfg.scale
        } else {
            0.0
        };
        let jitter = if cfg.jitter.is_finite() {
            cfg.jitter.clamp(0.0, 1.0)
        } else {
            0.0
        };
        Self {
            enabled: cfg.enabled,
            min,
            max,
            scale,
            jitter,
        }
    }
}

/// Compute the delay to hold before streaming a reply.
///
/// `last_gap` is the time since the previous user message (`None` when there is
/// no prior message, e.g. the first turn — treated as a zero base so the reply
/// lands at the `min` floor). Returns [`Duration::ZERO`] when disabled.
///
/// The base delay is `scale × last_gap`; jitter multiplies it by a factor drawn
/// uniformly from `[1 - jitter, 1 + jitter]`; the result is clamped to
/// `[min, max]`. Because of the clamp, active conversation (tiny gap) settles at
/// `min` and a cold re-engagement (huge gap) settles at `max`.
#[expect(
    clippy::float_arithmetic,
    reason = "the response-delay curve scales and jitters a duration in f64 seconds"
)]
pub fn compute_delay<R: Rng>(
    last_gap: Option<Duration>,
    params: &ResponseDelayParams,
    rng: &mut R,
) -> Duration {
    if !params.enabled {
        return Duration::ZERO;
    }

    let gap_secs = last_gap.map_or(0.0, |g| g.as_secs_f64());
    let base = gap_secs * params.scale;

    let factor = if params.jitter > 0.0 {
        1.0 + rng.random_range(-params.jitter..=params.jitter)
    } else {
        1.0
    };

    // `base` and `factor` are both finite and non-negative-ish; guard the
    // product so `from_secs_f64` never sees a negative or NaN.
    let jittered_secs = (base * factor).max(0.0);
    let jittered = Duration::try_from_secs_f64(jittered_secs).unwrap_or(params.max);

    jittered.clamp(params.min, params.max)
}

/// Render the inline-system note that tells the character it kept the user
/// waiting, injected when the delay reaches `notify_after`. Rounds to a natural
/// unit and keeps the guidance soft so the character can acknowledge the wait
/// without being forced into an apology.
#[expect(
    clippy::integer_division,
    reason = "rounding a duration to whole minutes/hours for a human-readable note"
)]
pub fn format_delay_note(delay: Duration) -> String {
    let secs = delay.as_secs();
    let human = if secs < 90 {
        format!("{secs} seconds")
    } else if secs < 120 * 60 {
        // Round to the nearest minute.
        format!("{} minutes", secs.saturating_add(30) / 60)
    } else {
        // Round to the nearest hour (integer math — the daemon denies float).
        format!("{} hours", secs.saturating_add(1800) / 3600)
    };
    format!(
        "(You let about {human} pass before replying to the user's latest \
         message. It's fine to acknowledge the wait naturally if it fits — no \
         need to over-apologize or explain yourself.)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn params() -> ResponseDelayParams {
        ResponseDelayParams {
            enabled: true,
            min: Duration::from_secs(2),
            max: Duration::from_secs(90),
            scale: 0.1,
            jitter: 0.3,
        }
    }

    #[test]
    fn disabled_returns_zero() {
        let mut p = params();
        p.enabled = false;
        let mut rng = StdRng::seed_from_u64(1);
        assert_eq!(
            compute_delay(Some(Duration::from_hours(1)), &p, &mut rng),
            Duration::ZERO
        );
    }

    #[test]
    fn active_conversation_floors_at_min() {
        // A tiny preceding gap yields a base well under `min`, so the clamp
        // pins it to the floor regardless of jitter.
        let p = params();
        let mut rng = StdRng::seed_from_u64(2);
        for _ in 0..50 {
            let d = compute_delay(Some(Duration::from_secs(1)), &p, &mut rng);
            assert_eq!(d, p.min);
        }
    }

    #[test]
    fn cold_reengagement_caps_at_max() {
        // A huge preceding gap blows past `max` before jitter, so it always
        // clamps to the ceiling.
        let p = params();
        let mut rng = StdRng::seed_from_u64(3);
        for _ in 0..50 {
            let d = compute_delay(Some(Duration::from_hours(24)), &p, &mut rng);
            assert_eq!(d, p.max);
        }
    }

    #[test]
    fn no_prior_message_floors_at_min() {
        let p = params();
        let mut rng = StdRng::seed_from_u64(4);
        assert_eq!(compute_delay(None, &p, &mut rng), p.min);
    }

    #[test]
    fn midrange_gap_stays_within_bounds() {
        let p = params();
        let mut rng = StdRng::seed_from_u64(5);
        for _ in 0..200 {
            // 5 minutes * 0.1 = 30s base, within [2s, 90s] even after ±30%.
            let d = compute_delay(Some(Duration::from_mins(5)), &p, &mut rng);
            assert!(d >= p.min && d <= p.max, "delay {d:?} out of bounds");
        }
    }

    #[test]
    fn jitter_zero_is_deterministic() {
        let mut p = params();
        p.jitter = 0.0;
        let mut rng = StdRng::seed_from_u64(6);
        // 300s * 0.1 = 30s, no jitter, within bounds.
        let d = compute_delay(Some(Duration::from_mins(5)), &p, &mut rng);
        assert_eq!(d, Duration::from_secs(30));
    }

    #[test]
    fn jitter_actually_varies_the_delay() {
        let p = params();
        let mut rng = StdRng::seed_from_u64(7);
        let gap = Some(Duration::from_mins(5));
        let first = compute_delay(gap, &p, &mut rng);
        let differs = (0..20).any(|_| compute_delay(gap, &p, &mut rng) != first);
        assert!(differs, "jitter should produce varying delays");
    }

    #[test]
    fn format_delay_note_rounds_to_natural_units() {
        assert!(format_delay_note(Duration::from_secs(45)).contains("45 seconds"));
        // 35m05s rounds to 35 minutes.
        assert!(format_delay_note(Duration::from_secs(35 * 60 + 5)).contains("35 minutes"));
        // 2h29m rounds to 2 hours; 2h31m rounds to 3.
        assert!(
            format_delay_note(Duration::from_hours(2) + Duration::from_mins(29))
                .contains("2 hours")
        );
        assert!(
            format_delay_note(Duration::from_hours(2) + Duration::from_mins(31))
                .contains("3 hours")
        );
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "sanitized scale/jitter are assigned exact literals (0.0 / 1.0), so exact comparison is correct"
    )]
    fn from_config_sanitizes_bad_values() {
        let cfg = ResponseDelayConfig {
            enabled: true,
            min: shore_config::ConfigDuration::from_secs(100),
            max: shore_config::ConfigDuration::from_secs(10),
            scale: f64::NAN,
            jitter: 5.0,
            ..ResponseDelayConfig::default()
        };
        let p = ResponseDelayParams::from_config(&cfg);
        // min capped to max, scale defaulted, jitter clamped.
        assert_eq!(p.min, Duration::from_secs(10));
        assert_eq!(p.max, Duration::from_secs(10));
        assert_eq!(p.scale, 0.0);
        assert_eq!(p.jitter, 1.0);
        // And compute_delay stays panic-free with the sanitized params.
        let mut rng = StdRng::seed_from_u64(8);
        let d = compute_delay(Some(Duration::from_mins(5)), &p, &mut rng);
        assert_eq!(d, Duration::from_secs(10));
    }
}
