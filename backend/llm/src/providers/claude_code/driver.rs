//! Subprocess driver for the `claude_code` provider.
//!
//! Spawns the local `claude` CLI per request (fresh-spawn path), pipes
//! a single user frame through stdin, parses the stream-json output via
//! [`super::parser::StreamJsonParser`], and returns the accumulated
//! events / blocks. The long-lived subprocess cache (pattern 3 hot
//! path) is a follow-up commit; this file only implements fresh-spawn.

use std::io::Write as _;

use serde_json::{json, Value};
use shore_protocol::types::ContentBlock;
use tempfile::NamedTempFile;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout};
use tracing::debug;

use crate::providers::claude_code::{parser, quota, recipe::CliRecipe};
use crate::providers::stream_helpers::extract_system_text;
use crate::types::{LlmRequest, StreamEvent};
use crate::LlmError;

/// What the driver returns once a subprocess turn completes.
#[derive(Debug, Default)]
pub(super) struct DriverOutput {
    pub events: Vec<StreamEvent>,
    pub blocks: Vec<ContentBlock>,
    pub model: String,
    pub stderr: String,
}

/// Engine-supplied glue extracted from `LlmRequest.provider_options`.
#[derive(Debug)]
struct ProviderConfig {
    mcp_endpoint: String,
    allowed_tools: Vec<String>,
    effort: Option<String>,
    session_id: String,
}

impl ProviderConfig {
    fn from_request(request: &LlmRequest) -> Result<Self, LlmError> {
        let opts = request
            .provider_options
            .as_ref()
            .ok_or_else(|| LlmError::Provider {
                message: "claude_code provider requires provider_options.mcp_endpoint".into(),
            })?;
        let mcp_endpoint = opts
            .get("mcp_endpoint")
            .and_then(Value::as_str)
            .ok_or_else(|| LlmError::Provider {
                message: "claude_code provider requires provider_options.mcp_endpoint".into(),
            })?
            .to_string();
        let allowed_tools = opts
            .get("allowed_tools")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let effort = opts
            .get("effort")
            .and_then(Value::as_str)
            .map(String::from);
        let session_id = opts
            .get("session_id")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(fallback_session_id);
        Ok(Self {
            mcp_endpoint,
            allowed_tools,
            effort,
            session_id,
        })
    }
}

