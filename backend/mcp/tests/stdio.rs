//! End-to-end test of the MCP client against a Python stdio stub server.
//!
//! Skipped (not failed) when `python3` is unavailable, so the suite stays green
//! on minimal CI images.

use std::collections::BTreeMap;

use serde_json::json;
use shore_mcp_client::{McpClient, McpError, McpServerSpec, Transport};

fn python3_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn stub_spec() -> McpServerSpec {
    let fixture = format!(
        "{}/tests/fixtures/stub_server.py",
        env!("CARGO_MANIFEST_DIR")
    );
    McpServerSpec {
        name: "stub".to_owned(),
        transport: Transport::Stdio {
            command: "python3".to_owned(),
            args: vec![fixture],
            env: BTreeMap::new(),
        },
    }
}

#[tokio::test]
async fn connect_list_call_and_error() {
    if !python3_available() {
        // No python3 on this host — skip rather than fail (minimal CI images).
        return;
    }

    let client = McpClient::connect(&stub_spec())
        .await
        .expect("connect to stub server");

    // tools/list surfaces the single echo tool, tagged with the server name.
    let tools = client.list_tools().await.expect("list tools");
    assert_eq!(tools.len(), 1);
    let echo = tools.first().expect("one tool present");
    assert_eq!(echo.name, "echo");
    assert_eq!(echo.server, "stub");
    assert_eq!(echo.description, "Echo back the message.");
    assert_eq!(
        echo.input_schema.get("type").and_then(|v| v.as_str()),
        Some("object")
    );

    // tools/call returns flattened text.
    let result = client
        .call("echo", json!({"message": "hi"}))
        .await
        .expect("call echo");
    assert_eq!(result.as_str(), Some("echo: hi"));

    // A tool that reports isError maps to McpError::ToolFailed.
    let err = client
        .call("nope", json!({}))
        .await
        .expect_err("unknown tool should error");
    assert!(
        matches!(err, McpError::ToolFailed { .. }),
        "expected ToolFailed, got {err:?}"
    );

    client.shutdown().await;
}
