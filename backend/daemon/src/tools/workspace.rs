//! Workspace filesystem tools — read, write, edit, list, search, delete, exec.
//!
//! These tools give the assistant access to a real filesystem workspace
//! (`{character}/workspace/`), mirroring OpenClaw's model of agent-curated files.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{json, Value};
use shore_config::app::RetrievalConfig;
use shore_llm::embed::Embedder;

use crate::convert::u64_to_usize;
use crate::memory::workspace_index::{self, HybridMode};

use super::{ToolCategory, ToolDef, ToolError};

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

#[expect(
    clippy::too_many_lines,
    reason = "workspace tool schema literals are tracked for extraction in #109"
)]
pub fn tool_defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "read",
            description: crate::include_prompt!("../../prompts/tools/workspace/read.md"),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path within your workspace."
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
            description: crate::include_prompt!("../../prompts/tools/workspace/write.md"),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path within your workspace."
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
            description: crate::include_prompt!("../../prompts/tools/workspace/edit.md"),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path within your workspace."
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
            description: crate::include_prompt!("../../prompts/tools/workspace/list_files.md"),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative directory path within your workspace. Omit for workspace root."
                    }
                },
                "required": []
            }),
            category: ToolCategory::Other,
        },
        ToolDef {
            name: "search",
            description: crate::include_prompt!("../../prompts/tools/workspace/search.md"),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Keyword, phrase, or natural-language description to search for."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["hybrid", "lexical", "vector"],
                        "description": "Ranking mode. `hybrid` (default) blends semantic similarity with substring matching. `lexical` is case-insensitive substring only, ordered by file recency. `vector` is pure semantic similarity."
                    },
                    "path": {
                        "type": "string",
                        "description": "Optional relative path to scope the search to a subtree. Works in all modes — hybrid/vector queries are filtered to this subtree after ranking against the workspace-wide embedding index."
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
            description: crate::include_prompt!("../../prompts/tools/workspace/delete.md"),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path to the file to remove, within your workspace."
                    }
                },
                "required": ["path"]
            }),
            category: ToolCategory::Other,
        },
        ToolDef {
            name: "exec",
            description: crate::include_prompt!("../../prompts/tools/workspace/exec.md"),
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

// ---------------------------------------------------------------------------
// Path resolution
// ---------------------------------------------------------------------------

pub(crate) fn resolve_roots(
    workspace_dir: &str,
    relative_raw: &str,
) -> Result<(PathBuf, String), ToolError> {
    if workspace_dir.is_empty() {
        return Err(ToolError::InvalidArgs("workspace not configured".into()));
    }

    let relative = relative_raw.trim();
    if relative.is_empty() {
        return Err(ToolError::InvalidArgs("path is empty".into()));
    }

    let workspace_root = PathBuf::from(workspace_dir);

    let (root, stripped) = if relative == "workspace" {
        (workspace_root, String::new())
    } else if let Some(rest) = relative.strip_prefix("workspace/") {
        (workspace_root, rest.to_owned())
    } else if relative == "memory" {
        (workspace_root.join("memory"), String::new())
    } else if let Some(rest) = relative.strip_prefix("memory/") {
        (workspace_root.join("memory"), rest.to_owned())
    } else {
        (workspace_root, relative.to_owned())
    };

    Ok((root, stripped))
}

pub(crate) fn resolve_path(workspace_dir: &str, relative: &str) -> Result<PathBuf, ToolError> {
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
            std::path::Component::CurDir | std::path::Component::Normal(_) => {}
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
        None | Some("" | ".") => Ok(PathBuf::from(workspace_dir)),
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

    Ok(query.to_owned())
}

fn search_result_limit(input: &Value) -> usize {
    input
        .get("max_results")
        .and_then(Value::as_u64)
        .map_or(SEARCH_DEFAULT_MAX_RESULTS, u64_to_usize)
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

fn find_case_insensitive_match(line: &str, query_lower: &str) -> Option<(usize, usize)> {
    let folded = line.to_lowercase();
    let folded_start = folded.find(query_lower)?;
    let folded_end = folded_start.saturating_add(query_lower.len());

    let mut folded_pos = 0_usize;
    let mut original_start = None;
    let mut original_end = None;

    for (original_idx, ch) in line.char_indices() {
        let original_next = original_idx.saturating_add(ch.len_utf8());
        let char_folded_start = folded_pos;
        for folded_ch in ch.to_lowercase() {
            folded_pos = folded_pos.saturating_add(folded_ch.len_utf8());
        }
        let char_folded_end = folded_pos;

        if char_folded_end > folded_start && char_folded_start < folded_end {
            let _ignored = original_start.get_or_insert(original_idx);
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

#[expect(
    clippy::string_slice,
    reason = "`end` is a char-boundary byte offset (caller passes char_indices/len-derived positions), so `text[..end]` is valid"
)]
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

#[expect(
    clippy::string_slice,
    reason = "`start` is a char-boundary byte offset (caller passes char_indices/len-derived positions), so `text[start..]` is valid"
)]
fn byte_index_after_chars(text: &str, start: usize, count: usize) -> usize {
    let mut end = start;
    for _ in 0..count {
        let Some((offset, ch)) = text[end..].char_indices().next() else {
            return text.len();
        };
        end = end.saturating_add(offset).saturating_add(ch.len_utf8());
    }
    end
}

#[expect(
    clippy::string_slice,
    reason = "match offsets are clamped to `trimmed.len()` and excerpt bounds come from byte_index_*_chars(), so every slice bound lands on a char boundary"
)]
fn excerpt_line(line: &str, raw_match_start: usize, raw_match_end: usize) -> String {
    let trimmed_start = line.trim_start();
    let leading_trimmed_bytes = line.len().saturating_sub(trimmed_start.len());
    let trimmed = trimmed_start.trim_end();

    let match_start = raw_match_start
        .saturating_sub(leading_trimmed_bytes)
        .min(trimmed.len());
    let match_end = raw_match_end
        .saturating_sub(leading_trimmed_bytes)
        .min(trimmed.len())
        .max(match_start);

    let match_chars = trimmed[match_start..match_end].chars().count();
    let available_before = trimmed[..match_start].chars().count();
    let available_after = trimmed[match_end..].chars().count();
    let context_chars = SEARCH_EXCERPT_CHARS.saturating_sub(match_chars);
    let half_context_chars = context_chars.checked_div(2).unwrap_or_default();
    let mut before_chars = half_context_chars.min(available_before);
    let mut after_chars = context_chars
        .saturating_sub(before_chars)
        .min(available_after);

    let unused_after = context_chars.saturating_sub(before_chars.saturating_add(after_chars));
    if unused_after > 0 {
        let extra_before = available_before
            .saturating_sub(before_chars)
            .min(unused_after);
        before_chars = before_chars.saturating_add(extra_before);
    }

    let unused_before = context_chars.saturating_sub(before_chars.saturating_add(after_chars));
    if unused_before > 0 {
        let extra_after = available_after
            .saturating_sub(after_chars)
            .min(unused_before);
        after_chars = after_chars.saturating_add(extra_after);
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
        .and_then(Value::as_u64)
        .map_or(1, u64_to_usize)
        .saturating_sub(1)
        .min(total_lines);

    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(total_lines, u64_to_usize);

    let end = offset.saturating_add(limit).min(total_lines);
    let selected: Vec<&str> = lines
        .iter()
        .skip(offset)
        .take(end.saturating_sub(offset))
        .copied()
        .collect();
    let result_text = selected.join("\n");

    let mut result = json!({
        "path": path_str,
        "content": result_text,
        "total_lines": total_lines,
    });

    if offset > 0 || end < total_lines {
        if let Some(obj) = result.as_object_mut() {
            let _ignored = obj.insert("offset".into(), json!(offset.saturating_add(1)));
            _ = obj.insert("returned_lines".into(), json!(end.saturating_sub(offset)));
            if end < total_lines {
                _ = obj.insert(
                    "note".into(),
                    json!(format!(
                        "Showing lines {}–{} of {}. Use offset={} to continue.",
                        offset.saturating_add(1),
                        end,
                        total_lines,
                        end.saturating_add(1)
                    )),
                );
            }
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

    let mut replacements_made = 0_usize;

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
        replacements_made = replacements_made.saturating_add(count);
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

/// Top-level entry point: dispatches to lexical or hybrid based on the
/// caller-supplied `mode` and whether an embedder is wired up.
///
/// `embedder` and `workspace_index_path` are optional so tests (and
/// embedder-less production setups) can call this without a configured
/// embedding profile — they fall through to the lexical path.
pub async fn handle_search(
    input: Value,
    workspace_dir: &str,
    retrieval_config_opt: Option<&RetrievalConfig>,
    embedder: Option<&dyn Embedder>,
    workspace_index_path: Option<&Path>,
) -> Result<Value, ToolError> {
    let retrieval_config = retrieval_config_opt.cloned().unwrap_or_default();
    let requested_mode = parse_search_mode(input.get("mode"))?;
    let requested_hybrid = !matches!(requested_mode, RequestedMode::Lexical);
    let path_str = input.get("path").and_then(|v| v.as_str());

    if let (true, Some(embedder_ref), Some(index_path)) =
        (requested_hybrid, embedder, workspace_index_path)
    {
        let mode = match requested_mode {
            RequestedMode::Vector => HybridMode::Vector,
            RequestedMode::Hybrid | RequestedMode::Lexical => HybridMode::Hybrid,
        };
        let scope = match path_str {
            Some(raw) if !raw.is_empty() && raw != "." => {
                Some(scope_prefix_for(workspace_dir, raw)?)
            }
            Some(_) | None => None,
        };
        return handle_search_hybrid(
            input,
            workspace_dir,
            &retrieval_config,
            mode,
            embedder_ref,
            index_path,
            scope.as_deref(),
        )
        .await;
    }

    let mut response = handle_search_lexical(input, workspace_dir, &retrieval_config).await?;
    if let Some(obj) = response.as_object_mut() {
        let _ignored = obj.insert("mode".into(), json!("lexical"));
        if requested_hybrid {
            _ = obj.insert(
                "semantic_unavailable".into(),
                json!("embedder not configured"),
            );
        }
    }
    Ok(response)
}

/// Returns the workspace-relative display prefix used to scope a path-filtered
/// hybrid search (e.g. `"workspace/notes"` → `"notes"`, `"memory/x"` →
/// `"memory/x"`). The empty string means "no scope" and is filtered out by
/// `hybrid_search`.
fn scope_prefix_for(workspace_dir: &str, raw: &str) -> Result<String, ToolError> {
    let resolved = resolve_list_path(workspace_dir, Some(raw))?;
    Ok(display_path_for(workspace_dir, &resolved))
}

#[derive(Debug, Clone, Copy)]
enum RequestedMode {
    Hybrid,
    Lexical,
    Vector,
}

fn parse_search_mode(raw: Option<&Value>) -> Result<RequestedMode, ToolError> {
    let Some(v) = raw else {
        return Ok(RequestedMode::Hybrid);
    };
    match v.as_str() {
        None => Err(ToolError::InvalidArgs(
            "search `mode` must be a string".into(),
        )),
        Some("hybrid") => Ok(RequestedMode::Hybrid),
        Some("lexical") => Ok(RequestedMode::Lexical),
        Some("vector") => Ok(RequestedMode::Vector),
        Some(other) => Err(ToolError::InvalidArgs(format!(
            "unknown search mode '{other}'; expected hybrid, lexical, or vector"
        ))),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "lexical workspace search walk/rank/render split is tracked in #109"
)]
async fn handle_search_lexical(
    input: Value,
    workspace_dir: &str,
    retrieval_config: &RetrievalConfig,
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

    // ── Phase 1: enumerate candidate files and capture mtimes. ────────
    //
    // We collect every searchable file up front so results can be ranked by
    // recency rather than by directory traversal order. Symlinks are still
    // skipped here — resolve_path canonicalizes for direct read/delete, but
    // descendants discovered by walking are joined onto the workspace root
    // without re-checking containment, so a symlink pointing outside (e.g.
    // → /etc/passwd) would otherwise be read like any regular file.
    let mut pending = vec![root];
    let mut candidates: Vec<(PathBuf, SystemTime)> = Vec::new();
    let mut skipped_binary_or_large = 0_usize;

    while let Some(path) = pending.pop() {
        let Ok(meta) = tokio::fs::symlink_metadata(&path).await else {
            continue;
        };

        if meta.file_type().is_symlink() {
            continue;
        }

        if meta.is_dir() {
            let mut entries = Vec::new();
            let Ok(mut read_dir) = tokio::fs::read_dir(&path).await else {
                continue;
            };
            while let Ok(Some(entry)) = read_dir.next_entry().await {
                entries.push(entry.path());
            }
            pending.extend(entries);
            continue;
        }

        if !meta.is_file() {
            continue;
        }

        if meta.len() > retrieval_config.max_file_bytes {
            skipped_binary_or_large = skipped_binary_or_large.saturating_add(1);
            continue;
        }

        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        candidates.push((path, mtime));
    }

    // Newest first; fall back to path order so output is stable when mtimes
    // tie (e.g. tests that write files in quick succession).
    candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut results = Vec::new();
    let mut files_summary: Vec<Value> = Vec::new();
    let mut searched_files = 0_usize;

    for (path, _) in candidates {
        let Ok(bytes) = tokio::fs::read(&path).await else {
            continue;
        };
        let Ok(content) = String::from_utf8(bytes) else {
            skipped_binary_or_large = skipped_binary_or_large.saturating_add(1);
            continue;
        };

        searched_files = searched_files.saturating_add(1);
        let display = display_path_for(workspace_dir, &path);
        let mut file_hits = 0_usize;
        for (line_idx, line) in content.lines().enumerate() {
            let Some((match_start, match_end)) = find_case_insensitive_match(line, &query_lower)
            else {
                continue;
            };
            results.push(json!({
                "path": display.clone(),
                "line": line_idx.saturating_add(1),
                "excerpt": excerpt_line(line, match_start, match_end),
            }));
            file_hits = file_hits.saturating_add(1);
            if results.len() >= max_results {
                break;
            }
        }
        if file_hits > 0 {
            files_summary.push(json!({"path": display, "hits": file_hits}));
        }
        if results.len() >= max_results {
            break;
        }
    }

    let count = results.len();
    let mut response = json!({
        "query": query,
        "results": results,
        "count": count,
        "searched_files": searched_files,
        "skipped_binary_or_large": skipped_binary_or_large,
    });

    if count > 0 {
        if let Some(obj) = response.as_object_mut() {
            let _ignored = obj.insert("files".into(), json!(files_summary));
            _ = obj.insert(
                "note".into(),
                json!(
                    "These are line-level excerpts, ordered by file recency. \
                     Call `read` on the top file paths to see surrounding context — \
                     excerpts almost never contain the full answer, and one file \
                     often references others worth reading too."
                ),
            );
        }
    }

    Ok(response)
}

async fn handle_search_hybrid(
    input: Value,
    workspace_dir: &str,
    retrieval_config: &RetrievalConfig,
    mode: HybridMode,
    embedder: &dyn Embedder,
    index_path: &Path,
    path_filter: Option<&str>,
) -> Result<Value, ToolError> {
    let query = normalize_search_query(&input)?;
    let max_results = search_result_limit(&input);

    let result = match workspace_index::hybrid_search(
        workspace_dir,
        retrieval_config,
        &query,
        mode,
        embedder,
        index_path,
        path_filter,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "hybrid search failed; falling back to lexical");
            let mut lex = handle_search_lexical(input, workspace_dir, retrieval_config).await?;
            if let Some(obj) = lex.as_object_mut() {
                let _ignored = obj.insert("mode".into(), json!("lexical"));
                _ = obj.insert("semantic_unavailable".into(), json!(e.to_string()));
            }
            return Ok(lex);
        }
    };

    let q_lower = query.to_lowercase();
    let mut results: Vec<Value> = Vec::new();

    for f in result.files.iter().take(max_results) {
        let content = f.content.as_deref().unwrap_or("");
        let (line_no, excerpt) = best_line_excerpt(content, &q_lower);
        results.push(json!({
            "path": f.display_path,
            "line": line_no,
            "excerpt": excerpt,
            "lexical_score": f.lexical_score,
            "semantic_score": f.semantic_score,
            "combined_score": f.combined_score,
        }));
    }

    let count = results.len();
    let mode_label = match mode {
        HybridMode::Hybrid => "hybrid",
        HybridMode::Vector => "vector",
    };
    let mut response = json!({
        "query": query,
        "mode": mode_label,
        "results": results,
        "count": count,
        "searched_files": result.searched_files,
        "embedded_files": result.embedded_files,
        "skipped_binary_or_large": result.skipped_binary_or_large,
    });

    if count > 0 {
        if let Some(obj) = response.as_object_mut() {
            let _ignored = obj.insert(
                "note".into(),
                json!(
                    "Files ranked by combined semantic + lexical score. Excerpts are \
                     best-effort line-level snippets; call `read` on the top paths for \
                     full context — one file often references others worth reading too."
                ),
            );
        }
    }

    Ok(response)
}

fn best_line_excerpt(content: &str, q_lower: &str) -> (usize, String) {
    for (i, line) in content.lines().enumerate() {
        if let Some((s, e)) = find_case_insensitive_match(line, q_lower) {
            return (i.saturating_add(1), excerpt_line(line, s, e));
        }
    }

    let terms = search_excerpt_terms(q_lower);
    if !terms.is_empty() {
        let frequencies = term_line_frequencies(content, &terms);
        if let Some((line_no, line, term)) =
            best_term_matched_line(content, &terms, &frequencies, false)
                .or_else(|| best_term_matched_line(content, &terms, &frequencies, true))
        {
            if let Some((s, e)) = find_case_insensitive_match(line, term) {
                return (line_no, excerpt_line(line, s, e));
            }
            return (line_no, truncate_excerpt_line(line.trim()));
        }
    }

    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            return (i.saturating_add(1), truncate_excerpt_line(trimmed));
        }
    }
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return (i.saturating_add(1), truncate_excerpt_line(trimmed));
        }
    }
    (1, String::new())
}

