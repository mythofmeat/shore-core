//! Integration tests for organic heartbeat semantics.
//!
//! A heartbeat is a bounded private tool loop governed by HEARTBEAT.md. The
//! runtime may deliver `<sendMessage>` and intercept `set_next_wake`, but it
//! must not force recap generation or write daily memory notes by itself.

use std::time::Duration;

use serde_json::json;
use shore_test_harness::{TestConfigBuilder, TestHarness};

const CHARACTER: &str = "TestChar";

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off")),
        )
        .with_test_writer()
        .try_init();
}

async fn primed_harness(max_tool_rounds: u32) -> TestHarness {
    let mut harness = TestHarness::boot_with(
        TestConfigBuilder::new()
            .autonomy(true)
            .heartbeat_max_tool_rounds(max_tool_rounds),
    )
    .await;
    harness.mock_llm.enqueue_text("ack").await;
    let _ = harness.send_and_collect("hello").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    harness
}

async fn fire_tick(harness: &TestHarness) {
    let dormant = harness.autonomy.heartbeat_tick_now(CHARACTER);
    assert_eq!(dormant, Some(false));
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(15)).await;
    for _ in 0..100 {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::resume();
}

#[tokio::test]
async fn heartbeat_ok_is_one_call_and_writes_no_memory_file() {
    init_tracing();
    let harness = primed_harness(12).await;
    let before = harness.mock_llm.received_requests().await.len();

    harness.mock_llm.enqueue_json_text("HEARTBEAT_OK").await;
    fire_tick(&harness).await;

    let requests = harness.mock_llm.received_requests().await;
    assert_eq!(
        requests.len() - before,
        1,
        "a no-tool heartbeat should make exactly one LLM call"
    );
    let memory_dir = shore_config::character_memory_dir(&harness.config.dirs.config, CHARACTER);
    assert!(
        !memory_dir.join("daily").exists(),
        "heartbeat must not create daily notes automatically"
    );
    let active = std::fs::read_to_string(harness.data_dir.join(CHARACTER).join("active.jsonl"))
        .unwrap_or_default();
    assert!(
        !active.contains("HEARTBEAT_OK"),
        "HEARTBEAT_OK is an ack/drop signal, not a user-visible message"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn send_message_is_delivered_without_recap() {
    init_tracing();
    let harness = primed_harness(12).await;
    let body = "I found the thread again.";

    harness
        .mock_llm
        .enqueue_json_text(&format!("<sendMessage>{body}</sendMessage>"))
        .await;
    fire_tick(&harness).await;

    let active = std::fs::read_to_string(harness.data_dir.join(CHARACTER).join("active.jsonl"))
        .unwrap_or_default();
    assert!(active.contains(body), "sendMessage content should persist");
    let memory_dir = shore_config::character_memory_dir(&harness.config.dirs.config, CHARACTER);
    assert!(
        !memory_dir.join("daily").exists(),
        "sendMessage should not require or create a recap note"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn set_next_wake_still_schedules_from_tool_use() {
    init_tracing();
    let harness = primed_harness(1).await;

    harness
        .mock_llm
        .enqueue_json_tool_use(
            "tu_wake_01",
            "set_next_wake",
            json!({ "hours_from_now": 2.0, "reason": "pick this up later" }),
        )
        .await;
    fire_tick(&harness).await;

    let events = harness.autonomy.heartbeat_log(CHARACTER, 20);
    assert!(
        events
            .iter()
            .any(|event| event.detail.contains("set_next_wake: 2.0h")),
        "set_next_wake should be intercepted and logged"
    );

    harness.shutdown().await;
}
