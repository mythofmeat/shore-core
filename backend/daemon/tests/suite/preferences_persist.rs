//! Phase 10 — Per-model + per-character sampler persistence E2E.
//!
//! Boots a daemon with two static chat aliases (`haiku`, `sonnet`) on a
//! shared `[openrouter]` provider and two characters (`TestChar`,
//! `Bob`). Each character connects via its own SWP session and writes
//! per-model sampler overrides through the daemon's `set_model_setting`
//! command. Then the daemon is crashed and rebooted with the same
//! builder, and the same connections re-issue `model_settings` against
//! both models.
//!
//! Asserts:
//!
//! * Each `(character, provider, model_id)` triple keeps an independent
//!   value — switching models inside a character does not stomp the
//!   other model's setting.
//! * Switching characters does not stomp the other character's settings.
//! * After a daemon restart, both characters and both models still
//!   resolve their previously-written values.

use serde_json::{json, Value};
use shore_protocol::server_msg::{CommandOutput, ServerMessage};
use shore_swp_client::connection::SWPConnection;
use shore_test_harness::{TestConfigBuilder, TestHarness};

fn extract(messages: &[ServerMessage], expected_cmd: &str) -> Value {
    messages
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
            | ServerMessage::UsageWarning(_) => None,
        })
        .unwrap_or_else(|| panic!("no CommandOutput for {expected_cmd}: {messages:#?}"))
}

async fn switch_model(conn: &mut SWPConnection, name: &str) {
    let messages =
        TestHarness::send_command_on(conn, "switch_model", json!({ "name": name })).await;
    let data = extract(&messages, "switch_model");
    assert_eq!(
        data["active"], name,
        "switch_model should report new active"
    );
}

async fn set_temperature(conn: &mut SWPConnection, value: f64) {
    let messages = TestHarness::send_command_on(
        conn,
        "set_model_setting",
        json!({ "key": "temperature", "value": value, "scope": "character" }),
    )
    .await;
    let data = extract(&messages, "set_model_setting");
    assert_eq!(data["changed"], true);
    assert_eq!(data["scope"], "character");
}

async fn read_temperature(conn: &mut SWPConnection) -> Option<f64> {
    let messages = TestHarness::send_command_on(conn, "model_settings", json!({})).await;
    let data = extract(&messages, "model_settings");
    data["effective_sampler"]["temperature"].as_f64()
}

fn builder() -> TestConfigBuilder {
    TestConfigBuilder::new()
        .extra_chat_alias("sonnet", "anthropic/claude-sonnet-4.5")
        .extra_character(
            "Bob",
            "You are Bob, a second test character. Keep replies very short.",
        )
}

#[tokio::test]
async fn per_model_and_per_character_sampler_persists_across_restart() {
    let harness = TestHarness::boot_with(builder()).await;

    // ── TestChar: haiku=0.7, sonnet=1.2 ────────────────────────────────
    let mut alice = harness.connect_as_character("TestChar").await;
    switch_model(&mut alice, "haiku").await;
    set_temperature(&mut alice, 0.7).await;

    switch_model(&mut alice, "sonnet").await;
    set_temperature(&mut alice, 1.2).await;

    // Switch back to haiku — must still see 0.7, not 1.2.
    switch_model(&mut alice, "haiku").await;
    assert_eq!(
        read_temperature(&mut alice).await,
        Some(0.7),
        "haiku must keep its own temperature after toggling to sonnet and back"
    );
    switch_model(&mut alice, "sonnet").await;
    assert_eq!(read_temperature(&mut alice).await, Some(1.2));

    // ── Bob: haiku=0.5 (independent from TestChar.haiku=0.7) ────────────
    let mut bob = harness.connect_as_character("Bob").await;
    switch_model(&mut bob, "haiku").await;
    set_temperature(&mut bob, 0.5).await;
    assert_eq!(read_temperature(&mut bob).await, Some(0.5));

    // Sanity: TestChar.haiku is still 0.7 after Bob's writes.
    switch_model(&mut alice, "haiku").await;
    assert_eq!(
        read_temperature(&mut alice).await,
        Some(0.7),
        "TestChar.haiku must not be overwritten by writes against Bob"
    );

    // Drop both connections so the SWP server doesn't trip on aborted reads.
    drop(alice);
    drop(bob);

    // ── crash + reboot ──────────────────────────────────────────────────
    let crashed = harness.crash().await;
    let harness = crashed.reboot_with(builder()).await;

    // ── verify all three (character, model) values survived restart ────
    let mut alice = harness.connect_as_character("TestChar").await;
    switch_model(&mut alice, "haiku").await;
    assert_eq!(
        read_temperature(&mut alice).await,
        Some(0.7),
        "TestChar.haiku.temperature must survive daemon restart"
    );
    switch_model(&mut alice, "sonnet").await;
    assert_eq!(
        read_temperature(&mut alice).await,
        Some(1.2),
        "TestChar.sonnet.temperature must survive daemon restart"
    );

    let mut bob = harness.connect_as_character("Bob").await;
    switch_model(&mut bob, "haiku").await;
    assert_eq!(
        read_temperature(&mut bob).await,
        Some(0.5),
        "Bob.haiku.temperature must survive daemon restart"
    );

    drop(alice);
    drop(bob);
    harness.shutdown().await;
}
