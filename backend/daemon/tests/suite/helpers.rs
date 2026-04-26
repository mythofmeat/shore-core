use std::time::Duration;

use shore_protocol::server_msg::ServerMessage;
use shore_test_harness::TestHarness;
use tokio::time::{sleep, timeout, Instant};

/// Collect whatever messages arrive within a bounded duration.
pub async fn collect_messages_for(
    conn: &mut shore_swp_client::connection::SWPConnection,
    duration: Duration,
) -> Vec<ServerMessage> {
    let deadline = Instant::now() + duration;
    let mut messages = Vec::new();

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return messages;
        }

        match timeout(remaining.min(Duration::from_millis(50)), conn.recv()).await {
            Ok(Ok(msg)) => messages.push(msg),
            Ok(Err(_)) => return messages,
            Err(_) => return messages,
        }
    }
}

pub async fn wait_for_mock_requests(harness: &TestHarness, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if harness.mock_llm.received_requests().await.len() >= expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Timed out waiting for {expected} mock LLM request(s)"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

pub fn is_request_scoped_message(msg: &ServerMessage) -> bool {
    matches!(
        msg,
        ServerMessage::CommandOutput(_)
            | ServerMessage::Error(_)
            | ServerMessage::StreamStart(_)
            | ServerMessage::StreamChunk(_)
            | ServerMessage::StreamEnd(_)
            | ServerMessage::ToolCall(_)
            | ServerMessage::ToolResult(_)
            | ServerMessage::SendImage(_)
            | ServerMessage::Phase(_)
    )
}
