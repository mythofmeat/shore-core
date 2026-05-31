//! Wire-shape regressions for shore-llm's provider adapters.
//!
//! Note: cache-control tests intentionally hold the ENV_LOCK
//! `std::sync::Mutex` across `.await` points — the lock's purpose IS to
//! pin process-global env-var state for the entire request lifecycle.
//! The await-holding-lock lint is correctly identifying it, but in this
//! file the pattern is load-bearing.
#![allow(clippy::await_holding_lock)]

//!
//! Each test boots a [`MockLlmServer`] (wiremock-backed), points an
//! `LlmRequest` at it, and asserts on the raw POST body the adapter sent.
//! These are the Rust counterpart to TS daemon-ts's
//! `tests/anthropic_thinking_wire.test.ts` / `cache_placement.test.ts`
//! /`_fake_anthropic.ts`. The property under test is the bytes WE send;
//! the upstream provider's behavior is not in scope.
//!
//! Each test exercises ONE adapter shape, with the MockLlmServer playing
//! both roles: response source AND request inspector. Tests do nothing
//! that would require a live provider key.

use std::sync::Mutex;

use serde_json::{json, Value};
use shore_config::models::Sdk;
use shore_llm::types::{ContentBlock, LlmRequest};
use shore_llm::LlmClient;
use shore_test_harness::mock_llm::{
    find_cache_control_paths, AnthropicJsonBuilder, AnthropicStreamBuilder, MockLlmServer,
    OpenAiResponseBuilder,
};

/// shore-llm reads `SHORE_CACHE_PINNED_POSITION` /
/// `SHORE_CACHE_DEPTH_TURNS` env vars to override cache-breakpoint
/// defaults. Cargo runs tests in parallel within a binary, and env vars
/// are process-global, so any test that depends on a specific env-var
/// state must hold this mutex for its entire body.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// RAII guard that snapshots an env var on construction and restores its
/// prior value (set or unset) on drop, so a panic inside a test can't
/// leak overrides into later tests in the same binary.
struct EnvVarGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }

    fn unset(key: &'static str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(prev) => std::env::set_var(self.key, prev),
            None => std::env::remove_var(self.key),
        }
    }
}

/// Build a baseline LlmRequest aimed at the mock. Tests mutate it further.
fn base_request(base_url: &str) -> LlmRequest {
    LlmRequest {
        sdk: Sdk::Anthropic,
        model: "claude-sonnet-4-6".into(),
        api_key: "sk-test".into(),
        api_key_name: Some("default".into()),
        base_url: Some(base_url.to_string()),
        messages: vec![json!({
            "role": "user",
            "content": [{"type": "text", "text": "Hi."}]
        })],
        system: Some(json!("You are Casey.")),
        tools: None,
        max_tokens: 1024,
        temperature: None,
        top_p: None,
        provider_options: None,
        provider_key: Some("anthropic".into()),
        rid: None,
        forensic_character: None,
        retain_long: false,
    }
}

