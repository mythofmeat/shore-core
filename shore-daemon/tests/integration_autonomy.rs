//! Integration tests for the autonomy subsystem — cache keepalive pings.
//!
//! These tests use `tokio::time::pause()` and `tokio::time::advance()` to
//! control virtual time, verifying that keepalive pings fire (or don't)
//! under the correct conditions.
//!
//! IMPORTANT: `tokio::time::pause()` is called AFTER boot and any user message
//! exchange, because the boot process and SWP message flow involve real network
//! I/O that does not work under paused time (timeouts fire instantly).

use std::time::Duration;

use shore_test_harness::TestHarness;

/// Helper: yield the runtime multiple times so spawned tasks (especially the
/// autonomy tick loop) have a chance to process after a time advance.
async fn yield_many(n: usize) {
    for _ in 0..n {
        tokio::task::yield_now().await;
        // Small sleep to let async tasks settle (instant under paused time).
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// Test 1: After sending a user message (which primes last_request and warms
/// the cache), advancing past the 59-minute keepalive interval should cause
/// the daemon's tick loop to fire a keepalive ping to the mock LLM.
///
/// This would have caught Bug 1 (phantom pings — on_cache_warmed called even
/// when execute_dormant_ping skips due to None last_request) and Bug 2 (dead
/// timer — next_ping_at is None on startup, so no pings fire until the first
/// user message).
#[tokio::test]
async fn test_keepalive_ping_fires_after_59_minutes() {
    let mut harness = TestHarness::boot().await;

    // Send user message under real time — this primes last_request and warms cache.
    harness.mock_llm.enqueue_text("Hello from mock!").await;
    let _response = harness.send_and_collect("Hi there").await;

    // Give the daemon a moment to persist and notify autonomy.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Record how many requests the mock has received so far.
    let baseline = harness.mock_llm.received_requests().await.len();
    assert!(baseline >= 1, "Expected at least 1 request from user message");

    // Enqueue a JSON response for the keepalive ping (non-streaming generate).
    harness
        .mock_llm
        .enqueue_json_text_optional("ping")
        .await;

    // Now pause time and advance past the 59-minute ping interval.
    tokio::time::pause();

    tokio::time::advance(Duration::from_secs(59 * 60 + 30)).await;
    yield_many(20).await;

    // Advance a bit more to give the tick loop another cycle.
    tokio::time::advance(Duration::from_secs(60)).await;
    yield_many(20).await;

    let after = harness.mock_llm.received_requests().await.len();
    assert!(
        after > baseline,
        "Expected keepalive ping request after 59+ minutes. \
         Baseline: {baseline}, After: {after}"
    );

    // Resume real time for graceful shutdown.
    tokio::time::resume();
    harness.shutdown().await;
}

/// Test 2: If no user message has been sent (so last_request is None and the
/// cache keepalive has no ping deadline), advancing well past the keepalive
/// interval should produce zero LLM requests.
///
/// This would have caught Bug 1 (phantom pings) — the old code
/// unconditionally called on_cache_warmed after execute_dormant_ping, causing
/// the timer to reset and "fire" every 59 minutes even without a real request.
#[tokio::test]
async fn test_no_phantom_ping_without_prior_request() {
    let harness = TestHarness::boot().await;

    // Give the boot process a moment to settle under real time.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Pause and advance well past keepalive interval (120 minutes).
    tokio::time::pause();

    tokio::time::advance(Duration::from_secs(120 * 60)).await;
    yield_many(20).await;

    tokio::time::advance(Duration::from_secs(60)).await;
    yield_many(20).await;

    let requests = harness.mock_llm.received_requests().await;
    assert!(
        requests.is_empty(),
        "Expected zero requests without a prior user message, got {}",
        requests.len()
    );

    tokio::time::resume();
    harness.shutdown().await;
}

/// Test 3: A failed keepalive ping should still count as an attempt (the
/// request was made). After the failure, the next tick cycle should retry.
///
/// This would have caught Bug 3 (rebuilt request lost) — after a failed ping,
/// the request should still be available for retry.
#[tokio::test]
async fn test_failed_ping_retries() {
    let mut harness = TestHarness::boot().await;

    // Prime the cache with a user message under real time.
    harness.mock_llm.enqueue_text("Hello!").await;
    let _response = harness.send_and_collect("Hi").await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let baseline = harness.mock_llm.received_requests().await.len();

    // Enqueue an error response for the first keepalive ping attempt.
    harness
        .mock_llm
        .enqueue_error_optional(500, "Internal Server Error")
        .await;

    // Pause time and advance past the ping interval.
    tokio::time::pause();

    tokio::time::advance(Duration::from_secs(59 * 60 + 30)).await;
    yield_many(20).await;

    tokio::time::advance(Duration::from_secs(60)).await;
    yield_many(20).await;

    let after_error = harness.mock_llm.received_requests().await.len();
    assert!(
        after_error > baseline,
        "Expected at least one request attempt (even if it failed). \
         Baseline: {baseline}, After error: {after_error}"
    );

    // Enqueue a success response for the retry.
    harness
        .mock_llm
        .enqueue_json_text_optional("retry ping ok")
        .await;

    // Advance another full ping cycle for the retry.
    tokio::time::advance(Duration::from_secs(59 * 60 + 30)).await;
    yield_many(20).await;

    tokio::time::advance(Duration::from_secs(60)).await;
    yield_many(20).await;

    let after_retry = harness.mock_llm.received_requests().await.len();
    assert!(
        after_retry > after_error,
        "Expected a retry request after the failed ping. \
         After error: {after_error}, After retry: {after_retry}"
    );

    tokio::time::resume();
    harness.shutdown().await;
}
