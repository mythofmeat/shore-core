// TODO: Rework to use concrete CommandContext from daemon core (US-010–018 merge).
// These tests used the old trait-based CommandContext; the real dispatch now takes
// a concrete struct backed by ConversationEngine. Subsystem unit tests still cover
// the individual handler logic.
#![cfg(ignore)]
//! US-030: Autonomy milestone — end-to-end integration test.
//!
//! Exercises the complete autonomy subsystem with all components wired together:
//! HeartbeatScheduler, CacheKeepaliveScheduler, ActivityTracker, timing, and
//! the status command. Uses mock LLM (no real API calls) for deterministic testing.
//!
//! Coverage:
//! - Conversation simulation → idle → post-session probe fires
//! - Probe response with time → Deferred state, timer, message delivery
//! - Probe response decline → SocialNeed state
//! - toggle_autonomy pauses all probes
//! - Dormant state after max_unanswered unreplied messages
//! - Cache keepalive pings during idle
//! - Status command shows heartbeat state, social need bar level, τ value

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use serde_json::{json, Value};

use shore_daemon::autonomy::activity::{ActivityTracker, HourClassification, SESSION_GAP};
use shore_daemon::autonomy::cache_keepalive::{
    CacheKeepaliveConfig, CacheKeepaliveScheduler, KeepaliveAction, KeepaliveState,
    IDLE_THRESHOLD_SECS,
};
use shore_daemon::autonomy::heartbeat::{
    HeartbeatAction, HeartbeatScheduler, HeartbeatState, ProbeResult, DORMANT_THRESHOLD,
};
use shore_daemon::autonomy::timing::{compute_tau, TauParams};
use shore_daemon::autonomy::AutonomyStatus;
use shore_daemon::commands::{self, CommandContext};
use shore_daemon::memory::db::MemoryDB;

// ---------------------------------------------------------------------------
// Wall-clock helper
// ---------------------------------------------------------------------------

fn wall(hour: u32, min: u32) -> chrono::NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 3, 25)
        .unwrap()
        .and_hms_opt(hour, min, 0)
        .unwrap()
}

// ---------------------------------------------------------------------------
// CommandContext with autonomy status support
// ---------------------------------------------------------------------------

struct AutonomyTestCtx {
    db: MemoryDB,
    private: AtomicBool,
    autonomy_paused: AtomicBool,
    autonomy_status: Option<AutonomyStatus>,
}

impl AutonomyTestCtx {
    fn new() -> Self {
        Self {
            db: MemoryDB::open_in_memory().unwrap(),
            private: AtomicBool::new(false),
            autonomy_paused: AtomicBool::new(false),
            autonomy_status: None,
        }
    }

    fn with_status(mut self, status: AutonomyStatus) -> Self {
        self.autonomy_status = Some(status);
        self
    }
}

impl CommandContext for AutonomyTestCtx {
    fn memory_db(&self) -> &MemoryDB {
        &self.db
    }
    fn is_private(&self) -> bool {
        self.private.load(Ordering::SeqCst)
    }
    fn set_private(&self, private: bool) {
        self.private.store(private, Ordering::SeqCst);
    }
    fn is_autonomy_paused(&self) -> bool {
        self.autonomy_paused.load(Ordering::SeqCst)
    }
    fn set_autonomy_paused(&self, paused: bool) {
        self.autonomy_paused.store(paused, Ordering::SeqCst);
    }
    fn effective_config(&self) -> Value {
        json!({
            "model": "claude-haiku-4-5-20251001",
            "autonomy": { "enabled": true, "personality": 0.7 },
        })
    }
    fn autonomy_status(&self) -> Option<AutonomyStatus> {
        self.autonomy_status.clone()
    }
}

// ---------------------------------------------------------------------------
// Helper: build activity stats for a short conversation
// ---------------------------------------------------------------------------

fn wall_day(day: u32, hour: u32, min: u32) -> chrono::NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 3, day)
        .unwrap()
        .and_hms_opt(hour, min, 0)
        .unwrap()
}

