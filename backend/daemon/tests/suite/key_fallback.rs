//! End-to-end coverage for Phase 4: multi-key non-sticky credential fallback.
//!
//! These tests boot the real daemon with a `[providers.openrouter]`
//! registry holding two ordered keys, then enqueue mock-LLM responses
//! to drive the rotation paths. The mock LLM matches every request
//! regardless of which key was sent — we observe rotation indirectly
//! through:
//!
//! * `mock_llm.received_requests()` — number of upstream calls
//! * `harness.diagnostics` — `key_fallbacks` ring buffer
//! * `response.raw_messages` — `ServerMessage::ProviderFallbackWarning`
//!   frames delivered to the client
//!
//! ## Env-var isolation
//!
//! Cargo runs tests in parallel within a binary; `std::env` is shared
//! across threads. Each test below picks unique env-var names (built
//! from a per-test prefix) so concurrent tests cannot stomp each
//! other's keys. This avoids the need for a global mutex around env
//! access and keeps the tests fast.

use shore_protocol::server_msg::ServerMessage;
use shore_test_harness::{TestConfigBuilder, TestHarness};

/// Build a `[providers.openrouter]` registry section that points the
/// budget/overflow keys at the given env-var names. The budget key
/// carries `warn_on_fallback = true` so rotations away from it are
/// visible to the user.
fn registry_with(budget_env: &str, overflow_env: &str) -> String {
    format!(
        r#"
[providers.openrouter]
sdk = "anthropic"

[[providers.openrouter.keys]]
name = "budget"
env = "{budget_env}"
warn_on_fallback = true

[[providers.openrouter.keys]]
name = "overflow"
env = "{overflow_env}"
"#
    )
}

fn fallback_warnings(
    resp: &shore_test_harness::CollectedResponse,
) -> Vec<&shore_protocol::server_msg::ProviderFallbackWarning> {
    resp.raw_messages
        .iter()
        .filter_map(|m| match m {
            ServerMessage::ProviderFallbackWarning(w) => Some(w),
            ServerMessage::Hello(_)
            | ServerMessage::History(_)
            | ServerMessage::Shutdown(_)
            | ServerMessage::Ping(_)
            | ServerMessage::CommandOutput(_)
            | ServerMessage::Error(_)
            | ServerMessage::StreamStart(_)
            | ServerMessage::StreamChunk(_)
            | ServerMessage::StreamEnd(_)
            | ServerMessage::Phase(_)
            | ServerMessage::NewMessage(_)
            | ServerMessage::ToolCall(_)
            | ServerMessage::ToolResult(_)
            | ServerMessage::SendImage(_)
            | ServerMessage::CacheWarning(_)
            | ServerMessage::UsageWarning(_)
            | ServerMessage::Unknown => None,
        })
        .collect()
}

#[tokio::test]
async fn first_key_succeeds_no_fallback_warning() {
    let budget_env = "FB_HAPPY_BUDGET";
    let overflow_env = "FB_HAPPY_OVERFLOW";
    std::env::set_var(budget_env, "sk-budget");
    std::env::set_var(overflow_env, "sk-overflow");

    let mut harness = TestHarness::boot_with(
        TestConfigBuilder::new().provider_registry_toml(&registry_with(budget_env, overflow_env)),
    )
    .await;

    harness.mock_llm.enqueue_text("hello").await;

    let response = harness.send_and_collect("ping").await;
    response.assert_text_contains("hello");

    assert!(
        fallback_warnings(&response).is_empty(),
        "expected no fallback warnings on happy path"
    );

    {
        let diag = harness.diagnostics.lock().unwrap();
        assert!(
            diag.key_fallbacks.is_empty(),
            "expected no key_fallbacks records on happy path"
        );
    }

    let req_count = harness.mock_llm.received_requests().await.len();
    assert_eq!(
        req_count, 1,
        "expected exactly one upstream call on happy path"
    );

    std::env::remove_var(budget_env);
    std::env::remove_var(overflow_env);
    harness.shutdown().await;
}

