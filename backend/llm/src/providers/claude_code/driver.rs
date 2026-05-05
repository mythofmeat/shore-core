//! Subprocess driver for the `claude_code` provider.
//!
//! Spawns the local `claude` CLI per request (fresh-spawn path), pipes
//! a single user frame through stdin, parses the stream-json output via
//! [`super::parser::StreamJsonParser`], and returns the accumulated
//! events / blocks. The long-lived cache reuses the low-level helpers
//! in this module but keeps the child process open between turns.

use std::io::Write as _;

use serde_json::{json, Value};
use shore_protocol::types::ContentBlock;
use tempfile::NamedTempFile;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, Lines,
};
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
pub(super) struct ProviderConfig {
    pub mcp_endpoint: String,
    pub allowed_tools: Vec<String>,
    pub effort: Option<String>,
    pub session_id: String,
    pub subprocess_key: Option<String>,
    pub include_partial_messages: bool,
}

impl ProviderConfig {
    pub(super) fn from_request(request: &LlmRequest) -> Result<Self, LlmError> {
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
            .or_else(|| opts.get("reasoning_effort"))
            .and_then(Value::as_str)
            .map(String::from);
        let session_id = opts
            .get("session_id")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(fallback_session_id);
        let subprocess_key = opts
            .get("subprocess_key")
            .and_then(Value::as_str)
            .map(String::from);
        let include_partial_messages = opts
            .get("include_partial_messages")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        Ok(Self {
            mcp_endpoint,
            allowed_tools,
            effort,
            session_id,
            subprocess_key,
            include_partial_messages,
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
    let prompt_text = render_system_prompt_text(request);
    let prompt_file = write_system_prompt_file(&prompt_text)?;
    let user_frame = render_user_frame(request);

    let recipe = recipe_for_request(request, &cfg, prompt_file.path().to_path_buf());
    let mut cmd = recipe.into_command();
    debug!(
        rid = request.rid.as_deref().unwrap_or("-"),
        model = %request.model,
        subprocess_key = cfg.subprocess_key.as_deref().unwrap_or("-"),
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

    let write_handle = tokio::spawn(write_user_frame_and_close(stdin, user_frame));
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

    // Translate quota-shaped CLI errors into HttpStatus 429 before
    // checking process status. Claude Code exits 1 for these, but the
    // stream-json result event carries the typed 429 body we need for
    // Shore's existing quota handling.
    if let Some(err) = classify_output_error(&output) {
        return Err(err);
    }

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

    Ok(output)
}

/// Run a single fresh subprocess turn and forward parsed Shore stream events
/// as soon as each Claude stream-json line arrives.
pub(super) async fn run_fresh_spawn_streaming<W>(
    request: &LlmRequest,
    writer: &mut W,
) -> Result<DriverOutput, LlmError>
where
    W: AsyncWrite + Unpin,
{
    let cfg = ProviderConfig::from_request(request)?;

    let prompt_text = render_system_prompt_text(request);
    let prompt_file = write_system_prompt_file(&prompt_text)?;
    let user_frame = render_user_frame(request);

    let recipe = recipe_for_request(request, &cfg, prompt_file.path().to_path_buf());
    let mut cmd = recipe.into_command();
    debug!(
        rid = request.rid.as_deref().unwrap_or("-"),
        model = %request.model,
        subprocess_key = cfg.subprocess_key.as_deref().unwrap_or("-"),
        "claude_code: spawning subprocess (fresh-spawn streaming)"
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

    let write_handle = tokio::spawn(write_user_frame_and_close(stdin, user_frame));
    let stderr_handle = tokio::spawn(drain_stderr(stderr));
    let mut lines = BufReader::new(stdout).lines();
    let read_result = read_stream_json_lines_forwarding(&mut lines, writer).await;
    let _ = write_handle.await;
    let stderr_text = stderr_handle.await.unwrap_or_default();
    let exit = child.wait().await.map_err(|e| LlmError::Provider {
        message: format!("failed to await child: {e}"),
    })?;

    let mut output = match read_result {
        Ok(o) => o,
        Err(LlmError::IncompleteStream) => {
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

    if let Some(err) = classify_output_error(&output) {
        return Err(err);
    }

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

    Ok(output)
}

pub(super) fn recipe_for_request(
    request: &LlmRequest,
    cfg: &ProviderConfig,
    system_prompt_path: std::path::PathBuf,
) -> CliRecipe {
    CliRecipe {
        model: request.model.clone(),
        mcp_endpoint: cfg.mcp_endpoint.clone(),
        allowed_tools: cfg.allowed_tools.clone(),
        system_prompt_path,
        session_id: cfg.session_id.clone(),
        effort: cfg.effort.clone(),
        include_partial_messages: cfg.include_partial_messages,
    }
}

pub(super) async fn write_user_frame_to(
    stdin: &mut ChildStdin,
    frame: String,
) -> std::io::Result<()> {
    stdin.write_all(frame.as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await?;
    Ok(())
}

async fn write_user_frame_and_close(mut stdin: ChildStdin, frame: String) -> std::io::Result<()> {
    write_user_frame_to(&mut stdin, frame).await?;
    // Closing stdin (drop on return) triggers --print to finish the
    // response and exit cleanly.
    Ok(())
}

pub(super) async fn drain_stderr(stderr: ChildStderr) -> String {
    let mut reader = BufReader::new(stderr);
    let mut buf = String::new();
    let _ = reader.read_to_string(&mut buf).await;
    buf
}

async fn read_stream_json(stdout: ChildStdout) -> Result<DriverOutput, LlmError> {
    let mut lines = BufReader::new(stdout).lines();
    read_stream_json_lines(&mut lines).await
}

pub(super) async fn read_stream_json_lines<R>(
    lines: &mut Lines<R>,
) -> Result<DriverOutput, LlmError>
where
    R: AsyncBufRead + Unpin,
{
    let mut sink = tokio::io::sink();
    read_stream_json_lines_forwarding(lines, &mut sink).await
}

pub(super) async fn read_stream_json_lines_forwarding<R, W>(
    lines: &mut Lines<R>,
    writer: &mut W,
) -> Result<DriverOutput, LlmError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut p = parser::StreamJsonParser::new();
    let mut output = DriverOutput::default();
    let mut saw_done = false;

    while let Some(line) = lines.next_line().await.map_err(|e| LlmError::Provider {
        message: format!("read from claude stdout: {e}"),
    })? {
        let step = p.handle_line(&line);
        for event in &step.events {
            let line = super::serialize_event(event);
            writer
                .write_all(line.as_bytes())
                .await
                .map_err(|e| LlmError::Provider {
                    message: format!("write streamed claude event: {e}"),
                })?;
            writer
                .write_all(b"\n")
                .await
                .map_err(|e| LlmError::Provider {
                    message: format!("write streamed claude event newline: {e}"),
                })?;
            writer.flush().await.map_err(|e| LlmError::Provider {
                message: format!("flush streamed claude event: {e}"),
            })?;
        }
        output.events.extend(step.events);
        output.blocks.extend(step.blocks);
        if step.done {
            saw_done = true;
            break;
        }
    }
    if let Some(m) = p.model() {
        output.model = m.to_string();
    }
    if !saw_done {
        return Err(LlmError::IncompleteStream);
    }
    Ok(output)
}

pub(super) fn classify_output_error(output: &DriverOutput) -> Option<LlmError> {
    let Some(StreamEvent::Done {
        content,
        finish_reason,
        ..
    }) = output.events.last()
    else {
        return None;
    };
    if finish_reason != "error" {
        return None;
    }
    quota::classify_result_error(content, true).or_else(|| {
        Some(LlmError::Provider {
            message: format!("claude CLI returned error: {content}"),
        })
    })
}

/// Render `request.system` and prior history into a temp file. The current
/// turn goes through stdin as a stream-json user frame; assistant frames in
/// stdin are silently discarded by the CLI (FINDINGS.md §A), so prior turns
/// must be flattened into the system prompt.
#[cfg(test)]
fn render_system_prompt(request: &LlmRequest) -> Result<NamedTempFile, LlmError> {
    let text = render_system_prompt_text(request);
    write_system_prompt_file(&text)
}

pub(super) fn render_system_prompt_text(request: &LlmRequest) -> String {
    let mut text = render_static_system_prompt_text(request);

    let history_end = current_turn_start(&request.messages);
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
    text
}

pub(super) fn render_static_system_prompt_text(request: &LlmRequest) -> String {
    match request.system.as_ref() {
        Some(v) => extract_system_text(v),
        None => String::new(),
    }
}

pub(super) fn write_system_prompt_file(text: &str) -> Result<NamedTempFile, LlmError> {
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

fn current_turn_start(messages: &[Value]) -> usize {
    if messages.is_empty() {
        return 0;
    }

    let mut trailing_system_start = messages.len();
    while trailing_system_start > 0
        && message_role(&messages[trailing_system_start - 1]) == Some("system")
    {
        trailing_system_start -= 1;
    }

    if trailing_system_start < messages.len() {
        if trailing_system_start > 0
            && message_role(&messages[trailing_system_start - 1]) == Some("user")
        {
            trailing_system_start - 1
        } else {
            trailing_system_start
        }
    } else if message_role(messages.last().expect("non-empty messages")) == Some("user") {
        messages.len() - 1
    } else {
        messages.len()
    }
}

/// Build the single stream-json frame to send through stdin.
///
/// FINDINGS.md §A: `content` MUST be a content-block array even when
/// the frame would otherwise be discarded. A bare string trips a JS
/// error in Claude Code's input parser.
pub(super) fn render_user_frame(request: &LlmRequest) -> String {
    let content = render_current_turn_content(&request.messages);
    json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": content,
        }
    })
    .to_string()
}

fn render_current_turn_content(messages: &[Value]) -> Vec<Value> {
    let start = current_turn_start(messages);
    if start >= messages.len() {
        return vec![text_block("")];
    }

    let mut content = Vec::new();
    for msg in &messages[start..] {
        let blocks = render_current_message_blocks(msg);
        if blocks.is_empty() {
            continue;
        }
        if !content.is_empty() {
            content.push(text_block("\n\n"));
        }
        content.extend(blocks);
    }
    if content.is_empty() {
        content.push(text_block(""));
    }
    content
}

fn render_current_message_blocks(message: &Value) -> Vec<Value> {
    match message_role(message) {
        Some("system") => {
            let text = extract_inline_system_text(message);
            if text.is_empty() {
                Vec::new()
            } else {
                vec![text_block(format!(
                    "<system_instruction>{text}</system_instruction>"
                ))]
            }
        }
        _ => render_content_blocks_for_stdin(message.get("content")),
    }
}

fn render_content_blocks_for_stdin(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(s)) => vec![text_block(s)],
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(render_stdin_block)
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    }
}

fn render_stdin_block(block: &Value) -> Option<Value> {
    let ty = block.get("type").and_then(Value::as_str)?;
    match ty {
        "text" => block.get("text").and_then(Value::as_str).map(text_block),
        "image" => Some(block.clone()),
        "tool_use" | "tool_result" => render_block(block).map(text_block),
        _ => None,
    }
}

fn text_block(text: impl Into<String>) -> Value {
    json!({ "type": "text", "text": text.into() })
}

fn message_role(message: &Value) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

fn extract_inline_system_text(message: &Value) -> String {
    message
        .get("content")
        .map(extract_system_text)
        .unwrap_or_default()
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
        "tool_result" => b
            .get("content")
            .map(|c| format!("<tool_result>{}</tool_result>", extract_tool_result_text(c))),
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

    fn frame_content_text(frame: &Value) -> String {
        frame["message"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|block| {
                if block["type"] == "text" {
                    block["text"].as_str()
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("")
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
        assert!(cfg.include_partial_messages);
    }

    #[test]
    fn provider_config_can_disable_partial_messages_explicitly() {
        let req = make_request(
            vec![],
            Some(json!({
                "mcp_endpoint": "http://127.0.0.1:7321/mcp/abc",
                "include_partial_messages": false,
            })),
        );
        let cfg = ProviderConfig::from_request(&req).unwrap();
        assert!(!cfg.include_partial_messages);
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

    #[tokio::test]
    async fn read_stream_json_lines_requires_result_event() {
        let input = concat!(
            r#"{"type":"system","subtype":"init","model":"claude-sonnet-4-5"}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"partial"}]}}"#,
            "\n",
        );
        let mut lines = tokio::io::BufReader::new(input.as_bytes()).lines();

        let err = read_stream_json_lines(&mut lines).await.unwrap_err();

        assert!(matches!(err, LlmError::IncompleteStream));
    }

    #[tokio::test]
    async fn read_stream_json_lines_forwarding_emits_before_result() {
        let (mut input_write, input_read) = tokio::io::duplex(1024);
        let (mut event_write, event_read) = tokio::io::duplex(1024);

        let producer = tokio::spawn(async move {
            input_write
                .write_all(
                    concat!(
                        r#"{"type":"system","subtype":"init","model":"claude-sonnet-4-5"}"#,
                        "\n",
                        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hel"}}}"#,
                        "\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            input_write.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            input_write
                .write_all(
                    concat!(
                        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}}"#,
                        "\n",
                        r#"{"type":"result","subtype":"success","is_error":false,"result":"hello","stop_reason":"end_turn","usage":{},"duration_ms":300}"#,
                        "\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });

        let parser = tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(input_read).lines();
            read_stream_json_lines_forwarding(&mut lines, &mut event_write).await
        });

        let mut event_lines = tokio::io::BufReader::new(event_read).lines();
        let first = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            event_lines.next_line(),
        )
        .await
        .expect("first event should arrive before final result")
        .unwrap()
        .unwrap();
        let second = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            event_lines.next_line(),
        )
        .await
        .expect("partial text should arrive before final result")
        .unwrap()
        .unwrap();

        assert_eq!(
            serde_json::from_str::<Value>(&first).unwrap()["type"],
            "start"
        );
        let second: Value = serde_json::from_str(&second).unwrap();
        assert_eq!(second["type"], "text");
        assert_eq!(second["text"], "hel");

        let output = parser.await.unwrap().unwrap();
        producer.await.unwrap();
        assert_eq!(output.events.len(), 4);
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
    fn render_user_frame_preserves_current_turn_image_blocks() {
        let image = json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": "iVBORw0KGgo="
            }
        });
        let req = make_request(
            vec![json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "What color is this?"},
                    image.clone(),
                ]
            })],
            None,
        );

        let frame_str = render_user_frame(&req);
        let frame: Value = serde_json::from_str(&frame_str).unwrap();

        assert_eq!(
            frame["message"]["content"][0]["text"],
            "What color is this?"
        );
        assert_eq!(frame["message"]["content"][1], image);
    }

    #[test]
    fn render_user_frame_does_not_copy_history_images_into_current_turn() {
        let image = json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": "iVBORw0KGgo="
            }
        });
        let req = make_request(
            vec![
                json!({"role": "user", "content": [image]}),
                json!({"role": "assistant", "content": "It is red."}),
                json!({"role": "user", "content": "What did you say?"}),
            ],
            None,
        );

        let frame_str = render_user_frame(&req);
        let frame: Value = serde_json::from_str(&frame_str).unwrap();

        assert_eq!(frame["message"]["content"].as_array().unwrap().len(), 1);
        assert_eq!(frame["message"]["content"][0]["text"], "What did you say?");
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
    fn render_user_frame_merges_trailing_system_after_user() {
        let req = make_request(
            vec![
                json!({"role": "user", "content": "first question"}),
                json!({"role": "assistant", "content": "first answer"}),
                json!({"role": "user", "content": "summarize now"}),
                json!({"role": "system", "content": "use the compaction format"}),
            ],
            None,
        );
        let frame_str = render_user_frame(&req);
        let frame: Value = serde_json::from_str(&frame_str).unwrap();
        let text = frame_content_text(&frame);
        assert!(text.contains("summarize now"));
        assert!(text.contains("<system_instruction>use the compaction format</system_instruction>"));
    }

    #[test]
    fn render_user_frame_sends_trailing_system_as_current_turn() {
        let req = make_request(
            vec![
                json!({"role": "user", "content": "first question"}),
                json!({"role": "assistant", "content": "first answer"}),
                json!({"role": "system", "content": "heartbeat now"}),
            ],
            None,
        );
        let frame_str = render_user_frame(&req);
        let frame: Value = serde_json::from_str(&frame_str).unwrap();
        assert_eq!(
            frame["message"]["content"][0]["text"],
            "<system_instruction>heartbeat now</system_instruction>"
        );
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
    fn render_system_prompt_keeps_history_before_trailing_system_turn() {
        let req = make_request(
            vec![
                json!({"role": "user", "content": "first question"}),
                json!({"role": "assistant", "content": "first answer"}),
                json!({"role": "system", "content": "heartbeat now"}),
            ],
            None,
        );
        let file = render_system_prompt(&req).unwrap();
        let text = std::fs::read_to_string(file.path()).unwrap();
        assert!(text.contains("first question"));
        assert!(text.contains("first answer"));
        assert!(
            !text.contains("heartbeat now"),
            "trailing system instruction goes through stdin as current turn"
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
