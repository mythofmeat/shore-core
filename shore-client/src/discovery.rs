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

/// Convenience: discover or fall back to the default Unix socket.
pub fn discover_or_default(config_path: Option<&str>) -> ServerAddr {
    match discover(config_path) {
        Ok(addr) => addr,
        Err(_) => {
            let sock = default_socket_path();
            ServerAddr::Unix(sock.to_string_lossy().into_owned())
        }
    }
}
