use shore_config::{
    character_active_jsonl, character_compaction_manifest, character_data_dir,
    character_segments_dir,
};
use shore_test_harness::{TestConfigBuilder, TestHarness};

use crate::helpers::wait_for_persisted_messages;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Build a standard compaction-enabled harness:
/// max_turns=3, min_turns=2, keep_recent=1.
async fn compaction_harness_3turns() -> TestHarness {
    TestHarness::boot_with(
        TestConfigBuilder::new()
            .compaction(true)
            .compaction_max_turns(3)
            .compaction_min_turns(2)
            .compaction_keep_recent(1),
    )
    .await
}

/// Memory write a compaction tool-loop pass should emit. The harness
/// helper enqueues the two-round wire shape (tool_use → end_turn) the
/// daemon's tool loop drives.
async fn enqueue_compaction_write(harness: &mut TestHarness, topic: &str) {
    let path = format!("memory/topics/{topic}.md");
    let content = format!(
        "# {topic}\n\n- User and assistant discussed {topic}\n- The exchange was informative\n",
    );
    harness
        .mock_llm
        .enqueue_json_compaction_write_optional(&path, &content)
        .await;
}

/// Send N user messages, enqueuing a chat LLM response before each send.
/// Compaction mocks must be enqueued AFTER this, to avoid mock ordering conflicts.
async fn send_n_messages(harness: &mut TestHarness, n: usize) {
    for i in 1..=n {
        harness.mock_llm.enqueue_text(&format!("Reply {i}")).await;
        let _resp = harness.send_and_collect(&format!("Message {i}")).await;
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// After 3 user messages (max_turns=3), compaction should trim active.jsonl.
/// With keep_recent=1, only 1 user+assistant pair should remain.
#[tokio::test]
async fn test_compaction_triggers_on_max_turns() {
    let mut harness = compaction_harness_3turns().await;

    // Send 3 messages — active.jsonl will have 6 entries afterwards.
    send_n_messages(&mut harness, 3).await;

    // Ensure all 3 turns (6 messages) are persisted before compaction snapshots
    // the transcript; if a turn is still in flight it sees < max_turns and skips.
    let _ignored = wait_for_persisted_messages(
        &harness,
        |m| m.len() >= 6,
        "3 turns to persist before compaction",
    )
    .await;

    // Enqueue compaction mocks AFTER chat messages so ordering doesn't interfere.
    // The compaction LLM call uses non-streaming JSON (generate endpoint)
    // and now runs a two-round tool loop: tool_use(write) → end_turn.
    enqueue_compaction_write(&mut harness, "messages").await;
    // Optional hybrid retrieval indexing may ask for one embedding. dimensions=8 per TestConfigBuilder.
    harness.mock_llm.enqueue_embedding_optional(8).await;

    // Directly trigger compaction — bypasses the 30s autonomy tick.
    harness.trigger_compaction_now("TestChar").await;

    // Poll for the trimmed transcript instead of a fixed sleep — the rewrite of
    // active.jsonl can lag the compaction call's return on a loaded runner.
    // Without compaction: 3 user + 3 assistant = 6 messages.
    // With compaction (keep_recent=1): only 1 user + 1 assistant = 2 messages.
    let messages = wait_for_persisted_messages(
        &harness,
        |m| m.len() < 6,
        "compaction to trim active.jsonl below 6 messages (does the mock write \
         response include valid <memory><write> blocks?)",
    )
    .await;
    assert!(
        messages.len() < 6,
        "Expected compaction to trim active.jsonl below 6 messages, got {}.",
        messages.len()
    );

    harness.shutdown().await;
}

/// With keep_recent=2 and max_turns=4, compaction should preserve the
/// 2 most-recent user+assistant turn pairs.
#[tokio::test]
async fn test_compaction_keeps_recent_turns() {
    let mut harness = TestHarness::boot_with(
        TestConfigBuilder::new()
            .compaction(true)
            .compaction_max_turns(4)
            .compaction_min_turns(2)
            .compaction_keep_recent(2),
    )
    .await;

    // Send 4 messages — last two should be preserved after compaction.
    for i in 1..=4 {
        harness.mock_llm.enqueue_text(&format!("Reply {i}")).await;
        let _resp = harness
            .send_and_collect(&format!("Unique message {i}"))
            .await;
    }

    // Ensure all 4 turns (8 messages) are persisted before compaction snapshots.
    let _ignored = wait_for_persisted_messages(
        &harness,
        |m| m.len() >= 8,
        "4 turns to persist before compaction",
    )
    .await;

    // Enqueue compaction mocks after chat messages.
    enqueue_compaction_write(&mut harness, "recent-turns").await;
    harness.mock_llm.enqueue_embedding_optional(8).await;

    harness.trigger_compaction_now("TestChar").await;

    // Without compaction: 4 user + 4 assistant = 8.
    // With compaction (keep_recent=2): 2 user + 2 assistant = 4.
    let messages = wait_for_persisted_messages(
        &harness,
        |m| m.len() < 8,
        "compaction to trim active.jsonl below 8 messages",
    )
    .await;
    assert!(
        messages.len() < 8,
        "Expected compaction to trim active.jsonl below 8 messages, got {}",
        messages.len()
    );

    // The last 2 user messages must still be present.
    let raw = messages
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        raw.contains("Unique message 3") || raw.contains("Unique message 4"),
        "Expected at least one recent user message to survive compaction; messages:\n{raw}"
    );

    harness.shutdown().await;
}

/// After compaction fires, the daemon must still be able to handle new messages.
#[tokio::test]
async fn test_messages_still_work_after_compaction() {
    let mut harness = compaction_harness_3turns().await;

    // Trigger compaction via 3 messages.
    send_n_messages(&mut harness, 3).await;

    // Ensure all 3 turns are persisted before compaction snapshots the transcript.
    let _ignored = wait_for_persisted_messages(
        &harness,
        |m| m.len() >= 6,
        "3 turns to persist before compaction",
    )
    .await;

    // Enqueue compaction mocks after chat messages.
    enqueue_compaction_write(&mut harness, "post-compaction").await;
    harness.mock_llm.enqueue_embedding_optional(8).await;

    harness.trigger_compaction_now("TestChar").await;

    // Send one more message post-compaction.
    harness.mock_llm.enqueue_text("Post-compaction reply").await;
    let response = harness.send_and_collect("Post-compaction message").await;

    response.assert_text_contains("Post-compaction reply");
    assert!(
        response.stream_ended,
        "Expected post-compaction response to complete successfully"
    );

    // Wait for the new message to land on disk rather than sleeping a fixed
    // interval — persistence lags the stream end on a loaded runner.
    let messages = wait_for_persisted_messages(
        &harness,
        |m| {
            m.iter()
                .any(|v| v.to_string().contains("Post-compaction message"))
        },
        "post-compaction user message to persist",
    )
    .await;
    let raw = messages
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        raw.contains("Post-compaction message"),
        "Expected post-compaction user message to be persisted; active.jsonl content:\n{raw}"
    );

    harness.shutdown().await;
}

