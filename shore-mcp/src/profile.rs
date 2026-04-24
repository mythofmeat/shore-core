use std::path::PathBuf;

use shore_client::{discover, ClientError, DiscoveryKind, SWPConnection, ServerAddr};

use crate::cli::Cli;

/// Kind of profile we resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileKind {
    /// `--attach-main`: user's real daemon.
    Main,
    /// Default: persistent test profile at $XDG_DATA_HOME/shore-mcp-test.
    PersistentTest,
    /// `--ephemeral`: tempdir, torn down on exit.
    Ephemeral,
}

/// Resolved profile info. Consumers set env vars before spawning the daemon.
#[derive(Debug)]
pub struct ResolvedProfile {
    pub kind: ProfileKind,
    /// (env_var_name, value) pairs to export before starting shore-client
    /// discovery or spawning a daemon. Empty for `Main`.
    pub env_overrides: Vec<(String, String)>,
    /// Tempdir handle, only set for `Ephemeral`. Drop-on-exit keeps the
    /// profile directory alive for the lifetime of the MCP server.
    #[allow(dead_code)]
    pub tempdir: Option<tempfile::TempDir>,
}

impl ResolvedProfile {
    /// Whether mutation tools are gated (i.e., this is NOT the main profile).
    pub fn is_test(&self) -> bool {
        !matches!(self.kind, ProfileKind::Main)
    }
}

/// Resolve which profile to use from parsed CLI args.
pub fn resolve_profile(cli: Cli) -> anyhow::Result<ResolvedProfile> {
    if cli.attach_main {
        return Ok(ResolvedProfile {
            kind: ProfileKind::Main,
            env_overrides: Vec::new(),
            tempdir: None,
        });
    }

    if cli.ephemeral {
        let td = tempfile::tempdir()?;
        let base = td.path().to_path_buf();
        let overrides = build_env_overrides(&base);
        return Ok(ResolvedProfile {
            kind: ProfileKind::Ephemeral,
            env_overrides: overrides,
            tempdir: Some(td),
        });
    }

    // Persistent test profile.
    let base = persistent_test_base();
    let overrides = build_env_overrides(&base);
    Ok(ResolvedProfile {
        kind: ProfileKind::PersistentTest,
        env_overrides: overrides,
        tempdir: None,
    })
}

/// Default location for the persistent test profile.
///
/// Uses `$XDG_DATA_HOME/shore-mcp-test/` or `$HOME/.local/share/shore-mcp-test/`
/// as a fallback. Never returns a path inside the user's real Shore profile.
fn persistent_test_base() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("shore-mcp-test");
        }
    }
    if let Some(home) = dirs::home_dir() {
        return home.join(".local").join("share").join("shore-mcp-test");
    }
    // Last-resort fallback. If HOME is unset the user has bigger problems.
    PathBuf::from("/tmp/shore-mcp-test")
}

/// The stable instance ID that `shore-mcp` uses when spawning a test daemon.
pub const MCP_INSTANCE_ID: &str = "shore-mcp-test";

/// Resolve a live daemon connection for the given profile.
///
/// Decision tree (matches the spec):
/// - `--daemon-addr` set: connect directly, skip discovery and spawning.
/// - `Main`: normal shore-client discovery.
/// - `PersistentTest` / `Ephemeral`:
///     - Export env overrides so discovery resolves to the test profile.
///     - Look up `MCP_INSTANCE_ID` in that profile's instances.json.
///     - If found, attach.
///     - Otherwise, spawn a shore-daemon child process with
///       `--instance-id=MCP_INSTANCE_ID`, wait for registration, then attach.
pub async fn attach(profile: &ResolvedProfile, cli: &Cli) -> anyhow::Result<SWPConnection> {
    // 1. Export env overrides BEFORE any discovery or spawn.
    for (k, v) in &profile.env_overrides {
        std::env::set_var(k, v);
    }

    // 2. Explicit --daemon-addr wins.
    if let Some(addr) = &cli.daemon_addr {
        let (conn, _hello, _history) =
            SWPConnection::connect(&ServerAddr(addr.clone()), "mcp", "shore-mcp", None).await?;
        return Ok(conn);
    }

    // 3. Main profile: normal discovery, no spawning.
    if matches!(profile.kind, ProfileKind::Main) {
        let addr = discover(None)?;
        let (conn, _hello, _history) =
            SWPConnection::connect(&addr, "mcp", "shore-mcp", None).await?;
        return Ok(conn);
    }

    // 4. Test profile: look up MCP_INSTANCE_ID, spawn on miss.
    match discover(Some(MCP_INSTANCE_ID)) {
        Ok(addr) => {
            let (conn, _hello, _history) =
                SWPConnection::connect(&addr, "mcp", "shore-mcp", None).await?;
            Ok(conn)
        }
        Err(ClientError::Discovery { kind, .. }) if is_spawnable_discovery_miss(kind) => {
            // No live test daemon — spawn one.
            spawn_and_attach_test_daemon().await
        }
        Err(e) => Err(e.into()),
    }
}

/// Whether a discovery miss is benign enough to justify spawning our own
/// test daemon. Registry corruption or I/O errors bubble up instead — those
/// mean something is wrong with the user's environment, not that a daemon
/// is merely missing.
fn is_spawnable_discovery_miss(kind: DiscoveryKind) -> bool {
    matches!(
        kind,
        DiscoveryKind::RegistryMissing | DiscoveryKind::RegistryEmpty | DiscoveryKind::NoMatch,
    )
}

