use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::{debug, warn};

use crate::connection::ServerAddr;
use crate::error::{ClientError, Result};

/// One entry in `$XDG_RUNTIME_DIR/shore/instances.json`.
#[derive(Deserialize, Debug, Clone)]
pub struct InstanceEntry {
    /// Instance ID.
    #[serde(default)]
    pub id: Option<String>,
    /// TCP address where the daemon listens.
    pub addr: String,
    /// PID of the daemon process.
    #[serde(default)]
    pub pid: Option<u32>,
    /// Resolved data directory (written by daemon at registration).
    #[serde(default)]
    pub data_dir: Option<String>,
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
    debug!(path = %path.display(), "reading instances file");
    let data = std::fs::read_to_string(&path)
        .map_err(|e| ClientError::Discovery(format!("cannot read {}: {e}", path.display())))?;
    let entries: InstancesFile = serde_json::from_str(&data)
        .map_err(|e| ClientError::Discovery(format!("invalid JSON in {}: {e}", path.display())))?;
    let total = entries.len();
    let live: Vec<_> = entries.into_iter().filter(entry_alive).collect();
    debug!(total, live = live.len(), "discovered daemon instances");
    Ok(live)
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
            .ok_or_else(|| ClientError::Discovery(format!("no daemon found for id: {wanted}")))?,
        None => entries
            .first()
            .ok_or_else(|| ClientError::Discovery("instances file is empty".into()))?,
    };

    Ok(ServerAddr(entry.addr.clone()))
}

/// Discover the data directory from the first live daemon instance.
///
/// Returns `None` if no instance is registered or the entry lacks `data_dir`.
pub fn discover_data_dir() -> Option<PathBuf> {
    read_instances()
        .ok()?
        .first()
        .and_then(|e| e.data_dir.as_deref())
        .map(PathBuf::from)
}

pub const DEFAULT_ADDR: &str = "127.0.0.1:7320";

/// Convenience: check client.toml, then discover, then fall back to the
/// default TCP address.
pub fn discover_or_default(config_path: Option<&str>) -> ServerAddr {
    if let Some(cfg) = crate::client_config::load_client_config() {
        if let Some(addr) = &cfg.default_address {
            debug!(addr = %addr, "using address from client.toml");
            return ServerAddr(addr.clone());
        }
    }
    match discover(config_path) {
        Ok(addr) => {
            debug!(addr = ?addr, "resolved daemon via instance discovery");
            addr
        }
        Err(e) => {
            warn!(error = %e, fallback = DEFAULT_ADDR, "instance discovery failed, using default address");
            ServerAddr(DEFAULT_ADDR.to_string())
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
            addr: "127.0.0.1:7320".into(),
            pid: None,
            data_dir: None,
        };
        assert!(entry_alive(&entry));
    }

    #[test]
    fn entry_alive_current_process() {
        let entry = InstanceEntry {
            id: None,
            addr: "127.0.0.1:7320".into(),
            pid: Some(std::process::id()),
            data_dir: None,
        };
        assert!(entry_alive(&entry));
    }

    #[test]
    fn entry_alive_bogus_pid() {
        let entry = InstanceEntry {
            id: None,
            addr: "127.0.0.1:7320".into(),
            pid: Some(u32::MAX - 1),
            data_dir: None,
        };
        assert!(!entry_alive(&entry));
    }

    // ── InstanceEntry deserialization ─────────────────────────────────

    #[test]
    fn instance_entry_full_fields() {
        let json = r#"[{
            "id": "default",
            "addr": "127.0.0.1:7320",
            "pid": 12345,
            "data_dir": "/home/user/data"
        }]"#;
        let entries: Vec<InstanceEntry> = serde_json::from_str(json).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id.as_deref(), Some("default"));
        assert_eq!(entries[0].addr, "127.0.0.1:7320");
        assert_eq!(entries[0].pid, Some(12345));
        assert_eq!(entries[0].data_dir.as_deref(), Some("/home/user/data"));
    }

    #[test]
    fn instance_entry_minimal_fields_use_defaults() {
        let json = r#"[{"addr": "127.0.0.1:7320"}]"#;
        let entries: Vec<InstanceEntry> = serde_json::from_str(json).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].id.is_none());
        assert!(entries[0].pid.is_none());
        assert_eq!(entries[0].addr, "127.0.0.1:7320");
    }
}
