//! Workspace filesystem tools — read, write, edit, list, exec.
//!
//! These tools give the assistant access to a real filesystem workspace
//! (`{character}/workspace/`) and its memory directory (`{character}/workspace/memory/`),
//! mirroring OpenClaw's model of agent-curated files.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{ToolCategory, ToolDef, ToolError};

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

pub fn tool_defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "read",
            description: "Read the contents of a file in your workspace or memory directory. Paths without a prefix are resolved under workspace/. Use `memory/...` to access durable memory files. Returns the file content as text; use offset and limit for large files.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path. Bare paths resolve under workspace/. Use `workspace/...` or `memory/...` for an explicit root."
                    },
                    "offset": {
                        "type": "number",
                        "description": "Line number to start reading from (1-based). Optional."
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of lines to read. Optional."
                    }
                },
                "required": ["path"]
            }),
            category: ToolCategory::Other,
        },
        ToolDef {
            name: "write",
            description: "Write or overwrite a file in your workspace or memory directory. Bare paths resolve under workspace/. Use `memory/...` to write durable memory files. Parent directories are created automatically. Overwrites without confirmation.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path. Bare paths resolve under workspace/. Use `workspace/...` or `memory/...` for an explicit root."
                    },
                    "content": {
                        "type": "string",
                        "description": "Full content to write."
                    }
                },
                "required": ["path", "content"]
            }),
            category: ToolCategory::Other,
        },
        ToolDef {
            name: "edit",
            description: "Edit an existing file by replacing specific text. Bare paths resolve under workspace/. Use `memory/...` to edit durable memory files. Each replacement must match the old_string exactly, including whitespace and newlines.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path. Bare paths resolve under workspace/. Use `workspace/...` or `memory/...` for an explicit root."
                    },
                    "edits": {
                        "type": "array",
                        "description": "List of replacements to apply in order.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_string": {
                                    "type": "string",
                                    "description": "Exact text to find and replace. Must match whitespace and newlines precisely."
                                },
                                "new_string": {
                                    "type": "string",
                                    "description": "Text to replace old_string with."
                                }
                            },
                            "required": ["old_string", "new_string"]
                        }
                    }
                },
                "required": ["path", "edits"]
            }),
            category: ToolCategory::Other,
        },
        ToolDef {
            name: "list_files",
            description: "List files and directories under a path in your workspace or memory directory. Bare paths resolve under workspace/. Use `memory/...` to inspect durable memory files. Returns each entry's name, type, and size.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative directory path. Bare paths resolve under workspace/. Use `workspace/...` or `memory/...` for an explicit root. Omit for workspace root."
                    }
                },
                "required": []
            }),
            category: ToolCategory::Other,
        },
        ToolDef {
            name: "search",
            description: "Search text files across your workspace and memory directory. Bare paths resolve under workspace/. Use `memory/...` to search durable memory files. Returns matching file paths, line numbers, and excerpts. Use this for discovery before reading a full file.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Keyword or phrase to search for (case-insensitive)."
                    },
                    "path": {
                        "type": "string",
                        "description": "Optional relative path to limit the search. Bare paths resolve under workspace/. Use `workspace/...` or `memory/...` for an explicit root."
                    },
                    "max_results": {
                        "type": "number",
                        "description": "Maximum matches to return. Defaults to 20, maximum 100."
                    }
                },
                "required": ["query"]
            }),
            category: ToolCategory::Other,
        },
        ToolDef {
            name: "delete",
            description: "Move a file in your workspace or memory directory to a trash folder. Bare paths resolve under workspace/. Use `memory/...` to remove a durable memory file. The file is moved out of your workspace into a timestamped trash folder, not permanently erased. Refuses prompt-visible files (SOUL.md, USER.md, AGENTS.md, TOOLS.md, HEARTBEAT.md, MEMORY.md) and directories.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path to the file to remove. Bare paths resolve under workspace/. Use `workspace/...` or `memory/...` for an explicit root."
                    }
                },
                "required": ["path"]
            }),
            category: ToolCategory::Other,
        },
        ToolDef {
            name: "exec",
            description: "Run an allowlisted host command. The command string is parsed into argv and executed directly; shell features like pipes, redirects, command substitution, and `;` chaining are not supported. Use this for search, git, and build/test commands when a file tool is awkward.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Shell command to execute."
                    },
                    "workdir": {
                        "type": "string",
                        "description": "Working directory for the command (relative to workspace root). Optional."
                    }
                },
                "required": ["command"]
            }),
            category: ToolCategory::Other,
        },
    ]
}

