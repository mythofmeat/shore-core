//! Daemon-side glue for requests routed through the `claude_code` provider.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{json, Value};
use shore_config::models::Sdk;
use shore_llm::types::{GenerateResponse, LlmRequest};
use shore_protocol::types::ContentBlock;

use crate::engine::mcp_session::{ImageAttachment, LedgerEntry, McpSessionGuard};
use crate::http::DaemonHttpState;
use crate::tools::ToolContext;

pub(crate) struct EmptyToolContext;

impl ToolContext for EmptyToolContext {
    fn image_dir(&self) -> &str {
        ""
    }

    fn llm_client(&self) -> Option<&shore_llm::LlmClient> {
        None
    }

    fn image_gen_config(&self) -> Option<&crate::memory::compaction_impls::ImageGenConfig> {
        None
    }

    fn search_config(&self) -> &shore_config::app::SearchConfig {
        static CONFIG: std::sync::OnceLock<shore_config::app::SearchConfig> =
            std::sync::OnceLock::new();
        CONFIG.get_or_init(shore_config::app::SearchConfig::default)
    }
}

pub(crate) fn empty_tool_context() -> Arc<dyn ToolContext + Send + Sync> {
    Arc::new(EmptyToolContext)
}

pub(crate) async fn prepare_request(
    request: &mut LlmRequest,
    http: Option<&Arc<DaemonHttpState>>,
    subprocess_key: Option<String>,
    tool_ctx: Arc<dyn ToolContext + Send + Sync>,
) -> Result<Option<McpSessionGuard>, String> {
    if request.sdk != Sdk::ClaudeCode {
        return Ok(None);
    }

    request.provider_key = Some(Sdk::ClaudeCode.as_str().to_string());
    let http = http.ok_or_else(|| {
        "claude_code requires [daemon.http].enabled = true so the local claude CLI can call back into shore tools"
            .to_string()
    })?;
    let tool_defs = request.tools.clone().unwrap_or_default();
    let image_attachments = collect_current_turn_image_attachments(request);
    let mut tool_defs = tool_defs;
    if !image_attachments.is_empty() {
        tool_defs.push(attached_image_tool_def());
    }
    let bare_tool_names: Vec<String> = tool_defs
        .iter()
        .filter_map(|d| d.get("name").and_then(Value::as_str).map(String::from))
        .collect();
    let allowed_bare: HashSet<String> = bare_tool_names.iter().cloned().collect();
    let allowed_for_cli: Vec<String> = bare_tool_names
        .iter()
        .map(|name| format!("mcp__shore__{name}"))
        .collect();

    let subprocess_key_for_options = subprocess_key.clone();
    let guard = if let Some(key) = subprocess_key {
        http.mcp_sessions
            .allocate_keyed(key, allowed_bare, tool_defs, tool_ctx, image_attachments)
            .await
    } else {
        http.mcp_sessions.allocate_with_attachments(
            uuid::Uuid::new_v4().to_string(),
            allowed_bare,
            tool_defs,
            tool_ctx,
            image_attachments,
        )
    };

    let session_id = guard.id().to_string();
    let mcp_endpoint = guard.endpoint(&http.base_url());
    let opts = request
        .provider_options
        .get_or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if !opts.is_object() {
        *opts = serde_json::Value::Object(serde_json::Map::new());
    }
    let map = opts.as_object_mut().expect("provider_options object");
    map.insert("mcp_endpoint".into(), json!(mcp_endpoint));
    map.insert("allowed_tools".into(), json!(allowed_for_cli));
    map.insert("session_id".into(), json!(session_id));
    map.remove("subprocess_key");
    if let Some(key) = subprocess_key_for_options {
        map.insert("subprocess_key".into(), json!(key));
    }

    Ok(Some(guard))
}

pub(crate) const ATTACHED_IMAGE_TOOL: &str = "shore_attached_image";

fn attached_image_tool_def() -> Value {
    json!({
        "name": ATTACHED_IMAGE_TOOL,
        "description": "Inspect a current user-provided image attachment by one-based index. Use this when the prompt refers to an attached image.",
        "input_schema": {
            "type": "object",
            "properties": {
                "index": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "The one-based image attachment index from the current user message."
                }
            },
            "required": ["index"]
        }
    })
}