/// End-to-end pin for the compaction-tail wire shape
/// (`COMPACTION_TAIL_ENTRY_COUNT`, `append_compaction_tail`).
///
/// The cached compaction path appends exactly two entries after the chat
/// prefix: one `role:"user"` ("compact now") plus one pinned inline
/// `role:"system"` (the compaction instruction). The daemon sends this
/// canonical request to the sidecar; provider-specific merging/wrapping
/// happens after the sidecar boundary. The inline-system shape
/// (instead of `system_suffix`) is what keeps the compact-now slot
/// byte-stable across compaction tool-loop rounds; the contract is pinned
/// at the unit level by
/// `compaction_tool_loop_keeps_compact_now_user_byte_stable_across_rounds`
/// in `memory::compaction_impls::tests`.
///
/// This complements the unit-level shore-llm regression tests by driving
/// the actual daemon flow end-to-end: a regression at the `summarize`
/// caller in `compaction/mod.rs` (e.g. constructing a 2-item `llm_messages`
/// for the cached path) would pass the shore-llm tests but fail here.
#[tokio::test]
async fn test_compaction_cached_path_appends_exactly_one_tail() {
    // Use a high max_turns so the daemon doesn't fire inline compaction
    // during the warm-up sends — we want to drive compaction manually
    // from a known cached-request state.
    let mut harness = TestHarness::boot_with(
        TestConfigBuilder::new()
            .compaction(true)
            .compaction_max_turns(100)
            .compaction_min_turns(2)
            .compaction_keep_recent(1),
    )
    .await;

    // Warm the chat cache with 3 user/assistant exchanges.
    send_n_messages(&mut harness, 3).await;

    // Capture the cached request the autonomy manager will hand to
    // compaction. This is the same `cached_last_request` lookup
    // `trigger_compaction_now` performs internally.
    let cached_at_compaction = harness
        .autonomy
        .cached_last_request("TestChar")
        .expect("cached request should exist after 3 chat turns");
    let cached_prefix_len = cached_at_compaction.messages.len();
    assert!(
        cached_prefix_len > 0,
        "cached prefix must be non-empty after 3 chat sends"
    );

    let pre_trigger_request_count = harness.mock_llm.received_requests().await.len();

    enqueue_compaction_write(&mut harness, "tail-length").await;
    harness.mock_llm.enqueue_embedding_optional(8).await;

    harness.trigger_compaction_now("TestChar").await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Pick the first non-streaming `messages` POST after the warm-up —
    // streaming chat sends always have `stream: true`, embeddings hit
    // a different path. The compaction LLM call is the single
    // non-streaming `messages` POST in this window.
    let all_requests = harness.mock_llm.received_requests().await;
    let compaction_req = all_requests
        .iter()
        .skip(pre_trigger_request_count)
        .find(|r| {
            r.get("messages").is_some()
                && r.get("stream").and_then(serde_json::Value::as_bool) != Some(true)
        })
        .expect("expected one non-streaming compaction LLM call after trigger");

    let compaction_msgs = compaction_req
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .expect("compaction request must carry a messages array");

    // The cached request carried `cached_prefix_len` messages at the
    // moment `trigger_compaction_now` snapshotted it. `append_compaction_tail`
    // adds exactly 2 entries (one user, one pinned inline `role:"system"`),
    // and the daemon sends that canonical shape to the sidecar. The `+ 2` is
    // hardcoded on purpose: pinning the wire shape against a literal keeps this
    // regression test from silently tracking an accidental change to
    // `COMPACTION_TAIL_ENTRY_COUNT`.
    assert_eq!(
        compaction_msgs.len(),
        cached_prefix_len + 2,
        "cached compaction sidecar shape must be `cached_prefix_len + 2`; \
         got {} vs expected {} = {} + 2.\n\
         If this fails, check `append_compaction_tail` and `build_compaction_request`.",
        compaction_msgs.len(),
        cached_prefix_len + 2,
        cached_prefix_len,
    );

    // And the appended tail must be compact-now user followed by pinned inline
    // system; the sidecar adapter owns any provider-specific merge after this.
    let compact_now_tail = &compaction_msgs[cached_prefix_len];
    assert_eq!(
        compact_now_tail
            .get("role")
            .and_then(serde_json::Value::as_str),
        Some("user"),
        "first compaction tail message must be the compact-now user turn"
    );
    let system_tail = compaction_msgs
        .last()
        .expect("compaction request must have a tail message");
    assert_eq!(
        system_tail.get("role").and_then(serde_json::Value::as_str),
        Some("system"),
        "final compaction tail message must be the pinned inline system"
    );

    harness.shutdown().await;
}

