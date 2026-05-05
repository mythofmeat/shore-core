//! Native Claude Code session-file replay.
//!
//! Claude Code's `--print` mode does not write resumable session files, but
//! `--resume <session-id>` will consume JSONL files in `~/.claude/projects`.
//! For cold starts with Shore history, we synthesize that JSONL from the prior
//! turns and let Claude Code resume natively instead of flattening the whole
//! transcript into the system prompt.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::{json, Value};

use crate::providers::claude_code::driver::{current_turn_start, ProviderConfig};
use crate::types::LlmRequest;
use crate::LlmError;

#[derive(Debug, Clone)]
pub(super) struct NativeSession {
    #[allow(dead_code)]
    pub path: PathBuf,
}

pub(super) fn prepare_native_session(
    request: &LlmRequest,
    cfg: &ProviderConfig,
) -> Result<Option<NativeSession>, LlmError> {
    if !cfg.native_session_replay {
        return Ok(None);
    }

    let history_end = current_turn_start(&request.messages);
    if history_end == 0 {
        return Ok(None);
    }

    let path = session_path(&cfg.session_id)?;
    write_session_file(
        &path,
        &cfg.session_id,
        &request.messages[..history_end],
        &request.model,
    )?;
    Ok(Some(NativeSession { path }))
}

fn session_path(session_id: &str) -> Result<PathBuf, LlmError> {
    let home = std::env::var_os("HOME").ok_or_else(|| LlmError::Provider {
        message: "cannot prepare Claude native session replay without HOME".into(),
    })?;
    let cwd = std::env::current_dir().map_err(|e| LlmError::Provider {
        message: format!("resolve current directory for Claude native session replay: {e}"),
    })?;
    Ok(PathBuf::from(home)
        .join(".claude")
        .join("projects")
        .join(project_slug(&cwd))
        .join(format!("{session_id}.jsonl")))
}

fn project_slug(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn write_session_file(
    path: &Path,
    session_id: &str,
    messages: &[Value],
    model: &str,
) -> Result<(), LlmError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| LlmError::Provider {
            message: format!("create Claude native session directory: {e}"),
        })?;
    }

    let cwd = std::env::current_dir().map_err(|e| LlmError::Provider {
        message: format!("resolve current directory for Claude native session replay: {e}"),
    })?;
    let timestamp = Utc::now().to_rfc3339();
    let mut parent_uuid: Option<String> = None;
    let mut rows = Vec::new();

    for message in messages {
        let Some(row) = render_row(
            session_id,
            &cwd,
            &timestamp,
            parent_uuid.as_deref(),
            message,
            model,
        ) else {
            continue;
        };
        parent_uuid = row.get("uuid").and_then(Value::as_str).map(String::from);
        rows.push(row);
    }

    let body = rows
        .into_iter()
        .map(|row| serde_json::to_string(&row))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| LlmError::Provider {
            message: format!("serialize Claude native session replay: {e}"),
        })?
        .join("\n");
    std::fs::write(path, format!("{body}\n")).map_err(|e| LlmError::Provider {
        message: format!("write Claude native session replay: {e}"),
    })?;
    Ok(())
}

fn render_row(
    session_id: &str,
    cwd: &Path,
    timestamp: &str,
    parent_uuid: Option<&str>,
    source: &Value,
    model: &str,
) -> Option<Value> {
    let role = source.get("role").and_then(Value::as_str)?;
    let uuid = uuid::Uuid::new_v4().to_string();
    let message = match role {
        "assistant" => render_assistant_message(source, model),
        "user" => json!({
            "role": "user",
            "content": source.get("content").cloned().unwrap_or_else(|| Value::String(String::new())),
        }),
        "system" => {
            let text = source
                .get("content")
                .map(crate::providers::stream_helpers::extract_system_text)
                .unwrap_or_default();
            if text.is_empty() {
                return None;
            }
            json!({
                "role": "user",
                "content": format!("<system_instruction>{text}</system_instruction>"),
            })
        }
        _ => return None,
    };

    Some(json!({
        "type": if role == "assistant" { "assistant" } else { "user" },
        "sessionId": session_id,
        "uuid": uuid,
        "parentUuid": parent_uuid,
        "timestamp": timestamp,
        "cwd": cwd.to_string_lossy(),
        "version": "shore",
        "gitBranch": "",
        "isSidechain": false,
        "userType": "external",
        "entrypoint": "cli",
        "permissionMode": "default",
        "message": message,
    }))
}

fn render_assistant_message(source: &Value, model: &str) -> Value {
    let content = match source.get("content") {
        Some(Value::Array(blocks)) => Value::Array(blocks.clone()),
        Some(Value::String(text)) => json!([{ "type": "text", "text": text }]),
        _ => json!([]),
    };
    json!({
        "id": format!("msg_shore_{}", uuid::Uuid::new_v4().simple()),
        "type": "message",
        "role": "assistant",
        "model": session_model_name(model),
        "content": content,
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "stop_details": null,
        "usage": {
            "input_tokens": 0,
            "output_tokens": 0,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0,
        },
    })
}

fn session_model_name(model: &str) -> &str {
    match model {
        "claude-sonnet-4-5" => "claude-sonnet-4-5-20250929",
        "claude-haiku-4-5" => "claude-haiku-4-5-20251001",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn project_slug_matches_claude_path_style() {
        assert_eq!(
            project_slug(Path::new("/tmp/shore-claude-session-probe")),
            "-tmp-shore-claude-session-probe"
        );
    }

    #[test]
    fn render_rows_chain_user_and_assistant_messages() {
        let cwd = Path::new("/tmp/project");
        let user = json!({"role": "user", "content": "remember alpha"});
        let assistant = json!({"role": "assistant", "content": [{"type": "text", "text": "ok"}]});
        let first = render_row("sid", cwd, "2026-05-05T00:00:00Z", None, &user, "model").unwrap();
        let second = render_row(
            "sid",
            cwd,
            "2026-05-05T00:00:00Z",
            first["uuid"].as_str(),
            &assistant,
            "model",
        )
        .unwrap();

        assert_eq!(first["type"], "user");
        assert_eq!(first["message"]["content"], "remember alpha");
        assert_eq!(second["type"], "assistant");
        assert_eq!(second["parentUuid"], first["uuid"]);
        assert_eq!(second["message"]["content"][0]["text"], "ok");
    }
}
