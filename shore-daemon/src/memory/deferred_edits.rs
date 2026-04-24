use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use shore_config::{
    character_config_dir, character_memory_dir, character_workspace_dir, character_workspace_file,
    AGENTS_FILE, HEARTBEAT_FILE, SOUL_FILE, TOOLS_FILE, USER_FILE,
};

/// Top-level workspace files that are editable immediately but only become
/// prompt-active after the next compaction/reload boundary.
const PROTECTED_PATHS: &[&str] = &[
    SOUL_FILE,
    USER_FILE,
    AGENTS_FILE,
    TOOLS_FILE,
    HEARTBEAT_FILE,
];

/// Persisted snapshot directory under the character data dir.
const ACTIVE_PROMPT_DIR: &str = "active_prompt";

/// Compaction-produced recent-memory digest injected into normal turns.
pub const RECENT_MEMORY_DIGEST_FILE: &str = "RECENT_MEMORY.md";

const DEFAULT_TOOLS_GUIDANCE: &str = "\
# TOOLS

Use tools when they materially help.

- Read files before editing them.
- Search memory files before guessing facts about the user or past events.
- Prefer concise, direct tool use over busywork.
";

const DEFAULT_HEARTBEAT_GUIDANCE: &str = "\
# HEARTBEAT

- Check whether anything from recent memory files needs follow-up.
- Check whether you promised the user anything that still matters.
- If nothing needs action, respond HEARTBEAT_OK.
";

/// Normalize a workspace path to one of the protected workspace-root files.
pub fn normalize_protected_path(path: &str) -> Option<String> {
    let mut normalized = path
        .trim()
        .trim_start_matches('/')
        .trim_start_matches('\\')
        .replace('\\', "/");

    while let Some(rest) = normalized.strip_prefix("./") {
        normalized = rest.to_string();
    }

    if let Some(rest) = normalized.strip_prefix("workspace/") {
        normalized = rest.to_string();
    }

    PROTECTED_PATHS
        .iter()
        .any(|&protected| normalized == protected)
        .then_some(normalized)
}

pub fn is_protected_path(path: &str) -> bool {
    normalize_protected_path(path).is_some()
}

pub fn active_prompt_dir(character_data_dir: &Path) -> PathBuf {
    character_data_dir.join(ACTIVE_PROMPT_DIR)
}

pub fn active_prompt_file(character_data_dir: &Path, name: &str) -> PathBuf {
    active_prompt_dir(character_data_dir).join(name)
}

pub fn recent_memory_digest_path(character_data_dir: &Path) -> PathBuf {
    active_prompt_file(character_data_dir, RECENT_MEMORY_DIGEST_FILE)
}

pub fn load_active_prompt_file(character_data_dir: &Path, name: &str) -> Option<String> {
    fs::read_to_string(active_prompt_file(character_data_dir, name))
        .ok()
        .filter(|content| !content.trim().is_empty())
}

pub fn write_recent_memory_digest(character_data_dir: &Path, digest: &str) -> io::Result<()> {
    let path = recent_memory_digest_path(character_data_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, digest.trim().to_owned() + "\n")
}

/// Return deduped protected paths waiting for activation.
pub fn pending_deferred_edit_paths(character_data_dir: &Path) -> io::Result<Vec<String>> {
    let queue_path = character_data_dir.join("deferred_edits.jsonl");
    if !queue_path.exists() {
        return Ok(vec![]);
    }

    let content = fs::read_to_string(queue_path)?;
    let mut paths = BTreeSet::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(path) = entry
            .get("path")
            .and_then(|v| v.as_str())
            .and_then(normalize_protected_path)
        {
            paths.insert(path);
        }
    }
    Ok(paths.into_iter().collect())
}

