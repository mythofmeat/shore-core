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
//! The tests drive `claude` to completion against the spike's MCP
//! server. One smoke test asserts a non-empty text response; the MCP
//! test forces the `ping` tool and checks the server log for the real
//! `tools/call` roundtrip.

use serde_json::json;
use shore_config::models::{ResolvedModel, Sdk};
use shore_llm::{types::LlmRequest, LlmClient};

fn live_request(user: &str) -> LlmRequest {
    live_request_with_messages(vec![json!({"role": "user", "content": user})])
}

fn live_request_with_messages(messages: Vec<serde_json::Value>) -> LlmRequest {
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
        messages,
        Some(json!("You are a terse assistant. Answer in one sentence.")),
        None,
        Some(provider_options),
    )
}

fn mcp_log_path() -> String {
    std::env::var("MCP_HTTP_LOG").unwrap_or_else(|_| "/tmp/mcp-http.log".into())
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

#[tokio::test]
#[ignore = "requires claude CLI on PATH, OAuth login, and the spike's mcp_http_server.py running"]
async fn live_generate_invokes_mcp_ping_tool() {
    let client = LlmClient::new();
    let token = format!("shore-live-mcp-{}", std::process::id());
    let request = live_request(&format!(
        "Use the ping tool with message \"{token}\". Reply with only the exact tool response."
    ));

    let response = client
        .generate(&request)
        .await
        .expect("generate against live claude CLI with MCP failed");

    eprintln!("tool content: {}", response.content);
    assert!(
        response.content.contains(&format!("pong: {token}")),
        "response did not include ping tool result: {}",
        response.content
    );

    let log = std::fs::read_to_string(mcp_log_path()).expect("could not read MCP HTTP log");
    assert!(log.contains("\"method\": \"tools/call\""));
    assert!(log.contains(&token));
}

#[tokio::test]
#[ignore = "requires claude CLI on PATH, OAuth login, and the spike's mcp_http_server.py running"]
async fn live_generate_image_block_documents_current_cli_non_parity() {
    let client = LlmClient::new();
    let red_pixel_png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";
    let request = live_request_with_messages(vec![json!({
        "role": "user",
        "content": [
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": red_pixel_png
                }
            },
            {"type": "text", "text": "What color is this image? Answer with one word."}
        ]
    })]);

    let response = client
        .generate(&request)
        .await
        .expect("generate against live claude CLI with image failed");

    eprintln!("image response: {}", response.content);
    let lower = response.content.to_lowercase();
    assert!(
        lower.contains("cannot see")
            || lower.contains("can't see")
            || lower.contains("no image")
            || lower.contains("share the image"),
        "Claude Code may have gained image support; reassess provider parity. Response: {}",
        response.content
    );
}

#[tokio::test]
#[ignore = "requires claude CLI on PATH, OAuth login, and the spike's mcp_http_server.py running"]
async fn live_generate_resumes_shore_written_native_session_history() {
    let client = LlmClient::new();
    let token = format!("shore-native-session-{}", std::process::id());
    let mut request = live_request_with_messages(vec![
        json!({"role": "user", "content": format!("Remember the token {token}.")}),
        json!({"role": "assistant", "content": "I will remember it."}),
        json!({"role": "user", "content": "What token did I ask you to remember? Reply with only the token."}),
    ]);
    let port = std::env::var("MCP_HTTP_PORT").unwrap_or_else(|_| "9998".into());
    request.provider_options = Some(json!({
        "mcp_endpoint": format!("http://127.0.0.1:{port}/mcp"),
        "allowed_tools": [],
        "session_id": "88888888-8888-4888-8888-888888888888",
        "native_session_replay": true,
    }));

    let response = client
        .generate(&request)
        .await
        .expect("generate against live claude CLI with native session replay failed");

    eprintln!("native session response: {}", response.content);
    assert!(
        response.content.contains(&token),
        "expected replayed history token {token}, got: {}",
        response.content
    );
}