#[expect(
    clippy::indexing_slicing,
    reason = "indexes known-shape command-output JSON / Vec fixtures and panics on mismatch"
)]
#[tokio::test]
async fn missing_first_key_rotates_to_second() {
    let budget_env = "FB_MISSING_BUDGET";
    let overflow_env = "FB_MISSING_OVERFLOW";
    // Budget intentionally unset.
    std::env::remove_var(budget_env);
    std::env::set_var(overflow_env, "sk-overflow");

    let mut harness = TestHarness::boot_with(
        TestConfigBuilder::new().provider_registry_toml(&registry_with(budget_env, overflow_env)),
    )
    .await;

    harness.mock_llm.enqueue_text("rotated to overflow").await;

    let response = harness.send_and_collect("ping").await;
    response.assert_text_contains("rotated to overflow");

    let warnings = fallback_warnings(&response);
    assert_eq!(
        warnings.len(),
        1,
        "expected one fallback warning, got: {warnings:#?}"
    );
    let w = warnings[0];
    assert_eq!(w.provider, "openrouter");
    assert_eq!(w.from_key, "budget");
    assert_eq!(w.to_key, "overflow");
    assert_eq!(w.kind, "missing_key");
    assert!(
        !w.message.contains(budget_env) && !w.message.contains("sk-"),
        "warning message must not leak env var names or key values: {:?}",
        w.message
    );

    {
        let diag = harness.diagnostics.lock().unwrap();
        assert_eq!(diag.key_fallbacks.len(), 1);
        let entry = diag.key_fallbacks.iter().next().unwrap();
        assert_eq!(entry.provider, "openrouter");
        assert_eq!(entry.from_key, "budget");
        assert_eq!(entry.to_key.as_deref(), Some("overflow"));
        assert_eq!(entry.kind, "missing_key");
    }

    let req_count = harness.mock_llm.received_requests().await.len();
    assert_eq!(
        req_count, 1,
        "missing-key skip must not hit the network for the skipped key"
    );

    std::env::remove_var(overflow_env);
    harness.shutdown().await;
}

#[expect(
    clippy::indexing_slicing,
    reason = "indexes known-shape command-output JSON / Vec fixtures and panics on mismatch"
)]
#[tokio::test]
async fn invalid_first_key_rotates_to_second() {
    let budget_env = "FB_INVALID_BUDGET";
    let overflow_env = "FB_INVALID_OVERFLOW";
    std::env::set_var(budget_env, "sk-bad-key");
    std::env::set_var(overflow_env, "sk-overflow");

    let mut harness = TestHarness::boot_with(
        TestConfigBuilder::new().provider_registry_toml(&registry_with(budget_env, overflow_env)),
    )
    .await;

    harness
        .mock_llm
        .enqueue_error(401, r#"{"error":"invalid_api_key"}"#)
        .await;
    harness
        .mock_llm
        .enqueue_text("recovered after invalid key")
        .await;

    let response = harness.send_and_collect("ping").await;
    response.assert_text_contains("recovered after invalid key");

    let warnings = fallback_warnings(&response);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].kind, "invalid_key");
    assert_eq!(warnings[0].status, Some(401));

    {
        let diag = harness.diagnostics.lock().unwrap();
        assert_eq!(diag.key_fallbacks.len(), 1);
        let entry = diag.key_fallbacks.iter().next().unwrap();
        assert_eq!(entry.kind, "invalid_key");
        assert_eq!(entry.status, Some(401));
        assert!(
            !entry.reason.contains("invalid_api_key"),
            "diagnostics reason must not echo response body: {:?}",
            entry.reason
        );
    }

    let req_count = harness.mock_llm.received_requests().await.len();
    assert_eq!(req_count, 2);

    std::env::remove_var(budget_env);
    std::env::remove_var(overflow_env);
    harness.shutdown().await;
}