fn search_excerpt_terms(query_lower: &str) -> Vec<String> {
    query_lower
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|t| t.len() >= 2)
        .map(str::to_owned)
        .collect()
}

fn term_line_frequencies(content: &str, terms: &[String]) -> Vec<usize> {
    terms
        .iter()
        .map(|term| {
            content
                .lines()
                .filter(|line| line.to_lowercase().contains(term))
                .count()
                .max(1)
        })
        .collect()
}

fn best_term_matched_line<'val>(
    content: &'val str,
    terms: &'val [String],
    frequencies: &[usize],
    allow_heading: bool,
) -> Option<(usize, &'val str, &'val str)> {
    content
        .lines()
        .enumerate()
        .filter_map(|(line_idx, line)| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            if !allow_heading && trimmed.starts_with('#') {
                return None;
            }

            let lower = trimmed.to_lowercase();
            let mut best_term: Option<(&str, usize)> = None;
            let mut line_score = 0_usize;
            for (idx, term) in terms.iter().enumerate() {
                if lower.contains(term) {
                    let denom = frequencies.get(idx).copied().unwrap_or(1).max(1);
                    let term_score = 100_usize
                        .checked_div(denom)
                        .unwrap_or(0)
                        .saturating_add(term.len());
                    line_score = line_score.saturating_add(term_score);
                    let replace_best = match best_term {
                        Some((_, score)) => term_score > score,
                        None => true,
                    };
                    if replace_best {
                        best_term = Some((term.as_str(), term_score));
                    }
                }
            }

            best_term.map(|(term, _)| (line_idx.saturating_add(1), line, term, line_score))
        })
        .max_by(|a, b| a.3.cmp(&b.3).then_with(|| b.0.cmp(&a.0)))
        .map(|(line_no, line, term, _)| (line_no, line, term))
}