/// End-to-end pin for the `retain_long` payload-log routing introduced in
/// the 2026-05-14 refactor. Every background call site (compaction,
/// dreaming, heartbeat) must set `LlmRequest::retain_long = true` so
/// `debug_log::log_request` routes the call to
/// `<cache>/debug/api_logs_long/` instead of the chat-volume
/// `<cache>/debug/api_logs/`.
///
/// A missing `request.retain_long = true` at any of the three sites is
/// indistinguishable from "feature disabled" at the unit level — the
/// chat payloads would still land in `api_logs/`. This test asserts the
/// long-retention tier picks up the compaction call specifically; the
/// chat warm-up sends are pinned to land in the chat tier.
#[tokio::test]
async fn test_retain_long_routes_background_payloads_to_long_tier() {
    let mut harness = TestHarness::boot_with(
        TestConfigBuilder::new()
            .compaction(true)
            .compaction_max_turns(100)
            .compaction_min_turns(2)
            .compaction_keep_recent(1)
            .api_payload_logging(true),
    )
    .await;

    send_n_messages(&mut harness, 3).await;

    let cache_dir = harness.config.dirs.cache.clone();
    let chat_logs = cache_dir.join("debug").join("api_logs");
    let long_logs = cache_dir.join("debug").join("api_logs_long");

    // Chat warm-up: every chat send must have written its request + response
    // JSON under `api_logs/`. The long-tier directory must not have been
    // created or remain empty.
    let chat_files_before_compaction: Vec<_> = std::fs::read_dir(&chat_logs)
        .expect("api_logs dir must exist after chat warm-up")
        .filter_map(Result::ok)
        .collect();
    assert!(
        chat_files_before_compaction.len() >= 3,
        "expected at least 3 chat-tier files (one per chat send), got {} in {}",
        chat_files_before_compaction.len(),
        chat_logs.display(),
    );
    assert!(
        !long_logs.exists() || std::fs::read_dir(&long_logs).map_or(0, Iterator::count) == 0,
        "long-retention dir should be empty before any background task runs; \
         path: {}",
        long_logs.display(),
    );

    enqueue_compaction_write(&mut harness, "retain-long").await;
    harness.mock_llm.enqueue_embedding_optional(8).await;

    harness.trigger_compaction_now("TestChar").await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // The compaction call must have landed in the long-retention tier.
    let long_files: Vec<_> = std::fs::read_dir(&long_logs)
        .unwrap_or_else(|e| {
            panic!(
                "api_logs_long dir must exist after compaction (the daemon \
                 sets retain_long=true on compaction requests); error: {e}; \
                 path: {}",
                long_logs.display()
            )
        })
        .filter_map(Result::ok)
        .collect();
    assert!(
        !long_files.is_empty(),
        "compaction payload must land in {}; an empty dir here means \
         `RealCompactionLlm::build_compaction_request` forgot to set \
         `request.retain_long = true`",
        long_logs.display(),
    );

    // Sanity-check that the chat-tier counter did NOT pick up the
    // compaction call (i.e., we didn't double-route).
    let chat_files_after: Vec<_> = std::fs::read_dir(&chat_logs)
        .expect("chat tier dir present")
        .filter_map(Result::ok)
        .collect();
    assert_eq!(
        chat_files_after.len(),
        chat_files_before_compaction.len(),
        "compaction must not write into the chat-tier dir; \
         before={}, after={}",
        chat_files_before_compaction.len(),
        chat_files_after.len(),
    );

    harness.shutdown().await;
}