/// Drain a streaming response so the mock observes the request completing.
async fn drain_stream(client: &LlmClient, req: &LlmRequest) {
    use tokio::io::AsyncBufReadExt;
    let mut reader = client.stream_raw(req).await.expect("stream open");
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await.expect("stream read");
        if n == 0 {
            break;
        }
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// First (and only) request body the mock observed. Panics if zero or
/// more than one — wire tests typically issue exactly one request.
async fn single_request(mock: &MockLlmServer) -> Value {
    let mut reqs = mock.received_requests().await;
    assert_eq!(
        reqs.len(),
        1,
        "expected exactly one request, got {}",
        reqs.len()
    );
    reqs.remove(0)
}

// ─────────────────────────────────────────────────────────────────────────────
// cache_control placement
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn cache_control_pinned_zero_marks_last_system_block() {
    // pinned=[0] means "last system block" — the override our live
    // cache_regression example uses for single-block system prompts.
    // With cache_ttl set, exactly one cache_control should land on the
    // sole system block.
    let _guard = ENV_LOCK.lock().unwrap();
    let _pinned = EnvVarGuard::set("SHORE_CACHE_PINNED_POSITION", "0");
    let _depth = EnvVarGuard::set("SHORE_CACHE_DEPTH_TURNS", "");

    let mock = MockLlmServer::start().await;
    mock.enqueue_stream(AnthropicStreamBuilder::new().text("ok"))
        .await;

    let client = LlmClient::new();
    let mut req = base_request(&mock.base_url());
    req.provider_options = Some(json!({"cache_ttl": "5m"}));
    drain_stream(&client, &req).await;

    let body = single_request(&mock).await;
    let paths = find_cache_control_paths(&body);
    assert_eq!(
        paths,
        vec!["system[0]".to_string()],
        "expected exactly one cache_control on system[0], got {paths:?}"
    );
}

#[tokio::test]
async fn cache_control_default_with_two_system_blocks_anchors_at_minus_one() {
    // TS-default anchors on the last system block whose `_label` is NOT
    // `"memory_index"` — memory_index churns every dreaming/compaction
    // pass, so anchoring there busts the prefix. With two blocks where
    // system[1] is the memory_index slot, the anchor lands on system[0].
    let _guard = ENV_LOCK.lock().unwrap();
    let _pinned = EnvVarGuard::unset("SHORE_CACHE_PINNED_POSITION");
    let _depth = EnvVarGuard::unset("SHORE_CACHE_DEPTH_TURNS");

    let mock = MockLlmServer::start().await;
    mock.enqueue_stream(AnthropicStreamBuilder::new().text("ok"))
        .await;

    let client = LlmClient::new();
    let mut req = base_request(&mock.base_url());
    req.system = Some(json!([
        {"type": "text", "text": "stable base"},
        {"type": "text", "text": "memory_index simulated", "_label": "memory_index"}
    ]));
    req.provider_options = Some(json!({"cache_ttl": "5m"}));
    drain_stream(&client, &req).await;

    let body = single_request(&mock).await;
    let paths = find_cache_control_paths(&body);
    assert!(
        paths.iter().any(|p| p == "system[0]"),
        "expected cache_control on system[0] (second-to-last), got {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p == "system[1]"),
        "memory_index slot system[1] must NOT receive cache_control, got {paths:?}"
    );
}

#[tokio::test]
async fn cache_control_disabled_when_cache_ttl_absent() {
    let mock = MockLlmServer::start().await;
    mock.enqueue_stream(AnthropicStreamBuilder::new().text("ok"))
        .await;

    let client = LlmClient::new();
    let req = base_request(&mock.base_url());
    drain_stream(&client, &req).await;

    let body = single_request(&mock).await;
    let paths = find_cache_control_paths(&body);
    assert!(
        paths.is_empty(),
        "expected no cache_control markers when cache_ttl absent, got {paths:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// redacted_thinking preservation (the regression we just fixed)
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn openrouter_redacted_thinking_preserved_on_streaming_path() {
    // Streaming-path companion to the JSON-path test below. The adapter
    // serializes redacted_thinking SSE events into NDJSON events on the
    // wire to its consumer; the `data` must survive verbatim.
    use tokio::io::AsyncBufReadExt;

    let mock = MockLlmServer::start().await;
    mock.enqueue_stream(
        AnthropicStreamBuilder::new()
            .redacted_thinking("openrouter.reasoning: signed payload")
            .text("hi"),
    )
    .await;

    let client = LlmClient::new();
    let req = base_request(&mock.base_url());
    let mut reader = client.stream_raw(&req).await.expect("stream open");

    let mut saw_redacted = false;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await.expect("read line");
        if n == 0 {
            break;
        }
        if let Ok(event) = serde_json::from_str::<Value>(line.trim()) {
            if event["type"] == "redacted_thinking"
                && event["data"] == "openrouter.reasoning: signed payload"
            {
                saw_redacted = true;
            }
        }
    }
    assert!(
        saw_redacted,
        "streaming adapter must forward redacted_thinking events with the \
         `openrouter.reasoning:` payload verbatim"
    );
}

#[tokio::test]
async fn openrouter_redacted_thinking_preserved_in_generate_response() {
    // Non-streaming generate(): JSON shape includes redacted_thinking →
    // GenerateResponse.content_blocks must contain it with the same data.
    let mock = MockLlmServer::start().await;
    mock.enqueue_json(
        AnthropicJsonBuilder::new()
            .redacted_thinking("openrouter.reasoning: signed payload")
            .text("hi"),
    )
    .await;

    let client = LlmClient::new();
    let req = base_request(&mock.base_url());
    let resp = client.generate(&req).await.expect("generate");

    let kept = resp.content_blocks.iter().any(|b| match b {
        ContentBlock::RedactedThinking { data } => data == "openrouter.reasoning: signed payload",
        _ => false,
    });
    assert!(
        kept,
        "redacted_thinking with openrouter.reasoning: prefix must be preserved verbatim; \
         got blocks: {:?}",
        resp.content_blocks
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Block-order round-trip on tool-loop iter 2
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn tool_loop_iter2_request_preserves_assistant_block_order() {
    // Mock returns: thinking → tool_use. Caller appends assistant + a
    // user(tool_result) and resubmits. The second request must echo back
    // the assistant content in the same order (thinking, tool_use)
    // immediately followed by the tool_result user turn. Order matters
    // because Anthropic's cache walker hashes block sequences.
    let mock = MockLlmServer::start().await;
    mock.enqueue_stream(
        AnthropicStreamBuilder::new()
            .thinking("Considering options.", Some("sig_xyz"))
            .tool_use("toolu_1", "roll_dice", json!({"count": 1, "sides": 20})),
    )
    .await;
    mock.enqueue_stream(AnthropicStreamBuilder::new().text("You rolled a 14."))
        .await;

    let client = LlmClient::new();
    let mut req = base_request(&mock.base_url());
    req.tools = Some(vec![json!({
        "name": "roll_dice",
        "description": "roll dice",
        "input_schema": {"type": "object"}
    })]);
    drain_stream(&client, &req).await;

    // Caller-side tool-loop continuation: re-emit the assistant content
    // verbatim, then append a tool_result user turn.
    req.messages.push(json!({
        "role": "assistant",
        "content": [
            {"type": "thinking", "thinking": "Considering options.", "signature": "sig_xyz"},
            {"type": "tool_use", "id": "toolu_1", "name": "roll_dice",
             "input": {"count": 1, "sides": 20}},
        ]
    }));
    req.messages.push(json!({
        "role": "user",
        "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "14"}]
    }));
    drain_stream(&client, &req).await;

    let bodies = mock.received_requests().await;
    assert_eq!(bodies.len(), 2);
    let iter2 = &bodies[1];

    let msgs = iter2["messages"].as_array().expect("messages array");
    assert_eq!(
        msgs.len(),
        3,
        "iter 2 should have user/asst/user_tool_result"
    );
    let asst_content = msgs[1]["content"].as_array().expect("asst content array");
    let types: Vec<&str> = asst_content
        .iter()
        .map(|b| b["type"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        types,
        vec!["thinking", "tool_use"],
        "assistant content order must be thinking → tool_use (preserves cache prefix)"
    );

    let tool_result_content = msgs[2]["content"].as_array().expect("tool_result array");
    assert_eq!(tool_result_content[0]["type"], "tool_result");
    assert_eq!(tool_result_content[0]["tool_use_id"], "toolu_1");
}

// ─────────────────────────────────────────────────────────────────────────────
// thinking config wire shape (mirrors TS anthropic_thinking_wire.test.ts)
// ─────────────────────────────────────────────────────────────────────────────

async fn capture_anthropic_body(opts: Value) -> Value {
    capture_anthropic_body_for_model(opts, "claude-sonnet-4-6").await
}

async fn capture_anthropic_body_for_model(opts: Value, model: &str) -> Value {
    let mock = MockLlmServer::start().await;
    mock.enqueue_stream(AnthropicStreamBuilder::new().text("ok"))
        .await;
    let client = LlmClient::new();
    let mut req = base_request(&mock.base_url());
    req.model = model.into();
    // Realistic ceiling so manual `budget_tokens` clamping (which requires
    // 1024 <= budget < max_tokens) has room — base_request's 1024 is too low.
    req.max_tokens = 16_000;
    req.provider_options = Some(opts);
    drain_stream(&client, &req).await;
    single_request(&mock).await
}

#[tokio::test]
async fn thinking_named_effort_high_sends_adaptive_plus_output_config() {
    let body = capture_anthropic_body(json!({"reasoning_effort": "high"})).await;
    assert_eq!(
        body["thinking"],
        json!({"type": "adaptive", "display": "summarized"})
    );
    assert_eq!(body["output_config"], json!({"effort": "high"}));
}

#[tokio::test]
async fn thinking_literal_adaptive_omits_output_config() {
    let body = capture_anthropic_body(json!({"reasoning_effort": "adaptive"})).await;
    assert_eq!(
        body["thinking"],
        json!({"type": "adaptive", "display": "summarized"})
    );
    assert!(
        body.get("output_config").is_none(),
        "literal `adaptive` must NOT set output_config, got {body:#}"
    );
}

#[tokio::test]
async fn thinking_explicit_budget_uses_enabled_mode() {
    let body = capture_anthropic_body(json!({"budget_tokens": 2048})).await;
    assert_eq!(
        body["thinking"],
        json!({"type": "enabled", "budget_tokens": 2048})
    );
    assert!(body.get("output_config").is_none());
}

#[tokio::test]
async fn thinking_disabled_when_no_thinking_options() {
    let body = capture_anthropic_body(json!({})).await;
    assert!(body.get("thinking").is_none(), "no thinking expected");
    assert!(body.get("output_config").is_none());
}

#[tokio::test]
async fn thinking_named_effort_downgrades_to_budget_on_legacy_model() {
    // Sonnet 4.5 rejects adaptive — effort must map to enabled+budget.
    let body =
        capture_anthropic_body_for_model(json!({"reasoning_effort": "high"}), "claude-sonnet-4-5")
            .await;
    assert_eq!(body["thinking"]["type"], "enabled");
    assert!(body["thinking"]["budget_tokens"].is_number());
    assert!(
        body.get("output_config").is_none(),
        "enabled mode must not carry output_config, got {body:#}"
    );
}

#[tokio::test]
async fn thinking_budget_upgrades_to_adaptive_on_opus_48() {
    // Opus 4.8 rejects enabled — a legacy budget request must become adaptive.
    let body =
        capture_anthropic_body_for_model(json!({"budget_tokens": 4096}), "claude-opus-4-8").await;
    assert_eq!(
        body["thinking"],
        json!({"type": "adaptive", "display": "summarized"})
    );
    assert!(body.get("output_config").is_none());
}

// ─────────────────────────────────────────────────────────────────────────────
// OpenAI-compatible adapter wire shape
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn openai_text_only_generate_round_trip() {
    let mock = MockLlmServer::start().await;
    mock.enqueue_openai_json(
        OpenAiResponseBuilder::new()
            .text("Hello there.")
            .usage(123, 7, 0),
    )
    .await;

    let client = LlmClient::new();
    let req = LlmRequest {
        sdk: Sdk::Openai,
        model: "gpt-test".into(),
        api_key: "sk-test".into(),
        api_key_name: Some("default".into()),
        base_url: Some(mock.base_url()),
        messages: vec![json!({"role": "user", "content": "ping"})],
        system: Some(json!("You are helpful.")),
        tools: None,
        max_tokens: 32,
        temperature: None,
        top_p: None,
        provider_options: None,
        provider_key: Some("openai".into()),
        rid: None,
        forensic_character: None,
        retain_long: false,
    };
    let resp = client.generate(&req).await.expect("generate");
    let text: String = resp
        .content_blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello there.");
    assert_eq!(resp.usage.input_tokens, 123);
    assert_eq!(resp.usage.output_tokens, 7);

    let body = single_request(&mock).await;
    assert_eq!(body["model"], "gpt-test");
    // system prompt rides as the first message in OpenAI shape, not on a
    // top-level `system` field.
    let msgs = body["messages"].as_array().expect("messages");
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[1]["role"], "user");
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared cache-shape contract for EVERY background tool loop that injects a
// task-specific system instruction — compaction, the dreaming/librarian sweep,
// and heartbeat ticks. All three attach the instruction via
// `LlmRequest::push_inline_system`, which pins it at a fixed build-time slot;
// this test pins the provider-level invariant they all rely on.
//
// Pushing the system message inline at a fixed slot keeps the preceding user
// turn's bytes byte-identical across tool-loop rounds. The shape is
// provider-agnostic because each adapter's existing inline-system handling does
// the right thing (Anthropic merges into preceding user; OpenAI emits
// role:"system" or wraps as user via `<system_instruction>` per
// `ctx.wrap_inline_system`).
//
// The removed `system_suffix` affordance produced the opposite shape: it
// re-appended the system block at the *moving* tail on every dispatch, so
// after the tool loop pushed assistant + user(tool_result) the block merged
// into the new last user and the preceding-user bytes diverged — full cache
// invalidation from that slot onward (the #80 / #84 bug). Builder-level
// counterparts: `compaction_tool_loop_keeps_compact_now_user_byte_stable_across_rounds`
// and `librarian_tool_loop_keeps_user_prompt_byte_stable_across_rounds`.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn inline_system_message_at_build_time_is_stable_across_tool_rounds() {
    let mock = MockLlmServer::start().await;
    mock.enqueue_stream(AnthropicStreamBuilder::new().tool_use(
        "toolu_1",
        "write",
        json!({"path": "memory/x.md", "content": "ok"}),
    ))
    .await;
    mock.enqueue_stream(AnthropicStreamBuilder::new().text("done"))
        .await;

    let client = LlmClient::new();
    let mut req = base_request(&mock.base_url());
    // Same chat prefix as the bug-repro test above…
    req.messages = vec![
        json!({"role": "user", "content": [{"type": "text", "text": "earlier user turn"}]}),
        json!({"role": "assistant", "content": [{"type": "text", "text": "earlier assistant turn"}]}),
        json!({"role": "user", "content": [{"type": "text", "text": "compact now"}]}),
        // …but the system instruction lives inline as its own message,
        // pushed once at a fixed build-time slot (the shape
        // `push_inline_system` produces).
        json!({"role": "system", "content": "compaction system instruction"}),
    ];
    req.tools = Some(vec![json!({
        "name": "write",
        "description": "write a file",
        "input_schema": {"type": "object"}
    })]);

    drain_stream(&client, &req).await;

    // Tool-loop continuation: assistant + user(tool_result) appear AFTER
    // the inline system message, not in place of it.
    req.messages.push(json!({
        "role": "assistant",
        "content": [
            {"type": "tool_use", "id": "toolu_1", "name": "write",
             "input": {"path": "memory/x.md", "content": "ok"}},
        ]
    }));
    req.messages.push(json!({
        "role": "user",
        "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"}]
    }));

    drain_stream(&client, &req).await;

    let bodies = mock.received_requests().await;
    assert_eq!(bodies.len(), 2);

    let iter0_msgs = bodies[0]["messages"].as_array().expect("iter0 messages");
    let iter1_msgs = bodies[1]["messages"].as_array().expect("iter1 messages");

    // Anthropic's `convert_inline_system_messages` merged the inline system
    // into the preceding user (compact_now_user) on both iterations.
    // Because the inline system message's POSITION is fixed (right after
    // compact_now_user) on both calls, the merge target is the same user
    // on both calls — its content stays identical.
    let iter0_compact_now = &iter0_msgs[2];
    let iter1_compact_now = &iter1_msgs[2];
    assert_eq!(
        iter0_compact_now, iter1_compact_now,
        "FIX VALIDATED: compact_now_user bytes are identical across iter-0 \
         and iter-1. Anthropic's prompt cache prefix matches; no \
         invalidation from this position onward.\n\
         iter-0: {iter0_compact_now}\n\
         iter-1: {iter1_compact_now}"
    );
    // And the merge did happen on both — the suffix text is inside the
    // user message, not floating off in some new tail slot.
    assert!(
        iter0_compact_now
            .to_string()
            .contains("compaction system instruction"),
        "iter-0 compact_now must contain the merged system instruction"
    );
    assert!(
        iter1_compact_now
            .to_string()
            .contains("compaction system instruction"),
        "iter-1 compact_now must contain the merged system instruction"
    );
}
