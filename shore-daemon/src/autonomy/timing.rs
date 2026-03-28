use super::activity::{ActivityStats, HourClassification};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// τ used before sufficient data is collected (3 hours).
pub const WARMUP_TAU: f64 = 10800.0;

/// Base interval between social-need probability checks (30 minutes).
pub const SOCIAL_NEED_CHECK_SECS: f64 = 1800.0;

/// Jitter fraction applied to social-need check intervals (±50%).
/// Each check is scheduled at `SOCIAL_NEED_CHECK_SECS × (1 ± SOCIAL_NEED_JITTER)`,
/// giving intervals between 15–45 minutes.
pub const SOCIAL_NEED_JITTER: f64 = 0.5;

/// Maximum deferral hours for character-chosen times.
pub const MAX_DEFERRAL_HOURS: f64 = 24.0;

/// Heatmap factor for peak hours.
const HEATMAP_PEAK_FACTOR: f64 = 0.7;
/// Heatmap factor for trough hours.
const HEATMAP_TROUGH_FACTOR: f64 = 2.0;
/// Heatmap factor for normal hours.
const HEATMAP_NORMAL_FACTOR: f64 = 1.0;

// ---------------------------------------------------------------------------
// τ computation
// ---------------------------------------------------------------------------

/// Parameters for τ modulation.
#[derive(Debug, Clone)]
pub struct TauParams {
    /// Whether the last heartbeat was reciprocated (user responded).
    pub reciprocated: bool,
    /// Current hour's classification from the heatmap.
    pub hour_class: HourClassification,
    /// Character personality factor (0.0 = low social need, 1.0 = high).
    pub personality: f64,
}

/// Compute the time constant τ (seconds) for social-need probability rolls.
///
/// ```text
/// base_τ       = 86400 / sessions_per_day
/// recip_factor = 1.0 if reciprocated, 0.4 if not
/// engage_factor = 1.5 - engagement_score        (range 0.5–1.5)
/// heatmap_factor = peak: 0.7, trough: 2.0, normal: 1.0
/// person_factor  = 1.5 - personality             (range 0.5–1.5)
///
/// τ = base_τ × recip_factor × engage_factor × heatmap_factor × person_factor
/// ```
pub fn compute_tau(stats: &ActivityStats, params: &TauParams) -> f64 {
    if !stats.has_sufficient_data {
        return WARMUP_TAU;
    }

    let base_tau = if stats.sessions_per_day > 0.0 {
        86400.0 / stats.sessions_per_day
    } else {
        WARMUP_TAU
    };

    let recip_factor = if params.reciprocated { 1.0 } else { 0.4 };
    let engage_factor = (1.5 - stats.engagement_score).clamp(0.5, 1.5);
    let heatmap_factor = match params.hour_class {
        HourClassification::Peak => HEATMAP_PEAK_FACTOR,
        HourClassification::Trough => HEATMAP_TROUGH_FACTOR,
        HourClassification::Normal => HEATMAP_NORMAL_FACTOR,
    };
    let person_factor = (1.5 - params.personality).clamp(0.5, 1.5);

    base_tau * recip_factor * engage_factor * heatmap_factor * person_factor
}

// ---------------------------------------------------------------------------
// Probability roll
// ---------------------------------------------------------------------------

/// Compute per-roll probability: `p = 1 - e^(-check_interval / τ)`.
pub fn roll_probability(check_interval: f64, tau: f64) -> f64 {
    if tau <= 0.0 {
        return 1.0;
    }
    1.0 - (-check_interval / tau).exp()
}

/// Determine if a probability roll succeeds given a random value in [0, 1).
pub fn roll_succeeds(probability: f64, random_value: f64) -> bool {
    random_value < probability
}

// ---------------------------------------------------------------------------
// Heatmap curve adjustment
// ---------------------------------------------------------------------------

