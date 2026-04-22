//! Workspace filesystem tools — read, write, edit, list, exec.
//!
//! These tools give the assistant access to a real filesystem workspace
//! (`{character}/workspace/`) and its memories directory (`{character}/memories/`),
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
            description: "Read the contents of a file in your workspace or memories. Use this to check existing files before editing, review your own notes, or pull up context you saved earlier. Returns the full file content as a string. If the file is large you may read it in chunks using offset and limit.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path to the file under workspace/ or memories/."
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
            description: "Write or overwrite a file in your workspace or memories. Use this to create new notes, save structured information, or write drafts. Parent directories are created automatically. Overwrites without confirmation — if you want to preserve existing content, read it first and merge manually.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path to the file under workspace/ or memories/."
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
            description: "Edit an existing file by replacing specific text. Provide one or more replacements. Each replacement must match the old_string exactly (including whitespace and newlines). If the old_string appears multiple times, every occurrence is replaced. Use read first if you are unsure of the exact text.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path to the file under workspace/ or memories/."
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
            description: "List files and directories under a path in your workspace or memories. Use this to explore your file tree, find where something is saved, or orient yourself before reading or writing. Returns each entry's name, type (file or directory), and size.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative directory path under workspace/ or memories/. Omit for root."
                    }
                },
                "required": []
            }),
            category: ToolCategory::Other,
        },
        ToolDef {
            name: "exec",
            description: "Run a shell command on the host. Use this for file search (rg, find), version control (git), build tools, or any task that is easier done with a command than with file tools. Commands run against an allowlist — restricted commands are rejected. Output is returned as text.",
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

fn resolve_path(workspace_dir: &str, relative: &str) -> Result<PathBuf, ToolError> {
    if workspace_dir.is_empty() {
        return Err(ToolError::InvalidArgs("workspace not configured".into()));
    }

    let relative = relative.trim();
    if relative.is_empty() {
        return Err(ToolError::InvalidArgs("path is empty".into()));
    }

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

    let base = PathBuf::from(workspace_dir);
    let resolved = base.join(relative);

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

    let base = PathBuf::from(workspace_dir);

    match relative {
        None | Some("") | Some(".") => Ok(base),
        Some(rel) => resolve_path(workspace_dir, rel),
    }
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
    let selected: Vec<&str> = lines.iter().skip(offset).take(end - offset).copied().collect();
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
            let snippet = if content.len() <= snippet_limit {
                content.clone()
            } else {
                format!("{}\n... (truncated)", &content[..snippet_limit])
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

// ---------------------------------------------------------------------------
// Exec allowlist
// ---------------------------------------------------------------------------

/// Default allowed commands for the exec tool.
static DEFAULT_ALLOWLIST: &[&str] = &[
    "ls", "cat", "rg", "git", "find", "head", "tail", "wc", "pwd", "echo", "mkdir", "cp",
    "mv", "touch", "sort", "uniq", "grep", "awk", "sed", "xargs", "dirname", "basename",
    "file", "stat", "du", "df", "which", "whoami", "date", "env", "printenv", "tree", "fd",
    "tldr", "man", "help", "cargo", "rustc", "python3", "python", "node", "npm", "pnpm",
    "yarn", "make", "cmake", "gcc", "g++", "clang", "go", "ruby", "perl", "php", "lua",
    "javac", "java", "kotlin", "scala", "swift", "dotnet", "zig", "odin", "v", "nim",
    "composer", "bundle", "gem", "pip", "pip3", "poetry", "uv", "conda", "mvn", "gradle",
    "sbt", "leiningen", "mix", "rebar3", "dune", "ocamlfind", "opam", "stack", "cabal",
    "ghc", "racket", "raco", "chicken", "csi", "gosh", "gauche", "mit-scheme", "guile",
    "chez", "chibi-scheme", "bigloo", "larceny", "ypsilon", "ironscheme", "ikarus",
    "mosh", "sagittarius", "foment", "ol", " Owl Lisp", "s7", "tinyscheme", "minischeme",
    "scheme48", "scsh", " Stalin", "vc", "loko", "tr7", "umb", "sxm", "scheme9", "s9fes",
    "chicken5", "chicken4", "gambit", "gsi", "gsc", "gerbil", "gxc", "lfe", "joxa",
    "elixir", "iex", "erl", "escript", "rebar", "rebar3", "mix", "hex", "cargo", "rustup",
    "clippy", "rustfmt", "rls", "rust-analyzer", "deno", "bun", "ts-node", "tsc",
    "eslint", "prettier", "jest", "vitest", "playwright", "cypress", "webpack", "vite",
    "rollup", "parcel", "esbuild", "swc", "babel", "turbopack", "next", "nuxt", "astro",
    "svelte-kit", "remix", "gatsby", "hexo", "hugo", "jekyll", "eleventy", "mkdocs",
    "docusaurus", "vuepress", "vitepress", "docsify", "gitbook", "slate", "redoc",
    "swagger", "openapi", "postman", "insomnia", "hoppscotch", "bruno", "k6", "artillery",
    "locust", "jmeter", "gatling", "tsung", " vegeta", "wrk", "wrk2", "ab", "siege",
    "hey", "boom", "cobra", "cobra-cli", "viper", "urfave", "gin", "echo", "fiber",
    "chi", "mux", "gorilla", "pat", "httprouter", "fasthttp", "atreugo", "vapor",
    "leaf", "kitura", "perfect", "swifter", "hummingbird", "vapor", "vapor-beta",
];

fn is_command_allowed(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }

    // Extract the first token (the command itself, before any args or pipes)
    let first_token = trimmed
        .split(|c: char| c.is_whitespace() || c == '|' || c == ';' || c == '&' || c == '>' || c == '<')
        .next()
        .unwrap_or("")
        .trim();

    if first_token.is_empty() {
        return false;
    }

    // Allow absolute paths to allowed binaries
    let cmd_name = if first_token.contains('/') {
        std::path::Path::new(first_token)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(first_token)
    } else {
        first_token
    };

    DEFAULT_ALLOWLIST.iter().any(|&allowed| allowed == cmd_name)
}

pub async fn handle_exec(input: Value, workspace_dir: &str) -> Result<Value, ToolError> {
    let command = input
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing required field: command".into()))?;

    if !is_command_allowed(command) {
        return Err(ToolError::InvalidArgs(format!(
            "command '{}' is not in the allowlist",
            command
        )));
    }

    let workdir = input
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(|w| resolve_path(workspace_dir, w))
        .transpose()?;

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(command);

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
        assert_eq!(tool_defs().len(), 5);
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

    #[test]
    fn exec_allowlist_basic() {
        assert!(is_command_allowed("ls -la"));
        assert!(is_command_allowed("git status"));
        assert!(is_command_allowed("rg pattern"));
        assert!(!is_command_allowed("rm -rf /"));
        assert!(!is_command_allowed("sudo apt install"));
        assert!(!is_command_allowed(""));
        assert!(!is_command_allowed("  "));
    }

    #[tokio::test]
    async fn exec_runs_allowed_command() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        let ws_str = ws.to_string_lossy().to_string();

        let result = handle_exec(json!({"command": "echo hello"}), &ws_str)
            .await
            .unwrap();
        assert_eq!(result["stdout"], "hello\n");
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
    async fn exec_with_workdir() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let ws_str = ws.to_string_lossy().to_string();

        tokio::fs::create_dir_all(ws.join("subdir")).await.unwrap();

        let result = handle_exec(
            json!({"command": "pwd", "workdir": "subdir"}),
            &ws_str,
        )
        .await
        .unwrap();
        let stdout = result["stdout"].as_str().unwrap();
        assert!(stdout.contains("subdir"), "expected subdir in pwd output: {stdout}");
    }
}