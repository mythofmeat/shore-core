//! Integration test: auto-spawned shore-daemon must outlive shore-mcp exit.
//!
//! Reproduces the bug where `drop(child)` on the spawned daemon was claimed
//! to detach but actually left it sharing our process group / controlling
//! terminal — so tearing down shore-mcp would take the daemon with it.
//!
//! Prerequisites:
//!   - `shore-mcp` binary (cargo provides this automatically via
//!     `CARGO_BIN_EXE_shore-mcp` when invoked via `cargo test`).
//!   - `shore-daemon` binary built in the same target dir (not automatic —
//!     build it explicitly before running this test).
//!
//! Run with:
//!   cargo build -p shore-daemon
//!   cargo test -p shore-mcp --test autospawn_detach -- --ignored --nocapture

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

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
        if value.get("id").is_some() {
            return Ok(value);
        }
    }
}

fn pid_alive(pid: u32) -> bool {
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    // EPERM means the process exists but we can't signal it — still alive.
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    )
}

#[tokio::test]
#[ignore] // Requires built binaries; see header for invocation.
async fn autospawned_daemon_survives_mcp_shutdown() {
    // 1. Isolated profile root. Using a fresh tempdir as XDG_DATA_HOME means
    //    shore-mcp's persistent-test profile path resolves under here, with
    //    no chance of colliding with the user's real shore-mcp-test daemon.
    let tempdir = tempfile::tempdir().expect("tempdir");
    let xdg_data = tempdir.path();

    // 2. Locate binaries. CARGO_BIN_EXE_shore-mcp is cargo-provided for this
    //    crate's own binary; shore-daemon lives in the same target dir.
    let mcp_bin = std::env::var("CARGO_BIN_EXE_shore-mcp")
        .expect("CARGO_BIN_EXE_shore-mcp — run via `cargo test -p shore-mcp`");
    let mcp_bin = PathBuf::from(mcp_bin);
    let target_dir = mcp_bin
        .parent()
        .expect("mcp bin has parent dir")
        .to_path_buf();
    let daemon_bin = target_dir.join("shore-daemon");
    assert!(
        daemon_bin.exists(),
        "shore-daemon not built at {} — run `cargo build -p shore-daemon` first",
        daemon_bin.display()
    );

    // 3. Compute the instances.json path. shore-mcp sets
    //    SHORE_RUNTIME_DIR=<xdg_data>/shore-mcp-test/runtime, and shore-config
    //    uses SHORE_RUNTIME_DIR as-is (no /shore suffix).
    let instances_path = xdg_data
        .join("shore-mcp-test")
        .join("runtime")
        .join("instances.json");

    // 4. Launch shore-mcp pointed at our isolated profile.
    //
    //    `process_group(0)` puts shore-mcp into its own fresh process group
    //    (pgid == pid). Without the setsid fix, the auto-spawned daemon
    //    inherits this pgroup — so sending SIGTERM to the pgroup at shutdown
    //    takes the daemon with it. With the fix, the daemon sits in a
    //    separate session and is immune. This is what turns the test into a
    //    real regression check instead of a happy-path smoke test.
    let mut child = Command::new(&mcp_bin)
        .env("XDG_DATA_HOME", xdg_data)
        .env("SHORE_DAEMON_BIN", &daemon_bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .process_group(0)
        .spawn()
        .expect("failed to spawn shore-mcp");

    let mut stdin = child.stdin.take().expect("stdin piped");
    let stdout = child.stdout.take().expect("stdout piped");
    let mut reader = BufReader::new(stdout);

    // 5. initialize — shore-mcp lazily attaches on the first tool call, so we
    //    follow up with a cheap read-only call to force the daemon spawn path.
    send_jsonrpc(
        &mut stdin,
        "initialize",
        1,
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "autospawn-detach", "version": "0" }
        }),
    )
    .await
    .expect("send initialize");
    let init = tokio::time::timeout(Duration::from_secs(10), recv_jsonrpc_response(&mut reader))
        .await
        .expect("initialize timed out")
        .expect("initialize read failed");
    assert!(
        init.get("result").is_some(),
        "initialize produced no result: {init}"
    );

    // 6. status — read-only. Triggers attach → spawn_and_attach_test_daemon.
    send_jsonrpc(
        &mut stdin,
        "tools/call",
        2,
        serde_json::json!({ "name": "status", "arguments": {} }),
    )
    .await
    .expect("send status");
    // Any response (result OR error) proves the call reached the daemon,
    // which means auto-spawn succeeded. The isolated profile has no
    // characters configured, so an error payload is expected and fine here.
    let status = tokio::time::timeout(Duration::from_secs(20), recv_jsonrpc_response(&mut reader))
        .await
        .expect("status timed out")
        .expect("status read failed");
    assert_eq!(status["id"], 2, "unexpected response shape: {status}");

    // 7. Read the daemon PID out of instances.json.
    let data = std::fs::read_to_string(&instances_path).unwrap_or_else(|e| {
        panic!(
            "instances.json not written at {}: {e}",
            instances_path.display()
        )
    });
    let entries: serde_json::Value =
        serde_json::from_str(&data).expect("instances.json is valid JSON");
    let pid = entries
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|e| e.get("pid"))
        .and_then(|p| p.as_u64())
        .expect("no pid in instances.json") as u32;

    // 8. Tear down shore-mcp by signalling its whole process group. This
    //    mimics the real-world teardown pattern (MCP client killing the
    //    server's pgroup, terminal hangup reaching the foreground pgroup)
    //    that actually triggered the bug.
    let mcp_pid = child.id().expect("mcp child has pid") as libc::pid_t;
    unsafe {
        libc::kill(-mcp_pid, libc::SIGTERM);
    }
    drop(stdin);
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;

    // 9. Give any stray signal the old buggy path would have propagated a
    //    moment to arrive. 500ms is well past the kernel signal-delivery
    //    window and keeps the test fast.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 10. The daemon must still be running. Capture liveness BEFORE cleanup
    //     so we don't race ourselves.
    let alive = pid_alive(pid);

    // 11. Cleanup: terminate the daemon regardless of outcome. Do this before
    //     the assert so test failures don't leak a daemon process.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }

    assert!(
        alive,
        "shore-daemon (pid {pid}) died when shore-mcp exited — detach regressed"
    );
}