fn collect_current_turn_image_attachments(request: &LlmRequest) -> Vec<ImageAttachment> {
    let start = current_turn_start(&request.messages);
    request.messages[start..]
        .iter()
        .flat_map(|message| {
            message
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(image_attachment_from_block)
        .enumerate()
        .map(|(i, (media_type, data))| ImageAttachment {
            index: i + 1,
            media_type,
            data,
        })
        .collect()
}

fn image_attachment_from_block(block: &Value) -> Option<(String, String)> {
    if block.get("type").and_then(Value::as_str) != Some("image") {
        return None;
    }
    let source = block.get("source")?;
    let media_type = source.get("media_type").and_then(Value::as_str)?;
    if !media_type.starts_with("image/") {
        return None;
    }
    let data = source.get("data").and_then(Value::as_str)?;
    Some((media_type.to_string(), data.to_string()))
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

fn message_role(message: &Value) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

pub(crate) async fn splice_generate_response_from_session(
    response: &mut GenerateResponse,
    session: Option<&McpSessionGuard>,
) {
    let Some(session) = session else {
        return;
    };
    let ledger = session.drain().await;
    splice_generate_response(response, ledger);
}

pub(crate) fn splice_generate_response(response: &mut GenerateResponse, ledger: Vec<LedgerEntry>) {
    let ledger: Vec<LedgerEntry> = ledger
        .into_iter()
        .filter(|entry| entry.name != ATTACHED_IMAGE_TOOL)
        .collect();
    if ledger.is_empty() {
        return;
    }

    let existing = std::mem::take(&mut response.content_blocks);
    let mut spliced = Vec::new();
    let mut matched: HashSet<usize> = HashSet::new();

    if existing.is_empty() && !response.content.is_empty() {
        spliced.push(ContentBlock::Text {
            text: response.content.clone(),
        });
    }

    for block in existing {
        match block {
            ContentBlock::ToolUse { id, name, input } => {
                let bare_name = strip_mcp_tool_name(&name);
                let match_idx = ledger_match(&ledger, &matched, &id, &bare_name, &input);
                spliced.push(ContentBlock::ToolUse {
                    id: id.clone(),
                    name: bare_name,
                    input: input.clone(),
                });
                if let Some(i) = match_idx {
                    matched.insert(i);
                    spliced.push(ledger_tool_result_block(&ledger[i], &id));
                }
            }
            other => spliced.push(other),
        }
    }

    for (i, entry) in ledger.iter().enumerate() {
        if matched.contains(&i) {
            continue;
        }
        spliced.push(ContentBlock::ToolUse {
            id: entry.tool_use_id.clone(),
            name: entry.name.clone(),
            input: entry.input.clone(),
        });
        spliced.push(ledger_tool_result_block(entry, &entry.tool_use_id));
    }

    response.content_blocks = spliced;
}

pub(crate) fn ledger_tool_result_block(entry: &LedgerEntry, tool_use_id: &str) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: tool_use_id.to_string(),
        content: entry
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        is_error: entry.is_error,
    }
}

pub(crate) fn strip_mcp_tool_name(name: &str) -> String {
    name.strip_prefix("mcp__shore__")
        .unwrap_or(name)
        .to_string()
}

fn ledger_match(
    ledger: &[LedgerEntry],
    matched: &HashSet<usize>,
    tool_use_id: &str,
    name: &str,
    input: &Value,
) -> Option<usize> {
    ledger
        .iter()
        .enumerate()
        .find(|(i, entry)| !matched.contains(i) && entry.tool_use_id == tool_use_id)
        .map(|(i, _)| i)
        .or_else(|| {
            ledger
                .iter()
                .enumerate()
                .find(|(i, entry)| {
                    !matched.contains(i) && entry.name == name && entry.input == *input
                })
                .map(|(i, _)| i)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use shore_config::models::Sdk;
    use shore_llm::types::{Timing, Usage};

    fn request(provider_options: Option<Value>) -> LlmRequest {
        LlmRequest {
            sdk: Sdk::ClaudeCode,
            model: "claude-sonnet-4-5".into(),
            api_key: String::new(),
            api_key_name: None,
            base_url: None,
            messages: vec![json!({"role": "user", "content": "hi"})],
            system: None,
            tools: Some(vec![json!({
                "name": "check_time",
                "description": "time",
                "input_schema": {"type": "object"}
            })]),
            max_tokens: 128,
            temperature: None,
            top_p: None,
            provider_options,
            provider_key: None,
            rid: None,
            forensic_character: None,
            system_suffix: None,
            retain_long: false,
        }
    }

    fn http_state() -> Arc<DaemonHttpState> {
        Arc::new(DaemonHttpState {
            bind_addr: "127.0.0.1:43210".parse().unwrap(),
            mcp_sessions: crate::engine::mcp_session::McpSessionRegistry::new(),
        })
    }

    #[tokio::test]
    async fn prepare_unkeyed_request_clears_stale_subprocess_key() {
        let http = http_state();
        let mut req = request(Some(json!({
            "subprocess_key": "chat-key",
            "reasoning_effort": "medium"
        })));

        let guard = prepare_request(&mut req, Some(&http), None, empty_tool_context())
            .await
            .unwrap()
            .unwrap();
        let opts = req.provider_options.as_ref().unwrap();

        assert_eq!(req.provider_key.as_deref(), Some("claude_code"));
        assert_eq!(
            opts["mcp_endpoint"].as_str().unwrap(),
            format!("http://127.0.0.1:43210/mcp/{}", guard.id())
        );
        assert_eq!(opts["allowed_tools"], json!(["mcp__shore__check_time"]));
        assert_eq!(opts["reasoning_effort"], "medium");
        assert!(opts.get("subprocess_key").is_none());
    }

    #[tokio::test]
    async fn prepare_keyed_request_sets_subprocess_key() {
        let http = http_state();
        let mut req = request(None);

        let _guard = prepare_request(
            &mut req,
            Some(&http),
            Some("data:alice".into()),
            empty_tool_context(),
        )
        .await
        .unwrap()
        .unwrap();
        let opts = req.provider_options.as_ref().unwrap();

        assert_eq!(opts["subprocess_key"], "data:alice");
    }

    #[tokio::test]
    async fn prepare_request_adds_private_image_attachment_tool_for_current_images() {
        let http = http_state();
        let mut req = request(None);
        req.messages = vec![json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "what color?"},
                {
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/png",
                        "data": "abc123"
                    }
                }
            ]
        })];

        let guard = prepare_request(&mut req, Some(&http), None, empty_tool_context())
            .await
            .unwrap()
            .unwrap();
        let opts = req.provider_options.as_ref().unwrap();

        assert_eq!(
            opts["allowed_tools"],
            json!(["mcp__shore__check_time", "mcp__shore__shore_attached_image"])
        );
        assert!(guard.session().allows(ATTACHED_IMAGE_TOOL));
        let attachment = guard.session().image_attachment(1).unwrap();
        assert_eq!(attachment.media_type, "image/png");
        assert_eq!(attachment.data, "abc123");
    }

    #[test]
    fn splice_generate_response_adds_tool_result_blocks() {
        let mut resp = GenerateResponse {
            content: "done".into(),
            content_blocks: vec![ContentBlock::ToolUse {
                id: "toolu_1".into(),
                name: "mcp__shore__check_time".into(),
                input: json!({}),
            }],
            finish_reason: "end_turn".into(),
            usage: Usage::default(),
            timing: Timing::default(),
            model: "claude-sonnet-4-5".into(),
        };

        splice_generate_response(
            &mut resp,
            vec![LedgerEntry {
                tool_use_id: "toolu_1".into(),
                name: "check_time".into(),
                input: json!({}),
                content: vec![ContentBlock::Text {
                    text: "noon".into(),
                }],
                is_error: false,
            }],
        );

        assert!(matches!(
            &resp.content_blocks[1],
            ContentBlock::ToolResult { tool_use_id, content, is_error }
                if tool_use_id == "toolu_1" && content == "noon" && !is_error
        ));
    }

    #[test]
    fn splice_generate_response_omits_private_image_attachment_ledger() {
        let mut resp = GenerateResponse {
            content: "red".into(),
            content_blocks: vec![ContentBlock::Text { text: "red".into() }],
            finish_reason: "end_turn".into(),
            usage: Usage::default(),
            timing: Timing::default(),
            model: "claude-sonnet-4-5".into(),
        };

        splice_generate_response(
            &mut resp,
            vec![LedgerEntry {
                tool_use_id: "toolu_image".into(),
                name: ATTACHED_IMAGE_TOOL.into(),
                input: json!({"index": 1}),
                content: vec![ContentBlock::Text {
                    text: "[image attachment 1: image/png]".into(),
                }],
                is_error: false,
            }],
        );

        assert_eq!(
            resp.content_blocks,
            vec![ContentBlock::Text { text: "red".into() }]
        );
    }
}
