//! Cache keepalive scheduler for Anthropic prompt cache TTL refresh.
//!
//! Sends minimal API calls (max_tokens=1) to keep the prompt cache warm
//! during idle periods. See §13.2 of ARCHITECTURE.md.

use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default interval between keepalive pings (seconds).
/// Also used as the idle threshold before the first ping.
pub const DEFAULT_PING_INTERVAL_SECS: u64 = 240; // 4 minutes

/// Maximum number of keepalive pings before stopping.
pub const MAX_PINGS: u32 = 15;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Configuration required to activate cache keepalive.
#[derive(Debug, Clone)]
pub struct CacheKeepaliveConfig {
    /// Provider name (keepalive only activates for "anthropic").
    pub provider: String,
    /// Whether cache_ttl is configured in provider_options.
    pub cache_ttl_configured: bool,
    /// Interval between pings in seconds. Defaults to [`DEFAULT_PING_INTERVAL_SECS`].
    pub ping_interval_secs: u64,
    /// Maximum pings before auto-stop. Defaults to [`MAX_PINGS`].
    pub max_pings: u32,
}

impl CacheKeepaliveConfig {
    /// Returns true if keepalive should be active for this configuration.
    pub fn is_eligible(&self) -> bool {
        self.provider == "anthropic" && self.cache_ttl_configured
    }

    /// Build runtime config from resolved model fields.
    ///
    /// `provider` — provider key from the resolved model (e.g. `"anthropic"`).
    /// `has_cache_ttl` — whether the resolved model has a `cache_ttl` value.
    /// `keepalive_enabled` — explicit opt-out if `Some(false)`.
    /// `ttl_minutes` — explicit keepalive TTL override in minutes.
    /// `cache_ttl` — the `cache_ttl` string (e.g. `"1h"`, `"5m"`); parsed as
    ///   fallback when `ttl_minutes` is `None`.
    /// `max_pings` — maximum keepalive pings before stopping.
    pub fn from_resolved_model(
        provider: &str,
        has_cache_ttl: bool,
        keepalive_enabled: Option<bool>,
        ttl_minutes: Option<u32>,
        cache_ttl: Option<&str>,
        max_pings: Option<u32>,
    ) -> Self {
        // Derive effective TTL minutes: explicit override > parsed cache_ttl string.
        let effective_ttl_minutes = ttl_minutes.or_else(|| {
            cache_ttl.and_then(parse_cache_ttl_minutes)
        });

        // Ping slightly before TTL expires: (ttl - 60s), floored to the default.
        let ping_interval_secs = effective_ttl_minutes
            .map(|m| (m as u64 * 60).saturating_sub(60).max(DEFAULT_PING_INTERVAL_SECS))
            .unwrap_or(DEFAULT_PING_INTERVAL_SECS);

        // Treat explicit keepalive_enabled = false as disabling keepalive entirely.
        let effective_provider = if keepalive_enabled == Some(false) { "" } else { provider };

        Self {
            provider: effective_provider.to_string(),
            cache_ttl_configured: has_cache_ttl,
            ping_interval_secs,
            max_pings: max_pings.unwrap_or(MAX_PINGS),
        }
    }
}

/// Parse a `cache_ttl` duration string (e.g. `"1h"`, `"5m"`, `"30m"`) into minutes.
/// Returns `None` for unrecognised formats.
fn parse_cache_ttl_minutes(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(h) = s.strip_suffix('h') {
        h.parse::<u32>().ok().map(|v| v * 60)
    } else if let Some(m) = s.strip_suffix('m') {
        m.parse::<u32>().ok()
    } else {
        None
    }
}

impl Default for CacheKeepaliveConfig {
    fn default() -> Self {
        Self {
            provider: String::new(),
            cache_ttl_configured: false,
            ping_interval_secs: DEFAULT_PING_INTERVAL_SECS,
            max_pings: MAX_PINGS,
        }
    }
}

