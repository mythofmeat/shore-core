use shore_test_harness::{CrashedHarness, TestHarness};

/// After a crash and reboot, previously persisted messages must still be on disk.
#[tokio::test]
async fn test_history_survives_restart() {
    let mut harness = TestHarness::boot().await;

    harness.mock_llm.enqueue_text("History response").await;
    let _response = harness.send_and_collect("Remember this").await;

    // Give the daemon a moment to flush persistence.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let messages_before = harness.read_persisted_messages();
    assert!(
        messages_before.len() >= 2,
        "Expected at least 2 persisted messages before crash, got {}",
        messages_before.len()
    );

    // Crash the daemon — no graceful shutdown.
    let crashed: CrashedHarness = harness.crash().await;

    // Reboot from existing on-disk state.
    let harness2 = crashed.reboot().await;

    let messages_after = harness2.read_persisted_messages();
    assert_eq!(
        messages_before.len(),
        messages_after.len(),
        "Message count changed across crash/reboot: before={} after={}",
        messages_before.len(),
        messages_after.len()
    );

    harness2.shutdown().await;
}

/// After a crash the stale socket must not prevent a successful reboot.
/// The rebooted daemon must be able to handle a new conversation end-to-end.
#[tokio::test]
async fn test_socket_cleanup_on_restart() {
    let mut harness = TestHarness::boot().await;

    harness.mock_llm.enqueue_text("Pre-crash response").await;
    let pre = harness.send_and_collect("Before crash").await;
    // Verify the first mock was actually served before we crash.
    pre.assert_text_contains("Pre-crash response");

    // Give the daemon a moment to fully process the response before crashing.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Crash (crash() already removes the stale socket file).
    let crashed: CrashedHarness = harness.crash().await;

    // Reboot — must succeed despite the previous socket being stale.
    let mut harness2 = crashed.reboot().await;

    // Enqueue a new mock response and send a message on the rebooted daemon.
    harness2.mock_llm.enqueue_text("Post-reboot response").await;
    let response = harness2.send_and_collect("After reboot").await;

    response.assert_text_contains("Post-reboot response");
    assert!(
        response.stream_ended,
        "Expected stream_ended after reboot, but it was false"
    );

    harness2.shutdown().await;
}
