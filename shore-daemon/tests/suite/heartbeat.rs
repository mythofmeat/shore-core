//! Integration tests for the heartbeat wrap-up path.
//!
//! Covers the forced wrap-up branch in
//! `autonomy::manager::execute_heartbeat_tick`: whenever the tool loop ends
//! without a `<recap>` (iteration cap, soft deadline, or natural early exit),
//! the manager makes one more non-streaming `generate()` call with an
//! explicit wrap-up system message asking for a recap, and persists any
//! `<recap>` it produces to a markdown daily note under
//! `characters/{character}/workspace/memory/daily/`.
//!
//! Permitted under the testing-policy Rule 2 (trait/HTTP doubles upstream of
//! shore-llm-client) — the autonomy manager is the caller under test, and the
//! wiremock-backed `MockLlmServer` stands in for Anthropic at the HTTP layer.

use std::time::Duration;

use serde_json::json;
use shore_test_harness::{TestConfigBuilder, TestHarness};

const CHARACTER: &str = "TestChar";

fn daily_notes_contain(config_dir: &std::path::Path, needle: &str) -> bool {
    let daily_dir = shore_config::character_memory_dir(config_dir, CHARACTER).join("daily");
    let Ok(entries) = std::fs::read_dir(&daily_dir) else {
        return false;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        if std::fs::read_to_string(&path)
            .ok()
            .is_some_and(|content| content.contains(needle))
        {
            return true;
        }
    }

    false
}