async fn spawn_and_attach_test_daemon() -> anyhow::Result<SWPConnection> {
    use std::process::Stdio;
    use tokio::process::Command;
    use tokio::time::{sleep, Duration};

    // Find the shore-daemon binary in the current target dir.
    // Precedence: explicit SHORE_DAEMON_BIN env var, then $CARGO_TARGET_DIR,
    // then ./target/debug/shore-daemon as a fallback.
    let binary = shore_daemon_path()?;

    // Bind port 0 trick: let the daemon pick a free port via --addr=127.0.0.1:0.
    // The daemon will register the resolved addr in instances.json for us to discover.
    let mut cmd = Command::new(&binary);
    cmd.arg("--instance-id")
        .arg(MCP_INSTANCE_ID)
        .arg("--addr")
        .arg("127.0.0.1:0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // Detach the daemon into its own session before exec. Without setsid(),
    // the child inherits our process group and controlling terminal — so a
    // SIGHUP on terminal close, a process-group kill from the MCP client, or
    // similar teardown signals would take the daemon with us and leave a
    // stale instances.json entry. With setsid() the daemon is session leader
    // of a fresh session with no controlling tty, immune to those signals.
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn {}: {e}", binary.display()))?;

    // The daemon is now in a detached session and will survive our exit.
    // Drop the Child to release our handle; tokio's default `kill_on_drop`
    // is false, so no signal is sent.
    drop(child);

    // Poll instances.json for up to 5 seconds waiting for registration.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(addr) = discover(Some(MCP_INSTANCE_ID)) {
            let (conn, _hello, _history) =
                SWPConnection::connect(&addr, "mcp", "shore-mcp", None).await?;
            return Ok(conn);
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!(
                "spawned shore-daemon did not register instance '{MCP_INSTANCE_ID}' within 5s"
            );
        }
        sleep(Duration::from_millis(100)).await;
    }
}

fn shore_daemon_path() -> anyhow::Result<PathBuf> {
    if let Ok(explicit) = std::env::var("SHORE_DAEMON_BIN") {
        let p = PathBuf::from(explicit);
        if p.exists() {
            return Ok(p);
        }
    }
    // Fall back to PATH lookup.
    if let Ok(p) = which::which("shore-daemon") {
        return Ok(p);
    }
    anyhow::bail!(
        "could not find shore-daemon binary. Set SHORE_DAEMON_BIN to an explicit path, \
         or put shore-daemon on PATH (e.g. after `cargo build -p shore-daemon`)."
    )
}

fn build_env_overrides(base: &std::path::Path) -> Vec<(String, String)> {
    let config = base.join("config");
    let data = base.join("data");
    let runtime = base.join("runtime");
    vec![
        (
            "SHORE_CONFIG_DIR".into(),
            config.to_string_lossy().into_owned(),
        ),
        ("SHORE_DATA_DIR".into(), data.to_string_lossy().into_owned()),
        (
            "SHORE_RUNTIME_DIR".into(),
            runtime.to_string_lossy().into_owned(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank_cli() -> Cli {
        Cli {
            attach_main: false,
            ephemeral: false,
            allow_main_writes: false,
            daemon_addr: None,
        }
    }

    #[tokio::test]
    async fn attach_uses_daemon_addr_override_when_set() {
        use std::io::Write;

        // Stand up a bogus TCP listener so the connect attempt fails with
        // a protocol error rather than a connect error — enough to prove
        // we went to the overridden address and skipped discovery.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());

        // Accept one connection and write garbage so the SWP handshake fails.
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let _ = s.write_all(b"not-a-valid-hello\n");
        });

        let cli = Cli {
            attach_main: true,
            ephemeral: false,
            allow_main_writes: false,
            daemon_addr: Some(addr.clone()),
        };
        let resolved = resolve_profile(cli.clone()).unwrap();

        let result = attach(&resolved, &cli).await;
        assert!(result.is_err(), "handshake should fail on bogus daemon");
        // The error message should mention protocol, not discovery.
        let err_str = format!("{}", result.unwrap_err());
        assert!(
            err_str.contains("protocol")
                || err_str.contains("hello")
                || err_str.contains("version")
                || err_str.contains("deserialization"),
            "expected protocol-level error, got: {err_str}"
        );

        handle.join().unwrap();
    }

    #[test]
    fn main_profile_has_no_env_overrides() {
        let cli = Cli {
            attach_main: true,
            ..blank_cli()
        };
        let resolved = resolve_profile(cli).unwrap();
        assert_eq!(resolved.kind, ProfileKind::Main);
        assert!(resolved.env_overrides.is_empty());
        assert!(!resolved.is_test());
    }

    #[test]
    fn persistent_profile_under_xdg_data_home() {
        std::env::set_var("XDG_DATA_HOME", "/tmp/test-shore-mcp-xdg");
        let resolved = resolve_profile(blank_cli()).unwrap();
        assert_eq!(resolved.kind, ProfileKind::PersistentTest);
        for (_, path) in &resolved.env_overrides {
            assert!(path.starts_with("/tmp/test-shore-mcp-xdg/shore-mcp-test"));
        }
        std::env::remove_var("XDG_DATA_HOME");
    }

    #[test]
    fn ephemeral_profile_keeps_tempdir_alive() {
        let cli = Cli {
            ephemeral: true,
            ..blank_cli()
        };
        let resolved = resolve_profile(cli).unwrap();
        assert_eq!(resolved.kind, ProfileKind::Ephemeral);
        let tempdir_path = resolved.tempdir.as_ref().unwrap().path().to_path_buf();
        assert!(tempdir_path.exists());
        // Env overrides must live under the tempdir.
        for (_, path) in &resolved.env_overrides {
            assert!(path.starts_with(tempdir_path.to_str().unwrap()));
        }
    }
}