/// Queue a deferred refresh for a protected file.
pub fn queue_deferred_edit(character_data_dir: &Path, path: &str) -> io::Result<()> {
    let Some(path) = normalize_protected_path(path) else {
        return Ok(());
    };

    fs::create_dir_all(character_data_dir)?;
    let queue_path = character_data_dir.join("deferred_edits.jsonl");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&queue_path)?;
    let line = serde_json::json!({
        "path": path,
        "timestamp": chrono::Local::now().to_rfc3339(),
    });
    writeln!(file, "{line}")?;
    Ok(())
}

/// Refresh the active prompt snapshot from the canonical config workspace and
/// clear any deferred-edit queue.
pub fn apply_deferred_edits(
    character_data_dir: &Path,
    config_dir: &Path,
    char_name: &str,
) -> io::Result<()> {
    refresh_active_prompt_snapshot(character_data_dir, config_dir, char_name)?;

    let queue_path = character_data_dir.join("deferred_edits.jsonl");
    if queue_path.exists() {
        fs::remove_file(queue_path)?;
    }
    Ok(())
}

/// Ensure the workspace-first layout exists for a character and migrate any
/// legacy bootstrap files or memory files into it.
pub fn ensure_character_workspace(
    character_data_dir: &Path,
    config_dir: &Path,
    char_name: &str,
) -> io::Result<()> {
    let char_config_dir = character_config_dir(config_dir, char_name);
    let workspace_dir = character_workspace_dir(config_dir, char_name);
    let memory_dir = character_memory_dir(config_dir, char_name);

    fs::create_dir_all(&workspace_dir)?;
    fs::create_dir_all(&memory_dir)?;

    migrate_legacy_file(
        char_config_dir.join("character.md"),
        workspace_dir.join(SOUL_FILE),
    )?;
    migrate_legacy_file(
        char_config_dir.join("user.md"),
        workspace_dir.join(USER_FILE),
    )?;
    migrate_legacy_file(
        char_config_dir.join("prompts").join("system.md"),
        workspace_dir.join(AGENTS_FILE),
    )?;

    let global_user = config_dir.join("user.md");
    let workspace_user = workspace_dir.join(USER_FILE);
    if global_user.exists() && !workspace_user.exists() {
        fs::copy(global_user, workspace_user)?;
    }

    write_default_if_missing(workspace_dir.join(TOOLS_FILE), DEFAULT_TOOLS_GUIDANCE)?;
    write_default_if_missing(
        workspace_dir.join(HEARTBEAT_FILE),
        DEFAULT_HEARTBEAT_GUIDANCE,
    )?;

    let legacy_memories = character_data_dir.join("memories");
    if legacy_memories.exists() {
        copy_tree_if_missing(&legacy_memories, &memory_dir)?;
    }

    Ok(())
}

/// Ensure the active prompt snapshot exists. Missing files are seeded from the
/// canonical config workspace, but an existing snapshot is left intact so edits
/// remain deferred until compaction.
pub fn ensure_active_prompt_snapshot(
    character_data_dir: &Path,
    config_dir: &Path,
    char_name: &str,
) -> io::Result<()> {
    ensure_character_workspace(character_data_dir, config_dir, char_name)?;

    let active_dir = active_prompt_dir(character_data_dir);
    fs::create_dir_all(&active_dir)?;

    for name in PROTECTED_PATHS {
        let dst = active_dir.join(name);
        if !dst.exists() {
            let src = character_workspace_file(config_dir, char_name, name);
            if src.exists() {
                fs::copy(src, dst)?;
            }
        }
    }

    Ok(())
}

/// Refresh every protected file in the active prompt snapshot.
pub fn refresh_active_prompt_snapshot(
    character_data_dir: &Path,
    config_dir: &Path,
    char_name: &str,
) -> io::Result<()> {
    ensure_character_workspace(character_data_dir, config_dir, char_name)?;

    let active_dir = active_prompt_dir(character_data_dir);
    fs::create_dir_all(&active_dir)?;

    for name in PROTECTED_PATHS {
        let src = character_workspace_file(config_dir, char_name, name);
        let dst = active_dir.join(name);

        if src.exists() {
            fs::copy(src, dst)?;
        } else if dst.exists() {
            fs::remove_file(dst)?;
        }
    }

    Ok(())
}