/// State of the cache keepalive scheduler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeepaliveState {
    /// Not eligible (wrong provider or no cache_ttl).
    Inactive,
    /// Cache active, monitoring idle time — not yet pinging.
    Monitoring,
    /// Actively sending keepalive pings.
    Pinging,
    /// Stopped: max pings reached or cache miss detected.
    Stopped { reason: StopReason },
}

/// Why keepalive stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// Reached the maximum number of pings.
    MaxPings,
    /// Cache miss detected — prefix was invalidated.
    CacheMiss,
}

/// Action returned by [`CacheKeepaliveScheduler::tick`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeepaliveAction {
    /// Nothing to do this tick.
    None,
    /// Send a minimal API call to refresh cache TTL.
    SendPing,
    /// Cache miss detected — emit CacheWarning to clients.
    EmitCacheWarning {
        expected_tokens: u32,
        message: String,
    },
}

// ---------------------------------------------------------------------------
// CacheKeepaliveScheduler
// ---------------------------------------------------------------------------

/// Scheduler that keeps the Anthropic prompt cache warm during idle periods.
pub struct CacheKeepaliveScheduler {
    state: KeepaliveState,
    config: CacheKeepaliveConfig,
    paused: bool,
    /// Timestamp of last API call (user message, interiority, regen, or ping).
    last_api_call: Option<Instant>,
    /// Timestamp of last keepalive ping sent.
    last_ping: Option<Instant>,
    /// Number of pings sent in the current pinging session.
    ping_count: u32,
    /// Estimated cached prompt size (tokens), updated from LLM responses.
    estimated_cache_tokens: u32,
}

impl CacheKeepaliveScheduler {
    pub fn new(config: CacheKeepaliveConfig) -> Self {
        let state = if config.is_eligible() {
            KeepaliveState::Monitoring
        } else {
            KeepaliveState::Inactive
        };
        Self {
            state,
            config,
            paused: false,
            last_api_call: None,
            last_ping: None,
            ping_count: 0,
            estimated_cache_tokens: 0,
        }
    }

    /// Restore persisted state across daemon restarts.
    ///
    /// `ping_count` is intentionally not restored: max_pings applies per-session,
    /// and `last_api_call` (an Instant) resets on restart so pinging cannot resume
    /// until a real API call arrives anyway.
    pub fn restore_counters(&mut self, estimated_cache_tokens: u32) {
        self.estimated_cache_tokens = estimated_cache_tokens;
    }

    // -- accessors --------------------------------------------------------