/// Compute a continuous heatmap multiplier from the hour histogram density.
///
/// V2 improvement: instead of a hard 3-bucket classification, use a smooth
/// curve so low-activity hours contribute more meaningfully. The multiplier
/// ranges from `HEATMAP_PEAK_FACTOR` (high activity) to `HEATMAP_TROUGH_FACTOR`
/// (low activity), with a smooth transition.
///
/// `density` is the normalized density for the current hour (from histogram).
/// `avg_density` is the average non-zero density across all hours.
pub fn heatmap_curve_factor(density: f64, avg_density: f64) -> f64 {
    if avg_density < f64::EPSILON {
        return HEATMAP_NORMAL_FACTOR;
    }

    // Ratio of current hour density to average.
    let ratio = density / avg_density;

    // Sigmoid-like mapping: ratio=0 → trough factor, ratio=1 → normal, ratio≥1.5 → peak factor.
    // We use: factor = trough + (peak - trough) × sigmoid((ratio - 1.0) × 4)
    // where sigmoid maps ratio=1.0 to 0.5 of the range.
    let sigmoid = 1.0 / (1.0 + (-(ratio - 1.0) * 4.0).exp());
    // Invert: high activity → lower factor (more frequent checks).
    let factor = HEATMAP_TROUGH_FACTOR
        - (HEATMAP_TROUGH_FACTOR - HEATMAP_PEAK_FACTOR) * sigmoid;

    factor.clamp(HEATMAP_PEAK_FACTOR, HEATMAP_TROUGH_FACTOR)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autonomy::activity::HourClassification;
    use std::time::Instant;

    /// Helper: create ActivityStats with specific values.
    fn make_stats(
        engagement_score: f64,
        sessions_per_day: f64,
        has_sufficient_data: bool,
    ) -> ActivityStats {
        ActivityStats {
            engagement_score,
            consistency: 0.8,
            tempo_score: 0.7,
            session_count: 4,
            sessions_per_day,
            hour_histogram: [0.0; 24],
            hour_classifications: [HourClassification::Normal; 24],
            has_sufficient_data,
            has_sufficient_heatmap: false,
            median_session_gap: Some(7200.0),
            anomaly_z_score: None,
            computed_at: Instant::now(),
        }
    }

    // -- τ computation --------------------------------------------------------

    #[test]
    fn test_tau_warmup() {
        let stats = make_stats(0.8, 4.0, false);
        let params = TauParams {
            reciprocated: true,
            hour_class: HourClassification::Normal,
            personality: 0.5,
        };
        let tau = compute_tau(&stats, &params);
        assert!((tau - WARMUP_TAU).abs() < f64::EPSILON);
    }

    #[test]
    fn test_tau_basic() {
        // 4 sessions/day → base_τ = 86400/4 = 21600
        // reciprocated: 1.0
        // engagement=0.8 → engage_factor = 1.5-0.8 = 0.7
        // normal hour → 1.0
        // personality=0.5 → person_factor = 1.5-0.5 = 1.0
        // τ = 21600 * 1.0 * 0.7 * 1.0 * 1.0 = 15120
        let stats = make_stats(0.8, 4.0, true);
        let params = TauParams {
            reciprocated: true,
            hour_class: HourClassification::Normal,
            personality: 0.5,
        };
        let tau = compute_tau(&stats, &params);
        assert!((tau - 15120.0).abs() < 0.1, "got τ={tau}");
    }

    #[test]
    fn test_tau_unreciprocated() {
        // Same as above but unreciprocated → recip_factor = 0.4
        // τ = 21600 * 0.4 * 0.7 * 1.0 * 1.0 = 6048
        let stats = make_stats(0.8, 4.0, true);
        let params = TauParams {
            reciprocated: false,
            hour_class: HourClassification::Normal,
            personality: 0.5,
        };
        let tau = compute_tau(&stats, &params);
        assert!((tau - 6048.0).abs() < 0.1, "got τ={tau}");
    }

    #[test]
    fn test_tau_peak_hour() {
        // Peak hour → heatmap_factor = 0.7 (more frequent checks)
        // τ = 21600 * 1.0 * 0.7 * 0.7 * 1.0 = 10584
        let stats = make_stats(0.8, 4.0, true);
        let params = TauParams {
            reciprocated: true,
            hour_class: HourClassification::Peak,
            personality: 0.5,
        };
        let tau = compute_tau(&stats, &params);
        assert!((tau - 10584.0).abs() < 0.1, "got τ={tau}");
    }

    #[test]
    fn test_tau_trough_hour() {
        // Trough hour → heatmap_factor = 2.0 (less frequent)
        // τ = 21600 * 1.0 * 0.7 * 2.0 * 1.0 = 30240
        let stats = make_stats(0.8, 4.0, true);
        let params = TauParams {
            reciprocated: true,
            hour_class: HourClassification::Trough,
            personality: 0.5,
        };
        let tau = compute_tau(&stats, &params);
        assert!((tau - 30240.0).abs() < 0.1, "got τ={tau}");
    }

    #[test]
    fn test_tau_high_personality() {
        // personality=1.0 → person_factor = 0.5 (more social → shorter τ)
        // τ = 21600 * 1.0 * 0.7 * 1.0 * 0.5 = 7560
        let stats = make_stats(0.8, 4.0, true);
        let params = TauParams {
            reciprocated: true,
            hour_class: HourClassification::Normal,
            personality: 1.0,
        };
        let tau = compute_tau(&stats, &params);
        assert!((tau - 7560.0).abs() < 0.1, "got τ={tau}");
    }

    #[test]
    fn test_tau_low_engagement() {
        // engagement=0.2 → engage_factor = 1.3 (user less engaged → longer τ)
        // τ = 21600 * 1.0 * 1.3 * 1.0 * 1.0 = 28080
        let stats = make_stats(0.2, 4.0, true);
        let params = TauParams {
            reciprocated: true,
            hour_class: HourClassification::Normal,
            personality: 0.5,
        };
        let tau = compute_tau(&stats, &params);
        assert!((tau - 28080.0).abs() < 0.1, "got τ={tau}");
    }

    #[test]
    fn test_tau_zero_sessions() {
        // 0 sessions/day → use warmup τ
        let stats = make_stats(0.5, 0.0, true);
        let params = TauParams {
            reciprocated: true,
            hour_class: HourClassification::Normal,
            personality: 0.5,
        };
        let tau = compute_tau(&stats, &params);
        // base_tau falls back to WARMUP_TAU, then modulated.
        let expected = WARMUP_TAU * 1.0 * 1.0 * 1.0 * 1.0;
        assert!((tau - expected).abs() < 0.1, "got τ={tau}");
    }

    // -- probability roll -----------------------------------------------------

    #[test]
    fn test_roll_probability_basic() {
        // check_interval=600s, τ=10800 → p = 1 - e^(-600/10800) ≈ 0.054
        let p = roll_probability(600.0, 10800.0);
        assert!((p - 0.054).abs() < 0.01, "got p={p}");
    }

    #[test]
    fn test_roll_probability_large_interval() {
        // check_interval = τ → p = 1 - e^(-1) ≈ 0.632
        let p = roll_probability(10800.0, 10800.0);
        assert!((p - 0.632).abs() < 0.01, "got p={p}");
    }

    #[test]
    fn test_roll_probability_zero_tau() {
        let p = roll_probability(600.0, 0.0);
        assert!((p - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_roll_probability_zero_interval() {
        let p = roll_probability(0.0, 10800.0);
        assert!(p.abs() < f64::EPSILON);
    }

    #[test]
    fn test_roll_succeeds() {
        assert!(roll_succeeds(0.5, 0.3)); // 0.3 < 0.5
        assert!(!roll_succeeds(0.5, 0.7)); // 0.7 ≥ 0.5
        assert!(!roll_succeeds(0.5, 0.5)); // boundary: not strictly less
    }

    // -- heatmap curve --------------------------------------------------------

    #[test]
    fn test_heatmap_curve_high_activity() {
        // density >> avg → factor approaches peak (0.7)
        let factor = heatmap_curve_factor(0.3, 0.1);
        assert!(
            factor < HEATMAP_NORMAL_FACTOR,
            "high activity should reduce factor, got {factor}"
        );
    }

    #[test]
    fn test_heatmap_curve_low_activity() {
        // density << avg → factor approaches trough (2.0)
        let factor = heatmap_curve_factor(0.01, 0.1);
        assert!(
            factor > HEATMAP_NORMAL_FACTOR,
            "low activity should increase factor, got {factor}"
        );
    }

    #[test]
    fn test_heatmap_curve_average_activity() {
        // density ≈ avg → factor ≈ midpoint
        let factor = heatmap_curve_factor(0.1, 0.1);
        // At ratio=1.0, sigmoid=0.5, factor = 2.0 - (2.0-0.7)*0.5 = 2.0-0.65 = 1.35
        assert!(
            (factor - 1.35).abs() < 0.01,
            "avg activity factor should be ~1.35, got {factor}"
        );
    }

    #[test]
    fn test_heatmap_curve_zero_avg() {
        let factor = heatmap_curve_factor(0.0, 0.0);
        assert!((factor - HEATMAP_NORMAL_FACTOR).abs() < f64::EPSILON);
    }

    #[test]
    fn test_heatmap_curve_bounds() {
        // Extreme values should stay within bounds.
        let factor_high = heatmap_curve_factor(1.0, 0.01);
        assert!(factor_high >= HEATMAP_PEAK_FACTOR);
        assert!(factor_high <= HEATMAP_TROUGH_FACTOR);

        let factor_low = heatmap_curve_factor(0.0001, 1.0);
        assert!(factor_low >= HEATMAP_PEAK_FACTOR);
        assert!(factor_low <= HEATMAP_TROUGH_FACTOR);
    }
}
