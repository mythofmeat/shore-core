use shore_test_harness::TestHarness;

#[tokio::test]
async fn test_basic_message_roundtrip() {
    let mut harness = TestHarness::boot().await;

    harness.mock_llm.enqueue_text("Hello from mock!").await;

    let response = harness.send_and_collect("Hi there").await;

    response.assert_text_contains("Hello from mock!");
    assert!(
        response.stream_ended,
        "Expected stream_ended to be true, but it was false"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn test_message_persistence() {
    let mut harness = TestHarness::boot().await;

    harness.mock_llm.enqueue_text("Persisted response").await;

    let _response = harness.send_and_collect("Save this message").await;

    // Give the daemon a moment to flush persistence.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let messages = harness.read_persisted_messages();

    assert!(
        messages.len() >= 2,
        "Expected at least 2 persisted messages (user + assistant), got {}",
        messages.len()
    );

    let has_user = messages.iter().any(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"));
    let has_assistant =
        messages.iter().any(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"));

    assert!(has_user, "No user message found in persisted messages");
    assert!(
        has_assistant,
        "No assistant message found in persisted messages"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn test_streaming_chunks_arrive_in_order() {
    let mut harness = TestHarness::boot().await;

    harness.mock_llm.enqueue_text("Streaming works correctly").await;

    let response = harness.send_and_collect("Test streaming").await;

    response.assert_text_contains("Streaming works correctly");
    assert!(
        response.stream_ended,
        "Expected stream to end after collecting all chunks"
    );
    assert!(
        !response.raw_messages.is_empty(),
        "Expected at least one raw message in the collected response"
    );

    harness.shutdown().await;
}