fn truncate_excerpt_line(line: &str) -> String {
    let count = line.chars().count();
    if count <= SEARCH_EXCERPT_CHARS {
        return line.to_owned();
    }
    let truncated: String = line.chars().take(SEARCH_EXCERPT_CHARS).collect();
    format!("{truncated}...")
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
    let relative_under_workspace = path.strip_prefix(&workspace_root).map_or_else(
        |_| PathBuf::from(path.file_name().unwrap_or_default()),
        Path::to_path_buf,
    );

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
        let _ignored = tokio::fs::copy(&path, &trash_target).await.map_err(|e| {
            ToolError::Io(format!(
                "could not move file to trash (rename: {rename_err}, copy fallback: {e})"
            ))
        })?;
        tokio::fs::remove_file(&path)
            .await
            .map_err(|e| ToolError::Io(format!("could not remove original after copy: {e}")))?;
    }

    let character_data_root = PathBuf::from(character_data_dir);
    let trash_display_root = character_data_root.parent().unwrap_or(Path::new(""));
    let trashed_display = trash_target.strip_prefix(trash_display_root).map_or_else(
        |_| trash_target.to_string_lossy().replace('\\', "/"),
        |p| p.to_string_lossy().replace('\\', "/"),
    );

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
            std::path::Component::CurDir | std::path::Component::Normal(_) => {}
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
    let Some((program, program_args)) = argv.split_first() else {
        return Err(ToolError::InvalidArgs("command is empty".into()));
    };

    if !is_command_allowed(&argv) {
        return Err(ToolError::InvalidArgs(format!(
            "command '{program}' is not in the allowlist"
        )));
    }

    validate_exec_args(workspace_dir, &argv)?;

    let workdir = input
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(|w| resolve_path(workspace_dir, w))
        .transpose()?;

    let mut cmd = tokio::process::Command::new(program);
    let _ignored = cmd.args(program_args);

    if let Some(dir) = workdir {
        _ = cmd.current_dir(dir);
    } else if !workspace_dir.is_empty() {
        _ = cmd.current_dir(workspace_dir);
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
    use async_trait::async_trait;

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
        let write_result = handle_write(
            json!({"path": "test.txt", "content": "hello world"}),
            &ws_str,
        )
        .await
        .unwrap();
        assert_eq!(write_result["bytes_written"], 11);

        // Read
        let read_result = handle_read(json!({"path": "test.txt"}), &ws_str)
            .await
            .unwrap();
        assert_eq!(read_result["content"], "hello world");
        assert_eq!(read_result["total_lines"], 1);

        // List
        let list_result = handle_list_files(json!({}), &ws_str).await.unwrap();
        let entries = list_result["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["name"], "test.txt");
    }

    #[tokio::test]
    async fn edit_replaces_text() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        let _ignored = handle_write(
            json!({"path": "test.txt", "content": "hello world\nfoo bar\n"}),
            &ws_str,
        )
        .await
        .unwrap();

        let edit_result = handle_edit(
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
        assert_eq!(edit_result["replacements_made"], 1);

        let read_result = handle_read(json!({"path": "test.txt"}), &ws_str)
            .await
            .unwrap();
        assert_eq!(read_result["content"], "goodbye world\nfoo bar\n");
    }

    #[tokio::test]
    async fn edit_multiple_replacements() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        let _ignored = handle_write(
            json!({"path": "test.txt", "content": "foo foo foo"}),
            &ws_str,
        )
        .await
        .unwrap();

        let edit_result = handle_edit(
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
        assert_eq!(edit_result["replacements_made"], 3);

        let read_result = handle_read(json!({"path": "test.txt"}), &ws_str)
            .await
            .unwrap();
        assert_eq!(read_result["content"], "bar bar bar");
    }

    #[tokio::test]
    async fn edit_fails_on_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        let _ignored = handle_write(
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

        let _ignored = handle_write(
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

        let _ignored = handle_write(
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
        let _ignored = handle_write(json!({"path": "test.txt", "content": content}), &ws_str)
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

        let _ignored = handle_write(
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

    #[test]
    fn hybrid_excerpt_prefers_relevant_body_line_over_title() {
        let content = "# Christine - Ren's mother\n\nBackground notes mention mother.\n\nMarch note: opioid relapse concerns and a no-contact boundary.";

        let (line_no, excerpt) = best_line_excerpt(content, "christine mother opioid");

        assert_eq!(line_no, 5);
        assert!(excerpt.contains("opioid"));
    }

    #[tokio::test]
    async fn search_finds_workspace_and_memory_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        let _ignored = handle_write(
            json!({"path": "notes/ideas.md", "content": "Tea in the garden\nCoffee later"}),
            &ws_str,
        )
        .await
        .unwrap();
        _ = handle_write(
            json!({"path": "memory/people/ren.md", "content": "Ren likes tea."}),
            &ws_str,
        )
        .await
        .unwrap();

        let result = handle_search(json!({"query": "tea"}), &ws_str, None, None, None)
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
    async fn search_orders_results_newest_file_first() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        let _ignored = handle_write(
            json!({"path": "older.md", "content": "tea in the garden"}),
            &ws_str,
        )
        .await
        .unwrap();

        // mtime resolution can be coarse (e.g. 1s on some filesystems), so
        // bump the older file backwards in time rather than relying on the
        // write order alone.
        let past = SystemTime::now() - std::time::Duration::from_mins(1);
        std::fs::File::options()
            .write(true)
            .open(ws.join("older.md"))
            .unwrap()
            .set_modified(past)
            .unwrap();

        _ = handle_write(
            json!({"path": "newer.md", "content": "tea on the porch"}),
            &ws_str,
        )
        .await
        .unwrap();

        let result = handle_search(json!({"query": "tea"}), &ws_str, None, None, None)
            .await
            .unwrap();
        let results = result["results"].as_array().unwrap();
        let paths: Vec<&str> = results
            .iter()
            .map(|entry| entry["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, vec!["newer.md", "older.md"]);

        let files = result["files"].as_array().expect("files summary present");
        assert_eq!(files[0]["path"], "newer.md");
        assert_eq!(files[1]["path"], "older.md");
        assert!(result["note"].is_string());
    }

    #[tokio::test]
    async fn search_omits_note_when_no_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        let _ignored = handle_write(
            json!({"path": "notes.md", "content": "nothing relevant"}),
            &ws_str,
        )
        .await
        .unwrap();

        let result = handle_search(json!({"query": "absent"}), &ws_str, None, None, None)
            .await
            .unwrap();
        assert_eq!(result["count"], 0);
        assert!(result.get("note").is_none());
        assert!(result.get("files").is_none());
    }

    #[tokio::test]
    async fn search_excerpt_includes_match_in_long_single_line_file() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();
        let metadata = "metadata ".repeat(200);
        let line = format!(
            r#"{{"metadata":"{metadata}","message":"spotted a German Shepherd near the gate"}}"#
        );

        let _ignored = handle_write(
            json!({"path": "archive/chat.jsonl", "content": line}),
            &ws_str,
        )
        .await
        .unwrap();

        let result = handle_search(
            json!({"query": "german shepherd"}),
            &ws_str,
            None,
            None,
            None,
        )
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

    #[cfg(unix)]
    #[tokio::test]
    async fn search_skips_symlinks_pointing_outside_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        let _ignored = handle_write(
            json!({"path": "notes.md", "content": "no match here"}),
            &ws_str,
        )
        .await
        .unwrap();

        let outside_dir = tmp.path().join("outside");
        std::fs::create_dir_all(&outside_dir).unwrap();
        let secret = outside_dir.join("secret.md");
        std::fs::write(&secret, "secret_token_xyzzy").unwrap();

        std::os::unix::fs::symlink(&secret, ws.join("link_to_secret.md")).unwrap();
        std::os::unix::fs::symlink(&outside_dir, ws.join("outside_dir")).unwrap();

        let result = handle_search(json!({"query": "xyzzy"}), &ws_str, None, None, None)
            .await
            .unwrap();
        assert_eq!(result["count"], 0, "symlinked file content must not leak");
        let results = result["results"].as_array().unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn search_hybrid_mode_falls_back_to_lexical_without_embedder() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        let _ignored = handle_write(json!({"path": "notes.md", "content": "tea time"}), &ws_str)
            .await
            .unwrap();

        let result = handle_search(
            json!({"query": "tea", "mode": "hybrid"}),
            &ws_str,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(result["mode"], "lexical");
        assert_eq!(result["semantic_unavailable"], "embedder not configured");
        let results = result["results"].as_array().unwrap();
        assert!(!results.is_empty());
    }

    #[tokio::test]
    async fn search_lexical_mode_does_not_set_semantic_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        let _ignored = handle_write(json!({"path": "notes.md", "content": "tea time"}), &ws_str)
            .await
            .unwrap();

        let result = handle_search(
            json!({"query": "tea", "mode": "lexical"}),
            &ws_str,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(result["mode"], "lexical");
        assert!(result.get("semantic_unavailable").is_none());
    }

    struct DummyEmbedder;

    #[async_trait]
    impl Embedder for DummyEmbedder {
        async fn embed(&self, inputs: &[&str]) -> Result<Vec<Vec<f32>>, shore_llm::LlmError> {
            let dim = self.dimensions().unwrap_or(4);
            Ok(inputs.iter().map(|_| vec![0.0; dim]).collect())
        }

        fn model_id(&self) -> &'static str {
            "dummy"
        }

        fn dimensions(&self) -> Option<usize> {
            Some(4)
        }
    }

    #[tokio::test]
    async fn search_with_path_runs_hybrid_scoped_to_subtree() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        let _ignored = handle_write(
            json!({"path": "notes/a.md", "content": "tea time"}),
            &ws_str,
        )
        .await
        .unwrap();
        _ = handle_write(
            json!({"path": "other/b.md", "content": "tea ceremony"}),
            &ws_str,
        )
        .await
        .unwrap();

        let embedder = DummyEmbedder;
        let idx = tmp.path().join("index.json");

        let result = handle_search(
            json!({"query": "tea", "path": "notes/"}),
            &ws_str,
            None,
            Some(&embedder),
            Some(&idx),
        )
        .await
        .unwrap();

        assert_eq!(result["mode"], "hybrid");
        assert!(result.get("semantic_unavailable").is_none());
        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["path"], "notes/a.md");
    }

    #[tokio::test]
    async fn search_rejects_unknown_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        let _ignored = handle_write(json!({"path": "notes.md", "content": "tea time"}), &ws_str)
            .await
            .unwrap();

        let result = handle_search(
            json!({"query": "tea", "mode": "magic"}),
            &ws_str,
            None,
            None,
            None,
        )
        .await;

        match result {
            Err(ToolError::InvalidArgs(msg)) => assert!(
                msg.contains("magic"),
                "expected error to mention the bad mode, got: {msg}"
            ),
            other => panic!("expected InvalidArgs, got {other:?}"),
        }
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

        let _ignored = handle_write(json!({"path": "notes/old.md", "content": "stale"}), &ws_str)
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
            let dir_entry = entry.unwrap();
            let candidate = dir_entry.path().join("notes/old.md");
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

        let _ignored = handle_write(
            json!({"path": "memory/people/ren.md", "content": "Ren"}),
            &ws_str,
        )
        .await
        .unwrap();

        _ = handle_delete(json!({"path": "memory/people/ren.md"}), &ws_str, &data_str)
            .await
            .unwrap();

        assert!(!ws.join("memory/people/ren.md").exists());
        let trash = data_dir.join("trash");
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(&trash).unwrap() {
            let dir_entry = entry.unwrap();
            paths.push(dir_entry.path().join("memory/people/ren.md"));
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

        let _ignored = handle_write(json!({"path": "SOUL.md", "content": "soul"}), &ws_str)
            .await
            .unwrap();
        _ = handle_write(json!({"path": "MEMORY.md", "content": "idx"}), &ws_str)
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

        let _ignored = handle_write(json!({"path": "foo.md", "content": "x"}), &ws_str)
            .await
            .unwrap();

        let result = handle_delete(json!({"path": "foo.md"}), &ws_str, "").await;
        assert!(result.is_err());
        assert!(ws.join("foo.md").exists(), "file should not be moved");
    }
}