/// Cap `max_tool_rounds` at 1 and queue a single `tool_use` response so the
/// only loop iteration hits the cap. The wrap-up call then receives a text
/// response with a `<recap>`, which the manager must persist to a daily note.
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
            .heartbeat_max_tool_rounds(1),
    )
    .await;

    // 1. Prime the conversation so `ensure_state` spawns the tick task and
    //    `notify_last_request` stores a real `LlmRequest` the tick can reuse.
    harness.mock_llm.enqueue_text("ack").await;
    let _ = harness.send_and_collect("hello").await;

    // Let the handler's post-generation work (autonomy notifications,
    // persistence) settle before we tamper with state.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 2. Queue the tick's mock responses. The heartbeat tick uses the
    //    *non-streaming* `client.generate()` path, so both slots must be JSON
    //    (not SSE):
    //      iter 0 (CallType::Heartbeat) — tool_use stop → marks hit_cap.
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
    let dormant = harness.autonomy.heartbeat_tick_now(CHARACTER);
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
    let mut found = false;
    for _ in 0..500 {
        if daily_notes_contain(&harness.config.dirs.config, expected_recap) {
            found = true;
            break;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    tokio::time::resume();

    assert!(
        found,
        "expected wrap-up recap {expected_recap:?} in a daily markdown note under {}",
        shore_config::character_memory_dir(&harness.config.dirs.config, CHARACTER)
            .join("daily")
            .display(),
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
            .heartbeat_max_tool_rounds(12),
    )
    .await;

    // Prime the conversation so the autonomy state is spawned.
    harness.mock_llm.enqueue_text("ack").await;
    let _ = harness.send_and_collect("hello").await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Iter 0 (CallType::Heartbeat): plain text, no <recap>, no tool_use.
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

    let dormant = harness.autonomy.heartbeat_tick_now(CHARACTER);
    assert_eq!(
        dormant,
        Some(false),
        "autonomy state should exist and not be dormant after priming message"
    );

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(15)).await;

    let mut found = false;
    for _ in 0..500 {
        if daily_notes_contain(&harness.config.dirs.config, expected_recap) {
            found = true;
            break;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    tokio::time::resume();

    assert!(
        found,
        "expected natural-exit wrap-up recap {expected_recap:?} in a daily markdown note",
    );

    harness.shutdown().await;
}

/// Regression test for the "natural exit drops the final assistant turn" bug.
///
/// When the heartbeat tool loop runs at least one tool-use iteration and
/// then a *subsequent* iteration ends naturally (no tool_use, no `<recap>`),
/// the forced wrap-up call that follows must see the full conversation —
/// including the final assistant turn that triggered the exit. Dropping
/// that turn leaves the wrap-up call looking at a dangling
/// `user: tool_results` followed by the wrap-up system nudge, which real
/// models reliably mistake for "the user gave me tool results, continue
/// tool-calling" and respond with more tool calls rather than a recap.
///
/// This test pins the loop's message-history bookkeeping: the wrap-up
/// request the mock receives must contain the iter-1 assistant text as an
/// assistant turn in its `messages` array.
#[tokio::test]
async fn wrap_up_sees_final_assistant_turn_after_tool_use_then_natural_exit() {
    init_tracing();
    let mut harness = TestHarness::boot_with(
        TestConfigBuilder::new()
            .autonomy(true)
            .heartbeat_max_tool_rounds(12),
    )
    .await;

    // Prime the conversation so the autonomy state exists.
    harness.mock_llm.enqueue_text("ack").await;
    let _ = harness.send_and_collect("hello").await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Iter 0: tool_use. set_next_wake is intercepted inline by the manager,
    // so we don't need the tool to actually exist in the dispatch surface —
    // the mock just has to produce a tool_use stop so the loop continues.
    harness
        .mock_llm
        .enqueue_json_tool_use(
            "tu_natexit_01",
            "set_next_wake",
            json!({ "hours_from_now": 2.0, "reason": "let the thread rest" }),
        )
        .await;

    // Iter 1: plain text, finish_reason=end_turn, no <recap>, no tool_use.
    // The loop sees tool_uses.is_empty() and breaks → natural exit.
    let iter1_text = "just sitting with a thought and watching the light shift";
    harness.mock_llm.enqueue_json_text(iter1_text).await;

    // Wrap-up call: returns a recap. Mock is deterministic — what we're
    // actually testing is the shape of the request that reached this slot.
    let expected_recap = "closed the loop after a quiet moment.";
    harness
        .mock_llm
        .enqueue_json_text(&format!("done. <recap>{expected_recap}</recap>"))
        .await;

    harness.autonomy.heartbeat_tick_now(CHARACTER);

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(15)).await;

    // Wait for the wrap-up's recap to land on disk — that's the signal that
    // all three tick calls have been consumed by the mock and we can read
    // `received_requests()` for assertions.
    let mut recap_landed = false;
    for _ in 0..500 {
        if daily_notes_contain(&harness.config.dirs.config, expected_recap) {
            recap_landed = true;
            break;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::resume();
    assert!(
        recap_landed,
        "wrap-up recap never persisted; tick did not complete its mocks"
    );

    // Find the wrap-up request among all received calls. It's the only one
    // whose messages contain the wrap-up prompt's signature string. The
    // Anthropic provider wraps inline system-role messages in
    // `<system_instruction>` and emits them as a user turn, so the marker
    // appears in message content regardless of the wire role.
    let requests = harness.mock_llm.received_requests().await;
    let wrapup_req = requests
        .iter()
        .find(|r| {
            serde_json::to_string(r)
                .unwrap_or_default()
                .contains("Your private moment is ending")
        })
        .unwrap_or_else(|| {
            panic!(
                "no wrap-up request found in {} received requests; bodies: {:#?}",
                requests.len(),
                requests,
            )
        });

    // The iter-1 assistant turn (plain text, natural exit) must appear as an
    // assistant message in the wrap-up request. Without the fix, the loop
    // breaks before pushing iter-1's response onto request.messages, and this
    // assertion fails.
    let messages = wrapup_req
        .get("messages")
        .and_then(|m| m.as_array())
        .expect("wrap-up request missing messages array");
    let has_iter1_assistant = messages.iter().any(|msg| {
        let role_is_assistant = msg.get("role").and_then(|r| r.as_str()) == Some("assistant");
        let content_has_text = serde_json::to_string(msg.get("content").unwrap_or(&json!(null)))
            .unwrap_or_default()
            .contains(iter1_text);
        role_is_assistant && content_has_text
    });
    assert!(
        has_iter1_assistant,
        "wrap-up request must include the iter-1 assistant turn \
         ({iter1_text:?}); otherwise the wrap-up model sees a dangling \
         user turn and misreads the intent. messages={:#?}",
        messages,
    );

    harness.shutdown().await;
}

/// A completed heartbeat tick must persist its recap to a daily markdown note
/// instead of injecting a hidden `Role::System` message into `active.jsonl`.
#[tokio::test]
async fn tick_recap_persists_to_daily_markdown_note() {
    init_tracing();
    let mut harness = TestHarness::boot_with(
        TestConfigBuilder::new()
            .autonomy(true)
            .heartbeat_max_tool_rounds(12),
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

    harness.autonomy.heartbeat_tick_now(CHARACTER);

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(15)).await;

    let active_path = harness.data_dir.join(CHARACTER).join("active.jsonl");
    let mut found_daily_recap = false;
    for _ in 0..500 {
        if daily_notes_contain(&harness.config.dirs.config, expected_recap) {
            found_daily_recap = true;
            break;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    tokio::time::resume();

    assert!(
        found_daily_recap,
        "expected a daily markdown note containing {expected_recap:?} under {}",
        shore_config::character_memory_dir(&harness.config.dirs.config, CHARACTER)
            .join("daily")
            .display(),
    );

    let active_content = std::fs::read_to_string(&active_path).unwrap();
    assert!(
        !active_content.contains(expected_recap),
        "heartbeat recaps should no longer be injected into active.jsonl"
    );

    harness.shutdown().await;
}
