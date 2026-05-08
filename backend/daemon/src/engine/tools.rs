use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{json, Value};
use tokio::sync::mpsc;
use tracing::{debug, info, instrument, warn};

use crate::tools::{self as tool_system, ToolContext};
use shore_diagnostics::{self as diagnostics, Diagnostics};
use shore_ledger::{CallType, LedgerClient};
use shore_llm::stream::StreamConsumer;
use shore_llm::types::{LlmRequest, StreamResult};
use shore_llm::LlmError;
use shore_protocol::server_msg::{SendImage, ServerMessage, ToolCall, ToolResult as SwpToolResult};
use shore_protocol::types::{derive_content_from_blocks, ContentBlock, ImageRef, Message, Role};

// ── Errors ──────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ToolLoopError {
    #[error("LLM error during tool loop: {0}")]
    Llm(#[from] LlmError),
}

/// Result of the tool loop: the final LLM response plus any intermediate
/// messages (assistant tool_use + user tool_result) that should be persisted.
pub struct ToolLoopResult {
    /// The final stream result from the last LLM call.
    pub result: StreamResult,
    /// Intermediate messages generated during the tool loop, in order.
    /// These are assistant messages (with tool_use blocks) and user messages
    /// (with tool_result blocks) that should be persisted to the conversation.
    pub intermediate_messages: Vec<Message>,
}

// ── Tool loop ───────────────────────────────────────────────────────────