    pub fn state(&self) -> &KeepaliveState {
        &self.state
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn ping_count(&self) -> u32 {
        self.ping_count
    }

    pub fn config(&self) -> &CacheKeepaliveConfig {
        &self.config
    }

    /// Returns the `Instant` at which the next keepalive-relevant event fires,
    /// or `None` if keepalive is inactive or stopped.
    pub fn next_deadline(&self) -> Option<Instant> {
        let interval = Duration::from_secs(self.config.ping_interval_secs);
        match &self.state {
            KeepaliveState::Monitoring => self.last_api_call.map(|t| t + interval),
            KeepaliveState::Pinging => self.last_ping.map(|t| t + interval),
            _ => None,
        }
    }

    /// The configured ping interval in seconds (TTL - 60s, floored to default).
    pub fn ping_interval_secs(&self) -> u64 {
        self.config.ping_interval_secs
    }

    // -- control ----------------------------------------------------------

    /// Set pause state. When paused, tick() returns None.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Update configuration. Re-evaluates eligibility.
    pub fn update_config(&mut self, config: CacheKeepaliveConfig) {
        let was_eligible = self.config.is_eligible();
        let now_eligible = config.is_eligible();
        self.config = config;

        match (&self.state, was_eligible, now_eligible) {
            // Became eligible — start monitoring.
            (KeepaliveState::Inactive, _, true) => {
                self.state = KeepaliveState::Monitoring;
            }
            // Lost eligibility — go inactive.
            (_, true, false) => {
                self.state = KeepaliveState::Inactive;
                self.reset_ping_state();
            }
            _ => {}
        }
    }

    // -- event handlers ---------------------------------------------------

    /// Call when any API call completes (user message, interiority, regen).
    /// Updates the last-activity timestamp and cache token estimate.
    ///
    /// `cache_read_tokens` — from the LLM response usage data.
    /// `input_tokens` — total input tokens from the response.
    pub fn on_api_response(&mut self, now: Instant, cache_read_tokens: u32, input_tokens: u32) {
        self.last_api_call = Some(now);

        // Update cache size estimate from actual usage.
        if cache_read_tokens > 0 {
            self.estimated_cache_tokens = cache_read_tokens;
        } else if input_tokens > 0 && self.estimated_cache_tokens == 0 {
            // First call or cache miss — estimate from input tokens.
            self.estimated_cache_tokens = input_tokens;
        }

        // Any real API call (user message, interiority, regen) means the
        // conversation is active again — return to monitoring regardless of
        // cache hits.  The next response will re-establish the cache prefix.
        if matches!(self.state, KeepaliveState::Pinging | KeepaliveState::Stopped { .. }) {
            self.state = KeepaliveState::Monitoring;
            self.reset_ping_state();
        }
    }

    /// Call when a keepalive ping response is received.
    ///
    /// `cache_read_tokens` — from the ping response. If 0, cache was missed.
    pub fn on_ping_response(&mut self, now: Instant, cache_read_tokens: u32) -> KeepaliveAction {
        self.last_api_call = Some(now);

        if cache_read_tokens == 0 {
            // Cache miss — prefix was invalidated.
            let expected = self.estimated_cache_tokens;
            self.state = KeepaliveState::Stopped {
                reason: StopReason::CacheMiss,
            };
            return KeepaliveAction::EmitCacheWarning {
                expected_tokens: expected,
                message: format!(
                    "Cache keepalive ping returned 0 cache_read_tokens \
                     (expected ~{expected}). Prompt cache prefix was invalidated."
                ),
            };
        }

        // Update estimate from ping response.
        self.estimated_cache_tokens = cache_read_tokens;
        KeepaliveAction::None
    }

    // -- main tick --------------------------------------------------------

    /// Advance the scheduler by one tick.
    ///
    /// Returns an action telling the caller what to do.
    pub fn tick(&mut self, now: Instant) -> KeepaliveAction {
        if self.paused {
            return KeepaliveAction::None;
        }

        match &self.state {
            KeepaliveState::Inactive | KeepaliveState::Stopped { .. } => KeepaliveAction::None,
            KeepaliveState::Monitoring => self.tick_monitoring(now),
            KeepaliveState::Pinging => self.tick_pinging(now),
        }
    }

    // -- tick sub-handlers ------------------------------------------------

    fn tick_monitoring(&mut self, now: Instant) -> KeepaliveAction {
        let idle_secs = match self.last_api_call {
            Some(last) => now.duration_since(last).as_secs(),
            None => return KeepaliveAction::None, // No API calls yet.
        };

        if idle_secs >= self.config.ping_interval_secs {
            // Transition to pinging and send the first ping.
            self.state = KeepaliveState::Pinging;
            self.ping_count = 1;
            self.last_ping = Some(now);
            KeepaliveAction::SendPing
        } else {
            KeepaliveAction::None
        }
    }

    fn tick_pinging(&mut self, now: Instant) -> KeepaliveAction {
        // Check max pings.
        if self.ping_count >= self.config.max_pings {
            self.state = KeepaliveState::Stopped {
                reason: StopReason::MaxPings,
            };
            return KeepaliveAction::None;
        }

        let since_last_ping = match self.last_ping {
            Some(last) => now.duration_since(last).as_secs(),
            None => {
                // Shouldn't happen, but send a ping to recover.
                self.ping_count += 1;
                self.last_ping = Some(now);
                return KeepaliveAction::SendPing;
            }
        };

        if since_last_ping >= self.config.ping_interval_secs {
            self.ping_count += 1;
            self.last_ping = Some(now);
            KeepaliveAction::SendPing
        } else {
            KeepaliveAction::None
        }
    }

    // -- internal ---------------------------------------------------------

    fn reset_ping_state(&mut self) {
        self.ping_count = 0;
        self.last_ping = None;
    }
}

impl Default for CacheKeepaliveScheduler {
    fn default() -> Self {
        Self::new(CacheKeepaliveConfig::default())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn anthropic_config() -> CacheKeepaliveConfig {
        CacheKeepaliveConfig {
            provider: "anthropic".to_string(),
            cache_ttl_configured: true,
            ping_interval_secs: DEFAULT_PING_INTERVAL_SECS,
            max_pings: MAX_PINGS,
        }
    }

    fn openai_config() -> CacheKeepaliveConfig {
        CacheKeepaliveConfig {
            provider: "openai".to_string(),
            cache_ttl_configured: true,
            ping_interval_secs: DEFAULT_PING_INTERVAL_SECS,
            max_pings: MAX_PINGS,
        }
    }

    fn anthropic_no_cache_config() -> CacheKeepaliveConfig {
        CacheKeepaliveConfig {
            provider: "anthropic".to_string(),
            cache_ttl_configured: false,
            ping_interval_secs: DEFAULT_PING_INTERVAL_SECS,
            max_pings: MAX_PINGS,
        }
    }

    // -- eligibility tests ------------------------------------------------

    #[test]
    fn test_inactive_for_non_anthropic() {
        let sched = CacheKeepaliveScheduler::new(openai_config());
        assert_eq!(*sched.state(), KeepaliveState::Inactive);
    }

    #[test]
    fn test_inactive_without_cache_ttl() {
        let sched = CacheKeepaliveScheduler::new(anthropic_no_cache_config());
        assert_eq!(*sched.state(), KeepaliveState::Inactive);
    }

    #[test]
    fn test_monitoring_for_anthropic_with_cache() {
        let sched = CacheKeepaliveScheduler::new(anthropic_config());
        assert_eq!(*sched.state(), KeepaliveState::Monitoring);
    }

    // -- monitoring → pinging transition ----------------------------------

    #[test]
    fn test_no_ping_before_idle_threshold() {
        let mut sched = CacheKeepaliveScheduler::new(anthropic_config());
        let t0 = Instant::now();

        sched.on_api_response(t0, 1000, 1500);

        // Tick before idle threshold.
        let action = sched.tick(t0 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS - 1));
        assert_eq!(action, KeepaliveAction::None);
        assert_eq!(*sched.state(), KeepaliveState::Monitoring);
    }