pub(super) fn description_for_memory_access(
    name: &str,
    memory_namespace_available: bool,
) -> Option<&'static str> {
    if memory_namespace_available {
        return None;
    }

    match name {
        "read" => Some("Read the contents of a file in your workspace. Paths without a prefix are resolved under workspace/. Returns the file content as text; use offset and limit for large files."),
        "write" => Some("Write or overwrite a file in your workspace. Bare paths resolve under workspace/. Parent directories are created automatically. Overwrites without confirmation."),
        "edit" => Some("Edit an existing workspace file by replacing specific text. Bare paths resolve under workspace/. Each replacement must match the old_string exactly, including whitespace and newlines."),
        "list_files" => Some("List files and directories under a path in your workspace. Bare paths resolve under workspace/. Returns each entry's name, type, and size."),
        "search" => Some("Search text files across your workspace. Bare paths resolve under workspace/. Returns matching file paths, line numbers, and excerpts. Use this for discovery before reading a full file."),
        "delete" => Some("Move a file in your workspace to a trash folder. Bare paths resolve under workspace/. The file is moved out of your workspace into a timestamped trash folder, not permanently erased. Refuses prompt-visible files (SOUL.md, USER.md, AGENTS.md, TOOLS.md, HEARTBEAT.md, MEMORY.md) and directories."),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Path resolution
// ---------------------------------------------------------------------------

fn resolve_roots(workspace_dir: &str, relative: &str) -> Result<(PathBuf, String), ToolError> {
    if workspace_dir.is_empty() {
        return Err(ToolError::InvalidArgs("workspace not configured".into()));
    }

    let relative = relative.trim();
    if relative.is_empty() {
        return Err(ToolError::InvalidArgs("path is empty".into()));
    }

    let workspace_root = PathBuf::from(workspace_dir);

    let (root, stripped) = if relative == "workspace" {
        (workspace_root, String::new())
    } else if let Some(rest) = relative.strip_prefix("workspace/") {
        (workspace_root, rest.to_string())
    } else if relative == "memory" {
        (workspace_root.join("memory"), String::new())
    } else if let Some(rest) = relative.strip_prefix("memory/") {
        (workspace_root.join("memory"), rest.to_string())
    } else {
        (workspace_root, relative.to_string())
    };

    Ok((root, stripped))
}

fn resolve_path(workspace_dir: &str, relative: &str) -> Result<PathBuf, ToolError> {
    let (base, stripped) = resolve_roots(workspace_dir, relative)?;
    if stripped.is_empty() {
        return Err(ToolError::InvalidArgs("path is empty".into()));
    }

    for component in Path::new(&stripped).components() {
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

    let resolved = base.join(&stripped);

    if let Ok(canonical_base) = base.canonicalize() {
        if let Ok(canonical) = resolved.canonicalize() {
            if !canonical.starts_with(&canonical_base) {
                return Err(ToolError::InvalidArgs(
                    "resolved path escapes workspace".into(),
                ));
            }
        } else {
            let mut ancestor = resolved.as_path();
            while let Some(parent) = ancestor.parent() {
                if let Ok(canonical_parent) = parent.canonicalize() {
                    if !canonical_parent.starts_with(&canonical_base) {
                        return Err(ToolError::InvalidArgs(
                            "resolved path escapes workspace".into(),
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

fn resolve_list_path(workspace_dir: &str, relative: Option<&str>) -> Result<PathBuf, ToolError> {
    if workspace_dir.is_empty() {
        return Err(ToolError::InvalidArgs("workspace not configured".into()));
    }

    match relative {
        None | Some("") | Some(".") => Ok(PathBuf::from(workspace_dir)),
        Some(rel) => {
            let (base, stripped) = resolve_roots(workspace_dir, rel)?;
            if stripped.is_empty() {
                Ok(base)
            } else {
                resolve_path(workspace_dir, rel)
            }
        }
    }
}

fn truncate_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

const SEARCH_DEFAULT_MAX_RESULTS: usize = 20;
const SEARCH_MAX_RESULTS: usize = 100;
const SEARCH_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const SEARCH_EXCERPT_CHARS: usize = 1_200;

fn normalize_search_query(input: &Value) -> Result<String, ToolError> {
    let query = input
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing required field: query".into()))?
        .trim();

    if query.is_empty() {
        return Err(ToolError::InvalidArgs("query must not be empty".into()));
    }

    Ok(query.to_string())
}

fn search_result_limit(input: &Value) -> usize {
    input
        .get("max_results")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(SEARCH_DEFAULT_MAX_RESULTS)
        .clamp(1, SEARCH_MAX_RESULTS)
}

fn display_path_for(workspace_dir: &str, path: &Path) -> String {
    let workspace_root = Path::new(workspace_dir);
    if let Ok(rel) = path.strip_prefix(workspace_root) {
        let normalized = rel.to_string_lossy().replace('\\', "/");
        return normalized;
    }
    path.to_string_lossy().replace('\\', "/")
}

fn is_root_memory_dir(workspace_dir: &str, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(Path::new(workspace_dir)) else {
        return false;
    };
    let mut components = rel.components();
    matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(first)), None)
            if first == std::ffi::OsStr::new("memory")
    )
}

fn find_case_insensitive_match(line: &str, query_lower: &str) -> Option<(usize, usize)> {
    let folded = line.to_lowercase();
    let folded_start = folded.find(query_lower)?;
    let folded_end = folded_start + query_lower.len();

    let mut folded_pos = 0usize;
    let mut original_start = None;
    let mut original_end = None;

    for (original_idx, ch) in line.char_indices() {
        let original_next = original_idx + ch.len_utf8();
        let char_folded_start = folded_pos;
        for folded_ch in ch.to_lowercase() {
            folded_pos += folded_ch.len_utf8();
        }
        let char_folded_end = folded_pos;

        if char_folded_end > folded_start && char_folded_start < folded_end {
            original_start.get_or_insert(original_idx);
            original_end = Some(original_next);
            if char_folded_end >= folded_end {
                break;
            }
        }
    }

    Some((
        original_start.unwrap_or(0),
        original_end.unwrap_or(line.len()),
    ))
}

fn byte_index_before_chars(text: &str, end: usize, count: usize) -> usize {
    let mut start = end;
    for _ in 0..count {
        let Some((idx, _)) = text[..start].char_indices().next_back() else {
            return 0;
        };
        start = idx;
    }
    start
}

fn byte_index_after_chars(text: &str, start: usize, count: usize) -> usize {
    let mut end = start;
    for _ in 0..count {
        let Some((offset, ch)) = text[end..].char_indices().next() else {
            return text.len();
        };
        end += offset + ch.len_utf8();
    }
    end
}

fn excerpt_line(line: &str, match_start: usize, match_end: usize) -> String {
    let trimmed_start = line.trim_start();
    let leading_trimmed_bytes = line.len() - trimmed_start.len();
    let trimmed = trimmed_start.trim_end();

    let match_start = match_start
        .saturating_sub(leading_trimmed_bytes)
        .min(trimmed.len());
    let match_end = match_end
        .saturating_sub(leading_trimmed_bytes)
        .min(trimmed.len())
        .max(match_start);

    let match_chars = trimmed[match_start..match_end].chars().count();
    let available_before = trimmed[..match_start].chars().count();
    let available_after = trimmed[match_end..].chars().count();
    let context_chars = SEARCH_EXCERPT_CHARS.saturating_sub(match_chars);
    let mut before_chars = (context_chars / 2).min(available_before);
    let mut after_chars = (context_chars - before_chars).min(available_after);

    let unused_after = context_chars.saturating_sub(before_chars + after_chars);
    if unused_after > 0 {
        let extra_before = (available_before - before_chars).min(unused_after);
        before_chars += extra_before;
    }

    let unused_before = context_chars.saturating_sub(before_chars + after_chars);
    if unused_before > 0 {
        let extra_after = (available_after - after_chars).min(unused_before);
        after_chars += extra_after;
    }

    let excerpt_start = byte_index_before_chars(trimmed, match_start, before_chars);
    let excerpt_end = byte_index_after_chars(trimmed, match_end, after_chars);

    let mut excerpt = String::new();
    if excerpt_start > 0 {
        excerpt.push_str("...");
    }
    excerpt.push_str(&trimmed[excerpt_start..excerpt_end]);
    if excerpt_end < trimmed.len() {
        excerpt.push_str("...");
    }
    excerpt
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub async fn handle_read(input: Value, workspace_dir: &str) -> Result<Value, ToolError> {
    let path_str = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing required field: path".into()))?;

    let path = resolve_path(workspace_dir, path_str)?;

    if !path.exists() {
        return Err(ToolError::Io(format!("file not found: {path_str}")));
    }
    if !path.is_file() {
        return Err(ToolError::InvalidArgs(format!("{path_str} is not a file")));
    }

    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| ToolError::Io(e.to_string()))?;

    let lines: Vec<&str> = content.split('\n').collect();
    let total_lines = lines.len();

    let offset = input
        .get("offset")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(1)
        .saturating_sub(1)
        .min(total_lines);

    let limit = input
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(total_lines);

    let end = (offset + limit).min(total_lines);
    let selected: Vec<&str> = lines
        .iter()
        .skip(offset)
        .take(end - offset)
        .copied()
        .collect();
    let result_text = selected.join("\n");

    let mut result = json!({
        "path": path_str,
        "content": result_text,
        "total_lines": total_lines,
    });

    if offset > 0 || end < total_lines {
        result["offset"] = json!(offset + 1);
        result["returned_lines"] = json!(end - offset);
        if end < total_lines {
            result["note"] = json!(format!(
                "Showing lines {}–{} of {}. Use offset={} to continue.",
                offset + 1,
                end,
                total_lines,
                end + 1
            ));
        }
    }

    Ok(result)
}

pub async fn handle_write(input: Value, workspace_dir: &str) -> Result<Value, ToolError> {
    let path_str = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing required field: path".into()))?;
    let content = input
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing required field: content".into()))?;

    let path = resolve_path(workspace_dir, path_str)?;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| ToolError::Io(e.to_string()))?;
    }

    tokio::fs::write(&path, content)
        .await
        .map_err(|e| ToolError::Io(e.to_string()))?;

    Ok(json!({
        "path": path_str,
        "bytes_written": content.len(),
    }))
}

pub async fn handle_edit(input: Value, workspace_dir: &str) -> Result<Value, ToolError> {
    let path_str = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing required field: path".into()))?;

    let edits = input
        .get("edits")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or_else(|| ToolError::InvalidArgs("missing or empty 'edits' array".into()))?;

    let path = resolve_path(workspace_dir, path_str)?;

    if !path.exists() {
        return Err(ToolError::Io(format!("file not found: {path_str}")));
    }

    let mut content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| ToolError::Io(e.to_string()))?;

    let mut replacements_made = 0usize;

    for edit in edits {
        let old_str = edit
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("each edit must have 'old_string'".into()))?;
        let new_str = edit
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("each edit must have 'new_string'".into()))?;

        if old_str.is_empty() {
            return Err(ToolError::InvalidArgs(
                "old_string must not be empty".into(),
            ));
        }

        if !content.contains(old_str) {
            let snippet_limit = 800;
            let content_chars = content.chars().count();
            let snippet = if content_chars <= snippet_limit {
                content.clone()
            } else {
                format!(
                    "{}\n... (truncated)",
                    truncate_chars(&content, snippet_limit)
                )
            };
            return Err(ToolError::InvalidArgs(format!(
                "Could not find the exact text in {path_str}.\nCurrent file contents:\n{snippet}"
            )));
        }

        // Replace ALL occurrences
        let count = content.matches(old_str).count();
        content = content.replace(old_str, new_str);
        replacements_made += count;
    }

    tokio::fs::write(&path, content)
        .await
        .map_err(|e| ToolError::Io(e.to_string()))?;

    Ok(json!({
        "path": path_str,
        "replacements_made": replacements_made,
    }))
}

pub async fn handle_list_files(input: Value, workspace_dir: &str) -> Result<Value, ToolError> {
    let path_str = input.get("path").and_then(|v| v.as_str());
    let dir = resolve_list_path(workspace_dir, path_str)?;

    if !dir.exists() {
        return Ok(json!({ "entries": [], "note": "directory does not exist yet" }));
    }

    if !dir.is_dir() {
        return Err(ToolError::InvalidArgs(format!(
            "{} is not a directory",
            path_str.unwrap_or(".")
        )));
    }

    let mut entries = Vec::new();
    let mut read_dir = tokio::fs::read_dir(&dir)
        .await
        .map_err(|e| ToolError::Io(e.to_string()))?;

    while let Some(entry) = read_dir
        .next_entry()
        .await
        .map_err(|e| ToolError::Io(e.to_string()))?
    {
        let meta = entry
            .metadata()
            .await
            .map_err(|e| ToolError::Io(e.to_string()))?;
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

pub async fn handle_search(
    input: Value,
    workspace_dir: &str,
    include_memory: bool,
) -> Result<Value, ToolError> {
    if workspace_dir.is_empty() {
        return Err(ToolError::InvalidArgs("workspace not configured".into()));
    }

    let query = normalize_search_query(&input)?;
    let query_lower = query.to_lowercase();
    let max_results = search_result_limit(&input);
    let path_str = input.get("path").and_then(|v| v.as_str());
    let root = resolve_list_path(workspace_dir, path_str)?;

    if !root.exists() {
        return Ok(json!({
            "query": query,
            "results": [],
            "count": 0,
            "note": "path does not exist"
        }));
    }

    let mut pending = vec![root];
    let mut results = Vec::new();
    let mut searched_files = 0usize;
    let mut skipped_binary_or_large = 0usize;

    while let Some(path) = pending.pop() {
        if results.len() >= max_results {
            break;
        }

        let meta = match tokio::fs::metadata(&path).await {
            Ok(meta) => meta,
            Err(_) => continue,
        };

        if meta.is_dir() {
            if !include_memory && is_root_memory_dir(workspace_dir, &path) {
                continue;
            }

            let mut entries = Vec::new();
            let mut read_dir = match tokio::fs::read_dir(&path).await {
                Ok(read_dir) => read_dir,
                Err(_) => continue,
            };
            while let Ok(Some(entry)) = read_dir.next_entry().await {
                entries.push(entry.path());
            }
            entries.sort_by(|a, b| b.cmp(a));
            pending.extend(entries);
            continue;
        }

        if !meta.is_file() {
            continue;
        }

        if meta.len() > SEARCH_MAX_FILE_BYTES {
            skipped_binary_or_large += 1;
            continue;
        }

        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let Ok(content) = String::from_utf8(bytes) else {
            skipped_binary_or_large += 1;
            continue;
        };

        searched_files += 1;
        for (line_idx, line) in content.lines().enumerate() {
            let Some((match_start, match_end)) = find_case_insensitive_match(line, &query_lower)
            else {
                continue;
            };
            results.push(json!({
                "path": display_path_for(workspace_dir, &path),
                "line": line_idx + 1,
                "excerpt": excerpt_line(line, match_start, match_end),
            }));
            if results.len() >= max_results {
                break;
            }
        }
    }

    let count = results.len();
    Ok(json!({
        "query": query,
        "results": results,
        "count": count,
        "searched_files": searched_files,
        "skipped_binary_or_large": skipped_binary_or_large,
    }))
}

pub async fn handle_delete(
    input: Value,
    workspace_dir: &str,
    character_data_dir: &str,
) -> Result<Value, ToolError> {
    let path_str = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing required field: path".into()))?;

    if crate::memory::deferred_edits::is_prompt_visible_path(path_str) {
        return Err(ToolError::InvalidArgs(format!(
            "{path_str} is a prompt-visible file and cannot be deleted"
        )));
    }

    if character_data_dir.is_empty() {
        return Err(ToolError::InvalidArgs(
            "character data directory not configured".into(),
        ));
    }

    let path = resolve_path(workspace_dir, path_str)?;

    if !path.exists() {
        return Err(ToolError::Io(format!("file not found: {path_str}")));
    }
    if !path.is_file() {
        return Err(ToolError::InvalidArgs(format!(
            "{path_str} is not a file (delete only operates on regular files)"
        )));
    }

    let workspace_root = PathBuf::from(workspace_dir);
    let relative_under_workspace = path
        .strip_prefix(&workspace_root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| PathBuf::from(path.file_name().unwrap_or_default()));

    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ").to_string();
    let trash_root = PathBuf::from(character_data_dir).join("trash").join(&stamp);
    let trash_target = trash_root.join(&relative_under_workspace);

    if let Some(parent) = trash_target.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| ToolError::Io(format!("could not create trash directory: {e}")))?;
    }

    if let Err(rename_err) = tokio::fs::rename(&path, &trash_target).await {
        // Cross-device rename can fail with EXDEV. Fall back to copy + remove.
        tokio::fs::copy(&path, &trash_target).await.map_err(|e| {
            ToolError::Io(format!(
                "could not move file to trash (rename: {rename_err}, copy fallback: {e})"
            ))
        })?;
        tokio::fs::remove_file(&path)
            .await
            .map_err(|e| ToolError::Io(format!("could not remove original after copy: {e}")))?;
    }

    let trashed_display = trash_target
        .strip_prefix(
            PathBuf::from(character_data_dir)
                .parent()
                .unwrap_or(Path::new("")),
        )
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| trash_target.to_string_lossy().replace('\\', "/"));

    Ok(json!({
        "path": path_str,
        "deleted": true,
        "trashed_to": trashed_display,
    }))
}

// ---------------------------------------------------------------------------
// Exec allowlist
// ---------------------------------------------------------------------------

/// Default allowed commands for the exec tool.
static DEFAULT_ALLOWLIST: &[&str] = &[
    "ls",
    "cat",
    "rg",
    "git",
    "wc",
    "pwd",
    "sort",
    "uniq",
    "dirname",
    "basename",
    "file",
    "stat",
    "du",
    "df",
    "which",
    "whoami",
    "date",
    "tree",
    "fd",
    "cargo",
    "rustc",
    "rustfmt",
    "clippy",
    "rust-analyzer",
    "npm",
    "pnpm",
    "yarn",
    "make",
    "cmake",
];

fn parse_command(command: &str) -> Result<Vec<String>, ToolError> {
    let argv = shell_words::split(command)
        .map_err(|e| ToolError::InvalidArgs(format!("invalid command line: {e}")))?;
    if argv.is_empty() {
        return Err(ToolError::InvalidArgs("command is empty".into()));
    }
    Ok(argv)
}

fn is_command_allowed(argv: &[String]) -> bool {
    let Some(first_token) = argv.first() else {
        return false;
    };

    if first_token.contains('/') || first_token.contains('\\') {
        return false;
    }

    let cmd_name = first_token.as_str();

    DEFAULT_ALLOWLIST.contains(&cmd_name)
}

fn is_path_like_arg(arg: &str) -> bool {
    if arg.is_empty() || arg == "-" || arg == "--" {
        return false;
    }

    arg.starts_with('/')
        || arg.starts_with('\\')
        || arg.starts_with("./")
        || arg.starts_with("../")
        || arg.starts_with("~/")
        || arg.starts_with("~\\")
        || arg == "."
        || arg == ".."
        || arg.contains('/')
        || arg.contains('\\')
        || arg.starts_with("file:")
        || matches!(
            Path::new(arg).components().next(),
            Some(std::path::Component::Prefix(_))
        )
}

fn validate_exec_path_arg(workspace_dir: &str, arg: &str) -> Result<(), ToolError> {
    let path = Path::new(arg);
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                return Err(ToolError::InvalidArgs(format!(
                    "exec argument escapes workspace: {arg}"
                )));
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(ToolError::InvalidArgs(format!(
                    "exec argument uses an absolute path: {arg}"
                )));
            }
            _ => {}
        }
    }

    let resolved = resolve_path(workspace_dir, arg)?;
    let workspace_root = PathBuf::from(workspace_dir)
        .canonicalize()
        .map_err(|e| ToolError::Io(format!("workspace unavailable: {e}")))?;

    if let Ok(canonical) = resolved.canonicalize() {
        if !canonical.starts_with(&workspace_root) {
            return Err(ToolError::InvalidArgs(format!(
                "exec argument escapes workspace: {arg}"
            )));
        }
        return Ok(());
    }

    let mut ancestor = resolved.as_path();
    while let Some(parent) = ancestor.parent() {
        if let Ok(canonical_parent) = parent.canonicalize() {
            if !canonical_parent.starts_with(&workspace_root) {
                return Err(ToolError::InvalidArgs(format!(
                    "exec argument escapes workspace: {arg}"
                )));
            }
            return Ok(());
        }
        ancestor = parent;
    }

    Ok(())
}

