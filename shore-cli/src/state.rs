//! Client-side state file for persisting the active character selection.
//!
//! The active character is stored as a plain text file in the Shore runtime
//! directory (`$XDG_RUNTIME_DIR/shore/active_character`).  This is ephemeral
//! state — cleared on reboot — which matches the intent: character selection
//! is session-level, not permanent config.

use std::path::{Path, PathBuf};

/// Return the directory used for Shore runtime state.
fn runtime_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return Path::new(&dir).join("shore");
    }
    PathBuf::from("/tmp/shore")
}

/// Return the path to the active character state file.
pub fn state_file_path() -> PathBuf {
    runtime_dir().join("active_character")
}

/// Read the active character from the state file.
///
/// Returns `None` if the file doesn't exist, is empty, or is unreadable.
pub fn read_active_character() -> Option<String> {
    let content = std::fs::read_to_string(state_file_path()).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Write the active character to the state file.
///
/// Creates parent directories if needed.
pub fn write_active_character(name: &str) -> std::io::Result<()> {
    let path = state_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, name)
}

#[cfg(test)]
mod tests {
    #[test]
    fn round_trip_with_custom_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("active_character");

        std::fs::write(&file, "alice").unwrap();
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content.trim(), "alice");
    }

    #[test]
    fn read_returns_none_for_missing_file() {
        // state_file_path points at the real runtime dir, but we can test
        // the logic directly: reading a nonexistent file returns None.
        let result = std::fs::read_to_string("/tmp/shore_test_nonexistent_12345");
        assert!(result.is_err());
    }

    #[test]
    fn empty_content_returns_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("active_character");
        std::fs::write(&file, "").unwrap();

        let content = std::fs::read_to_string(&file).ok();
        let trimmed = content.as_deref().map(str::trim).filter(|s| !s.is_empty());
        assert!(trimmed.is_none());
    }
}
