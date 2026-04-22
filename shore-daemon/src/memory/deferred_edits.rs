use std::io::{self, Write};
use std::path::Path;

/// Paths relative to workspace/ that, when edited, should be deferred
/// until the next compaction boundary to avoid mid-conversation cache
/// invalidation.
const PROTECTED_PATHS: &[&str] = &[
    "character.md",
    "user.md",
    "prompts/system.md",
];

/// Check whether a workspace-relative path is a protected file.
pub fn is_protected_path(path: &str) -> bool {
    let normalized = path.trim_start_matches('/').trim_start_matches('\\');
    PROTECTED_PATHS.iter().any(|&p| normalized == p)
}

/// Queue a deferred edit for a protected file.
///
/// Writes a JSON line to `{character_data_dir}/deferred_edits.jsonl`.
pub fn queue_deferred_edit(character_data_dir: &Path, path: &str) -> io::Result<()> {
    let queue_path = character_data_dir.join("deferred_edits.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&queue_path)?;
    let line = serde_json::json!({
        "path": path,
        "timestamp": chrono::Local::now().to_rfc3339(),
    });
    writeln!(file, "{}", line)?;
    Ok(())
}

/// Apply all deferred edits by copying workspace files to the config dir.
///
/// Reads `{character_data_dir}/deferred_edits.jsonl`, copies each referenced
/// file from `workspace/` to the corresponding location under
/// `{config_dir}/characters/{char_name}/`, then removes the queue file.
///
/// Missing workspace files are silently skipped (the user may have deleted
/// them).
pub fn apply_deferred_edits(
    character_data_dir: &Path,
    config_dir: &Path,
    char_name: &str,
) -> io::Result<()> {
    let queue_path = character_data_dir.join("deferred_edits.jsonl");
    if !queue_path.exists() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&queue_path)?;
    if content.trim().is_empty() {
        std::fs::remove_file(&queue_path)?;
        return Ok(());
    }

    let char_config_dir = config_dir.join("characters").join(char_name);
    let workspace_dir = character_data_dir.join("workspace");

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Malformed deferred edit line, skipping: {e}");
                continue;
            }
        };
        let path = entry.get("path").and_then(|v| v.as_str()).unwrap_or("");
        if path.is_empty() {
            continue;
        }

        let src = workspace_dir.join(path);
        if !src.exists() {
            tracing::debug!(
                path = %path,
                "Deferred edit source missing in workspace, skipping"
            );
            continue;
        }

        let dst = char_config_dir.join(path);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::copy(&src, &dst)?;
        tracing::info!(
            path = %path,
            src = %src.display(),
            dst = %dst.display(),
            "Applied deferred edit"
        );
    }

    std::fs::remove_file(&queue_path)?;
    Ok(())
}

/// Bootstrap protected workspace files from the config dir if they don't
/// already exist in the workspace.
///
/// Copies `character.md`, `user.md`, and `prompts/system.md` from
/// `{config_dir}/characters/{char_name}/` to `{character_data_dir}/workspace/`
/// so the assistant can read and edit them.
pub fn bootstrap_workspace_files(
    character_data_dir: &Path,
    config_dir: &Path,
    char_name: &str,
) -> io::Result<()> {
    let char_config_dir = config_dir.join("characters").join(char_name);
    let workspace_dir = character_data_dir.join("workspace");

    for path in PROTECTED_PATHS {
        let src = char_config_dir.join(path);
        let dst = workspace_dir.join(path);
        if src.exists() && !dst.exists() {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&src, &dst)?;
            tracing::info!(
                path = %path,
                "Bootstrapped workspace file from config"
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_is_protected_path() {
        assert!(is_protected_path("character.md"));
        assert!(is_protected_path("user.md"));
        assert!(is_protected_path("prompts/system.md"));
        assert!(!is_protected_path("notes.md"));
        assert!(!is_protected_path("character.md.bak"));
    }

    #[test]
    fn test_queue_and_apply() {
        let tmp = TempDir::new().unwrap();
        let char_dir = tmp.path().join("char");
        let config_dir = tmp.path().join("config");
        let char_config = config_dir.join("characters").join("TestChar");
        let ws = char_dir.join("workspace");

        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(ws.join("prompts")).unwrap();
        std::fs::create_dir_all(&char_config.join("prompts")).unwrap();

        // Create workspace files
        std::fs::write(ws.join("character.md"), "new char def").unwrap();
        std::fs::write(ws.join("user.md"), "new user def").unwrap();
        std::fs::write(ws.join("prompts/system.md"), "new system").unwrap();

        // Queue edits
        queue_deferred_edit(&char_dir, "character.md").unwrap();
        queue_deferred_edit(&char_dir, "user.md").unwrap();

        // Apply
        apply_deferred_edits(&char_dir, &config_dir, "TestChar").unwrap();

        // Verify copies
        assert_eq!(
            std::fs::read_to_string(char_config.join("character.md")).unwrap(),
            "new char def"
        );
        assert_eq!(
            std::fs::read_to_string(char_config.join("user.md")).unwrap(),
            "new user def"
        );
        // prompts/system.md was not queued, so not copied
        assert!(!char_config.join("prompts/system.md").exists());

        // Queue file removed
        assert!(!char_dir.join("deferred_edits.jsonl").exists());
    }

    #[test]
    fn test_bootstrap() {
        let tmp = TempDir::new().unwrap();
        let char_dir = tmp.path().join("char");
        let config_dir = tmp.path().join("config");
        let char_config = config_dir.join("characters").join("TestChar");
        let ws = char_dir.join("workspace");

        std::fs::create_dir_all(&char_config.join("prompts")).unwrap();
        std::fs::write(char_config.join("character.md"), "orig char").unwrap();
        std::fs::write(char_config.join("user.md"), "orig user").unwrap();
        std::fs::write(char_config.join("prompts/system.md"), "orig system").unwrap();

        bootstrap_workspace_files(&char_dir, &config_dir, "TestChar").unwrap();

        assert_eq!(
            std::fs::read_to_string(ws.join("character.md")).unwrap(),
            "orig char"
        );
        assert_eq!(
            std::fs::read_to_string(ws.join("user.md")).unwrap(),
            "orig user"
        );
        assert_eq!(
            std::fs::read_to_string(ws.join("prompts/system.md")).unwrap(),
            "orig system"
        );

        // Second bootstrap is a no-op (files already exist)
        std::fs::write(char_config.join("character.md"), "changed").unwrap();
        bootstrap_workspace_files(&char_dir, &config_dir, "TestChar").unwrap();
        assert_eq!(
            std::fs::read_to_string(ws.join("character.md")).unwrap(),
            "orig char"
        );
    }

    #[test]
    fn test_apply_skips_missing_workspace_file() {
        let tmp = TempDir::new().unwrap();
        let char_dir = tmp.path().join("char");
        let config_dir = tmp.path().join("config");

        std::fs::create_dir_all(&char_dir).unwrap();
        queue_deferred_edit(&char_dir, "character.md").unwrap();

        // Workspace file doesn't exist — should not error
        apply_deferred_edits(&char_dir, &config_dir, "TestChar").unwrap();
        assert!(!char_dir.join("deferred_edits.jsonl").exists());
    }
}