/// Run the tool use agentic loop.
///
/// If the initial stream result has `finish_reason == "tool_use"`, executes
/// the requested tools via the unified `dispatch_tool()` system, appends
/// results to the request messages, and calls the LLM again. Repeats until
/// `finish_reason != "tool_use"` or `max_iterations` is reached.
///
/// Returns both the final result and any intermediate messages for persistence.
#[instrument(skip(client, direct_tx, request, result, ctx, diag), fields(char = character, max_iterations))]
#[allow(clippy::too_many_arguments)]
pub async fn run_tool_loop(
    client: &LedgerClient,
    direct_tx: &mpsc::Sender<ServerMessage>,
    request: &mut LlmRequest,
    mut result: StreamResult,
    ctx: &dyn ToolContext,
    max_iterations: u32,
    diag: &Arc<Mutex<Diagnostics>>,
    character: &str,
    thinking_enabled: bool,
) -> Result<ToolLoopResult, ToolLoopError> {
    let consumer = StreamConsumer::new(direct_tx.clone(), request.rid.clone());
    let mut intermediate_messages: Vec<Message> = Vec::new();

    for iteration in 0..max_iterations {
        if result.finish_reason != "tool_use" || result.tool_uses.is_empty() {
            return Ok(ToolLoopResult {
                result,
                intermediate_messages,
            });
        }

        // Emit StreamEnd for the prior LLM phase now that we've decided to
        // continue with tool execution. Intermediate phases are emitted
        // immediately (clients need the boundary to render tool calls); only
        // the FINAL phase's StreamEnd is deferred until after persistence.
        // is_final=false so aggregating clients (collect_stream) know to keep
        // reading past this boundary.
        shore_llm::stream::emit_stream_end(
            direct_tx,
            request.rid.clone(),
            &result,
            false,
            None,
            None,
        )
        .await;

        info!(
            iteration = iteration + 1,
            max = max_iterations,
            tool_count = result.tool_uses.len(),
            "Tool loop iteration"
        );

        // Build content blocks for the assistant message.
        // Use the accumulated content_blocks from streaming if available,
        // otherwise fall back to constructing from content + tool_uses.
        let assistant_blocks = if result.content_blocks.is_empty() {
            let mut blocks = Vec::new();
            if !result.content.is_empty() {
                blocks.push(ContentBlock::Text {
                    text: result.content.clone(),
                });
            }
            for tu in &result.tool_uses {
                blocks.push(ContentBlock::ToolUse {
                    id: tu.id.clone(),
                    name: tu.name.clone(),
                    input: tu.input.clone(),
                });
            }
            blocks
        } else {
            result.content_blocks.clone()
        };

        // Build LLM payload from content blocks. Provider adapters decide how
        // to project unsigned reasoning for SDKs that can accept it.
        let assistant_content: Vec<Value> = assistant_blocks
            .iter()
            .filter_map(|block| {
                crate::content_util::content_block_to_request_json_for_sdk(block, &request.sdk)
            })
            .collect();

        request.messages.push(json!({
            "role": "assistant",
            "content": assistant_content,
        }));

        // Persist assistant message with content_blocks.
        intermediate_messages.push(Message {
            msg_id: format!("m_{}", uuid::Uuid::new_v4()),
            role: Role::Assistant,
            content: derive_content_from_blocks(&assistant_blocks),
            images: vec![],
            content_blocks: assistant_blocks,
            alt_index: None,
            alt_count: None,
            alternatives: vec![],
            timestamp: chrono::Local::now().to_rfc3339(),
        });

        // Execute each tool and collect results.
        let mut tool_results: Vec<Value> = Vec::new();
        let mut tool_result_blocks: Vec<ContentBlock> = Vec::new();

        for tool_use in &result.tool_uses {
            // Push ToolCall event to SWP clients.
            let _ = direct_tx
                .send(ServerMessage::ToolCall(ToolCall {
                    rid: request.rid.clone(),
                    tool_id: tool_use.id.clone(),
                    tool_name: tool_use.name.clone(),
                    input: tool_use.input.clone(),
                }))
                .await;

            debug!(
                tool_id = %tool_use.id,
                tool_name = %tool_use.name,
                "Executing tool"
            );

            // Dispatch through unified tool system.
            let dispatch_start = Instant::now();
            let dispatch_result =
                tool_system::dispatch_tool(&tool_use.name, tool_use.input.clone(), ctx).await;
            let dispatch_ms = dispatch_start.elapsed().as_millis() as u64;

            let (output_str, is_error, ok_value) = match dispatch_result {
                Ok(value) => {
                    let s = if let Some(s) = value.as_str() {
                        s.to_string()
                    } else {
                        serde_json::to_string(&value).unwrap_or_default()
                    };
                    (s, false, Some(value))
                }
                Err(e) => (e.to_string(), true, None),
            };

            // `generate_image` produces a structured result whose `path`
            // should surface as an actual image attachment, not just a
            // tool-result string. Attach it to the assistant message that
            // issued the call (so log replay renders it inline) and broadcast
            // a SendImage event for live clients (TUI image cache, matrix
            // bridge collector).
            if !is_error && tool_use.name == "generate_image" {
                if let Some(value) = &ok_value {
                    if let Some(path) = value.get("path").and_then(|v| v.as_str()) {
                        let caption = value
                            .get("caption")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let image_ref = ImageRef {
                            path: path.to_string(),
                            caption: caption.clone(),
                            data: None,
                        };
                        if let Some(last) = intermediate_messages.last_mut() {
                            last.images.push(image_ref);
                        }
                        let _ = direct_tx
                            .send(ServerMessage::SendImage(SendImage {
                                rid: request.rid.clone(),
                                path: path.to_string(),
                                caption,
                                data: crate::handler::image_data_for_path(path),
                            }))
                            .await;
                    }
                }
            }

            // Record tool call in diagnostics ring buffer.
            {
                let input_str = serde_json::to_string(&tool_use.input).unwrap_or_default();
                let entry = diagnostics::ToolCallEntry {
                    timestamp: chrono::Local::now().to_rfc3339(),
                    tool_name: tool_use.name.clone(),
                    tool_id: tool_use.id.clone(),
                    success: !is_error,
                    duration_ms: dispatch_ms,
                    input_summary: diagnostics::truncate_summary(&input_str, 200),
                    output_summary: diagnostics::truncate_summary(&output_str, 200),
                };
                diag.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .tool_calls
                    .push(entry);
            }

            // Push ToolResult event to SWP clients.
            let _ = direct_tx
                .send(ServerMessage::ToolResult(SwpToolResult {
                    rid: request.rid.clone(),
                    tool_id: tool_use.id.clone(),
                    tool_name: tool_use.name.clone(),
                    output: output_str.clone(),
                    is_error,
                }))
                .await;

            debug!(
                tool_id = %tool_use.id,
                tool_name = %tool_use.name,
                is_error,
                "Tool completed"
            );

            // Content block for persistence.
            tool_result_blocks.push(ContentBlock::ToolResult {
                tool_use_id: tool_use.id.clone(),
                content: output_str.clone(),
                is_error,
            });

            // JSON for LLM payload.
            tool_results.push(crate::content_util::build_tool_result_json(
                &tool_use.id,
                &output_str,
                is_error,
            ));
        }

        // Append tool results as user message to LLM payload.
        request.messages.push(json!({
            "role": "user",
            "content": tool_results,
        }));

        // Persist user message with tool_result content_blocks.
        intermediate_messages.push(Message {
            msg_id: format!("m_{}", uuid::Uuid::new_v4()),
            role: Role::User,
            content: derive_content_from_blocks(&tool_result_blocks),
            images: vec![],
            content_blocks: tool_result_blocks,
            alt_index: None,
            alt_count: None,
            alternatives: vec![],
            timestamp: chrono::Local::now().to_rfc3339(),
        });

        // Call LLM again with the extended conversation. The Anthropic
        // provider re-resolves cache breakpoints on stable tool-result
        // boundaries so completed tool work can be reused by later iterations.
        let mut ledger_stream = client
            .stream_raw(request, CallType::ToolLoop, character, thinking_enabled)
            .await?;
        match consumer.consume(ledger_stream.reader_mut(), false).await {
            Ok(r) => {
                ledger_stream.finalize(&r);
                result = r;
            }
            Err(e) => {
                ledger_stream.finalize_error();
                return Err(e.into());
            }
        }
    }

    warn!(
        max_iterations,
        "Tool loop hit max iterations, returning last result"
    );
    Ok(ToolLoopResult {
        result,
        intermediate_messages,
    })
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestToolContext;
    use shore_llm::types::ToolUseEvent;
    use shore_llm::LlmClient;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    fn test_ledger_client(tmp: &tempfile::TempDir) -> LedgerClient {
        LedgerClient::new(LlmClient::new(), &tmp.path().join("ledger.db")).unwrap()
    }

    fn test_diag() -> Arc<Mutex<Diagnostics>> {
        Arc::new(Mutex::new(Diagnostics::default()))
    }

    /// Build a mock Anthropic SSE response that returns a text completion with end_turn.
    fn sse_text_end_turn(text: &str) -> String {
        format!(
            "event: message_start\n\
             data: {{\"type\":\"message_start\",\"message\":{{\"model\":\"test\",\"usage\":{{\"input_tokens\":20}}}}}}\n\n\
             event: content_block_start\n\
             data: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n\
             event: content_block_delta\n\
             data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{text}\"}}}}\n\n\
             event: content_block_stop\n\
             data: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
             event: message_delta\n\
             data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":10}}}}\n\n\
             event: message_stop\n\
             data: {{\"type\":\"message_stop\"}}\n\n"
        )
    }

    /// Build a mock Anthropic SSE response that returns a tool_use with end_turn.
    fn sse_tool_use(tool_id: &str, tool_name: &str) -> String {
        format!(
            "event: message_start\n\
             data: {{\"type\":\"message_start\",\"message\":{{\"model\":\"test\",\"usage\":{{\"input_tokens\":10}}}}}}\n\n\
             event: content_block_start\n\
             data: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"tool_use\",\"id\":\"{tool_id}\",\"name\":\"{tool_name}\"}}}}\n\n\
             event: content_block_delta\n\
             data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":\"{{}}\"}}}}\n\n\
             event: content_block_stop\n\
             data: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
             event: message_delta\n\
             data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"tool_use\"}},\"usage\":{{\"output_tokens\":5}}}}\n\n\
             event: message_stop\n\
             data: {{\"type\":\"message_stop\"}}\n\n"
        )
    }

    /// Spawn a mock HTTP server that serves an SSE response for each connection.
    /// Returns the base URL (e.g. "http://127.0.0.1:PORT") and the server handle.
    async fn mock_sse_server(
        sse_body: String,
        accept_count: usize,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base_url = format!("http://127.0.0.1:{port}");

        let handle = tokio::spawn(async move {
            for _ in 0..accept_count {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (mut reader, mut writer) = stream.split();

                // Drain the HTTP request.
                let mut buf = vec![0u8; 16384];
                let _ = tokio::io::AsyncReadExt::read(&mut reader, &mut buf).await;

                let response = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: text/event-stream\r\n\
                     \r\n\
                     {sse_body}"
                );
                writer.write_all(response.as_bytes()).await.unwrap();
                writer.shutdown().await.unwrap();
            }
        });

        (base_url, handle)
    }

    /// Build a test LlmRequest pointing at a mock server.
    fn test_request(base_url: &str, messages: Vec<Value>) -> LlmRequest {
        LlmRequest {
            sdk: shore_config::models::Sdk::Anthropic,
            model: "test".into(),
            api_key: "sk-test".into(),
            base_url: Some(base_url.to_string()),
            messages,
            system: None,
            tools: None,
            max_tokens: 4096,
            temperature: None,
            top_p: None,
            provider_options: None,
            provider_key: None,
            rid: None,
            forensic_character: None,
        }
    }

    // ── Tool loop ───────────────────────────────────────────────────

    #[test]
    fn tool_loop_returns_immediately_on_end_turn() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tmp = tempfile::tempdir().unwrap();
            let client = test_ledger_client(&tmp);
            let (push_tx, _rx) = mpsc::channel(16);
            let ctx = TestToolContext::new();

            let mut request = test_request("http://unused", vec![]);

            let result = StreamResult {
                content: "Hello".into(),
                model: "test".into(),
                finish_reason: "end_turn".into(),
                usage: Default::default(),
                timing: Default::default(),
                tool_uses: vec![],
                content_blocks: vec![],
            };

            let out = run_tool_loop(
                &client,
                &push_tx,
                &mut request,
                result,
                &ctx,
                10,
                &test_diag(),
                "test",
                false,
            )
            .await
            .unwrap();

            assert_eq!(out.result.finish_reason, "end_turn");
            assert_eq!(out.result.content, "Hello");
        });
    }

    #[tokio::test]
    async fn tool_loop_executes_tool_and_continues() {
        let sse = sse_text_end_turn("The current time is shown above.");
        let (base_url, server) = mock_sse_server(sse, 1).await;

        let tmp = tempfile::tempdir().unwrap();
        let client = test_ledger_client(&tmp);
        let (push_tx, mut push_rx) = mpsc::channel(64);
        let ctx = TestToolContext::new();

        let mut request = test_request(
            &base_url,
            vec![json!({"role": "user", "content": "What time is it?"})],
        );

        let initial = StreamResult {
            content: String::new(),
            model: "test".into(),
            finish_reason: "tool_use".into(),
            usage: Default::default(),
            timing: Default::default(),
            tool_uses: vec![ToolUseEvent {
                id: "t1".into(),
                name: "check_time".into(),
                input: json!({}),
            }],
            content_blocks: vec![],
        };

        let result = run_tool_loop(
            &client,
            &push_tx,
            &mut request,
            initial,
            &ctx,
            10,
            &test_diag(),
            "test",
            false,
        )
        .await
        .unwrap();

        assert_eq!(result.result.finish_reason, "end_turn");
        assert_eq!(result.result.content, "The current time is shown above.");

        // Intermediate messages should have been generated.
        assert_eq!(result.intermediate_messages.len(), 2);
        assert_eq!(result.intermediate_messages[0].role, Role::Assistant);
        assert_eq!(result.intermediate_messages[1].role, Role::User);
        assert!(!result.intermediate_messages[0].content_blocks.is_empty());
        assert!(!result.intermediate_messages[1].content_blocks.is_empty());

        // The intermediate StreamEnd (for the initial tool_use phase) is now
        // emitted by run_tool_loop itself before the tool dispatch — clients
        // need this boundary to render tool calls. The FINAL StreamEnd is
        // deferred to the caller (after persistence), so it is NOT in this
        // event sequence.
        let intermediate_end = push_rx.try_recv().unwrap();
        match intermediate_end {
            ServerMessage::StreamEnd(end) => {
                assert_eq!(end.finish_reason, "tool_use");
            }
            other => panic!("Expected intermediate StreamEnd(tool_use), got {:?}", other),
        }

        let tc = push_rx.try_recv().unwrap();
        match tc {
            ServerMessage::ToolCall(call) => {
                assert_eq!(call.tool_id, "t1");
                assert_eq!(call.tool_name, "check_time");
            }
            other => panic!("Expected ToolCall, got {:?}", other),
        }

        let tr = push_rx.try_recv().unwrap();
        match tr {
            ServerMessage::ToolResult(res) => {
                assert_eq!(res.tool_id, "t1");
                assert_eq!(res.tool_name, "check_time");
                assert!(!res.is_error);
                assert!(
                    res.output.contains(" at "),
                    "expected friendly format: {}",
                    res.output
                );
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }

        let ss = push_rx.try_recv().unwrap();
        assert!(matches!(ss, ServerMessage::StreamStart(_)));
        let sc = push_rx.try_recv().unwrap();
        assert!(matches!(sc, ServerMessage::StreamChunk(_)));
        // No final StreamEnd from the consumer — caller emits it post-persist.
        assert!(
            push_rx.try_recv().is_err(),
            "tool loop must not emit the final StreamEnd; caller emits after persist"
        );

        assert_eq!(request.messages.len(), 3);
        assert_eq!(request.messages[1]["content"][0]["name"], "check_time");
        assert_eq!(request.messages[2]["content"][0]["type"], "tool_result");

        server.await.unwrap();
    }

    #[tokio::test]
    async fn tool_loop_respects_max_iterations() {
        let sse = sse_tool_use("t1", "check_time");
        let (base_url, server) = mock_sse_server(sse, 3).await;

        let tmp = tempfile::tempdir().unwrap();
        let client = test_ledger_client(&tmp);
        let (push_tx, _rx) = mpsc::channel(64);
        let ctx = TestToolContext::new();

        let mut request = test_request(&base_url, vec![]);

        let initial = StreamResult {
            content: String::new(),
            model: "test".into(),
            finish_reason: "tool_use".into(),
            usage: Default::default(),
            timing: Default::default(),
            tool_uses: vec![ToolUseEvent {
                id: "t1".into(),
                name: "check_time".into(),
                input: json!({}),
            }],
            content_blocks: vec![],
        };

        let result = run_tool_loop(
            &client,
            &push_tx,
            &mut request,
            initial,
            &ctx,
            3,
            &test_diag(),
            "test",
            false,
        )
        .await
        .unwrap();

        assert_eq!(result.result.finish_reason, "tool_use");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn tool_loop_handles_tool_error() {
        // generate_image returns a tool error when no image-generation profile is configured.
        let sse = sse_text_end_turn("Image generation is not available.");
        let (base_url, server) = mock_sse_server(sse, 1).await;

        let tmp = tempfile::tempdir().unwrap();
        let client = test_ledger_client(&tmp);
        let (push_tx, mut push_rx) = mpsc::channel(64);
        let ctx = TestToolContext::new();

        let mut request = test_request(&base_url, vec![]);

        let initial = StreamResult {
            content: String::new(),
            model: "test".into(),
            finish_reason: "tool_use".into(),
            usage: Default::default(),
            timing: Default::default(),
            tool_uses: vec![ToolUseEvent {
                id: "t_img".into(),
                name: "generate_image".into(),
                input: json!({"prompt": "a cat"}),
            }],
            content_blocks: vec![],
        };

        let result = run_tool_loop(
            &client,
            &push_tx,
            &mut request,
            initial,
            &ctx,
            10,
            &test_diag(),
            "test",
            false,
        )
        .await
        .unwrap();

        // LLM should have received the error and responded.
        assert_eq!(result.result.finish_reason, "end_turn");

        // The tool loop emits the intermediate StreamEnd(tool_use) before
        // dispatching tools.
        let intermediate_end = push_rx.try_recv().unwrap();
        assert!(matches!(intermediate_end, ServerMessage::StreamEnd(_)));

        // ToolCall event should be present.
        let tc = push_rx.try_recv().unwrap();
        assert!(matches!(tc, ServerMessage::ToolCall(_)));

        // ToolResult should have is_error = true.
        let tr = push_rx.try_recv().unwrap();
        match tr {
            ServerMessage::ToolResult(res) => {
                assert!(res.is_error);
                assert_eq!(res.tool_name, "generate_image");
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }

        // The tool_result in request.messages should also have is_error.
        let tool_result_msg = &request.messages[1]["content"][0];
        assert_eq!(tool_result_msg["is_error"], json!(true));

        server.await.unwrap();
    }

    #[test]
    fn tool_loop_text_with_tool_use() {
        // Verify that when content accompanies a tool_use, both blocks
        // appear in the assistant message.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tmp = tempfile::tempdir().unwrap();
            let client = test_ledger_client(&tmp);
            let (push_tx, _rx) = mpsc::channel(16);
            let ctx = TestToolContext::new();

            let mut request = test_request("http://unused", vec![]);

            // Result with both text content and tool_uses, but no LLM server
            // to call -- the tool_uses are empty, so it should return immediately.
            let result = StreamResult {
                content: "Let me check the time...".into(),
                model: "test".into(),
                finish_reason: "end_turn".into(),
                usage: Default::default(),
                timing: Default::default(),
                tool_uses: vec![],
                content_blocks: vec![],
            };

            let out = run_tool_loop(
                &client,
                &push_tx,
                &mut request,
                result,
                &ctx,
                10,
                &test_diag(),
                "test",
                false,
            )
            .await
            .unwrap();

            assert_eq!(out.result.content, "Let me check the time...");
        });
    }

    #[tokio::test]
    async fn tool_loop_multiple_tools_single_response() {
        let sse = sse_text_end_turn("Done.");
        let (base_url, server) = mock_sse_server(sse, 1).await;

        let tmp = tempfile::tempdir().unwrap();
        let client = test_ledger_client(&tmp);
        let (push_tx, mut push_rx) = mpsc::channel(64);
        let ctx = TestToolContext::new();

        let mut request = test_request(&base_url, vec![]);

        // Two tools in one response.
        let initial = StreamResult {
            content: String::new(),
            model: "test".into(),
            finish_reason: "tool_use".into(),
            usage: Default::default(),
            timing: Default::default(),
            tool_uses: vec![
                ToolUseEvent {
                    id: "t1".into(),
                    name: "check_time".into(),
                    input: json!({}),
                },
                ToolUseEvent {
                    id: "t2".into(),
                    name: "roll_dice".into(),
                    input: json!({"notation": "1d6"}),
                },
            ],
            content_blocks: vec![],
        };

        let result = run_tool_loop(
            &client,
            &push_tx,
            &mut request,
            initial,
            &ctx,
            10,
            &test_diag(),
            "test",
            false,
        )
        .await
        .unwrap();

        assert_eq!(result.result.finish_reason, "end_turn");

        // Drain the intermediate StreamEnd(tool_use) emitted before the tool dispatch.
        let intermediate_end = push_rx.try_recv().unwrap();
        assert!(matches!(intermediate_end, ServerMessage::StreamEnd(_)));

        // Should have ToolCall + ToolResult for each tool (4 events), then stream events.
        let mut tool_calls = vec![];
        let mut tool_results = vec![];
        for _ in 0..4 {
            match push_rx.try_recv().unwrap() {
                ServerMessage::ToolCall(tc) => tool_calls.push(tc),
                ServerMessage::ToolResult(tr) => tool_results.push(tr),
                other => panic!("Unexpected event: {:?}", other),
            }
        }

        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_results.len(), 2);
        assert_eq!(tool_calls[0].tool_name, "check_time");
        assert_eq!(tool_calls[1].tool_name, "roll_dice");
        assert!(!tool_results[0].is_error);
        assert!(!tool_results[1].is_error);

        // The request should have: assistant msg (with 2 tool_use blocks) + user msg (with 2 tool_result blocks).
        assert_eq!(request.messages.len(), 2);
        let assistant_content = request.messages[0]["content"].as_array().unwrap();
        assert_eq!(assistant_content.len(), 2); // 2 tool_use blocks (no text)
        let user_content = request.messages[1]["content"].as_array().unwrap();
        assert_eq!(user_content.len(), 2); // 2 tool_result blocks

        server.await.unwrap();
    }
}