/// Generate a fresh session ID when the engine doesn't pre-supply
/// one. The daemon will always pass `provider_options.session_id` in
/// production; this fallback exists so the driver can be exercised
/// in isolation (tests, harness scripts). The CLI's `--session-id`
/// flag rejects anything that isn't a valid UUID.
fn fallback_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Run a single subprocess turn against the locked recipe.
pub(super) async fn run_fresh_spawn(request: &LlmRequest) -> Result<DriverOutput, LlmError> {
    let cfg = ProviderConfig::from_request(request)?;

    // Tempfile is held open for the lifetime of the subprocess so the
    // CLI can read it before we drop it.
    let prompt_file = render_system_prompt(request)?;
    let user_frame = render_user_frame(request);

    let recipe = CliRecipe {
        model: request.model.clone(),
        mcp_endpoint: cfg.mcp_endpoint,
        allowed_tools: cfg.allowed_tools,
        system_prompt_path: prompt_file.path().to_path_buf(),
        session_id: cfg.session_id,
        effort: cfg.effort,
    };
    let mut cmd = recipe.into_command();
    debug!(
        rid = request.rid.as_deref().unwrap_or("-"),
        model = %request.model,
        "claude_code: spawning subprocess (fresh-spawn)"
    );
    let mut child = cmd.spawn().map_err(|e| LlmError::Provider {
        message: format!("failed to spawn claude CLI: {e}"),
    })?;

    let stdin = child.stdin.take().ok_or_else(|| LlmError::Provider {
        message: "failed to take child stdin".into(),
    })?;
    let stdout = child.stdout.take().ok_or_else(|| LlmError::Provider {
        message: "failed to take child stdout".into(),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| LlmError::Provider {
        message: "failed to take child stderr".into(),
    })?;

    let write_handle = tokio::spawn(write_user_frame(stdin, user_frame));
    let read_handle = tokio::spawn(read_stream_json(stdout));
    let stderr_handle = tokio::spawn(drain_stderr(stderr));

    // Wait for the read task first — when it completes the CLI has
    // already emitted its `result` event (or died trying).
    let read_result = read_handle.await.map_err(|e| LlmError::Provider {
        message: format!("stdout reader task panicked: {e}"),
    })?;
    // Stdin write errors only matter if the read also failed; ignore
    // them here so a benign EPIPE doesn't mask the real cause.
    let _ = write_handle.await;
    let stderr_text = stderr_handle.await.unwrap_or_default();
    let exit = child.wait().await.map_err(|e| LlmError::Provider {
        message: format!("failed to await child: {e}"),
    })?;

    let mut output = match read_result {
        Ok(o) => o,
        Err(LlmError::IncompleteStream) => {
            // The CLI emitted no events before stdout closed. The
            // most useful diagnostic is the stderr content combined
            // with the exit status — surface both as a Provider error
            // instead of the bare IncompleteStream.
            return Err(LlmError::Provider {
                message: format!(
                    "claude CLI produced no stream-json events (exit {}): {}",
                    exit.code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "<signal>".into()),
                    stderr_text.trim()
                ),
            });
        }
        Err(e) => return Err(e),
    };
    output.stderr = stderr_text;

    if !exit.success() {
        return Err(LlmError::Provider {
            message: format!(
                "claude CLI exited with {}: {}",
                exit.code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "<signal>".into()),
                output.stderr.trim()
            ),
        });
    }

    // Translate quota-shaped CLI errors into HttpStatus 429 so the
    // existing credential classifier picks them up as QuotaExhausted.
    if let Some(StreamEvent::Done {
        content,
        finish_reason,
        ..
    }) = output.events.last()
    {
        if finish_reason == "error" {
            if let Some(err) = quota::classify_result_error(content, true) {
                return Err(err);
            }
            return Err(LlmError::Provider {
                message: format!("claude CLI returned error: {content}"),
            });
        }
    }

    drop(prompt_file);
    Ok(output)
}

async fn write_user_frame(mut stdin: ChildStdin, frame: String) -> std::io::Result<()> {
    stdin.write_all(frame.as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await?;
    // Closing stdin (drop on return) triggers --print to finish the
    // response and exit cleanly.
    Ok(())
}

async fn drain_stderr(stderr: ChildStderr) -> String {
    let mut reader = BufReader::new(stderr);
    let mut buf = String::new();
    let _ = reader.read_to_string(&mut buf).await;
    buf
}

async fn read_stream_json(stdout: ChildStdout) -> Result<DriverOutput, LlmError> {
    let mut p = parser::StreamJsonParser::new();
    let mut output = DriverOutput::default();
    let mut lines = BufReader::new(stdout).lines();

    while let Some(line) = lines.next_line().await.map_err(|e| LlmError::Provider {
        message: format!("read from claude stdout: {e}"),
    })? {
        let step = p.handle_line(&line);
        output.events.extend(step.events);
        output.blocks.extend(step.blocks);
        if step.done {
            break;
        }
    }
    if let Some(m) = p.model() {
        output.model = m.to_string();
    }
    if output.events.is_empty() {
        return Err(LlmError::IncompleteStream);
    }
    Ok(output)
}

/// Render `request.system` and history (everything before the trailing
/// user message) into a temp file. The trailing user message goes
/// through stdin as a stream-json frame; assistant frames in stdin are
/// silently discarded by the CLI (FINDINGS.md §A) so prior turns must
/// be flattened into the system prompt.
fn render_system_prompt(request: &LlmRequest) -> Result<NamedTempFile, LlmError> {
    let mut text = match request.system.as_ref() {
        Some(v) => extract_system_text(v),
        None => String::new(),
    };

    let history_end = trailing_user_index(&request.messages);
    for (i, msg) in request.messages.iter().enumerate() {
        if i >= history_end {
            break;
        }
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        let content_text = extract_message_text(msg);
        if content_text.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&format!("<turn role=\"{role}\">\n{content_text}\n</turn>"));
    }

    let mut file = NamedTempFile::new().map_err(|e| LlmError::Provider {
        message: format!("create system prompt tempfile: {e}"),
    })?;
    file.write_all(text.as_bytes())
        .map_err(|e| LlmError::Provider {
            message: format!("write system prompt: {e}"),
        })?;
    file.flush().map_err(|e| LlmError::Provider {
        message: format!("flush system prompt: {e}"),
    })?;
    Ok(file)
}

