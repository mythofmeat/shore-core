use std::time::Duration;

use shore_daemon::autonomy::HeartbeatEvent;
use shore_protocol::server_msg::ServerMessage;
use shore_test_harness::TestHarness;
use tokio::time::{sleep, timeout, Instant};

/// Collect whatever messages arrive within a bounded duration.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "arithmetic on fixed test values (deadlines/indices) with no meaningful overflow"
)]
pub(crate) async fn collect_messages_for(
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
            Ok(Err(_)) | Err(_) => return messages,
        }
    }
}

#[expect(
    clippy::arithmetic_side_effects,
    reason = "arithmetic on fixed test values (deadlines/indices) with no meaningful overflow"
)]
pub(crate) async fn wait_for_mock_requests(harness: &TestHarness, expected: usize) {
    // Matches the harness's COLLECT_TIMEOUT. 5s was too tight on loaded CI
    // runners where each tokio::test spins its own runtime; the request
    // would arrive after the deadline despite no real fault.
    let deadline = Instant::now() + Duration::from_secs(30);
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

#[expect(
    clippy::arithmetic_side_effects,
    reason = "arithmetic on fixed test values (deadlines/indices) with no meaningful overflow"
)]
pub(crate) async fn wait_for_heartbeat_detail(
    harness: &TestHarness,
    character: &str,
    detail_needle: &str,
) -> Vec<HeartbeatEvent> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let events = harness.autonomy.heartbeat_log(character, 20);
        if events
            .iter()
            .any(|event| event.detail.contains(detail_needle))
        {
            return events;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Timed out waiting for heartbeat log detail {detail_needle:?}, got: {events:#?}"
        );
        tokio::task::yield_now().await;
        sleep(Duration::from_millis(25)).await;
    }
}

#[expect(
    clippy::arithmetic_side_effects,
    reason = "arithmetic on fixed test values (deadlines/indices) with no meaningful overflow"
)]
pub(crate) async fn wait_for_file_contents(path: &std::path::Path, needle: &str) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let contents = std::fs::read_to_string(path).unwrap_or_default();
        if contents.contains(needle) {
            return contents;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Timed out waiting for {needle:?} in {}, got: {contents}",
            path.display()
        );
        tokio::task::yield_now().await;
        sleep(Duration::from_millis(25)).await;
    }
}

/// Poll the persisted `active.jsonl` message set until `pred` holds, or fail
/// after a wall-clock deadline.
///
/// `send_and_collect` returns once the stream ends, but the daemon persists
/// the turn to `active.jsonl` slightly later. Reading the file immediately
/// (or after a fixed sleep) races that write on a loaded CI runner; callers
/// that depend on a turn being on disk — e.g. compaction snapshotting the
/// transcript — must wait for the observable state instead. `what` names the
/// condition for the timeout message.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "arithmetic on fixed test values (deadlines/indices) with no meaningful overflow"
)]
pub(crate) async fn wait_for_persisted_messages(
    harness: &TestHarness,
    pred: impl Fn(&[serde_json::Value]) -> bool,
    what: &str,
) -> Vec<serde_json::Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let messages = harness.read_persisted_messages();
        if pred(&messages) {
            return messages;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Timed out waiting for {what}; persisted message count is {}",
            messages.len()
        );
        sleep(Duration::from_millis(25)).await;
    }
}

pub(crate) fn is_request_scoped_message(msg: &ServerMessage) -> bool {
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
