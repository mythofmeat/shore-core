//! E2E: a reply held by the response delay survives a daemon crash and is
//! recovered to a reconnected client.
//!
//! Boots the real daemon + SWP socket (mock LLM sidecar stands in for the
//! provider), enables `[behavior.response_delay]`, sends a message so the reply
//! is held, crashes the daemon mid-hold, and reboots from the on-disk state.
//! Asserts the recovered reply streams to the reconnected client, is persisted,
//! clears the deadline, and carries the "kept the user waiting" note.
//!
//! Exercises the full path: persisted deadline -> `ensure_state` on restart ->
//! connected-client recovery check -> idempotency guard -> streamed reply.

#![deny(clippy::arithmetic_side_effects, clippy::indexing_slicing)]

use std::time::Duration;

use shore_config::app::ResponseDelayConfig;
use shore_config::ConfigDuration;
use shore_test_harness::{TestConfigBuilder, TestHarness};

const CHARACTER: &str = "TestChar";

fn delay_config() -> ResponseDelayConfig {
    ResponseDelayConfig {
        enabled: true,
        // A long hold: the in-process generation task that serves the hold is
        // *detached*, not killed, when the harness "crashes" (a real `kill -9`
        // would take it down with the process). A long `min` keeps it asleep so
        // it cannot race the recovery; we simulate "restarted after the deadline
        // passed" by editing the persisted deadline into the recent past below.
        min: ConfigDuration::from_secs(300),
        max: ConfigDuration::from_secs(600),
        scale: 0.25,
        jitter: 0.0,
        // Low threshold so the recovered (overdue) reply carries the note.
        notify_after: ConfigDuration::from_secs(1),
    }
}

fn builder() -> TestConfigBuilder {
    TestConfigBuilder::new().response_delay(delay_config())
}

/// Poll the shared autonomy manager until the response-delay hold is active
/// (its deadline is set, and therefore persisted to disk). Returns whether it
/// became active before the timeout.
async fn wait_for_hold(harness: &TestHarness) -> bool {
    let cfg = &harness.config.app.behavior.response_delay;
    for _ in 0..200_u32 {
        let active = harness
            .autonomy
            .response_delay_status(CHARACTER, cfg)
            .is_some_and(|status| status.seconds_until_reply.is_some());
        if active {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn held_reply_recovers_after_crash() {
    let mut harness = TestHarness::boot_with(builder()).await;

    // The reply the *recovery* generation will produce after the restart.
    harness.mock_llm.enqueue_text("Sorry — I'm here now.").await;

    // Send a message and do NOT collect: the reply is held by the delay. This
    // persists the user turn to active.jsonl and the deadline to autonomy state.
    let _ignored = harness
        .conn
        .send_message("are you there?", true)
        .await
        .expect("send message");
    assert!(
        wait_for_hold(&harness).await,
        "response-delay hold never became active"
    );

    // Crash mid-hold (no graceful shutdown).
    let crashed = harness.crash().await;

    // Simulate restarting *after* the hold's deadline would have elapsed: move
    // the persisted deadline to the recent-past `last_user_at` so recovery is
    // due immediately on reboot (rather than waiting out the long `min`).
    let state_path = crashed.data_dir.join(CHARACTER).join("autonomy_state.json");
    let mut state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).expect("read autonomy state"))
            .expect("parse autonomy state");
    let last_user_at = state
        .get("last_user_at")
        .cloned()
        .expect("last_user_at set");
    let object = state.as_object_mut().expect("autonomy state is an object");
    let _previous = object.insert("pending_reply_at".to_owned(), last_user_at);
    std::fs::write(
        &state_path,
        serde_json::to_string_pretty(&state).expect("serialize autonomy state"),
    )
    .expect("write autonomy state");

    let mut rebooted = crashed.reboot_with(builder()).await;

    // The connected-client recovery check fires within a few seconds and streams
    // the recovered reply to the reconnected client.
    let collected = rebooted.collect_stream().await;
    collected.assert_text_contains("Sorry — I'm here now.");

    // The recovered reply is persisted (user + recovered assistant turn).
    let persisted = rebooted.read_persisted_messages();
    assert_eq!(
        persisted.len(),
        2,
        "expected user + recovered assistant turn, got {persisted:?}"
    );
    assert_eq!(
        persisted
            .get(1)
            .and_then(|m| m.get("role"))
            .and_then(|r| r.as_str()),
        Some("assistant")
    );

    // The deadline is cleared once the reply is produced.
    let cfg = rebooted.config.app.behavior.response_delay.clone();
    let status = rebooted
        .autonomy
        .response_delay_status(CHARACTER, &cfg)
        .expect("autonomy state exists after recovery");
    assert!(
        status.seconds_until_reply.is_none(),
        "deadline should be cleared after the recovered reply persists"
    );

    // The recovery request carried the "you kept the user waiting" note.
    let requests = rebooted.mock_llm.received_requests().await;
    let note_present = requests.iter().any(|req| {
        serde_json::to_string(req).is_ok_and(|s| s.contains("before replying to the user's latest"))
    });
    assert!(
        note_present,
        "recovery request should include the response-delay note"
    );

    rebooted.shutdown().await;
}
