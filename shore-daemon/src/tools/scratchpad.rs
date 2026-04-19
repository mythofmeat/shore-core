//! Scratchpad filesystem tools — persistent per-character scratch storage.
//!
//! Four tools: list, read, write, delete. All paths resolved relative to
//! `{data_dir}/{character}/scratchpad/`. Path traversal is rejected.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{ToolCategory, ToolDef, ToolError};

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

pub fn tool_defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "scratchpad_list",
            description: "List files and directories in your scratchpad. \
                          Returns names and sizes. Optionally pass a subdirectory path.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Subdirectory path to list (relative to scratchpad root). Omit for root."
                    }
                },
                "required": []
            }),
            category: ToolCategory::Other,
        },
        ToolDef {
            name: "scratchpad_read",
            description: "Read a file from your scratchpad.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path relative to scratchpad root."
                    }
                },
                "required": ["path"]
            }),
            category: ToolCategory::Other,
        },
        ToolDef {
            name: "scratchpad_write",
            description: "Write or overwrite a file in your scratchpad. Creates parent directories automatically.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path relative to scratchpad root."
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write."
                    }
                },
                "required": ["path", "content"]
            }),
            category: ToolCategory::Other,
        },
        ToolDef {
            name: "scratchpad_delete",
            description: "Delete a file or empty directory from your scratchpad. Your scratchpad is fully sandboxed from {{user}}'s filesystem so deleting is always safe.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File or empty directory path relative to scratchpad root."
                    }
                },
                "required": ["path"]
            }),
            category: ToolCategory::Other,
        },
    ]
}

// ---------------------------------------------------------------------------
// Path resolution & traversal protection
// ---------------------------------------------------------------------------

fn resolve_path(scratchpad_dir: &str, relative: &str) -> Result<PathBuf, ToolError> {
    if scratchpad_dir.is_empty() {
        return Err(ToolError::InvalidArgs("scratchpad not configured".into()));
    }

    let relative = relative.trim();
    if relative.is_empty() {
        return Err(ToolError::InvalidArgs("path is empty".into()));
    }

    // Reject path components that could escape the scratchpad.
    for component in Path::new(relative).components() {
        match component {
            std::path::Component::ParentDir => {
                return Err(ToolError::InvalidArgs(
                    "path traversal (..) is not allowed".into(),
                ));
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(ToolError::InvalidArgs(
                    "absolute paths are not allowed".into(),
                ));
            }
            _ => {}
        }
    }

    let base = PathBuf::from(scratchpad_dir);
    let resolved = base.join(relative);

    // Double-check: canonicalize what exists to catch symlink escapes.
    // Only perform the check when both the base and path (or an ancestor)
    // can be canonicalized — otherwise there are no symlinks to follow.
    if let Ok(canonical_base) = base.canonicalize() {
        if let Ok(canonical) = resolved.canonicalize() {
            // Full path exists — check directly.
            if !canonical.starts_with(&canonical_base) {
                return Err(ToolError::InvalidArgs(
                    "resolved path escapes scratchpad".into(),
                ));
            }
        } else {
            // Path doesn't exist yet — walk up to find the closest existing
            // ancestor and verify it's still inside the scratchpad.
            // This catches symlink escapes in intermediate directories.
            let mut ancestor = resolved.as_path();
            while let Some(parent) = ancestor.parent() {
                if let Ok(canonical_parent) = parent.canonicalize() {
                    if !canonical_parent.starts_with(&canonical_base) {
                        return Err(ToolError::InvalidArgs(
                            "resolved path escapes scratchpad".into(),
                        ));
                    }
                    break;
                }
                ancestor = parent;
            }
        }
    }

    Ok(resolved)
}