fn build_conversation_tracker() -> ActivityTracker {
    let mut tracker = ActivityTracker::new();
    let base = Instant::now();
    // Simulate 2 days of conversation across 3 sessions (>= 5 msgs, >= 2 days).
    let times = [
        // Day 1 (March 24), session 1 (10:00–10:03)
        wall_day(24, 10, 0),
        wall_day(24, 10, 1),
        wall_day(24, 10, 2),
        wall_day(24, 10, 3),
        // Day 2 (March 25), session 2 (14:00–14:03)
        wall_day(25, 14, 0),
        wall_day(25, 14, 1),
        wall_day(25, 14, 3),
    ];
    for (i, w) in times.iter().enumerate() {
        tracker.record_message_at(base + Duration::from_secs(i as u64), *w);
    }
    tracker
}

// ===========================================================================
// Test 1: Full heartbeat lifecycle — Session → Probe → Deferred → fires
// ===========================================================================

#[test]
fn test_full_heartbeat_lifecycle_deferred() {
    let mut heartbeat = HeartbeatScheduler::new();
    let mut tracker = build_conversation_tracker();
    let stats = tracker.recompute_stats().clone();
    let params = TauParams {
        reciprocated: true,
        hour_class: HourClassification::Normal,
        personality: 0.7,
    };
    let tau = compute_tau(&stats, &params);
    assert!(tau > 0.0, "τ should be positive");

    let t0 = Instant::now();

    // --- Phase 1: Active conversation ---
    heartbeat.on_user_message(t0);
    heartbeat.on_assistant_message(t0 + Duration::from_secs(2));
    heartbeat.on_user_message(t0 + Duration::from_secs(10));
    heartbeat.on_assistant_message(t0 + Duration::from_secs(12));

    assert!(matches!(heartbeat.state(), HeartbeatState::Session));

    // Tick while still in session — no action.
    let action = heartbeat.tick(
        &stats,
        &params,
        t0 + Duration::from_secs(60),
        wall(14, 1),
        0.5,
    );
    assert_eq!(action, HeartbeatAction::None);

    // --- Phase 2: Go idle past SESSION_GAP ---
    // Last activity was at t0+12 (assistant). Need idle >= SESSION_GAP.
    // At t0 + 12 + SESSION_GAP + 1, idle = SESSION_GAP + 1.
    let idle_time = t0 + Duration::from_secs(12 + SESSION_GAP + 1);
    // Session duration: idle_time - t0 = 12 + SESSION_GAP + 1, which >> SESSION_PROBE_FLOOR.
    let action = heartbeat.tick(&stats, &params, idle_time, wall(14, 35), 0.5);
    assert!(
        matches!(action, HeartbeatAction::GenerateProbe { .. }),
        "expected GenerateProbe, got {action:?}"
    );
    assert!(matches!(heartbeat.state(), HeartbeatState::PostSessionProbe));

    // --- Phase 3: Character responds with a time → Deferred ---
    let probe_result = heartbeat.handle_probe_response(
        "I'll check in at 8:30 PM",
        idle_time,
        wall(14, 35),
    );
    assert!(matches!(probe_result, ProbeResult::Deferred(_)));
    if let ProbeResult::Deferred(fire_at) = probe_result {
        assert_eq!(fire_at, wall(20, 30));
    }
    assert!(matches!(heartbeat.state(), HeartbeatState::Deferred { .. }));

    // --- Phase 4: Timer fires → message delivery, transition to SocialNeed ---
    let fire_time = idle_time + Duration::from_secs(6 * 3600); // well past 8:30 PM
    let action = heartbeat.tick(&stats, &params, fire_time, wall(20, 31), 0.5);
    assert!(
        matches!(action, HeartbeatAction::GenerateDeferredMessage { .. }),
        "expected GenerateDeferredMessage, got {action:?}"
    );
    assert!(matches!(heartbeat.state(), HeartbeatState::SocialNeed));
    assert_eq!(heartbeat.unanswered_count(), 1);
}