fn trailing_user_index(messages: &[Value]) -> usize {
    messages
        .iter()
        .rposition(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .unwrap_or(messages.len())
}

/// Build the single stream-json frame to send through stdin.
///
/// FINDINGS.md §A: `content` MUST be a content-block array even when
/// the frame would otherwise be discarded. A bare string trips a JS
/// error in Claude Code's input parser.
fn render_user_frame(request: &LlmRequest) -> String {
    let user_text = match request.messages.last() {
        Some(m) if m.get("role").and_then(Value::as_str) == Some("user") => extract_message_text(m),
        _ => String::new(),
    };
    json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": user_text}],
        }
    })
    .to_string()
}

fn extract_message_text(message: &Value) -> String {
    let content = match message.get("content") {
        Some(c) => c,
        None => return String::new(),
    };
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(render_block)
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn render_block(b: &Value) -> Option<String> {
    let ty = b.get("type").and_then(Value::as_str)?;
    match ty {
        "text" => b.get("text").and_then(Value::as_str).map(String::from),
        "tool_use" => Some(format!(
            "<tool_use name=\"{}\" input=\"{}\"/>",
            b.get("name").and_then(Value::as_str).unwrap_or(""),
            b.get("input").map(|i| i.to_string()).unwrap_or_default(),
        )),
        "tool_result" => b.get("content").map(|c| {
            format!(
                "<tool_result>{}</tool_result>",
                extract_tool_result_text(c)
            )
        }),
        _ => None,
    }
}

fn extract_tool_result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| {
                if b.get("type").and_then(Value::as_str) == Some("text") {
                    b.get("text").and_then(Value::as_str).map(String::from)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use shore_config::models::Sdk;

    fn make_request(messages: Vec<Value>, opts: Option<Value>) -> LlmRequest {
        LlmRequest {
            sdk: Sdk::ClaudeCode,
            model: "claude-sonnet-4-5".into(),
            api_key: String::new(),
            base_url: None,
            messages,
            system: Some(json!("You are a helpful assistant.")),
            tools: None,
            max_tokens: 4096,
            temperature: None,
            top_p: None,
            provider_options: opts,
            provider_key: Some("anthropic".into()),
            rid: None,
            forensic_character: None,
        }
    }

    #[test]
    fn provider_config_requires_options() {
        let req = make_request(vec![], None);
        let err = ProviderConfig::from_request(&req).unwrap_err();
        match err {
            LlmError::Provider { message } => {
                assert!(message.contains("mcp_endpoint"), "got {message}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn provider_config_requires_mcp_endpoint() {
        let req = make_request(vec![], Some(json!({"allowed_tools": []})));
        let err = ProviderConfig::from_request(&req).unwrap_err();
        match err {
            LlmError::Provider { message } => assert!(message.contains("mcp_endpoint")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn provider_config_parses_full_options() {
        let req = make_request(
            vec![],
            Some(json!({
                "mcp_endpoint": "http://127.0.0.1:7321/mcp/abc",
                "allowed_tools": ["mcp__shore__memory", "mcp__shore__search"],
                "effort": "high",
                "session_id": "explicit-session-id",
            })),
        );
        let cfg = ProviderConfig::from_request(&req).unwrap();
        assert_eq!(cfg.mcp_endpoint, "http://127.0.0.1:7321/mcp/abc");
        assert_eq!(cfg.allowed_tools.len(), 2);
        assert_eq!(cfg.effort.as_deref(), Some("high"));
        assert_eq!(cfg.session_id, "explicit-session-id");
    }

    #[test]
    fn provider_config_falls_back_to_synthesized_session_id() {
        let req = make_request(
            vec![],
            Some(json!({"mcp_endpoint": "http://localhost/mcp/x"})),
        );
        let cfg = ProviderConfig::from_request(&req).unwrap();
        // Must be a valid UUID — the CLI rejects anything else at
        // `--session-id`.
        uuid::Uuid::parse_str(&cfg.session_id)
            .unwrap_or_else(|e| panic!("not a uuid: {} ({e})", cfg.session_id));
    }

    #[test]
    fn render_user_frame_uses_array_content() {
        let req = make_request(
            vec![json!({"role": "user", "content": "hello world"})],
            None,
        );
        let frame_str = render_user_frame(&req);
        let frame: Value = serde_json::from_str(&frame_str).unwrap();
        assert_eq!(frame["type"], "user");
        assert_eq!(frame["message"]["role"], "user");
        assert!(frame["message"]["content"].is_array());
        assert_eq!(frame["message"]["content"][0]["type"], "text");
        assert_eq!(frame["message"]["content"][0]["text"], "hello world");
    }

    #[test]
    fn render_user_frame_empty_when_no_messages() {
        let req = make_request(vec![], None);
        let frame_str = render_user_frame(&req);
        let frame: Value = serde_json::from_str(&frame_str).unwrap();
        assert_eq!(frame["message"]["content"][0]["text"], "");
    }

    #[test]
    fn render_user_frame_handles_assistant_at_end() {
        // Defensive: if the last message is somehow assistant, the
        // user frame should be empty rather than echoing assistant text.
        let req = make_request(
            vec![
                json!({"role": "user", "content": "q"}),
                json!({"role": "assistant", "content": "a"}),
            ],
            None,
        );
        let frame_str = render_user_frame(&req);
        let frame: Value = serde_json::from_str(&frame_str).unwrap();
        assert_eq!(frame["message"]["content"][0]["text"], "");
    }

    #[test]
    fn render_system_prompt_includes_history_turns() {
        let req = make_request(
            vec![
                json!({"role": "user", "content": "first question"}),
                json!({"role": "assistant", "content": "first answer"}),
                json!({"role": "user", "content": "second question"}),
            ],
            None,
        );
        let file = render_system_prompt(&req).unwrap();
        let text = std::fs::read_to_string(file.path()).unwrap();
        assert!(text.contains("You are a helpful assistant."));
        assert!(text.contains("first question"));
        assert!(text.contains("first answer"));
        assert!(
            !text.contains("second question"),
            "trailing user message goes through stdin, not the system prompt"
        );
    }

    #[test]
    fn render_system_prompt_handles_no_history() {
        let req = make_request(vec![json!({"role": "user", "content": "hi"})], None);
        let file = render_system_prompt(&req).unwrap();
        let text = std::fs::read_to_string(file.path()).unwrap();
        assert!(text.contains("You are a helpful assistant."));
        assert!(!text.contains("hi"));
    }

    #[test]
    fn render_system_prompt_handles_no_system_field() {
        let mut req = make_request(vec![json!({"role": "user", "content": "hi"})], None);
        req.system = None;
        let file = render_system_prompt(&req).unwrap();
        let text = std::fs::read_to_string(file.path()).unwrap();
        // No system, no history before the trailing user — file is empty.
        assert!(text.is_empty());
    }

    #[test]
    fn extract_message_text_from_string_content() {
        let m = json!({"role": "user", "content": "plain"});
        assert_eq!(extract_message_text(&m), "plain");
    }

    #[test]
    fn extract_message_text_from_blocks() {
        let m = json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "text", "text": " world"},
            ]
        });
        assert_eq!(extract_message_text(&m), "hello world");
    }

    #[test]
    fn extract_message_text_renders_tool_blocks_inline() {
        let m = json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Calling memory."},
                {"type": "tool_use", "id": "x1", "name": "memory", "input": {"q": "name"}},
                {"type": "tool_result", "tool_use_id": "x1", "content": [{"type": "text", "text": "Alice"}]},
                {"type": "text", "text": " Done."}
            ]
        });
        let text = extract_message_text(&m);
        assert!(text.contains("Calling memory."));
        assert!(text.contains("memory"));
        assert!(text.contains("Alice"));
        assert!(text.contains(" Done."));
    }
}