#[expect(
    clippy::indexing_slicing,
    reason = "indexes known-shape command-output JSON / Vec fixtures and panics on mismatch"
)]
#[tokio::test]
async fn quota_exhausted_first_key_rotates_to_second() {
    let budget_env = "FB_QUOTA_BUDGET";
    let overflow_env = "FB_QUOTA_OVERFLOW";
    std::env::set_var(budget_env, "sk-quota");
    std::env::set_var(overflow_env, "sk-overflow");

    let mut harness = TestHarness::boot_with(
        TestConfigBuilder::new().provider_registry_toml(&registry_with(budget_env, overflow_env)),
    )
    .await;

    harness
        .mock_llm
        .enqueue_error(
            429,
            r#"{"error":{"type":"insufficient_quota","message":"quota exceeded"}}"#,
        )
        .await;
    harness
        .mock_llm
        .enqueue_text("recovered after quota exhaustion")
        .await;

    let response = harness.send_and_collect("ping").await;
    response.assert_text_contains("recovered after quota exhaustion");

    let warnings = fallback_warnings(&response);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].kind, "quota_exhausted");
    assert_eq!(warnings[0].status, Some(429));
    assert!(warnings[0].message.contains("over quota"));

    let req_count = harness.mock_llm.received_requests().await.len();
    assert_eq!(req_count, 2);

    std::env::remove_var(budget_env);
    std::env::remove_var(overflow_env);
    harness.shutdown().await;
}

#[expect(
    clippy::indexing_slicing,
    reason = "indexes known-shape command-output JSON / Vec fixtures and panics on mismatch"
)]
#[tokio::test]
async fn budget_402_emits_budget_warning() {
    let budget_env = "FB_BUDGET402_BUDGET";
    let overflow_env = "FB_BUDGET402_OVERFLOW";
    std::env::set_var(budget_env, "sk-budget");
    std::env::set_var(overflow_env, "sk-overflow");

    let mut harness = TestHarness::boot_with(
        TestConfigBuilder::new().provider_registry_toml(&registry_with(budget_env, overflow_env)),
    )
    .await;

    harness
        .mock_llm
        .enqueue_error(402, "Payment Required: insufficient credit")
        .await;
    harness
        .mock_llm
        .enqueue_text("recovered after budget exhaustion")
        .await;

    let response = harness.send_and_collect("ping").await;
    response.assert_text_contains("recovered after budget exhaustion");

    let warnings = fallback_warnings(&response);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].kind, "budget_exhausted");
    assert_eq!(warnings[0].status, Some(402));
    assert!(
        warnings[0].message.starts_with("Budget warning"),
        "budget warning text mismatch: {:?}",
        warnings[0].message
    );

    std::env::remove_var(budget_env);
    std::env::remove_var(overflow_env);
    harness.shutdown().await;
}