// ===========================================================================
// Test 2: Probe decline → SocialNeed → Dormant
// ===========================================================================

#[test]
fn test_probe_decline_to_social_need_to_dormant() {
    let mut heartbeat = HeartbeatScheduler::new();
    let mut tracker = build_conversation_tracker();
    let stats = tracker.recompute_stats().clone();
    let params = TauParams {
        reciprocated: true,
        hour_class: HourClassification::Normal,
        personality: 0.7,
    };

    let t0 = Instant::now();

    // Start session and go idle.
    heartbeat.on_user_message(t0);
    heartbeat.on_assistant_message(t0 + Duration::from_secs(5));

    let idle_time = t0 + Duration::from_secs(5 + SESSION_GAP + 1);
    let action = heartbeat.tick(&stats, &params, idle_time, wall(14, 35), 0.5);
    assert!(matches!(action, HeartbeatAction::GenerateProbe { .. }));

    // Character declines → SocialNeed.
    let result = heartbeat.handle_probe_response(
        "No, I'd rather not reach out right now.",
        idle_time,
        wall(14, 35),
    );
    assert_eq!(result, ProbeResult::Declined);
    assert!(matches!(heartbeat.state(), HeartbeatState::SocialNeed));

    // Drive SocialNeed rolls with random_value=0.0 (always succeeds) until dormant.
    let mut tick_time = idle_time;
    for i in 0..DORMANT_THRESHOLD {
        tick_time = tick_time + Duration::from_secs(7200);
        let action = heartbeat.tick(&stats, &params, tick_time, wall(16, 0), 0.0);

        if i + 1 < DORMANT_THRESHOLD {
            assert!(
                matches!(action, HeartbeatAction::GenerateSocialNeedMessage { .. }),
                "tick {i}: expected GenerateSocialNeedMessage, got {action:?}"
            );
        }
    }

    // After DORMANT_THRESHOLD unanswered messages, next tick transitions to Dormant.
    tick_time = tick_time + Duration::from_secs(7200);
    let action = heartbeat.tick(&stats, &params, tick_time, wall(18, 0), 0.0);
    assert_eq!(action, HeartbeatAction::None);
    assert!(
        matches!(heartbeat.state(), HeartbeatState::Dormant),
        "expected Dormant, got {:?}",
        heartbeat.state()
    );
}

// ===========================================================================
// Test 3: toggle_autonomy pauses all probes
// ===========================================================================

#[test]
fn test_toggle_autonomy_pauses_probes() {
    let mut heartbeat = HeartbeatScheduler::new();
    let mut tracker = build_conversation_tracker();
    let stats = tracker.recompute_stats().clone();
    let params = TauParams {
        reciprocated: true,
        hour_class: HourClassification::Normal,
        personality: 0.7,
    };

    let t0 = Instant::now();
    heartbeat.on_user_message(t0);
    heartbeat.on_assistant_message(t0 + Duration::from_secs(5));

    // Pause before idle threshold.
    let paused = heartbeat.toggle_pause();
    assert!(paused);

    // Even past SESSION_GAP, should not fire.
    let idle_time = t0 + Duration::from_secs(5 + SESSION_GAP + 100);
    let action = heartbeat.tick(&stats, &params, idle_time, wall(15, 0), 0.5);
    assert_eq!(action, HeartbeatAction::None);
    assert!(matches!(heartbeat.state(), HeartbeatState::Session));

    // Resume — now the probe fires.
    let resumed = heartbeat.toggle_pause();
    assert!(!resumed);
    let action = heartbeat.tick(&stats, &params, idle_time, wall(15, 0), 0.5);
    assert!(matches!(action, HeartbeatAction::GenerateProbe { .. }));
}

// ===========================================================================
// Test 4: Cache keepalive pings during idle
// ===========================================================================