fn migrate_legacy_file(src: PathBuf, dst: PathBuf) -> io::Result<()> {
    if src.exists() && !dst.exists() {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dst)?;
    }
    Ok(())
}

fn write_default_if_missing(path: PathBuf, content: &str) -> io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)
}

fn copy_tree_if_missing(src: &Path, dst: &Path) -> io::Result<()> {
    if src.is_file() {
        if !dst.exists() {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(src, dst)?;
        }
        return Ok(());
    }

    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_tree_if_missing(&src_path, &dst_path)?;
        } else if !dst_path.exists() {
            fs::copy(src_path, dst_path)?;
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
        assert!(is_protected_path("SOUL.md"));
        assert!(is_protected_path("workspace/SOUL.md"));
        assert!(is_protected_path("/workspace/USER.md"));
        assert!(is_protected_path(r"workspace\AGENTS.md"));
        assert!(is_protected_path("TOOLS.md"));
        assert!(!is_protected_path("notes.md"));
    }

    #[test]
    fn test_queue_and_apply_refreshes_snapshot() {
        let tmp = TempDir::new().unwrap();
        let char_dir = tmp.path().join("data").join("TestChar");
        let config_dir = tmp.path().join("config");
        let workspace = character_workspace_dir(&config_dir, "TestChar");

        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join(SOUL_FILE), "new soul").unwrap();
        fs::write(workspace.join(USER_FILE), "new user").unwrap();
        fs::write(workspace.join(AGENTS_FILE), "new agents").unwrap();
        fs::write(workspace.join(TOOLS_FILE), "new tools").unwrap();
        fs::write(workspace.join(HEARTBEAT_FILE), "new heartbeat").unwrap();

        queue_deferred_edit(&char_dir, "workspace/SOUL.md").unwrap();
        queue_deferred_edit(&char_dir, "USER.md").unwrap();

        apply_deferred_edits(&char_dir, &config_dir, "TestChar").unwrap();

        assert_eq!(
            fs::read_to_string(active_prompt_file(&char_dir, SOUL_FILE)).unwrap(),
            "new soul"
        );
        assert_eq!(
            fs::read_to_string(active_prompt_file(&char_dir, USER_FILE)).unwrap(),
            "new user"
        );
        assert!(!char_dir.join("deferred_edits.jsonl").exists());
    }

    #[test]
    fn test_pending_paths_deduped() {
        let tmp = TempDir::new().unwrap();
        let char_dir = tmp.path().join("char");
        fs::create_dir_all(&char_dir).unwrap();

        queue_deferred_edit(&char_dir, "workspace/SOUL.md").unwrap();
        queue_deferred_edit(&char_dir, "SOUL.md").unwrap();
        queue_deferred_edit(&char_dir, "AGENTS.md").unwrap();

        let paths = pending_deferred_edit_paths(&char_dir).unwrap();
        assert_eq!(paths, vec!["AGENTS.md".to_string(), "SOUL.md".to_string()]);
    }

    #[test]
    fn test_workspace_migration_and_global_seed() {
        let tmp = TempDir::new().unwrap();
        let char_dir = tmp.path().join("data").join("TestChar");
        let config_dir = tmp.path().join("config");
        let char_config = character_config_dir(&config_dir, "TestChar");

        fs::create_dir_all(char_config.join("prompts")).unwrap();
        fs::write(char_config.join("character.md"), "orig soul").unwrap();
        fs::write(char_config.join("user.md"), "orig user").unwrap();
        fs::write(char_config.join("prompts/system.md"), "orig agents").unwrap();
        fs::write(config_dir.join("user.md"), "global user").unwrap();
        fs::create_dir_all(char_dir.join("memories").join("daily")).unwrap();
        fs::write(
            char_dir
                .join("memories")
                .join("daily")
                .join("2026-01-01.md"),
            "note",
        )
        .unwrap();

        ensure_character_workspace(&char_dir, &config_dir, "TestChar").unwrap();

        let workspace = character_workspace_dir(&config_dir, "TestChar");
        assert_eq!(
            fs::read_to_string(workspace.join(SOUL_FILE)).unwrap(),
            "orig soul"
        );
        assert_eq!(
            fs::read_to_string(workspace.join(USER_FILE)).unwrap(),
            "orig user"
        );
        assert_eq!(
            fs::read_to_string(workspace.join(AGENTS_FILE)).unwrap(),
            "orig agents"
        );
        assert!(workspace.join(TOOLS_FILE).exists());
        assert!(workspace.join(HEARTBEAT_FILE).exists());
        assert_eq!(
            fs::read_to_string(
                character_memory_dir(&config_dir, "TestChar").join("daily/2026-01-01.md")
            )
            .unwrap(),
            "note"
        );
    }

    #[test]
    fn test_snapshot_seed_only_when_missing() {
        let tmp = TempDir::new().unwrap();
        let char_dir = tmp.path().join("data").join("TestChar");
        let config_dir = tmp.path().join("config");
        let workspace = character_workspace_dir(&config_dir, "TestChar");

        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join(SOUL_FILE), "workspace soul").unwrap();
        fs::write(workspace.join(USER_FILE), "workspace user").unwrap();
        fs::write(workspace.join(AGENTS_FILE), "workspace agents").unwrap();
        fs::write(workspace.join(TOOLS_FILE), "workspace tools").unwrap();
        fs::write(workspace.join(HEARTBEAT_FILE), "workspace heartbeat").unwrap();

        ensure_active_prompt_snapshot(&char_dir, &config_dir, "TestChar").unwrap();
        fs::write(workspace.join(SOUL_FILE), "edited later").unwrap();
        ensure_active_prompt_snapshot(&char_dir, &config_dir, "TestChar").unwrap();

        assert_eq!(
            fs::read_to_string(active_prompt_file(&char_dir, SOUL_FILE)).unwrap(),
            "workspace soul"
        );
    }

    #[test]
    fn test_protected_edit_stays_deferred_until_apply() {
        let tmp = TempDir::new().unwrap();
        let char_dir = tmp.path().join("data").join("TestChar");
        let config_dir = tmp.path().join("config");
        let workspace = character_workspace_dir(&config_dir, "TestChar");

        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join(SOUL_FILE), "active soul").unwrap();
        fs::write(workspace.join(USER_FILE), "active user").unwrap();
        fs::write(workspace.join(AGENTS_FILE), "active agents").unwrap();
        fs::write(workspace.join(TOOLS_FILE), "active tools").unwrap();
        fs::write(workspace.join(HEARTBEAT_FILE), "active heartbeat").unwrap();

        ensure_active_prompt_snapshot(&char_dir, &config_dir, "TestChar").unwrap();

        fs::write(workspace.join(SOUL_FILE), "edited soul").unwrap();
        queue_deferred_edit(&char_dir, "SOUL.md").unwrap();
        ensure_active_prompt_snapshot(&char_dir, &config_dir, "TestChar").unwrap();

        assert_eq!(
            fs::read_to_string(active_prompt_file(&char_dir, SOUL_FILE)).unwrap(),
            "active soul",
            "protected workspace edits must not live-activate before apply"
        );
        assert_eq!(
            pending_deferred_edit_paths(&char_dir).unwrap(),
            vec!["SOUL.md"]
        );

        apply_deferred_edits(&char_dir, &config_dir, "TestChar").unwrap();

        assert_eq!(
            fs::read_to_string(active_prompt_file(&char_dir, SOUL_FILE)).unwrap(),
            "edited soul",
            "apply_deferred_edits is the activation boundary"
        );
        assert!(!char_dir.join("deferred_edits.jsonl").exists());
    }
}
