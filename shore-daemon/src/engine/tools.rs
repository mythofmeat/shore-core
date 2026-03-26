use serde_json::{json, Value};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::llm_client::stream::{CacheContext, StreamConsumer};
use crate::llm_client::types::{LlmRequest, StreamResult};
use crate::llm_client::{LlmClient, LlmError};
use crate::tools::{self as tool_system, ToolContext};
use shore_protocol::server_msg::{ServerMessage, ToolCall, ToolResult as SwpToolResult};

// ── Errors ──────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("LLM error during tool loop: {0}")]
    Llm(#[from] LlmError),
}

// ── Tool loop ───────────────────────────────────────────────────────────

/// Run the tool use agentic loop.
///
/// If the initial stream result has `finish_reason == "tool_use"`, executes
/// the requested tools via the unified `dispatch_tool()` system, appends
/// results to the request messages, and calls the LLM again. Repeats until
/// `finish_reason != "tool_use"` or `max_iterations` is reached.
pub async fn run_tool_loop(
    client: &LlmClient,
    push_tx: &broadcast::Sender<ServerMessage>,
    request: &mut LlmRequest,
    mut result: StreamResult,
    ctx: &dyn ToolContext,
    max_iterations: u32,
    cache_ctx: &CacheContext,
) -> Result<StreamResult, ToolError> {
    let consumer = StreamConsumer::new(push_tx.clone());

    for iteration in 0..max_iterations {
        if result.finish_reason != "tool_use" || result.tool_uses.is_empty() {
            return Ok(result);
        }

        info!(
            iteration = iteration + 1,
            max = max_iterations,
            tool_count = result.tool_uses.len(),
            "Tool loop iteration"
        );

        // Build assistant message with tool use content blocks.
        let mut assistant_content: Vec<Value> = Vec::new();
        if !result.content.is_empty() {
            assistant_content.push(json!({
                "type": "text",
                "text": result.content,
            }));
        }
        for tool_use in &result.tool_uses {
            assistant_content.push(json!({
                "type": "tool_use",
                "id": tool_use.id,
                "name": tool_use.name,
                "input": tool_use.input,
            }));
        }
        request.messages.push(json!({
            "role": "assistant",
            "content": assistant_content,
        }));

        // Execute each tool and collect results.
        let mut tool_results: Vec<Value> = Vec::new();
        for tool_use in &result.tool_uses {
            // Push ToolCall event to SWP clients.
            let _ = push_tx.send(ServerMessage::ToolCall(ToolCall {
                tool_id: tool_use.id.clone(),
                tool_name: tool_use.name.clone(),
                input: tool_use.input.clone(),
            }));

            debug!(
                tool_id = %tool_use.id,
                tool_name = %tool_use.name,
                "Executing tool"
            );

            // Dispatch through unified tool system.
            let dispatch_result =
                tool_system::dispatch_tool(&tool_use.name, tool_use.input.clone(), ctx).await;

            let (output_str, is_error) = match dispatch_result {
                Ok(value) => {
                    // Convert Value to string for the tool result
                    let s = if let Some(s) = value.as_str() {
                        s.to_string()
                    } else {
                        serde_json::to_string(&value).unwrap_or_default()
                    };
                    (s, false)
                }
                Err(e) => (e.to_string(), true),
            };

            // Push ToolResult event to SWP clients.
            let _ = push_tx.send(ServerMessage::ToolResult(SwpToolResult {
                tool_id: tool_use.id.clone(),
                tool_name: tool_use.name.clone(),
                output: output_str.clone(),
                is_error,
            }));

            debug!(
                tool_id = %tool_use.id,
                tool_name = %tool_use.name,
                is_error,
                "Tool completed"
            );

            let mut result_block = json!({
                "type": "tool_result",
                "tool_use_id": tool_use.id,
                "content": output_str,
            });
            if is_error {
                result_block["is_error"] = json!(true);
            }
            tool_results.push(result_block);
        }

        // Append tool results as user message.
        request.messages.push(json!({
            "role": "user",
            "content": tool_results,
        }));

        // Call LLM again with the extended conversation.
        let mut reader = client.stream_raw(request, None).await?;
        result = consumer.consume(&mut reader, false, cache_ctx).await?;
    }

    warn!(
        max_iterations,
        "Tool loop hit max iterations, returning last result"
    );
    Ok(result)
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::models::{ResolvedModel, Sdk};
    use crate::llm_client::types::ToolUseEvent;
    use crate::memory::agent::types::{AgentError, AgentRag, RagHit};
    use crate::memory::agent::{CallerIdentity, MemoryAgent};
    use crate::memory::agent_llm::MockAgentLlm;
    use crate::memory::db::MemoryDB;
    use std::future::Future;
    use std::pin::Pin;
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixListener;

    fn test_model() -> ResolvedModel {
        ResolvedModel {
            name: "test".into(),
            qualified_name: "chat.test".into(),
            category: "chat".into(),
            provider_key: "anthropic".into(),
            sdk: Sdk::Anthropic,
            model_id: "claude-test".into(),
            api_key_env: Some("TEST_KEY".into()),
            base_url: None,
            max_context_tokens: None,
            max_tokens: Some(4096),
            temperature: Some(0.7),
            top_p: None,
            reasoning_effort: None,
            budget_tokens: None,
            cache_ttl: None,
            cache_control_depth: None,
            keepalive_enabled: None,
            openrouter_provider: None,
            vertex_project: None,
            vertex_location: None,
            gemini_generation: None,
            gemini_web_search: None,
        }
    }

    struct MockRag;
    impl AgentRag for MockRag {
        fn query(
            &self,
            _: &str,
            _: usize,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<RagHit>, AgentError>> + Send + '_>> {
            Box::pin(async { Ok(vec![]) })
        }
    }

    struct TestToolContext {
        db: MemoryDB,
        agent: MemoryAgent,
        agent_llm: MockAgentLlm,
        model: ResolvedModel,
        rag: MockRag,
    }

    impl TestToolContext {
        fn new() -> Self {
            Self {
                db: MemoryDB::open_in_memory().unwrap(),
                agent: MemoryAgent::one_shot(CallerIdentity::Char, "Test", "User"),
                agent_llm: MockAgentLlm::new(vec![]),
                model: test_model(),
                rag: MockRag,
            }
        }
    }

    impl ToolContext for TestToolContext {
        fn memory_db(&self) -> &MemoryDB { &self.db }
        fn memory_agent(&self) -> &MemoryAgent { &self.agent }
        fn agent_llm(&self) -> &dyn crate::memory::agent_llm::AgentLlm { &self.agent_llm }
        fn agent_model(&self) -> &ResolvedModel { &self.model }
        fn researcher_llm(&self) -> Option<&dyn crate::memory::agent_llm::AgentLlm> { None }
        fn researcher_model(&self) -> Option<&ResolvedModel> { None }
        fn memory_researcher(&self) -> Option<&crate::memory::researcher::MemoryResearcher> { None }
        fn indexer(&self) -> Option<&dyn crate::memory::agent::types::AgentIndexer> { None }
        fn rag(&self) -> &dyn AgentRag { &self.rag }
        fn image_dir(&self) -> &str { "/tmp/test_images" }
    }

    // ── Tool loop ───────────────────────────────────────────────────

    #[test]
    fn tool_loop_returns_immediately_on_end_turn() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let client = LlmClient::new("/tmp/unused.sock".into());
            let (push_tx, _rx) = broadcast::channel(16);
            let ctx = TestToolContext::new();
            let cache_ctx = CacheContext::default();

            let mut request = LlmRequest {
                provider: "anthropic".into(),
                model: "test".into(),
                api_key: "sk-test".into(),
                base_url: None,
                messages: vec![],
                system: None,
                tools: None,
                max_tokens: 4096,
                temperature: None,
                top_p: None,
                provider_options: None,
            };

            let result = StreamResult {
                content: "Hello".into(),
                model: "test".into(),
                finish_reason: "end_turn".into(),
                usage: Default::default(),
                timing: Default::default(),
                tool_uses: vec![],
            };

            let out = run_tool_loop(
                &client,
                &push_tx,
                &mut request,
                result,
                &ctx,
                10,
                &cache_ctx,
            )
            .await
            .unwrap();

            assert_eq!(out.finish_reason, "end_turn");
            assert_eq!(out.content, "Hello");
        });
    }

    #[tokio::test]
    async fn tool_loop_executes_tool_and_continues() {
        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("mock-llm.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = tokio::io::split(stream);
            let mut buf = vec![0u8; 16384];
            let _ = tokio::io::AsyncReadExt::read(&mut reader, &mut buf).await;

            let response = "HTTP/1.0 200 OK\r\n\
                            Content-Type: application/x-ndjson\r\n\
                            \r\n\
                            {\"type\":\"start\",\"model\":\"test\"}\n\
                            {\"type\":\"text\",\"text\":\"The current time is shown above.\"}\n\
                            {\"type\":\"done\",\"content\":\"The current time is shown above.\",\"finish_reason\":\"end_turn\",\"usage\":{\"input_tokens\":20,\"output_tokens\":10},\"timing\":{\"total_ms\":200}}\n";
            writer.write_all(response.as_bytes()).await.unwrap();
            writer.shutdown().await.unwrap();
        });

        let client = LlmClient::new(socket_path);
        let (push_tx, mut push_rx) = broadcast::channel(64);
        let ctx = TestToolContext::new();
        let cache_ctx = CacheContext::default();

        let mut request = LlmRequest {
            provider: "anthropic".into(),
            model: "test".into(),
            api_key: "sk-test".into(),
            base_url: None,
            messages: vec![json!({"role": "user", "content": "What time is it?"})],
            system: None,
            tools: None,
            max_tokens: 4096,
            temperature: None,
            top_p: None,
            provider_options: None,
        };

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
        };

        let result = run_tool_loop(
            &client,
            &push_tx,
            &mut request,
            initial,
            &ctx,
            10,
            &cache_ctx,
        )
        .await
        .unwrap();

        assert_eq!(result.finish_reason, "end_turn");
        assert_eq!(result.content, "The current time is shown above.");

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
                assert!(res.output.contains('T'));
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }

        let ss = push_rx.try_recv().unwrap();
        assert!(matches!(ss, ServerMessage::StreamStart(_)));
        let sc = push_rx.try_recv().unwrap();
        assert!(matches!(sc, ServerMessage::StreamChunk(_)));
        let se = push_rx.try_recv().unwrap();
        assert!(matches!(se, ServerMessage::StreamEnd(_)));

        assert_eq!(request.messages.len(), 3);
        assert_eq!(request.messages[1]["content"][0]["name"], "check_time");
        assert_eq!(request.messages[2]["content"][0]["type"], "tool_result");

        server.await.unwrap();
    }

    #[tokio::test]
    async fn tool_loop_respects_max_iterations() {
        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("mock-llm-loop.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let server = tokio::spawn(async move {
            for _ in 0..3 {
                let (stream, _) = listener.accept().await.unwrap();
                let (mut reader, mut writer) = tokio::io::split(stream);
                let mut buf = vec![0u8; 16384];
                let _ = tokio::io::AsyncReadExt::read(&mut reader, &mut buf).await;

                let response = "HTTP/1.0 200 OK\r\n\r\n\
                    {\"type\":\"start\",\"model\":\"test\"}\n\
                    {\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"check_time\",\"input\":{}}\n\
                    {\"type\":\"done\",\"content\":\"\",\"finish_reason\":\"tool_use\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5},\"timing\":{\"total_ms\":50}}\n";
                writer.write_all(response.as_bytes()).await.unwrap();
                writer.shutdown().await.unwrap();
            }
        });

        let client = LlmClient::new(socket_path);
        let (push_tx, _rx) = broadcast::channel(64);
        let ctx = TestToolContext::new();
        let cache_ctx = CacheContext::default();

        let mut request = LlmRequest {
            provider: "anthropic".into(),
            model: "test".into(),
            api_key: "sk-test".into(),
            base_url: None,
            messages: vec![],
            system: None,
            tools: None,
            max_tokens: 4096,
            temperature: None,
            top_p: None,
            provider_options: None,
        };

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
        };

        let result = run_tool_loop(
            &client,
            &push_tx,
            &mut request,
            initial,
            &ctx,
            3,
            &cache_ctx,
        )
        .await
        .unwrap();

        assert_eq!(result.finish_reason, "tool_use");
        server.await.unwrap();
    }
}
