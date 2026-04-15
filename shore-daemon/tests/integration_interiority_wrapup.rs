//! Integration test for the interiority wrap-up path.
//!
//! Covers the `hit_cap && recap_text.is_none()` branch in
//! `autonomy::manager::execute_unified_tick`: when the tool loop exhausts
//! `max_tool_rounds` without the model ever emitting a `<recap>`, the manager
//! makes one more non-streaming `generate()` call with an explicit wrap-up
//! system message asking for a recap, and persists any `<recap>` it produces
//! to `{data_dir}/{character}/recaps.jsonl`.
//!
//! Permitted under the testing-policy Rule 2 (trait/HTTP doubles upstream of
//! shore-llm-client) — the autonomy manager is the caller under test, and the
//! wiremock-backed `MockLlmServer` stands in for Anthropic at the HTTP layer.

use std::time::Duration;

use serde_json::json;
use shore_test_harness::{TestConfigBuilder, TestHarness};

const CHARACTER: &str = "TestChar";

/// Cap `max_tool_rounds` at 1 and queue a single `tool_use` response so the
/// only loop iteration hits the cap. The wrap-up call then receives a text
/// response with a `<recap>`, which the manager must persist to
/// `recaps.jsonl`.
fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off")),
        )
        .with_test_writer()
        .try_init();
}

#[tokio::test]
async fn wrap_up_persists_recap_when_iteration_cap_is_hit() {
    init_tracing();
    let mut harness = TestHarness::boot_with(
        TestConfigBuilder::new()
            .autonomy(true)
            .interiority_max_tool_rounds(1),
    )
    .await;

    // 1. Prime the conversation so `ensure_state` spawns the tick task and
    //    `notify_last_request` stores a real `LlmRequest` the tick can reuse.
    harness.mock_llm.enqueue_text("ack").await;
    let _ = harness.send_and_collect("hello").await;

    // Let the handler's post-generation work (autonomy notifications,
    // persistence) settle before we tamper with state.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 2. Queue the tick's mock responses. The interiority tick uses the
    //    *non-streaming* `client.generate()` path, so both slots must be JSON
    //    (not SSE):
    //      iter 0 (CallType::Interiority) — tool_use stop → marks hit_cap.
    //      wrap-up (CallType::ToolLoop)   — text with <recap> for extraction.
    //    `set_next_wake` is intercepted inline by the manager, so the tool
    //    need not be "real" — the mock just has to produce a tool_use stop.
    harness
        .mock_llm
        .enqueue_json_tool_use(
            "tu_wrapup_01",
            "set_next_wake",
            json!({ "hours_from_now": 2.0, "reason": "cap-hit-stub" }),
        )
        .await;

    let expected_recap = "Spent the tick deferring the next wake; \
        picking up this thread next time.";
    harness
        .mock_llm
        .enqueue_json_text(&format!("done. <recap>{expected_recap}</recap>"))
        .await;

    // 3. Schedule an immediate tick and advance virtual time past the
    //    per-character tick interval so the interval fires.
    let dormant = harness.autonomy.interiority_tick_now(CHARACTER);
    assert_eq!(
        dormant,
        Some(false),
        "autonomy state should exist and not be dormant after priming message"
    );

    tokio::time::pause();
    // Tick loop interval is 10s — advance well past it to guarantee a fire.
    tokio::time::advance(Duration::from_secs(15)).await;

    // 4. Poll the recap file — the wrap-up `generate()` round-trip is async,
    //    so we spin until the recap lands or a generous virtual-time budget
    //    expires. Each iteration yields + sleeps 10ms; under paused time
    //    this advances the virtual clock when the runtime is otherwise idle.
    let recap_path = harness.data_dir.join(CHARACTER).join("recaps.jsonl");
    let mut found = false;
    for _ in 0..500 {
        if recap_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&recap_path) {
                if content.contains(expected_recap) {
                    found = true;
                    break;
                }
            }
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    tokio::time::resume();

    // 5. Assert the wrap-up recap was persisted. The `recaps.jsonl` file is
    //    JSONL — one `RecapEntry` per line — so contains() is enough to prove
    //    the tick's wrap-up call reached the mock AND its response drove a
    //    successful persist.
    assert!(
        found,
        "expected wrap-up recap {expected_recap:?} in {}, existed={} contents={:?}",
        recap_path.display(),
        recap_path.exists(),
        std::fs::read_to_string(&recap_path).ok(),
    );

    harness.shutdown().await;
}
