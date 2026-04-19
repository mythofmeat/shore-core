//! Integration tests for the interiority wrap-up path.
//!
//! Covers the forced wrap-up branch in
//! `autonomy::manager::execute_unified_tick`: whenever the tool loop ends
//! without a `<recap>` (iteration cap, soft deadline, or natural early exit),
//! the manager makes one more non-streaming `generate()` call with an
//! explicit wrap-up system message asking for a recap, and persists any
//! `<recap>` it produces to `{data_dir}/{character}/recaps.jsonl`.
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

/// Queue a plain-text response with NO `<recap>` tag and NO `tool_use` block
/// for the first loop iteration, so the loop exits naturally after one round
/// with `hit_cap=false` and `recap_text=None`. The manager must still fire a
/// wrap-up call, which receives the recap we queue second.
///
/// Regression test for the bug where the wrap-up was gated on `hit_cap`,
/// causing recaps to silently drop whenever the model ended the tick without
/// volunteering one.
#[tokio::test]
async fn wrap_up_persists_recap_on_natural_exit_without_recap() {
    init_tracing();
    let mut harness = TestHarness::boot_with(
        TestConfigBuilder::new()
            .autonomy(true)
            .interiority_max_tool_rounds(12),
    )
    .await;

    // Prime the conversation so the autonomy state is spawned.
    harness.mock_llm.enqueue_text("ack").await;
    let _ = harness.send_and_collect("hello").await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Iter 0 (CallType::Interiority): plain text, no <recap>, no tool_use.
    // The loop sees finish_reason="end_turn" and breaks immediately.
    harness
        .mock_llm
        .enqueue_json_text("just sitting with a thought.")
        .await;

    // Wrap-up call (CallType::ToolLoop): returns the recap we expect.
    let expected_recap = "natural exit recap — picked up the thread and let it rest.";
    harness
        .mock_llm
        .enqueue_json_text(&format!("closing out. <recap>{expected_recap}</recap>"))
        .await;

    let dormant = harness.autonomy.interiority_tick_now(CHARACTER);
    assert_eq!(
        dormant,
        Some(false),
        "autonomy state should exist and not be dormant after priming message"
    );

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(15)).await;

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

    assert!(
        found,
        "expected natural-exit wrap-up recap {expected_recap:?} in {}, existed={} contents={:?}",
        recap_path.display(),
        recap_path.exists(),
        std::fs::read_to_string(&recap_path).ok(),
    );

    harness.shutdown().await;
}

/// A completed interiority tick must persist its recap to `active.jsonl` as a
/// `Role::System` message so the recap survives compaction and the next
/// payload sees it at its natural chronological position. This replaces the
/// old ephemeral re-injection from `recaps.jsonl` in `trim_messages`.
#[tokio::test]
async fn tick_recap_persists_as_system_message_in_active_jsonl() {
    use shore_protocol::types::{Message, Role};

    init_tracing();
    let mut harness = TestHarness::boot_with(
        TestConfigBuilder::new()
            .autonomy(true)
            .interiority_max_tool_rounds(12),
    )
    .await;

    // Prime the conversation so the autonomy state exists.
    harness.mock_llm.enqueue_text("ack").await;
    let _ = harness.send_and_collect("hello").await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Iter 0: plain text, natural exit.
    harness
        .mock_llm
        .enqueue_json_text("thinking quietly.")
        .await;

    // Wrap-up: returns the recap we expect to land as a System message.
    let expected_recap = "noticed the light changing; waited for ren to come back";
    harness
        .mock_llm
        .enqueue_json_text(&format!("done. <recap>{expected_recap}</recap>"))
        .await;

    harness.autonomy.interiority_tick_now(CHARACTER);

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(15)).await;

    let active_path = harness.data_dir.join(CHARACTER).join("active.jsonl");
    let mut found_system_recap = false;
    for _ in 0..500 {
        if let Ok(content) = std::fs::read_to_string(&active_path) {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(mut msg) = serde_json::from_str::<Message>(line) {
                    msg.normalize();
                    if msg.role == Role::System && msg.content.contains(expected_recap) {
                        found_system_recap = true;
                        break;
                    }
                }
            }
        }
        if found_system_recap {
            break;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    tokio::time::resume();

    assert!(
        found_system_recap,
        "expected a Role::System message containing {expected_recap:?} in {}; contents={:?}",
        active_path.display(),
        std::fs::read_to_string(&active_path).ok(),
    );

    harness.shutdown().await;
}