fn resolve_list_path(scratchpad_dir: &str, relative: Option<&str>) -> Result<PathBuf, ToolError> {
    if scratchpad_dir.is_empty() {
        return Err(ToolError::InvalidArgs("scratchpad not configured".into()));
    }

    let base = PathBuf::from(scratchpad_dir);

    match relative {
        None | Some("") | Some(".") => Ok(base),
        Some(rel) => resolve_path(scratchpad_dir, rel),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub async fn handle_scratchpad_list(
    input: Value,
    scratchpad_dir: &str,
) -> Result<Value, ToolError> {
    let path_str = input.get("path").and_then(|v| v.as_str());
    let dir = resolve_list_path(scratchpad_dir, path_str)?;

    if !dir.exists() {
        return Ok(json!({ "entries": [], "note": "scratchpad directory does not exist yet" }));
    }

    if !dir.is_dir() {
        return Err(ToolError::InvalidArgs(format!(
            "{} is not a directory",
            path_str.unwrap_or(".")
        )));
    }

    let mut entries = Vec::new();
    let read_dir = std::fs::read_dir(&dir).map_err(|e| ToolError::Io(e.to_string()))?;

    for entry in read_dir {
        let entry = entry.map_err(|e| ToolError::Io(e.to_string()))?;
        let meta = entry.metadata().map_err(|e| ToolError::Io(e.to_string()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        entries.push(json!({
            "name": name,
            "type": if meta.is_dir() { "directory" } else { "file" },
            "size": meta.len(),
        }));
    }

    entries.sort_by(|a, b| {
        a.get("name")
            .and_then(|v| v.as_str())
            .cmp(&b.get("name").and_then(|v| v.as_str()))
    });

    Ok(json!({ "entries": entries }))
}

pub async fn handle_scratchpad_read(
    input: Value,
    scratchpad_dir: &str,
) -> Result<Value, ToolError> {
    let path_str = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing required field: path".into()))?;

    let path = resolve_path(scratchpad_dir, path_str)?;

    if !path.exists() {
        return Err(ToolError::Io(format!("file not found: {path_str}")));
    }

    let content = std::fs::read_to_string(&path).map_err(|e| ToolError::Io(e.to_string()))?;

    Ok(json!({
        "path": path_str,
        "content": content,
    }))
}

pub async fn handle_scratchpad_write(
    input: Value,
    scratchpad_dir: &str,
) -> Result<Value, ToolError> {
    let path_str = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing required field: path".into()))?;
    let content = input
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing required field: content".into()))?;

    let path = resolve_path(scratchpad_dir, path_str)?;

    // Create parent directories.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ToolError::Io(e.to_string()))?;
    }

    std::fs::write(&path, content).map_err(|e| ToolError::Io(e.to_string()))?;

    Ok(json!({
        "path": path_str,
        "bytes_written": content.len(),
    }))
}

pub async fn handle_scratchpad_delete(
    input: Value,
    scratchpad_dir: &str,
) -> Result<Value, ToolError> {
    let path_str = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing required field: path".into()))?;

    let path = resolve_path(scratchpad_dir, path_str)?;

    if !path.exists() {
        return Err(ToolError::Io(format!("not found: {path_str}")));
    }

    if path.is_dir() {
        std::fs::remove_dir(&path).map_err(|e| ToolError::Io(e.to_string()))?;
    } else {
        std::fs::remove_file(&path).map_err(|e| ToolError::Io(e.to_string()))?;
    }

    Ok(json!({
        "path": path_str,
        "deleted": true,
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_defs_count() {
        assert_eq!(tool_defs().len(), 4);
    }

    #[test]
    fn resolve_rejects_traversal() {
        assert!(resolve_path("/tmp/sp", "../etc/passwd").is_err());
        assert!(resolve_path("/tmp/sp", "foo/../../etc/passwd").is_err());
    }

    #[test]
    fn resolve_rejects_absolute() {
        assert!(resolve_path("/tmp/sp", "/etc/passwd").is_err());
    }

    #[test]
    fn resolve_rejects_empty_dir() {
        assert!(resolve_path("", "file.txt").is_err());
    }

    #[test]
    fn resolve_normal_path() {
        let p = resolve_path("/tmp/sp", "notes/ideas.md").unwrap();
        assert_eq!(p, PathBuf::from("/tmp/sp/notes/ideas.md"));
    }

    #[tokio::test]
    async fn write_read_delete_cycle() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = tmp.path().join("scratchpad");
        let sp_str = sp.to_string_lossy().to_string();

        // Write
        let result = handle_scratchpad_write(
            json!({"path": "test.txt", "content": "hello world"}),
            &sp_str,
        )
        .await
        .unwrap();
        assert_eq!(result["bytes_written"], 11);

        // Read
        let result = handle_scratchpad_read(json!({"path": "test.txt"}), &sp_str)
            .await
            .unwrap();
        assert_eq!(result["content"], "hello world");

        // List
        let result = handle_scratchpad_list(json!({}), &sp_str).await.unwrap();
        let entries = result["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["name"], "test.txt");

        // Delete
        let result = handle_scratchpad_delete(json!({"path": "test.txt"}), &sp_str)
            .await
            .unwrap();
        assert!(result["deleted"].as_bool().unwrap());

        // Verify deleted
        assert!(handle_scratchpad_read(json!({"path": "test.txt"}), &sp_str)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn list_nonexistent_returns_empty() {
        let result = handle_scratchpad_list(json!({}), "/tmp/nonexistent_sp_test_12345")
            .await
            .unwrap();
        assert!(result["entries"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn write_creates_subdirectories() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = tmp.path().join("scratchpad");
        let sp_str = sp.to_string_lossy().to_string();

        handle_scratchpad_write(
            json!({"path": "deep/nested/file.txt", "content": "nested"}),
            &sp_str,
        )
        .await
        .unwrap();

        let result = handle_scratchpad_read(json!({"path": "deep/nested/file.txt"}), &sp_str)
            .await
            .unwrap();
        assert_eq!(result["content"], "nested");
    }

    /// A symlink inside the scratchpad pointing outside should be caught
    /// by resolve_path, even for new file paths (where the target doesn't
    /// exist yet and canonicalize would fail).
    #[test]
    fn resolve_path_rejects_symlink_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = tmp.path().join("scratchpad");
        std::fs::create_dir_all(&sp).unwrap();

        // Create an "escape" directory outside the scratchpad.
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();

        // Create a symlink inside the scratchpad that points outside.
        let link_path = sp.join("escape_link");
        std::os::unix::fs::symlink(&outside, &link_path).unwrap();

        let sp_str = sp.to_str().unwrap();

        // Writing through the symlink to a new file should be rejected.
        // The path "escape_link/secret.txt" resolves through the symlink
        // to outside/secret.txt, which is outside the scratchpad.
        let result = resolve_path(sp_str, "escape_link/secret.txt");
        assert!(
            result.is_err(),
            "resolve_path should reject paths that escape via symlink, got: {:?}",
            result.unwrap()
        );
    }
}
