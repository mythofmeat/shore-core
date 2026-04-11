use std::time::Duration;

use serde_json::json;
use shore_client::connection::SWPConnection;
use shore_protocol::server_msg::ServerMessage;
use shore_test_harness::collected::CollectedResponse;
use shore_test_harness::TestHarness;
use tokio::time::timeout;

/// Collect server messages from a raw SWPConnection until StreamEnd or Error.
async fn collect_stream_from(conn: &mut SWPConnection) -> CollectedResponse {
    let collect_timeout = Duration::from_secs(30);
    let mut collected = CollectedResponse::new();
    let deadline = tokio::time::Instant::now() + collect_timeout;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!(
                "collect_stream_from timed out; collected {} messages",
                collected.raw_messages.len(),
            );
        }
        let msg = timeout(remaining, conn.recv())
            .await
            .expect("collect_stream_from timed out waiting for message")
            .expect("failed to recv server message");

        if collected.push(msg) {
            return collected;
        }
    }
}

/// Both clients connected to the same daemon should receive broadcast messages
/// (stream chunks and StreamEnd) when a user message triggers a generation.
#[tokio::test]
async fn test_second_client_receives_broadcasts() {
    let mut harness = TestHarness::boot().await;
    let mut second = harness.connect_second_client().await;

    harness.mock_llm.enqueue_text("broadcast hello").await;

    // Send from the first client.
    harness
        .conn
        .send_message("Hi there", true)
        .await
        .expect("failed to send message");

    // Collect on both clients concurrently.
    let (r1, r2) = tokio::join!(harness.collect_stream(), collect_stream_from(&mut second),);

    r1.assert_text_contains("broadcast hello");
    r2.assert_text_contains("broadcast hello");
    assert!(r1.stream_ended);
    assert!(r2.stream_ended);

    harness.shutdown().await;
}

/// Sending a new message while the previous generation is in-flight should
/// abort the first generation and produce a response for the second.
#[tokio::test]
async fn test_new_message_during_generation_aborts_previous() {
    let mut harness = TestHarness::boot().await;

    // First: a hanging response that will never complete on its own.
    harness.mock_llm.enqueue_hanging_optional().await;

    // Send first message — starts a generation that blocks.
    harness
        .conn
        .send_message("first message", true)
        .await
        .expect("failed to send first message");

    // Give the daemon a moment to start the first generation and make the HTTP
    // request to the mock.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Enqueue the response for the second message.
    harness.mock_llm.enqueue_text("second response").await;

    // Send second message — should abort the first generation.
    harness
        .conn
        .send_message("second message", true)
        .await
        .expect("failed to send second message");

    // The first generation was aborted before it could emit any events
    // (the hanging mock delays the HTTP response, so no StreamStart was sent).
    // The second generation's stream is the only one we need to collect.
    //
    // However, the handler may emit a cancelled StreamEnd when aborting,
    // so we may need to drain one extra StreamEnd before the real response.
    let mut response = harness.collect_stream().await;
    if response.text.is_empty() && response.stream_ended {
        // This was the cancelled StreamEnd from the aborted first generation.
        response = harness.collect_stream().await;
    }

    response.assert_text_contains("second response");
    assert!(response.stream_ended);

    harness.shutdown().await;
}

/// Dropping the SWP connection during an in-flight generation should not crash
/// the daemon. A new client should be able to connect and get responses.
#[tokio::test]
async fn test_client_disconnect_during_generation() {
    let mut harness = TestHarness::boot().await;

    // Connect a "victim" client that we will drop mid-generation.
    let mut victim = harness.connect_second_client().await;

    // Hanging response — generation will block indefinitely.
    harness.mock_llm.enqueue_hanging_optional().await;

    // Send a message from the victim to start a hanging generation.
    victim
        .send_message("start hanging", true)
        .await
        .expect("failed to send message");

    // Give the daemon a moment to start generation.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Drop the harness's first connection too, so the server sees
    // AllClientsDisconnected only after the victim is dropped.
    // Actually — we keep harness.conn alive so the server does NOT
    // see AllClientsDisconnected. We only drop the victim.
    drop(victim);

    // Wait for the server to detect the victim's disconnect.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The daemon should still be alive. Drain any stale broadcast messages
    // (StreamStart, cancelled StreamEnd, etc.) that arrived on the first client
    // from the victim's generation.
    loop {
        match timeout(Duration::from_millis(500), harness.conn.recv()).await {
            Ok(Ok(_)) => continue, // absorb stale message
            _ => break,            // timeout or error — done draining
        }
    }

    // Enqueue a response for a new message.
    harness.mock_llm.enqueue_text("daemon is alive").await;

    // Send from the surviving first client. The handler will abort any
    // in-flight generation (the hanging one) when it receives a new message.
    let response = harness.send_and_collect("are you alive?").await;

    response.assert_text_contains("daemon is alive");
    assert!(response.stream_ended);

    harness.shutdown().await;
}

/// Both clients should receive ToolCall and ToolResult events during a tool
/// execution roundtrip.
#[tokio::test]
async fn test_multiple_clients_both_get_tool_events() {
    let mut harness = TestHarness::boot().await;
    let mut second = harness.connect_second_client().await;

    // Phase 1: LLM responds with a tool_use call.
    harness
        .mock_llm
        .enqueue_tool_use("toolu_concurrent01", "check_time", json!({}))
        .await;

    // Phase 2: LLM responds with final text after tool result.
    harness
        .mock_llm
        .enqueue_text("Time checked by both clients.")
        .await;

    // Send from first client.
    harness
        .conn
        .send_message("What time is it?", true)
        .await
        .expect("failed to send message");

    // Phase 1 stream: tool_use SSE (StreamStart → StreamEnd).
    let (p1_c1, p1_c2) = tokio::join!(harness.collect_stream(), collect_stream_from(&mut second),);
    assert!(p1_c1.stream_ended);
    assert!(p1_c2.stream_ended);

    // Phase 2 stream: ToolCall → ToolResult → StreamStart → chunks → StreamEnd.
    let (p2_c1, p2_c2) = tokio::join!(harness.collect_stream(), collect_stream_from(&mut second),);

    // Both clients should see the final text.
    p2_c1.assert_text_contains("Time checked by both clients");
    p2_c2.assert_text_contains("Time checked by both clients");

    // Both clients should have received ToolCall events.
    let has_tool_call =
        |msgs: &[ServerMessage]| msgs.iter().any(|m| matches!(m, ServerMessage::ToolCall(_)));
    assert!(
        has_tool_call(&p2_c1.raw_messages),
        "First client missing ToolCall event"
    );
    assert!(
        has_tool_call(&p2_c2.raw_messages),
        "Second client missing ToolCall event"
    );

    // Both clients should have received ToolResult events.
    let has_tool_result = |msgs: &[ServerMessage]| {
        msgs.iter()
            .any(|m| matches!(m, ServerMessage::ToolResult(_)))
    };
    assert!(
        has_tool_result(&p2_c1.raw_messages),
        "First client missing ToolResult event"
    );
    assert!(
        has_tool_result(&p2_c2.raw_messages),
        "Second client missing ToolResult event"
    );

    harness.shutdown().await;
}