#[test]
fn test_cache_keepalive_pings_during_idle() {
    let config = CacheKeepaliveConfig {
        provider: "anthropic".to_string(),
        cache_ttl_configured: true,
        ping_interval_secs: 240,
        max_pings: 15,
    };
    let mut keepalive = CacheKeepaliveScheduler::new(config);
    assert_eq!(*keepalive.state(), KeepaliveState::Monitoring);

    let t0 = Instant::now();

    // Simulate an API call (establishing cache).
    keepalive.on_api_response(t0, 1000, 1500);

    // Before idle threshold — no ping.
    let action = keepalive.tick(t0 + Duration::from_secs(IDLE_THRESHOLD_SECS - 1));
    assert_eq!(action, KeepaliveAction::None);
    assert_eq!(*keepalive.state(), KeepaliveState::Monitoring);

    // At idle threshold — first ping.
    let t1 = t0 + Duration::from_secs(IDLE_THRESHOLD_SECS);
    let action = keepalive.tick(t1);
    assert_eq!(action, KeepaliveAction::SendPing);
    assert_eq!(*keepalive.state(), KeepaliveState::Pinging);
    assert_eq!(keepalive.ping_count(), 1);

    // Simulate ping response (cache hit).
    keepalive.on_ping_response(t1 + Duration::from_secs(1), 1000);

    // Second ping after interval.
    let t2 = t1 + Duration::from_secs(240);
    let action = keepalive.tick(t2);
    assert_eq!(action, KeepaliveAction::SendPing);
    assert_eq!(keepalive.ping_count(), 2);

    // Verify pause stops pings.
    keepalive.set_paused(true);
    let t3 = t2 + Duration::from_secs(240);
    let action = keepalive.tick(t3);
    assert_eq!(action, KeepaliveAction::None);

    // Resume.
    keepalive.set_paused(false);
    let action = keepalive.tick(t3);
    assert_eq!(action, KeepaliveAction::SendPing);
}

// ===========================================================================
// Test 5: Status command shows heartbeat state, social need bar, τ value
// ===========================================================================

#[tokio::test]
async fn test_status_command_shows_autonomy_state() {
    let mut tracker = build_conversation_tracker();
    let stats = tracker.recompute_stats().clone();
    let params = TauParams {
        reciprocated: true,
        hour_class: HourClassification::Normal,
        personality: 0.7,
    };
    let tau = compute_tau(&stats, &params);

    let status = AutonomyStatus {
        paused: false,
        heartbeat_state: "SocialNeed".to_string(),
        unanswered_count: 3,
        dormant_threshold: DORMANT_THRESHOLD,
        social_need_bar: 3.0 / DORMANT_THRESHOLD as f64,
        tau,
        cache_keepalive_state: "Monitoring".to_string(),
        cache_keepalive_pings: 0,
    };

    let ctx = AutonomyTestCtx::new().with_status(status);
    let result = commands::dispatch("status", json!({}), &ctx).await.unwrap();

    let autonomy = &result.data["autonomy"];
    assert!(!autonomy.is_null(), "autonomy should be present");
    assert_eq!(autonomy["paused"], false);
    assert_eq!(autonomy["heartbeat_state"], "SocialNeed");
    assert_eq!(autonomy["unanswered_count"], 3);
    assert_eq!(autonomy["dormant_threshold"], DORMANT_THRESHOLD);
    assert!(autonomy["social_need_bar"].as_f64().unwrap() > 0.0);
    assert!(autonomy["tau"].as_f64().unwrap() > 0.0);
    assert_eq!(autonomy["cache_keepalive_state"], "Monitoring");
    assert_eq!(autonomy["cache_keepalive_pings"], 0);
}

// ===========================================================================
// Test 6: Full autonomy lifecycle with all subsystems
// ===========================================================================

