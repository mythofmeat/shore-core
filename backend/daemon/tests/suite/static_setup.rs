//! Phase 10 — Static-only setup E2E.
//!
//! Boots the daemon with no `[providers.<name>]` registry — only the
//! static `[chat.openrouter.haiku]` alias the harness builder writes by
//! default. Confirms that:
//!
//! * `list_models` surfaces the static alias and tags it `source = "static"`.
//! * `switch_model` accepts the alias and updates `active_model`.
//! * A normal chat send completes end-to-end through the mock LLM.
//!
//! This is the matrix's smallest configuration — the path where users
//! never touch the new provider registry should keep working untouched.

use serde_json::{json, Value};
use shore_protocol::server_msg::{CommandOutput, ServerMessage};
use shore_test_harness::TestHarness;

fn extract_command_output(messages: &[ServerMessage], expected_cmd: &str) -> Value {
    let output = messages
        .iter()
        .find_map(|m| match m {
            ServerMessage::CommandOutput(CommandOutput { name, data, .. })
                if name == expected_cmd =>
            {
                Some(data.clone())
            }
            ServerMessage::CommandOutput(_)
            | ServerMessage::Hello(_)
            | ServerMessage::History(_)
            | ServerMessage::Shutdown(_)
            | ServerMessage::Ping(_)
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
            | ServerMessage::ProviderFallbackWarning(_)
            | ServerMessage::UsageWarning(_)
            | ServerMessage::Unknown => None,
        })
        .unwrap_or_else(|| panic!("no CommandOutput for {expected_cmd}: {messages:#?}"));
    output
}

#[expect(
    clippy::indexing_slicing,
    reason = "indexes known-shape command-output JSON / Vec fixtures and panics on mismatch"
)]
#[tokio::test]
async fn static_only_setup_resolves_lists_and_sends() {
    let mut harness = TestHarness::boot().await;

    // ── list_models ──────────────────────────────────────────────────────
    let messages = harness.send_command("list_models").await;
    let data = extract_command_output(&messages, "list_models");
    let models = data["models"]
        .as_array()
        .expect("models must be an array")
        .clone();
    assert!(
        !models.is_empty(),
        "static-only setup must list at least one model: {data:#?}"
    );
    let haiku = models
        .iter()
        .find(|m| m["name"] == "haiku")
        .unwrap_or_else(|| panic!("static alias 'haiku' missing: {models:#?}"));
    assert_eq!(haiku["source"], "static");
    assert_eq!(haiku["provider"], "openrouter");
    assert_eq!(haiku["hidden"], false);
    // No discovery cache exists, so hidden_count must be zero.
    assert_eq!(data["hidden_count"], 0);

    // ── switch_model ─────────────────────────────────────────────────────
    let switch_messages = harness
        .send_command_with_args("switch_model", json!({ "name": "haiku" }))
        .await;
    let switch_data = extract_command_output(&switch_messages, "switch_model");
    assert_eq!(switch_data["active"], "haiku");
    assert_eq!(switch_data["provider"], "openrouter");
    assert_eq!(switch_data["changed"], true);

    // ── chat send ────────────────────────────────────────────────────────
    harness.mock_llm.enqueue_text("static path ok").await;
    let response = harness.send_and_collect("hello").await;
    response.assert_text_contains("static path ok");
    assert!(response.stream_ended);

    harness.shutdown().await;
}
