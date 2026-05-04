//! Live integration test for the `claude_code` provider.
//!
//! Ignored by default. To run:
//!
//! 1. Install the `claude` CLI (npm i -g @anthropic-ai/claude-code) and
//!    log into your Pro/Max subscription with `claude /login`.
//! 2. From the worktree root, start the spike's HTTP MCP server in
//!    another shell:
//!    ```sh
//!    python3 dev/spikes/claude-code-probe/mcp_http_server.py
//!    ```
//!    It binds to 127.0.0.1:9998 by default. Override with
//!    `MCP_HTTP_PORT=NNNN` if needed; the test reads the same env
//!    var so the two sides stay in sync.
//! 3. Run this test:
//!    ```sh
//!    cargo test -p shore-llm --test claude_code_live -- --ignored --nocapture
//!    ```
//!
//! The test sends a trivial user message, drives `claude` to
//! completion against the spike's MCP server, and asserts that we get
//! a non-empty response back.

use serde_json::json;
use shore_config::models::{ResolvedModel, Sdk};
use shore_llm::{types::LlmRequest, LlmClient};

fn live_request(user: &str) -> LlmRequest {
    let model = ResolvedModel {
        name: "live-test".into(),
        qualified_name: "chat.anthropic.live-test".into(),
        category: "chat".into(),
        provider_key: "anthropic".into(),
        sdk: Sdk::ClaudeCode,
        model_id: "claude-sonnet-4-5".into(),
        api_key_env: None,
        base_url: None,
        max_context_tokens: None,
        max_tokens: Some(1024),
        temperature: None,
        top_p: None,
        reasoning_effort: None,
        budget_tokens: None,
        cache_ttl: None,
        keepalive_enabled: None,
        keepalive_ttl: None,
        keepalive_max_pings: None,
        openrouter_provider: None,
        vertex_project: None,
        vertex_location: None,
        gemini_generation: None,
        gemini_web_search: None,
        zai_clear_thinking: None,
        zai_subscription: None,
    };
    let port = std::env::var("MCP_HTTP_PORT").unwrap_or_else(|_| "9998".into());
    let provider_options = json!({
        "mcp_endpoint": format!("http://127.0.0.1:{port}/mcp"),
        "allowed_tools": ["mcp__shore__ping"],
    });
    LlmClient::build_request_with_resolved_key(
        &model,
        String::new(),
        vec![json!({"role": "user", "content": user})],
        Some(json!("You are a terse assistant. Answer in one sentence.")),
        None,
        Some(provider_options),
    )
}

#[tokio::test]
#[ignore = "requires claude CLI on PATH, OAuth login, and the spike's mcp_http_server.py running"]
async fn live_generate_returns_nonempty_response() {
    let client = LlmClient::new();
    let request = live_request("What is 2+2? Answer in one word.");

    let response = client
        .generate(&request)
        .await
        .expect("generate against live claude CLI failed");

    eprintln!("model: {}", response.model);
    eprintln!("finish_reason: {}", response.finish_reason);
    eprintln!("content: {}", response.content);
    eprintln!(
        "usage: in={} out={}",
        response.usage.input_tokens, response.usage.output_tokens
    );

    assert!(!response.content.is_empty(), "response content was empty");
    assert!(
        response.usage.input_tokens > 0,
        "no input token count reported"
    );
    assert_eq!(response.finish_reason, "end_turn");
}