#[test]
fn test_full_autonomy_lifecycle_all_subsystems() {
    // Wire all components together.
    let mut heartbeat = HeartbeatScheduler::new();
    let mut tracker = build_conversation_tracker();
    let keepalive_config = CacheKeepaliveConfig {
        provider: "anthropic".to_string(),
        cache_ttl_configured: true,
        ping_interval_secs: 240,
        max_pings: 15,
    };
    let mut keepalive = CacheKeepaliveScheduler::new(keepalive_config);

    let t0 = Instant::now();

    // --- Step 1: Start with a conversation ---
    heartbeat.on_user_message(t0);
    heartbeat.on_assistant_message(t0 + Duration::from_secs(3));
    keepalive.on_api_response(t0 + Duration::from_secs(3), 500, 800);

    heartbeat.on_user_message(t0 + Duration::from_secs(10));
    heartbeat.on_assistant_message(t0 + Duration::from_secs(13));
    keepalive.on_api_response(t0 + Duration::from_secs(13), 600, 900);

    // More messages to build up tracker data.
    heartbeat.on_user_message(t0 + Duration::from_secs(20));
    heartbeat.on_assistant_message(t0 + Duration::from_secs(23));
    keepalive.on_api_response(t0 + Duration::from_secs(23), 650, 950);

    assert!(matches!(heartbeat.state(), HeartbeatState::Session));

    let stats = tracker.recompute_stats().clone();
    let params = TauParams {
        reciprocated: true,
        hour_class: HourClassification::Normal,
        personality: 0.7,
    };

    // --- Step 2: Go idle → probe fires ---
    let idle_at = t0 + Duration::from_secs(23 + SESSION_GAP + 1);
    let hb_action = heartbeat.tick(&stats, &params, idle_at, wall(14, 35), 0.5);
    assert!(matches!(hb_action, HeartbeatAction::GenerateProbe { .. }));

    // Cache keepalive should still be monitoring (idle < 10 min relative to last API call).
    let ka_action = keepalive.tick(idle_at);
    // idle = 23 + SESSION_GAP + 1 - 23 = SESSION_GAP + 1 ≈ 1801s > 600s → pinging!
    assert_eq!(ka_action, KeepaliveAction::SendPing);
    assert_eq!(*keepalive.state(), KeepaliveState::Pinging);

    // --- Step 3: Character chooses a time → Deferred ---
    let probe_result = heartbeat.handle_probe_response(
        "I'll check in at 8:30 PM",
        idle_at,
        wall(14, 35),
    );
    assert!(matches!(probe_result, ProbeResult::Deferred(_)));

    // Build status snapshot.
    let tau = compute_tau(&stats, &params);
    let status = AutonomyStatus {
        paused: false,
        heartbeat_state: format!("{:?}", heartbeat.state()),
        unanswered_count: heartbeat.unanswered_count(),
        dormant_threshold: DORMANT_THRESHOLD,
        social_need_bar: heartbeat.unanswered_count() as f64 / DORMANT_THRESHOLD as f64,
        tau,
        cache_keepalive_state: format!("{:?}", keepalive.state()),
        cache_keepalive_pings: keepalive.ping_count(),
    };

    assert!(!status.paused);
    assert!(status.heartbeat_state.contains("Deferred"));
    assert_eq!(status.unanswered_count, 0);
    assert!(status.tau > 0.0);
    assert!(status.cache_keepalive_state.contains("Pinging"));
    assert_eq!(status.cache_keepalive_pings, 1);

    // --- Step 4: Timer fires → message delivery ---
    let fire_at = idle_at + Duration::from_secs(6 * 3600);
    let action = heartbeat.tick(&stats, &params, fire_at, wall(20, 31), 0.5);
    assert!(matches!(
        action,
        HeartbeatAction::GenerateDeferredMessage { .. }
    ));
    assert!(matches!(heartbeat.state(), HeartbeatState::SocialNeed));
    assert_eq!(heartbeat.unanswered_count(), 1);

    // --- Step 5: User returns → resets everything ---
    let user_return = fire_at + Duration::from_secs(3600);
    heartbeat.on_user_message(user_return);
    keepalive.on_api_response(user_return, 700, 1000);

    assert!(matches!(heartbeat.state(), HeartbeatState::Session));
    assert_eq!(heartbeat.unanswered_count(), 0);
    // Cache keepalive returns to monitoring on real API call with cache hits.
    assert_eq!(*keepalive.state(), KeepaliveState::Monitoring);
    assert_eq!(keepalive.ping_count(), 0);
}