/// End-to-end pin for the `data_dir.join(character)` → `character_data_dir`
/// helper migration introduced in the 2026-05-14 refactor. Drives chat +
/// compaction + heartbeat against a real daemon, then asserts every
/// expected per-character file ended up under the canonical
/// `<data>/TestChar/` root (using the same `shore_config::character_*`
/// helpers that the production code now uses).
///
/// Catches: a stray `data_dir.join(character)` call that returns a
/// subtly wrong root (e.g., a helper that drops a path segment or a
/// caller that passes the data_dir-root instead of the character-root
/// to a downstream helper). Such a regression would write/read from a
/// sibling directory and only surface here, not in unit tests.
#[tokio::test]
async fn test_character_data_dir_paths_through_full_stack() {
    let mut harness = TestHarness::boot_with(
        TestConfigBuilder::new()
            .autonomy(true)
            .compaction(true)
            .compaction_max_turns(100)
            .compaction_min_turns(2)
            .compaction_keep_recent(1)
            .heartbeat_max_tool_rounds(2),
    )
    .await;

    let data_dir = harness.config.dirs.data.clone();
    let char_root = character_data_dir(&data_dir, "TestChar");

    // Chat warm-up — exercises handler::task::handle_generation, which
    // was migrated to call `character_data_dir(...)`.
    send_n_messages(&mut harness, 3).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // After three sends, the canonical active.jsonl path must exist.
    let active = character_active_jsonl(&data_dir, "TestChar");
    assert!(
        active.exists(),
        "active.jsonl must exist under canonical path {}",
        active.display()
    );
    assert!(
        active.starts_with(&char_root),
        "active.jsonl must live under the character root: {} not under {}",
        active.display(),
        char_root.display(),
    );

    // Trigger compaction — exercises memory/compaction/{mod,background}
    // and memory/compaction_impls, all of which were migrated.
    enqueue_compaction_write(&mut harness, "paths-fullstack").await;
    harness.mock_llm.enqueue_embedding_optional(8).await;
    harness.trigger_compaction_now("TestChar").await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let segments = character_segments_dir(&data_dir, "TestChar");
    let manifest = character_compaction_manifest(&data_dir, "TestChar");
    assert!(
        segments.exists(),
        "segments dir must exist after compaction: {}",
        segments.display()
    );
    assert!(
        manifest.exists(),
        "compaction.json must exist after compaction: {}",
        manifest.display()
    );

    let segment_files: Vec<_> = std::fs::read_dir(&segments)
        .expect("segments dir readable")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    assert!(
        !segment_files.is_empty(),
        "compaction must produce at least one segment file in {}",
        segments.display()
    );

    // Trigger a heartbeat tick — exercises autonomy/manager paths,
    // including `heartbeat_log_path = character_data_dir(...).join("heartbeat.jsonl")`.
    harness.mock_llm.enqueue_json_text("HEARTBEAT_OK").await;
    let _ignored = harness.autonomy.heartbeat_tick_now("TestChar");
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(15)).await;
    for _ in 0..100 {
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    tokio::time::resume();

    let heartbeat_log = char_root.join("heartbeat.jsonl");
    assert!(
        heartbeat_log.exists(),
        "heartbeat.jsonl must exist under {} after a tick",
        heartbeat_log.display()
    );

    // Finally: no stray files in sibling directories under `data_dir`.
    // Every directory directly under `data_dir` that looks like a
    // character workspace must be exactly `TestChar/` — a stray
    // `TestChar_extra/` or similar would indicate a path-construction
    // bug in one of the migrated sites.
    let stray_dirs: Vec<String> = std::fs::read_dir(&data_dir)
        .expect("data_dir readable")
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        // ledger.db / cache / runtime files; only character-shaped dirs.
        .filter(|name| !name.starts_with('.'))
        .filter(|name| name.as_str() != "TestChar")
        .collect();
    assert!(
        stray_dirs.is_empty(),
        "no character-shaped directories besides `TestChar/` should exist \
         under data_dir; found: {stray_dirs:?}"
    );

    harness.shutdown().await;
}
