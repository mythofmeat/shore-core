//! Regression test: when the daemon is spawned with `--addr 127.0.0.1:0`,
//! the kernel-assigned port must be reflected in the instance registry.
//!
//! Pre-fix bug: registry held the literal `127.0.0.1:0`, so any discovery
//! client tried to connect to port 0 and got `Connection refused`.

use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::sleep;

#[tokio::test]
async fn spawned_daemon_registers_resolved_port_when_bound_to_zero() {
    let bin = std::env::var("CARGO_BIN_EXE_shore-daemon")
        .expect("CARGO_BIN_EXE_shore-daemon — run via `cargo test -p shore-daemon`");

    // Each test gets its own isolated profile so we don't collide with the
    // user's main daemon or with sibling tests.
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = tmp.path();
    let config = base.join("config");
    let data = base.join("data");
    let runtime = base.join("runtime");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();
    // Minimal valid config — empty file is enough to satisfy the loader's
    // existence check; defaults fill in everything else.
    std::fs::write(config.join("config.toml"), "").unwrap();

    let instance_id = format!("test-bind-zero-{}", uuid_like());

    let mut child = Command::new(&bin)
        .args(["--instance-id", &instance_id, "--addr", "127.0.0.1:0"])
        .env("SHORE_CONFIG_DIR", &config)
        .env("SHORE_DATA_DIR", &data)
        .env("SHORE_RUNTIME_DIR", &runtime)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn shore-daemon");

    // Poll instances.json for up to 10s. The daemon may take a moment to
    // load config, bind, and write its registry entry.
    let registry_path = runtime.join("instances.json");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let resolved = loop {
        if let Ok(bytes) = std::fs::read(&registry_path) {
            if let Ok(entries) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if let Some(arr) = entries.as_array() {
                    if let Some(entry) = arr
                        .iter()
                        .find(|e| e.get("id").and_then(|v| v.as_str()) == Some(instance_id.as_str()))
                    {
                        if let Some(addr) = entry.get("addr").and_then(|v| v.as_str()) {
                            break addr.to_string();
                        }
                    }
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill().await;
            panic!("daemon never registered instance `{instance_id}` in {registry_path:?}");
        }
        sleep(Duration::from_millis(100)).await;
    };

    let _ = child.kill().await;
    let _ = child.wait().await;

    assert!(
        !resolved.ends_with(":0"),
        "registry recorded unresolved bind addr `{resolved}` — \
         the daemon must capture the kernel-assigned port before registering"
    );
    let port: u16 = resolved
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("registry addr `{resolved}` does not parse as host:port"));
    assert!(port > 0, "registry addr `{resolved}` has port 0");
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}
