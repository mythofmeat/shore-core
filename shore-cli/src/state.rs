//! Client-side state file for persisting the active character selection.
//!
//! The active character is stored as a plain text file in the Shore runtime
//! directory (`$XDG_RUNTIME_DIR/shore/active_character`).  This is ephemeral
//! state — cleared on reboot — which matches the intent: character selection
//! is session-level, not permanent config.

use std::path::PathBuf;

/// Return the directory used for Shore runtime state.
fn runtime_dir() -> PathBuf {
    shore_config::runtime_dir()
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
    use super::*;

    #[test]
    fn state_file_path_ends_with_active_character() {
        let path = state_file_path();
        assert!(
            path.ends_with("active_character"),
            "state_file_path should end with 'active_character', got: {path:?}"
        );
    }

    /// All env-var-dependent state tests are in one test to avoid
    /// `SHORE_RUNTIME_DIR` races across parallel test threads.
    #[test]
    fn read_write_active_character_lifecycle() {
        let tmp = tempfile::TempDir::new().unwrap();
        let runtime = tmp.path().join("shore");

        std::env::set_var("SHORE_RUNTIME_DIR", &runtime);
        let result = std::panic::catch_unwind(|| {
            // 1. Missing file → None.
            assert!(read_active_character().is_none(), "missing file should return None");

            // 2. Write and read back.
            write_active_character("alice").unwrap();
            assert_eq!(read_active_character().as_deref(), Some("alice"));

            // 3. Overwrite.
            write_active_character("bob").unwrap();
            assert_eq!(read_active_character().as_deref(), Some("bob"));

            // 4. Empty file → None.
            std::fs::write(state_file_path(), "").unwrap();
            assert!(read_active_character().is_none(), "empty file should return None");

            // 5. Whitespace trimming.
            std::fs::write(state_file_path(), "  carol  \n").unwrap();
            assert_eq!(read_active_character().as_deref(), Some("carol"));
        });
        std::env::remove_var("SHORE_RUNTIME_DIR");
        result.unwrap();
    }
}
