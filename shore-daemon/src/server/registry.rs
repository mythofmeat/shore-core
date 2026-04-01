use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

/// One daemon instance entry in the registry.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct InstanceInfo {
    pub id: String,
    pub pid: u32,
    pub socket_path: String,
    pub tcp_addr: Option<String>,
    pub started_at: String,
}

/// Handle to the instance registry file at a known path.
pub struct Registry {
    path: PathBuf,
}

impl Registry {
    /// Create a registry using the default path:
    /// `$SHORE_RUNTIME_DIR/instances.json` (or `/tmp/shore/instances.json`).
    pub fn default_path() -> Self {
        Self {
            path: shore_config::runtime_dir().join("instances.json"),
        }
    }

    /// Create a registry at a specific path (useful for testing).
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Path to the registry file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read-modify-write under an exclusive file lock.
    ///
    /// Automatically prunes entries whose PID is no longer alive.
    fn with_locked<F>(&self, f: F) -> std::io::Result<()>
    where
        F: FnOnce(&mut Vec<InstanceInfo>) -> std::io::Result<()>,
    {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.path)?;

        lock_file.lock_exclusive()?;

        let mut entries: Vec<InstanceInfo> = {
            let content = std::fs::read_to_string(&self.path).unwrap_or_default();
            if content.trim().is_empty() {
                Vec::new()
            } else {
                serde_json::from_str(&content).unwrap_or_default()
            }
        };

        // Prune entries whose PID is no longer alive.
        entries.retain(|e| pid_alive(e.pid));

        f(&mut entries)?;

        let json = serde_json::to_string_pretty(&entries)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&self.path, json)?;

        lock_file.unlock()?;
        Ok(())
    }

    /// Register a daemon instance (replaces any existing entry with the same id).
    pub fn register(&self, info: InstanceInfo) -> std::io::Result<()> {
        self.with_locked(|entries| {
            entries.retain(|e| e.id != info.id);
            entries.push(info);
            Ok(())
        })
    }

    /// Remove a daemon instance by id.
    pub fn unregister(&self, id: &str) -> std::io::Result<()> {
        self.with_locked(|entries| {
            entries.retain(|e| e.id != id);
            Ok(())
        })
    }

    /// List all registered instances, pruning any with dead PIDs.
    pub fn list(&self) -> std::io::Result<Vec<InstanceInfo>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        // Use exclusive lock so we can prune stale entries.
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)?;
        lock_file.lock_exclusive()?;

        let content = std::fs::read_to_string(&self.path)?;
        let mut entries: Vec<InstanceInfo> = if content.trim().is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&content).unwrap_or_default()
        };

        let before = entries.len();
        entries.retain(|e| pid_alive(e.pid));

        // Write back if we pruned anything.
        if entries.len() != before {
            let json = serde_json::to_string_pretty(&entries)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            std::fs::write(&self.path, json)?;
        }

        lock_file.unlock()?;
        Ok(entries)
    }
}

/// Check whether a PID is still alive.
fn pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_registry() -> (tempfile::TempDir, Registry) {
        let tmp = tempfile::tempdir().unwrap();
        let registry = Registry::at(tmp.path().join("shore").join("instances.json"));
        (tmp, registry)
    }

    fn sample_instance(id: &str) -> InstanceInfo {
        InstanceInfo {
            id: id.into(),
            pid: std::process::id(),
            socket_path: format!("/tmp/shore-{}.sock", id),
            tcp_addr: None,
            started_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn register_and_list() {
        let (_tmp, reg) = test_registry();
        let info = sample_instance("test-1");
        reg.register(info.clone()).unwrap();

        let entries = reg.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], info);
    }

    #[test]
    fn register_and_unregister() {
        let (_tmp, reg) = test_registry();
        reg.register(sample_instance("test-2")).unwrap();
        reg.register(sample_instance("test-3")).unwrap();

        assert_eq!(reg.list().unwrap().len(), 2);

        reg.unregister("test-2").unwrap();
        let entries = reg.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "test-3");
    }

    #[test]
    fn register_replaces_stale_entry() {
        let (_tmp, reg) = test_registry();
        reg.register(sample_instance("test-4")).unwrap();

        let mut updated = sample_instance("test-4");
        updated.socket_path = "/tmp/shore-test-4-new.sock".into();
        reg.register(updated.clone()).unwrap();

        let entries = reg.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].socket_path, "/tmp/shore-test-4-new.sock");
    }

    #[test]
    fn list_empty_when_no_registry() {
        let (_tmp, reg) = test_registry();
        let entries = reg.list().unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn cleanup_on_shutdown() {
        let (_tmp, reg) = test_registry();
        reg.register(sample_instance("daemon-a")).unwrap();
        reg.register(sample_instance("daemon-b")).unwrap();

        reg.unregister("daemon-a").unwrap();

        let entries = reg.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "daemon-b");
    }
}