fn validate_exec_args(workspace_dir: &str, argv: &[String]) -> Result<(), ToolError> {
    if workspace_dir.is_empty() {
        return Err(ToolError::InvalidArgs("workspace not configured".into()));
    }

    for arg in argv.iter().skip(1) {
        if arg.starts_with("file:") {
            return Err(ToolError::InvalidArgs(format!(
                "exec argument uses a file URL: {arg}"
            )));
        }

        if let Some((_, value)) = arg.split_once('=') {
            if is_path_like_arg(value) {
                validate_exec_path_arg(workspace_dir, value)?;
            }
        }

        if is_path_like_arg(arg) {
            validate_exec_path_arg(workspace_dir, arg)?;
        }
    }

    Ok(())
}

pub async fn handle_exec(input: Value, workspace_dir: &str) -> Result<Value, ToolError> {
    let command = input
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing required field: command".into()))?;

    let argv = parse_command(command)?;

    if !is_command_allowed(&argv) {
        return Err(ToolError::InvalidArgs(format!(
            "command '{}' is not in the allowlist",
            argv[0]
        )));
    }

    validate_exec_args(workspace_dir, &argv)?;

    let workdir = input
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(|w| resolve_path(workspace_dir, w))
        .transpose()?;

    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);

    if let Some(dir) = workdir {
        cmd.current_dir(dir);
    } else if !workspace_dir.is_empty() {
        cmd.current_dir(workspace_dir);
    }

    let output = cmd
        .output()
        .await
        .map_err(|e| ToolError::Io(e.to_string()))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    Ok(json!({
        "command": command,
        "exit_code": output.status.code(),
        "stdout": stdout,
        "stderr": stderr,
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
        assert_eq!(tool_defs().len(), 7);
    }

    #[test]
    fn resolve_rejects_traversal() {
        assert!(resolve_path("/tmp/ws", "../etc/passwd").is_err());
        assert!(resolve_path("/tmp/ws", "foo/../../etc/passwd").is_err());
    }

    #[test]
    fn resolve_rejects_absolute() {
        assert!(resolve_path("/tmp/ws", "/etc/passwd").is_err());
    }

    #[test]
    fn resolve_normal_path() {
        let p = resolve_path("/tmp/ws", "notes/ideas.md").unwrap();
        assert_eq!(p, PathBuf::from("/tmp/ws/notes/ideas.md"));
    }

    #[tokio::test]
    async fn write_read_delete_cycle() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        // Write
        let result = handle_write(
            json!({"path": "test.txt", "content": "hello world"}),
            &ws_str,
        )
        .await
        .unwrap();
        assert_eq!(result["bytes_written"], 11);

        // Read
        let result = handle_read(json!({"path": "test.txt"}), &ws_str)
            .await
            .unwrap();
        assert_eq!(result["content"], "hello world");
        assert_eq!(result["total_lines"], 1);

        // List
        let result = handle_list_files(json!({}), &ws_str).await.unwrap();
        let entries = result["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["name"], "test.txt");
    }

    #[tokio::test]
    async fn edit_replaces_text() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        handle_write(
            json!({"path": "test.txt", "content": "hello world\nfoo bar\n"}),
            &ws_str,
        )
        .await
        .unwrap();

        let result = handle_edit(
            json!({
                "path": "test.txt",
                "edits": [
                    {"old_string": "hello world", "new_string": "goodbye world"}
                ]
            }),
            &ws_str,
        )
        .await
        .unwrap();
        assert_eq!(result["replacements_made"], 1);

        let result = handle_read(json!({"path": "test.txt"}), &ws_str)
            .await
            .unwrap();
        assert_eq!(result["content"], "goodbye world\nfoo bar\n");
    }

    #[tokio::test]
    async fn edit_multiple_replacements() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        handle_write(
            json!({"path": "test.txt", "content": "foo foo foo"}),
            &ws_str,
        )
        .await
        .unwrap();

        let result = handle_edit(
            json!({
                "path": "test.txt",
                "edits": [
                    {"old_string": "foo", "new_string": "bar"}
                ]
            }),
            &ws_str,
        )
        .await
        .unwrap();
        assert_eq!(result["replacements_made"], 3);

        let result = handle_read(json!({"path": "test.txt"}), &ws_str)
            .await
            .unwrap();
        assert_eq!(result["content"], "bar bar bar");
    }

    #[tokio::test]
    async fn edit_fails_on_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        handle_write(
            json!({"path": "test.txt", "content": "hello world"}),
            &ws_str,
        )
        .await
        .unwrap();

        let result = handle_edit(
            json!({
                "path": "test.txt",
                "edits": [
                    {"old_string": "nonexistent", "new_string": "replaced"}
                ]
            }),
            &ws_str,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn edit_mismatch_with_unicode_content_does_not_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        handle_write(
            json!({"path": "test.txt", "content": "🙂".repeat(900)}),
            &ws_str,
        )
        .await
        .unwrap();

        let result = handle_edit(
            json!({
                "path": "test.txt",
                "edits": [
                    {"old_string": "missing", "new_string": "replaced"}
                ]
            }),
            &ws_str,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn write_creates_subdirectories() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        handle_write(
            json!({"path": "deep/nested/file.txt", "content": "nested"}),
            &ws_str,
        )
        .await
        .unwrap();

        let result = handle_read(json!({"path": "deep/nested/file.txt"}), &ws_str)
            .await
            .unwrap();
        assert_eq!(result["content"], "nested");
    }

    #[tokio::test]
    async fn read_with_offset_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        let content = "line1\nline2\nline3\nline4\nline5";
        handle_write(json!({"path": "test.txt", "content": content}), &ws_str)
            .await
            .unwrap();

        let result = handle_read(
            json!({"path": "test.txt", "offset": 2, "limit": 2}),
            &ws_str,
        )
        .await
        .unwrap();
        assert_eq!(result["content"], "line2\nline3");
        assert_eq!(result["total_lines"], 5);
    }

    #[tokio::test]
    async fn write_and_read_memory_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        handle_write(
            json!({"path": "memory/people/ren.md", "content": "# Ren\n\nLikes tea."}),
            &ws_str,
        )
        .await
        .unwrap();

        let result = handle_read(json!({"path": "memory/people/ren.md"}), &ws_str)
            .await
            .unwrap();
        assert_eq!(result["content"], "# Ren\n\nLikes tea.");
        assert!(tmp.path().join("workspace/memory/people/ren.md").exists());
    }

    #[test]
    fn search_excerpt_centers_match_and_marks_clipping() {
        let line = format!(
            "{}German Shepherd{}",
            "before ".repeat(140),
            " after".repeat(140)
        );
        let (match_start, match_end) =
            find_case_insensitive_match(&line, "german shepherd").unwrap();

        let excerpt = excerpt_line(&line, match_start, match_end);

        assert!(excerpt.contains("German Shepherd"));
        assert!(excerpt.starts_with("..."));
        assert!(excerpt.ends_with("..."));
        assert!(excerpt.chars().count() <= SEARCH_EXCERPT_CHARS + 6);
    }

    #[test]
    fn search_excerpt_preserves_source_casing() {
        let line = "metadata: the Important Phrase appears here";
        let (match_start, match_end) =
            find_case_insensitive_match(line, "important phrase").unwrap();

        let excerpt = excerpt_line(line, match_start, match_end);

        assert!(excerpt.contains("Important Phrase"));
        assert!(!excerpt.contains("important phrase appears"));
    }

    #[tokio::test]
    async fn search_finds_workspace_and_memory_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        handle_write(
            json!({"path": "notes/ideas.md", "content": "Tea in the garden\nCoffee later"}),
            &ws_str,
        )
        .await
        .unwrap();
        handle_write(
            json!({"path": "memory/people/ren.md", "content": "Ren likes tea."}),
            &ws_str,
        )
        .await
        .unwrap();

        let result = handle_search(json!({"query": "tea"}), &ws_str, true)
            .await
            .unwrap();
        let results = result["results"].as_array().unwrap();
        let paths: Vec<&str> = results
            .iter()
            .map(|entry| entry["path"].as_str().unwrap())
            .collect();
        assert!(paths.contains(&"notes/ideas.md"));
        assert!(paths.contains(&"memory/people/ren.md"));
    }

    #[tokio::test]
    async fn search_excerpt_includes_match_in_long_single_line_file() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();
        let metadata = "metadata ".repeat(200);
        let line = format!(
            r#"{{"metadata":"{}","message":"spotted a German Shepherd near the gate"}}"#,
            metadata
        );

        handle_write(
            json!({"path": "archive/chat.jsonl", "content": line}),
            &ws_str,
        )
        .await
        .unwrap();

        let result = handle_search(json!({"query": "german shepherd"}), &ws_str, true)
            .await
            .unwrap();
        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["path"], "archive/chat.jsonl");
        assert_eq!(results[0]["line"], 1);
        let excerpt = results[0]["excerpt"].as_str().unwrap();
        assert!(excerpt.contains("German Shepherd"));
        assert!(excerpt.starts_with("..."));
        assert!(excerpt.chars().count() <= SEARCH_EXCERPT_CHARS + 6);
    }

    #[tokio::test]
    async fn search_skips_memory_when_memory_not_included() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        handle_write(
            json!({"path": "memory/people/ren.md", "content": "Ren likes tea."}),
            &ws_str,
        )
        .await
        .unwrap();

        let result = handle_search(json!({"query": "tea"}), &ws_str, false)
            .await
            .unwrap();
        assert_eq!(result["count"], 0);
    }

    #[test]
    fn exec_allowlist_basic() {
        assert!(is_command_allowed(&parse_command("ls -la").unwrap()));
        assert!(is_command_allowed(&parse_command("git status").unwrap()));
        assert!(is_command_allowed(&parse_command("rg pattern").unwrap()));
        assert!(!is_command_allowed(
            &parse_command("/usr/bin/git status").unwrap()
        ));
        assert!(!is_command_allowed(
            &parse_command("python3 -c 'print(1)'").unwrap()
        ));
        assert!(parse_command("").is_err());
        assert!(parse_command("  ").is_err());
    }

    #[tokio::test]
    async fn exec_runs_allowed_command() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        let ws_str = ws.to_string_lossy().to_string();

        let result = handle_exec(json!({"command": "pwd"}), &ws_str)
            .await
            .unwrap();
        let stdout = result["stdout"].as_str().unwrap();
        assert!(
            stdout.contains("workspace"),
            "expected workspace path in pwd output: {stdout}"
        );
        assert_eq!(result["exit_code"], 0);
    }

    #[tokio::test]
    async fn exec_rejects_disallowed() {
        let tmp = tempfile::tempdir().unwrap();
        let ws_str = tmp.path().to_string_lossy().to_string();

        let result = handle_exec(json!({"command": "rm -rf /"}), &ws_str).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn exec_rejects_absolute_path_argument() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        let ws_str = ws.to_string_lossy().to_string();

        let result = handle_exec(json!({"command": "cat /etc/passwd"}), &ws_str).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn exec_rejects_parent_path_argument() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        let ws_str = ws.to_string_lossy().to_string();

        let result = handle_exec(json!({"command": "rg tea ../"}), &ws_str).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn exec_rejects_absolute_workdir_argument() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        let ws_str = ws.to_string_lossy().to_string();

        let result = handle_exec(json!({"command": "git -C /tmp status"}), &ws_str).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn exec_rejects_equals_absolute_path_argument() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        let ws_str = ws.to_string_lossy().to_string();

        let result = handle_exec(
            json!({"command": "cargo --manifest-path=/tmp/Cargo.toml test"}),
            &ws_str,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn exec_allows_workspace_relative_path_arguments() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        tokio::fs::create_dir_all(ws.join("src")).await.unwrap();
        tokio::fs::write(ws.join("src/note.txt"), "tea")
            .await
            .unwrap();
        let ws_str = ws.to_string_lossy().to_string();

        let result = handle_exec(json!({"command": "cat src/note.txt"}), &ws_str)
            .await
            .unwrap();
        assert_eq!(result["stdout"], "tea");
    }

    #[tokio::test]
    async fn exec_rejects_shell_chaining() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        let ws_str = ws.to_string_lossy().to_string();

        let result = handle_exec(json!({"command": "pwd; pwd"}), &ws_str).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn exec_with_workdir() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        tokio::fs::create_dir_all(ws.join("subdir")).await.unwrap();

        let result = handle_exec(json!({"command": "pwd", "workdir": "subdir"}), &ws_str)
            .await
            .unwrap();
        let stdout = result["stdout"].as_str().unwrap();
        assert!(
            stdout.contains("subdir"),
            "expected subdir in pwd output: {stdout}"
        );
    }

    #[tokio::test]
    async fn delete_moves_file_to_trash() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();
        let data_dir = tmp.path().join("data");
        let data_str = data_dir.to_string_lossy().to_string();

        handle_write(json!({"path": "notes/old.md", "content": "stale"}), &ws_str)
            .await
            .unwrap();

        let result = handle_delete(json!({"path": "notes/old.md"}), &ws_str, &data_str)
            .await
            .unwrap();
        assert_eq!(result["deleted"], true);
        assert_eq!(result["path"], "notes/old.md");

        assert!(!ws.join("notes/old.md").exists(), "file should be moved");
        let trash = data_dir.join("trash");
        assert!(trash.exists(), "trash directory should exist");

        let mut found = None;
        for entry in std::fs::read_dir(&trash).unwrap() {
            let entry = entry.unwrap();
            let candidate = entry.path().join("notes/old.md");
            if candidate.exists() {
                found = Some(candidate);
                break;
            }
        }
        let trashed = found.expect("trashed file should exist under timestamped folder");
        assert_eq!(std::fs::read_to_string(trashed).unwrap(), "stale");
    }

    #[tokio::test]
    async fn delete_preserves_memory_namespace_under_trash() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();
        let data_dir = tmp.path().join("data");
        let data_str = data_dir.to_string_lossy().to_string();

        handle_write(
            json!({"path": "memory/people/ren.md", "content": "Ren"}),
            &ws_str,
        )
        .await
        .unwrap();

        handle_delete(json!({"path": "memory/people/ren.md"}), &ws_str, &data_str)
            .await
            .unwrap();

        assert!(!ws.join("memory/people/ren.md").exists());
        let trash = data_dir.join("trash");
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(&trash).unwrap() {
            let entry = entry.unwrap();
            paths.push(entry.path().join("memory/people/ren.md"));
        }
        assert!(
            paths.iter().any(|p| p.exists()),
            "memory file should land under trash/<ts>/memory/people/ren.md"
        );
    }

    #[tokio::test]
    async fn delete_rejects_protected_files() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();
        let data_dir = tmp.path().join("data");
        let data_str = data_dir.to_string_lossy().to_string();

        handle_write(json!({"path": "SOUL.md", "content": "soul"}), &ws_str)
            .await
            .unwrap();
        handle_write(json!({"path": "MEMORY.md", "content": "idx"}), &ws_str)
            .await
            .unwrap();

        for path in [
            "SOUL.md",
            "USER.md",
            "AGENTS.md",
            "TOOLS.md",
            "HEARTBEAT.md",
            "MEMORY.md",
            "workspace/SOUL.md",
        ] {
            let result = handle_delete(json!({"path": path}), &ws_str, &data_str).await;
            assert!(result.is_err(), "delete must refuse {path}");
        }

        assert!(ws.join("SOUL.md").exists(), "SOUL.md must remain in place");
    }

    #[tokio::test]
    async fn delete_rejects_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        let ws_str = ws.to_string_lossy().to_string();
        let data_str = tmp.path().join("data").to_string_lossy().to_string();

        let result = handle_delete(json!({"path": "ghost.md"}), &ws_str, &data_str).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn delete_rejects_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        tokio::fs::create_dir_all(ws.join("notes")).await.unwrap();
        let ws_str = ws.to_string_lossy().to_string();
        let data_str = tmp.path().join("data").to_string_lossy().to_string();

        let result = handle_delete(json!({"path": "notes"}), &ws_str, &data_str).await;
        assert!(result.is_err(), "delete must refuse directories");
        assert!(ws.join("notes").exists(), "directory must remain");
    }

    #[tokio::test]
    async fn delete_rejects_path_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        let ws_str = ws.to_string_lossy().to_string();
        let data_str = tmp.path().join("data").to_string_lossy().to_string();

        let result = handle_delete(json!({"path": "../escape.md"}), &ws_str, &data_str).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn delete_rejects_when_data_dir_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        handle_write(json!({"path": "foo.md", "content": "x"}), &ws_str)
            .await
            .unwrap();

        let result = handle_delete(json!({"path": "foo.md"}), &ws_str, "").await;
        assert!(result.is_err());
        assert!(ws.join("foo.md").exists(), "file should not be moved");
    }
}
