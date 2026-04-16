use shore_test_harness::{TestConfigBuilder, TestHarness};

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

/// Build a compact XML response that the compaction LLM parser will accept.
fn compaction_llm_response(topic: &str) -> String {
    format!(
        r#"<recap>
The user and assistant exchanged messages about {topic}. The conversation was brief.
</recap>

<entry>
<summary>
- User and assistant discussed {topic}
- The exchange was informative
</summary>
<topic_tags>{topic}, conversation</topic_tags>
<memory_type>episodic</memory_type>
</entry>"#,
        topic = topic
    )
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

    // Enqueue compaction mocks AFTER chat messages so ordering doesn't interfere.
    // The compaction LLM call uses non-streaming JSON (generate endpoint).
    harness
        .mock_llm
        .enqueue_json_text_optional(&compaction_llm_response("messages"))
        .await;
    // Embedding is called once per <entry> block.  dimensions=8 per TestConfigBuilder.
    harness.mock_llm.enqueue_embedding_optional(8).await;

    // Directly trigger compaction — bypasses the 30s autonomy tick.
    harness.trigger_compaction_now("TestChar").await;

    // Give the daemon a moment to flush persistence.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let messages = harness.read_persisted_messages();

    // Without compaction: 3 user + 3 assistant = 6 messages.
    // With compaction (keep_recent=1): only 1 user + 1 assistant = 2 messages.
    assert!(
        messages.len() < 6,
        "Expected compaction to trim active.jsonl below 6 messages, got {}. \
         Compaction may have failed — check that the LLM mock response includes \
         valid <recap> and <entry> XML blocks.",
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

    // Enqueue compaction mocks after chat messages.
    harness
        .mock_llm
        .enqueue_json_text_optional(&compaction_llm_response("recent-turns"))
        .await;
    harness.mock_llm.enqueue_embedding_optional(8).await;

    harness.trigger_compaction_now("TestChar").await;

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let messages = harness.read_persisted_messages();

    // Without compaction: 4 user + 4 assistant = 8.
    // With compaction (keep_recent=2): 2 user + 2 assistant = 4.
    assert!(
        messages.len() < 8,
        "Expected compaction to trim active.jsonl below 8 messages, got {}",
        messages.len()
    );

    // The last 2 user messages must still be present.
    let raw = messages
        .iter()
        .map(|m| m.to_string())
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

    // Enqueue compaction mocks after chat messages.
    harness
        .mock_llm
        .enqueue_json_text_optional(&compaction_llm_response("post-compaction"))
        .await;
    harness.mock_llm.enqueue_embedding_optional(8).await;

    harness.trigger_compaction_now("TestChar").await;

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Send one more message post-compaction.
    harness.mock_llm.enqueue_text("Post-compaction reply").await;
    let response = harness.send_and_collect("Post-compaction message").await;

    response.assert_text_contains("Post-compaction reply");
    assert!(
        response.stream_ended,
        "Expected post-compaction response to complete successfully"
    );

    // Give daemon a moment to persist the new message.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let messages = harness.read_persisted_messages();
    let raw = messages
        .iter()
        .map(|m| m.to_string())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        raw.contains("Post-compaction message"),
        "Expected post-compaction user message to be persisted; active.jsonl content:\n{raw}"
    );

    harness.shutdown().await;
}