    #[test]
    fn test_ping_after_idle_threshold() {
        let mut sched = CacheKeepaliveScheduler::new(anthropic_config());
        let t0 = Instant::now();

        sched.on_api_response(t0, 1000, 1500);

        let action = sched.tick(t0 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS));
        assert_eq!(action, KeepaliveAction::SendPing);
        assert_eq!(*sched.state(), KeepaliveState::Pinging);
        assert_eq!(sched.ping_count(), 1);
    }

    #[test]
    fn test_no_ping_without_prior_api_call() {
        let mut sched = CacheKeepaliveScheduler::new(anthropic_config());
        let t0 = Instant::now();

        let action = sched.tick(t0 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS + 100));
        assert_eq!(action, KeepaliveAction::None);
        assert_eq!(*sched.state(), KeepaliveState::Monitoring);
    }

    // -- pinging interval -------------------------------------------------

    #[test]
    fn test_ping_interval_respected() {
        let mut sched = CacheKeepaliveScheduler::new(anthropic_config());
        let t0 = Instant::now();

        sched.on_api_response(t0, 1000, 1500);

        // First ping.
        let t1 = t0 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS);
        let action = sched.tick(t1);
        assert_eq!(action, KeepaliveAction::SendPing);

        // Simulate ping response with cache hit.
        sched.on_ping_response(t1 + Duration::from_secs(1), 1000);

        // Too soon for next ping.
        let action = sched.tick(t1 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS - 1));
        assert_eq!(action, KeepaliveAction::None);

        // Interval elapsed — next ping.
        let action = sched.tick(t1 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS));
        assert_eq!(action, KeepaliveAction::SendPing);
        assert_eq!(sched.ping_count(), 2);
    }

    // -- stop conditions --------------------------------------------------

    #[test]
    fn test_stop_after_max_pings() {
        let config = CacheKeepaliveConfig {
            max_pings: 3,
            ..anthropic_config()
        };
        let mut sched = CacheKeepaliveScheduler::new(config);
        let t0 = Instant::now();

        sched.on_api_response(t0, 1000, 1500);

        // Ping 1.
        let t1 = t0 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS);
        assert_eq!(sched.tick(t1), KeepaliveAction::SendPing);
        sched.on_ping_response(t1, 1000);

        // Ping 2.
        let t2 = t1 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS);
        assert_eq!(sched.tick(t2), KeepaliveAction::SendPing);
        sched.on_ping_response(t2, 1000);

        // Ping 3.
        let t3 = t2 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS);
        assert_eq!(sched.tick(t3), KeepaliveAction::SendPing);
        sched.on_ping_response(t3, 1000);

        // Ping 4 attempt — should stop.
        let t4 = t3 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS);
        assert_eq!(sched.tick(t4), KeepaliveAction::None);
        assert_eq!(
            *sched.state(),
            KeepaliveState::Stopped {
                reason: StopReason::MaxPings
            }
        );
    }

    #[test]
    fn test_stop_on_cache_miss() {
        let mut sched = CacheKeepaliveScheduler::new(anthropic_config());
        let t0 = Instant::now();

        sched.on_api_response(t0, 1000, 1500);

        // First ping.
        let t1 = t0 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS);
        assert_eq!(sched.tick(t1), KeepaliveAction::SendPing);

        // Ping response with cache miss.
        let action = sched.on_ping_response(t1 + Duration::from_secs(1), 0);
        assert!(matches!(action, KeepaliveAction::EmitCacheWarning { .. }));
        if let KeepaliveAction::EmitCacheWarning {
            expected_tokens,
            message,
        } = action
        {
            assert_eq!(expected_tokens, 1000);
            assert!(message.contains("invalidated"));
        }
        assert_eq!(
            *sched.state(),
            KeepaliveState::Stopped {
                reason: StopReason::CacheMiss
            }
        );
    }

    // -- pause behavior ---------------------------------------------------

    #[test]
    fn test_paused_returns_none() {
        let mut sched = CacheKeepaliveScheduler::new(anthropic_config());
        let t0 = Instant::now();

        sched.on_api_response(t0, 1000, 1500);
        sched.set_paused(true);

        // Even past idle threshold, should return None when paused.
        let action = sched.tick(t0 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS + 100));
        assert_eq!(action, KeepaliveAction::None);
        // State should still be Monitoring (not advanced while paused).
        assert_eq!(*sched.state(), KeepaliveState::Monitoring);
    }

    #[test]
    fn test_unpause_resumes() {
        let mut sched = CacheKeepaliveScheduler::new(anthropic_config());
        let t0 = Instant::now();

        sched.on_api_response(t0, 1000, 1500);
        sched.set_paused(true);

        let t1 = t0 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS);
        assert_eq!(sched.tick(t1), KeepaliveAction::None);

        sched.set_paused(false);
        assert_eq!(sched.tick(t1), KeepaliveAction::SendPing);
    }

    // -- non-user API calls sustain cache ---------------------------------

    #[test]
    fn test_api_response_resets_idle_timer() {
        let mut sched = CacheKeepaliveScheduler::new(anthropic_config());
        let t0 = Instant::now();

        sched.on_api_response(t0, 1000, 1500);

        // Almost at idle threshold.
        let t1 = t0 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS - 10);
        assert_eq!(sched.tick(t1), KeepaliveAction::None);

        // Non-user API call (e.g. interiority) resets the timer.
        sched.on_api_response(t1, 1000, 1500);

        // Now idle threshold is measured from t1, not t0.
        let t2 = t1 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS - 1);
        assert_eq!(sched.tick(t2), KeepaliveAction::None);
        assert_eq!(*sched.state(), KeepaliveState::Monitoring);
    }

    #[test]
    fn test_api_response_returns_pinging_to_monitoring() {
        let mut sched = CacheKeepaliveScheduler::new(anthropic_config());
        let t0 = Instant::now();

        sched.on_api_response(t0, 1000, 1500);

        // Enter pinging state.
        let t1 = t0 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS);
        assert_eq!(sched.tick(t1), KeepaliveAction::SendPing);
        assert_eq!(*sched.state(), KeepaliveState::Pinging);

        // User sends a message → real API call (even with 0 cache hits).
        sched.on_api_response(t1 + Duration::from_secs(5), 0, 1500);
        assert_eq!(*sched.state(), KeepaliveState::Monitoring);
        assert_eq!(sched.ping_count(), 0);
    }

    #[test]
    fn test_api_response_recovers_from_stopped() {
        let mut sched = CacheKeepaliveScheduler::new(anthropic_config());
        let t0 = Instant::now();

        sched.on_api_response(t0, 1000, 1500);

        // Enter pinging, then cache miss → stopped.
        let t1 = t0 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS);
        sched.tick(t1);
        sched.on_ping_response(t1, 0);
        assert!(matches!(
            sched.state(),
            KeepaliveState::Stopped { reason: StopReason::CacheMiss }
        ));

        // New user message recovers to monitoring even with 0 cache hits
        // (the next response will re-establish the cache prefix).
        sched.on_api_response(t1 + Duration::from_secs(60), 0, 1200);
        assert_eq!(*sched.state(), KeepaliveState::Monitoring);
        assert_eq!(sched.ping_count(), 0);
    }

    // -- config update ----------------------------------------------------

    #[test]
    fn test_update_config_activates() {
        let mut sched = CacheKeepaliveScheduler::new(openai_config());
        assert_eq!(*sched.state(), KeepaliveState::Inactive);

        sched.update_config(anthropic_config());
        assert_eq!(*sched.state(), KeepaliveState::Monitoring);
    }

    #[test]
    fn test_update_config_deactivates() {
        let mut sched = CacheKeepaliveScheduler::new(anthropic_config());
        assert_eq!(*sched.state(), KeepaliveState::Monitoring);

        sched.update_config(openai_config());
        assert_eq!(*sched.state(), KeepaliveState::Inactive);
    }

    // -- parse_cache_ttl_minutes ------------------------------------------

    #[test]
    fn test_parse_cache_ttl_minutes_hours() {
        assert_eq!(parse_cache_ttl_minutes("1h"), Some(60));
        assert_eq!(parse_cache_ttl_minutes("2h"), Some(120));
    }

    #[test]
    fn test_parse_cache_ttl_minutes_minutes() {
        assert_eq!(parse_cache_ttl_minutes("5m"), Some(5));
        assert_eq!(parse_cache_ttl_minutes("30m"), Some(30));
    }

    #[test]
    fn test_parse_cache_ttl_minutes_invalid() {
        assert_eq!(parse_cache_ttl_minutes(""), None);
        assert_eq!(parse_cache_ttl_minutes("abc"), None);
    }

    // -- from_resolved_model interval derivation --------------------------

    #[test]
    fn test_from_resolved_model_uses_cache_ttl_fallback() {
        // No explicit keepalive_ttl_minutes — should parse "1h" → 60 min → 3540s interval.
        let cfg = CacheKeepaliveConfig::from_resolved_model(
            "anthropic", true, None, None, Some("1h"), None,
        );
        assert_eq!(cfg.ping_interval_secs, 60 * 60 - 60); // 3540
    }

    #[test]
    fn test_from_resolved_model_explicit_ttl_overrides_cache_ttl() {
        // Explicit keepalive_ttl_minutes=30 should win over cache_ttl="1h".
        let cfg = CacheKeepaliveConfig::from_resolved_model(
            "anthropic", true, None, Some(30), Some("1h"), None,
        );
        assert_eq!(cfg.ping_interval_secs, 30 * 60 - 60); // 1740
    }

    #[test]
    fn test_from_resolved_model_no_ttl_uses_default() {
        // No keepalive_ttl_minutes, no cache_ttl → default interval.
        let cfg = CacheKeepaliveConfig::from_resolved_model(
            "anthropic", true, None, None, None, None,
        );
        assert_eq!(cfg.ping_interval_secs, DEFAULT_PING_INTERVAL_SECS);
    }

    // -- cache token estimate tracking ------------------------------------

    #[test]
    fn test_cache_tokens_updated_from_response() {
        let mut sched = CacheKeepaliveScheduler::new(anthropic_config());
        let t0 = Instant::now();

        // First response sets estimate from cache_read_tokens.
        sched.on_api_response(t0, 500, 800);
        assert_eq!(sched.estimated_cache_tokens, 500);

        // Second response updates.
        sched.on_api_response(t0 + Duration::from_secs(10), 600, 900);
        assert_eq!(sched.estimated_cache_tokens, 600);
    }

    #[test]
    fn test_cache_tokens_fallback_to_input() {
        let mut sched = CacheKeepaliveScheduler::new(anthropic_config());
        let t0 = Instant::now();

        // First response with 0 cache_read — uses input_tokens as estimate.
        sched.on_api_response(t0, 0, 800);
        assert_eq!(sched.estimated_cache_tokens, 800);
    }

    #[test]
    fn test_cache_warning_includes_estimated_tokens() {
        let mut sched = CacheKeepaliveScheduler::new(anthropic_config());
        let t0 = Instant::now();

        sched.on_api_response(t0, 1234, 2000);

        let t1 = t0 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS);
        sched.tick(t1);

        let action = sched.on_ping_response(t1, 0);
        if let KeepaliveAction::EmitCacheWarning {
            expected_tokens, ..
        } = action
        {
            assert_eq!(expected_tokens, 1234);
        } else {
            panic!("expected EmitCacheWarning");
        }
    }

    // -- next_deadline tests ----------------------------------------------

    #[test]
    fn test_next_deadline_monitoring_with_api_call() {
        let mut sched = CacheKeepaliveScheduler::new(anthropic_config());
        let t0 = Instant::now();

        sched.on_api_response(t0, 1000, 1500);

        let deadline = sched.next_deadline();
        assert_eq!(deadline, Some(t0 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS)));
    }

    #[test]
    fn test_next_deadline_monitoring_no_api_call() {
        let sched = CacheKeepaliveScheduler::new(anthropic_config());
        assert_eq!(sched.next_deadline(), None);
    }

    #[test]
    fn test_next_deadline_inactive() {
        let sched = CacheKeepaliveScheduler::new(openai_config());
        assert_eq!(sched.next_deadline(), None);
    }

    #[test]
    fn test_next_deadline_pinging() {
        let mut sched = CacheKeepaliveScheduler::new(anthropic_config());
        let t0 = Instant::now();

        sched.on_api_response(t0, 1000, 1500);
        let t1 = t0 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS);
        sched.tick(t1); // transitions to Pinging, last_ping = t1
        assert_eq!(*sched.state(), KeepaliveState::Pinging);
        // Now returns the next ping deadline so coordination still works.
        assert_eq!(sched.next_deadline(), Some(t1 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS)));
    }

    #[test]
    fn test_next_deadline_shifts_on_api_response() {
        let mut sched = CacheKeepaliveScheduler::new(anthropic_config());
        let t0 = Instant::now();

        sched.on_api_response(t0, 1000, 1500);
        assert_eq!(sched.next_deadline(), Some(t0 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS)));

        // New API call shifts the deadline forward.
        let t1 = t0 + Duration::from_secs(100);
        sched.on_api_response(t1, 1000, 1500);
        assert_eq!(sched.next_deadline(), Some(t1 + Duration::from_secs(DEFAULT_PING_INTERVAL_SECS)));
    }
}
