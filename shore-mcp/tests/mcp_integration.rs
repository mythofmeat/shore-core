//! Integration test: launch `shore-mcp` as a subprocess against an in-process
//! daemon booted by `TestHarness`, speak MCP JSON-RPC over stdin/stdout,
//! exercise a small representative tool set (initialize / tools/list / status / send).
//!
//! Prerequisites:
//!   - `shore-mcp` binary (cargo builds it automatically via
//!     `CARGO_BIN_EXE_shore-mcp`).
//!
//! Run with: `cargo test -p shore-mcp --test mcp_integration -- --ignored --nocapture`

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use shore_test_harness::TestHarness;

async fn send_jsonrpc(
    stdin: &mut tokio::process::ChildStdin,
    method: &str,
    id: u32,
    params: serde_json::Value,
) -> std::io::Result<()> {
    let frame = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    let line = serde_json::to_string(&frame).unwrap();
    stdin.write_all(line.as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await
}

async fn recv_jsonrpc_response(
    reader: &mut BufReader<tokio::process::ChildStdout>,
) -> std::io::Result<serde_json::Value> {
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "mcp stdout closed",
            ));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("json: {e}"))
        })?;
        // Skip notifications (no id).
        if value.get("id").is_some() {
            return Ok(value);
        }
    }
}

#[tokio::test]
#[ignore] // Requires the shore-mcp binary; see header for invocation.
async fn shore_mcp_initializes_and_calls_tools_against_real_daemon() {
    let harness = TestHarness::boot().await;
    let daemon_addr = harness.addr.clone();

    // Preload a response for the `send` tool.
    harness.mock_llm.enqueue_text("hello from mock llm").await;

    // Locate the binary built by this workspace.
    let bin = std::env::var("CARGO_BIN_EXE_shore-mcp")
        .expect("CARGO_BIN_EXE_shore-mcp — run via `cargo test -p shore-mcp --test mcp_integration -- --ignored`");

    // --attach-main treats this profile as main (gate closed); --allow-main-writes
    // opens the gate so `send` can fire. --daemon-addr bypasses discovery.
    let mut child = Command::new(bin)
        .args([
            "--attach-main",
            "--allow-main-writes",
            "--daemon-addr",
            &daemon_addr,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn shore-mcp");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    // 1. initialize
    send_jsonrpc(
        &mut stdin,
        "initialize",
        1,
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "integration-test", "version": "0" }
        }),
    )
    .await
    .unwrap();
    let init_resp = tokio::time::timeout(Duration::from_secs(5), recv_jsonrpc_response(&mut reader))
        .await
        .expect("initialize timed out")
        .expect("initialize read failed");
    assert_eq!(init_resp["id"], 1);
    assert!(init_resp.get("result").is_some(), "no result in initialize response");

    // 2. tools/list — verify representative tools are advertised.
    send_jsonrpc(&mut stdin, "tools/list", 2, serde_json::json!({}))
        .await
        .unwrap();
    let list_resp = tokio::time::timeout(Duration::from_secs(5), recv_jsonrpc_response(&mut reader))
        .await
        .expect("tools/list timed out")
        .expect("tools/list read failed");
    let tools = list_resp["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for required in &["status", "character_list", "send", "log_tail"] {
        assert!(
            names.contains(required),
            "missing tool `{required}` in advertised list: {names:?}"
        );
    }

    // 3. status — read-only.
    send_jsonrpc(
        &mut stdin,
        "tools/call",
        3,
        serde_json::json!({ "name": "status", "arguments": {} }),
    )
    .await
    .unwrap();
    let status_resp = tokio::time::timeout(Duration::from_secs(10), recv_jsonrpc_response(&mut reader))
        .await
        .expect("status timed out")
        .expect("status read failed");
    assert_eq!(status_resp["id"], 3);
    assert!(status_resp.get("error").is_none(), "status errored: {status_resp}");

    // 4. send — mutating, should fire thanks to --allow-main-writes.
    send_jsonrpc(
        &mut stdin,
        "tools/call",
        4,
        serde_json::json!({
            "name": "send",
            "arguments": { "text": "hi there" }
        }),
    )
    .await
    .unwrap();
    let send_resp = tokio::time::timeout(Duration::from_secs(30), recv_jsonrpc_response(&mut reader))
        .await
        .expect("send timed out")
        .expect("send read failed");
    assert_eq!(send_resp["id"], 4);
    assert!(send_resp.get("error").is_none(), "send errored: {send_resp}");
    let content_text = send_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text content");
    assert!(
        content_text.contains("hello from mock llm"),
        "send output did not contain mock text: {content_text}"
    );

    // Clean shutdown.
    drop(stdin);
    let _ = child.kill().await;
    harness.shutdown().await;
}