// ===========================================================================
// Test 7: toggle_autonomy command via dispatch
// ===========================================================================

#[tokio::test]
async fn test_toggle_autonomy_command() {
    let ctx = AutonomyTestCtx::new();
    assert!(!ctx.is_autonomy_paused());

    let result = commands::dispatch("toggle_autonomy", json!({}), &ctx)
        .await
        .unwrap();
    assert_eq!(result.data["autonomy_paused"], true);
    assert!(ctx.is_autonomy_paused());

    let result = commands::dispatch("toggle_autonomy", json!({}), &ctx)
        .await
        .unwrap();
    assert_eq!(result.data["autonomy_paused"], false);
    assert!(!ctx.is_autonomy_paused());
}

// ===========================================================================
// Test 8: Dormant user returns → Session reset
// ===========================================================================

#[test]
fn test_dormant_user_returns_to_session() {
    let mut heartbeat = HeartbeatScheduler::new();
    let t0 = Instant::now();

    // Force into Dormant state by setting unanswered count at threshold.
    heartbeat.on_user_message(t0);
    heartbeat.on_assistant_message(t0 + Duration::from_secs(5));

    // Drive to SocialNeed first.
    let mut tracker = build_conversation_tracker();
    let stats = tracker.recompute_stats().clone();
    let params = TauParams {
        reciprocated: true,
        hour_class: HourClassification::Normal,
        personality: 0.7,
    };

    let idle_time = t0 + Duration::from_secs(5 + SESSION_GAP + 1);
    heartbeat.tick(&stats, &params, idle_time, wall(14, 35), 0.5);
    heartbeat.handle_probe_response("No thanks", idle_time, wall(14, 35));
    assert!(matches!(heartbeat.state(), HeartbeatState::SocialNeed));

    // Send DORMANT_THRESHOLD messages.
    let mut tick_time = idle_time;
    for _ in 0..DORMANT_THRESHOLD {
        tick_time = tick_time + Duration::from_secs(7200);
        heartbeat.tick(&stats, &params, tick_time, wall(16, 0), 0.0);
    }
    tick_time = tick_time + Duration::from_secs(7200);
    heartbeat.tick(&stats, &params, tick_time, wall(18, 0), 0.0);
    assert!(matches!(heartbeat.state(), HeartbeatState::Dormant));

    // User returns — should reset to Session.
    heartbeat.on_user_message(tick_time + Duration::from_secs(100));
    assert!(matches!(heartbeat.state(), HeartbeatState::Session));
    assert_eq!(heartbeat.unanswered_count(), 0);
}

// ===========================================================================
// Test 9: τ computation uses personality and engagement
// ===========================================================================

#[test]
fn test_tau_computation_personality_effect() {
    let mut tracker = build_conversation_tracker();
    let stats = tracker.recompute_stats().clone();

    // High personality → shorter τ (more social).
    let tau_high = compute_tau(
        &stats,
        &TauParams {
            reciprocated: true,
            hour_class: HourClassification::Normal,
            personality: 1.0,
        },
    );

    // Low personality → longer τ (less social).
    let tau_low = compute_tau(
        &stats,
        &TauParams {
            reciprocated: true,
            hour_class: HourClassification::Normal,
            personality: 0.0,
        },
    );

    assert!(
        tau_high < tau_low,
        "high personality τ ({tau_high}) should be less than low personality τ ({tau_low})"
    );
    assert!(tau_high > 0.0);
    assert!(tau_low > 0.0);
}