#[tokio::test]
async fn generic_500_does_not_rotate_keys() {
    // 500 is transient — stream_with_retry handles it on the same key.
    // Phase 4 must NOT rotate to the next key on 500.
    let budget_env = "FB_500_BUDGET";
    let overflow_env = "FB_500_OVERFLOW";
    std::env::set_var(budget_env, "sk-budget");
    std::env::set_var(overflow_env, "sk-overflow");

    let mut harness = TestHarness::boot_with(
        TestConfigBuilder::new().provider_registry_toml(&registry_with(budget_env, overflow_env)),
    )
    .await;

    harness
        .mock_llm
        .enqueue_error(500, r#"{"error":"internal"}"#)
        .await;
    harness
        .mock_llm
        .enqueue_text("recovered after transient 500")
        .await;

    let response = harness.send_and_collect("ping").await;
    response.assert_text_contains("recovered after transient 500");

    let warnings = fallback_warnings(&response);
    assert!(
        warnings.is_empty(),
        "transient 5xx must not emit fallback warnings"
    );
    {
        let diag = harness.diagnostics.lock().unwrap();
        assert!(
            diag.key_fallbacks.is_empty(),
            "transient 5xx must not record a key fallback"
        );
    }

    std::env::remove_var(budget_env);
    std::env::remove_var(overflow_env);
    harness.shutdown().await;
}

#[tokio::test]
async fn malformed_400_does_not_rotate_keys() {
    let budget_env = "FB_400_BUDGET";
    let overflow_env = "FB_400_OVERFLOW";
    std::env::set_var(budget_env, "sk-budget");
    std::env::set_var(overflow_env, "sk-overflow");

    let mut harness = TestHarness::boot_with(
        TestConfigBuilder::new().provider_registry_toml(&registry_with(budget_env, overflow_env)),
    )
    .await;

    harness
        .mock_llm
        .enqueue_error(
            400,
            r#"{"error":{"type":"invalid_request","message":"messages required"}}"#,
        )
        .await;
    // The daemon must NOT rotate on a non-credential 400, so this
    // optional response should remain unconsumed.
    harness
        .mock_llm
        .enqueue_text_optional("would-be second key response")
        .await;

    let _response = harness.send_and_collect("ping").await;

    {
        let diag = harness.diagnostics.lock().unwrap();
        assert!(
            diag.key_fallbacks.is_empty(),
            "malformed-request 400 must not record a fallback, got: {} entries",
            diag.key_fallbacks.len()
        );
    }

    std::env::remove_var(budget_env);
    std::env::remove_var(overflow_env);
    harness.shutdown().await;
}

#[tokio::test]
async fn fallback_is_not_sticky_across_requests() {
    let budget_env = "FB_STICKY_BUDGET";
    let overflow_env = "FB_STICKY_OVERFLOW";
    std::env::set_var(budget_env, "sk-budget");
    std::env::set_var(overflow_env, "sk-overflow");

    let mut harness = TestHarness::boot_with(
        TestConfigBuilder::new().provider_registry_toml(&registry_with(budget_env, overflow_env)),
    )
    .await;

    // Request 1: budget rejected (401), overflow succeeds.
    harness
        .mock_llm
        .enqueue_error(401, r#"{"error":"invalid_api_key"}"#)
        .await;
    harness.mock_llm.enqueue_text("first request").await;

    // Request 2: budget succeeds — non-sticky resolution restarts at
    // the first enabled key.
    harness.mock_llm.enqueue_text("second request").await;

    let r1 = harness.send_and_collect("first").await;
    r1.assert_text_contains("first request");
    assert_eq!(fallback_warnings(&r1).len(), 1);

    let r2 = harness.send_and_collect("second").await;
    r2.assert_text_contains("second request");
    assert!(
        fallback_warnings(&r2).is_empty(),
        "second request must restart at the first key (non-sticky)"
    );

    {
        let diag = harness.diagnostics.lock().unwrap();
        assert_eq!(
            diag.key_fallbacks.len(),
            1,
            "only the first request should have recorded a fallback"
        );
    }

    std::env::remove_var(budget_env);
    std::env::remove_var(overflow_env);
    harness.shutdown().await;
}

#[tokio::test]
async fn disabled_keys_are_skipped_at_resolution() {
    let budget_env = "FB_DISABLED_BUDGET";
    let overflow_env = "FB_DISABLED_OVERFLOW";
    let registry = format!(
        r#"
[providers.openrouter]
sdk = "anthropic"

[[providers.openrouter.keys]]
name = "budget"
env = "{budget_env}"
warn_on_fallback = true
enabled = false

[[providers.openrouter.keys]]
name = "overflow"
env = "{overflow_env}"
"#
    );
    std::env::remove_var(budget_env);
    std::env::set_var(overflow_env, "sk-overflow");

    let mut harness =
        TestHarness::boot_with(TestConfigBuilder::new().provider_registry_toml(&registry)).await;

    harness
        .mock_llm
        .enqueue_text("served by overflow only")
        .await;

    let response = harness.send_and_collect("ping").await;
    response.assert_text_contains("served by overflow only");

    assert!(
        fallback_warnings(&response).is_empty(),
        "disabled keys must not produce fallback warnings"
    );
    {
        let diag = harness.diagnostics.lock().unwrap();
        assert!(
            diag.key_fallbacks.is_empty(),
            "disabled keys must not produce diagnostics records"
        );
    }

    std::env::remove_var(overflow_env);
    harness.shutdown().await;
}
