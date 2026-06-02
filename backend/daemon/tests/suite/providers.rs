use shore_protocol::server_msg::ServerMessage;
use shore_test_harness::TestHarness;

/// Verify that a 429 rate-limit response triggers a retry and the second
/// (success) response is delivered to the client.
///
/// Retry policy: shore-llm retries HTTP 429 up to `max_retries` times
/// (default 2). We enqueue exactly one error followed by one success, so the
/// retry succeeds on the first attempt.
#[tokio::test]
async fn test_rate_limit_triggers_retry() {
    let mut harness = TestHarness::boot().await;

    // Enqueue 429 first (served first by wiremock), then a successful response.
    harness
        .mock_llm
        .enqueue_error(
            429,
            r#"{"error":{"type":"rate_limit_error","message":"Rate limited"}}"#,
        )
        .await;
    harness
        .mock_llm
        .enqueue_text("Rate limit retry succeeded")
        .await;

    let response = harness.send_and_collect("Hello after rate limit").await;

    response.assert_text_contains("Rate limit retry succeeded");
    assert!(
        response.stream_ended,
        "Expected stream_ended after successful retry, got: {:?}",
        response.raw_messages
    );

    let requests = harness.mock_llm.received_requests().await;
    assert!(
        requests.len() >= 2,
        "Expected at least 2 requests to mock LLM (original + retry), got {}",
        requests.len()
    );

    harness.shutdown().await;
}

/// Verify that a 500 server error response triggers a retry and the second
/// (success) response is delivered to the client.
#[tokio::test]
async fn test_server_error_triggers_retry() {
    let mut harness = TestHarness::boot().await;

    harness
        .mock_llm
        .enqueue_error(
            500,
            r#"{"error":{"type":"server_error","message":"Internal server error"}}"#,
        )
        .await;
    harness
        .mock_llm
        .enqueue_text("Server error retry succeeded")
        .await;

    let response = harness.send_and_collect("Hello after server error").await;

    response.assert_text_contains("Server error retry succeeded");
    assert!(
        response.stream_ended,
        "Expected stream_ended after successful retry, got: {:?}",
        response.raw_messages
    );

    let requests = harness.mock_llm.received_requests().await;
    assert!(
        requests.len() >= 2,
        "Expected at least 2 requests to mock LLM (original + retry), got {}",
        requests.len()
    );

    harness.shutdown().await;
}

/// Verify that a malformed/garbage NDJSON response doesn't cause the daemon to
/// hang indefinitely — either an Error message is received, or the stream ends.
///
/// The 30-second collect_stream timeout in TestHarness is the hard backstop
/// against hangs. If the daemon propagates the parse error correctly, we expect
/// either a ServerMessage::Error or stream_ended = true (stream closes cleanly).
#[tokio::test]
async fn test_malformed_ndjson_returns_error() {
    let mut harness = TestHarness::boot().await;

    harness
        .mock_llm
        .enqueue_raw_ndjson("this is not valid NDJSON\ngarbage\n".into())
        .await;

    harness
        .conn
        .send_message("Trigger malformed NDJSON", true)
        .await
        .expect("failed to send message");

    // collect_stream has a 30s timeout — if it completes, the daemon didn't hang.
    let response = harness.collect_stream().await;

    let received_error = response
        .raw_messages
        .iter()
        .any(|m| matches!(m, ServerMessage::Error(_)));

    assert!(
        received_error || response.stream_ended,
        "Expected either an Error message or stream_ended after malformed NDJSON, \
         but got {} messages with stream_ended={}: {:?}",
        response.raw_messages.len(),
        response.stream_ended,
        response.raw_messages,
    );

    harness.shutdown().await;
}
