use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::connection::ServerAddr;
use crate::error::{ClientError, Result};

/// One entry in `$XDG_RUNTIME_DIR/shore/instances.json`.
#[derive(Deserialize, Debug, Clone)]
pub struct InstanceEntry {
    /// Instance ID.
    #[serde(default)]
    pub id: Option<String>,
    /// Socket path (Unix) or `host:port` (TCP) where the daemon listens.
    pub socket_path: String,
    /// PID of the daemon process.
    #[serde(default)]
    pub pid: Option<u32>,
    /// Optional TCP address.
    #[serde(default)]
    pub tcp_addr: Option<String>,
}

/// The instances file is a JSON array of `InstanceEntry`.
type InstancesFile = Vec<InstanceEntry>;

/// Return the default path to the Shore instances file.
///
/// Uses `shore_config::runtime_dir()` so that `SHORE_RUNTIME_DIR`,
/// `XDG_RUNTIME_DIR`, and platform defaults are respected consistently.
pub fn instances_path() -> PathBuf {
    shore_config::runtime_dir().join("instances.json")
}

/// Read the instances file and return all live entries (dead PIDs are skipped).
pub fn read_instances() -> Result<Vec<InstanceEntry>> {
    let path = instances_path();
    let data = std::fs::read_to_string(&path).map_err(|e| {
        ClientError::Discovery(format!("cannot read {}: {e}", path.display()))
    })?;
    let entries: InstancesFile =
        serde_json::from_str(&data).map_err(|e| {
            ClientError::Discovery(format!("invalid JSON in {}: {e}", path.display()))
        })?;
    Ok(entries.into_iter().filter(|e| entry_alive(e)).collect())
}

/// Check whether an instance entry's PID is still running.
fn entry_alive(entry: &InstanceEntry) -> bool {
    match entry.pid {
        Some(pid) => Path::new(&format!("/proc/{pid}")).exists(),
        None => true, // no PID recorded — can't prune, assume alive
    }
}

/// Find the `ServerAddr` for a daemon whose config matches `config_path`.
///
/// If `config_path` is `None`, returns the first (default) entry.
pub fn discover(config_path: Option<&str>) -> Result<ServerAddr> {
    let entries = read_instances()?;

    let entry = match config_path {
        Some(wanted) => entries
            .iter()
            .find(|e| e.id.as_deref() == Some(wanted))
            .ok_or_else(|| {
                ClientError::Discovery(format!(
                    "no daemon found for id: {wanted}"
                ))
            })?,
        None => entries.first().ok_or_else(|| {
            ClientError::Discovery("instances file is empty".into())
        })?,
    };

    Ok(addr_from_socket(&entry.socket_path))
}

/// Convert a socket string to a `ServerAddr`.
///
/// Strings that look like file paths become `Unix`, everything else becomes `Tcp`.
fn addr_from_socket(socket: &str) -> ServerAddr {
    if crate::connection::is_unix_path(socket) {
        ServerAddr::Unix(socket.to_string())
    } else {
        ServerAddr::Tcp(socket.to_string())
    }
}

/// Return the default socket path used when no instances file is present.
pub fn default_socket_path() -> PathBuf {
    shore_config::runtime_dir().join("shore.sock")
}

/// Convenience: check client.toml, then discover, then fall back to the
/// default Unix socket.
pub fn discover_or_default(config_path: Option<&str>) -> ServerAddr {
    // 1. Check client.toml for a default_address.
    if let Some(cfg) = crate::client_config::load_client_config() {
        if let Some(addr) = &cfg.default_address {
            return addr_from_socket(addr);
        }
    }

    // 2. Instance discovery (optionally filtered by --config ID).
    match discover(config_path) {
        Ok(addr) => addr,
        Err(_) => {
            // 3. Fall back to default Unix socket.
            let sock = default_socket_path();
            ServerAddr::Unix(sock.to_string_lossy().into_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── entry_alive ──────────────────────────────────────────────────

    #[test]
    fn entry_alive_no_pid_assumes_alive() {
        let entry = InstanceEntry {
            id: None,
            socket_path: "/tmp/shore.sock".into(),
            pid: None,
            tcp_addr: None,
        };
        assert!(entry_alive(&entry));
    }

    #[test]
    fn entry_alive_current_process() {
        let entry = InstanceEntry {
            id: None,
            socket_path: "/tmp/shore.sock".into(),
            pid: Some(std::process::id()),
            tcp_addr: None,
        };
        assert!(entry_alive(&entry));
    }

    #[test]
    fn entry_alive_bogus_pid() {
        let entry = InstanceEntry {
            id: None,
            socket_path: "/tmp/shore.sock".into(),
            pid: Some(u32::MAX - 1),
            tcp_addr: None,
        };
        assert!(!entry_alive(&entry));
    }

    // ── addr_from_socket ─────────────────────────────────────────────

    #[test]
    fn addr_from_socket_absolute_path() {
        match addr_from_socket("/run/shore/shore.sock") {
            ServerAddr::Unix(p) => assert_eq!(p, "/run/shore/shore.sock"),
            other => panic!("expected Unix, got {other:?}"),
        }
    }

    #[test]
    fn addr_from_socket_tcp_address() {
        match addr_from_socket("localhost:7320") {
            ServerAddr::Tcp(a) => assert_eq!(a, "localhost:7320"),
            other => panic!("expected Tcp, got {other:?}"),
        }
    }

    #[test]
    fn addr_from_socket_relative_path() {
        match addr_from_socket("./shore.sock") {
            ServerAddr::Unix(p) => assert_eq!(p, "./shore.sock"),
            other => panic!("expected Unix, got {other:?}"),
        }
    }

    // ── InstanceEntry deserialization ─────────────────────────────────

    #[test]
    fn instance_entry_full_fields() {
        let json = r#"[{
            "id": "default",
            "socket_path": "/run/user/1000/shore/shore.sock",
            "pid": 12345,
            "tcp_addr": "127.0.0.1:7320"
        }]"#;
        let entries: Vec<InstanceEntry> = serde_json::from_str(json).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id.as_deref(), Some("default"));
        assert_eq!(entries[0].socket_path, "/run/user/1000/shore/shore.sock");
        assert_eq!(entries[0].pid, Some(12345));
        assert_eq!(entries[0].tcp_addr.as_deref(), Some("127.0.0.1:7320"));
    }

    #[test]
    fn instance_entry_minimal_fields_use_defaults() {
        let json = r#"[{"socket_path": "/tmp/shore.sock"}]"#;
        let entries: Vec<InstanceEntry> = serde_json::from_str(json).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].id.is_none());
        assert!(entries[0].pid.is_none());
        assert!(entries[0].tcp_addr.is_none());
        assert_eq!(entries[0].socket_path, "/tmp/shore.sock");
    }
}
