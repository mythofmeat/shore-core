use std::collections::HashMap;

use rand::Rng;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::llm_client::stream::{CacheContext, StreamConsumer};
use crate::llm_client::types::{LlmRequest, StreamResult};
use crate::llm_client::{LlmClient, LlmError};
use shore_protocol::server_msg::{ServerMessage, ToolCall, ToolResult as SwpToolResult};

// ── Errors ──────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("LLM error during tool loop: {0}")]
    Llm(#[from] LlmError),
}

// ── Tool output ─────────────────────────────────────────────────────────

/// Result of executing a single tool.
pub struct ToolOutput {
    pub output: String,
    pub is_error: bool,
}

// ── Dice notation parsing ───────────────────────────────────────────────

/// Parsed dice notation (e.g., `2d6+3` → count=2, sides=6, modifier=3).
#[derive(Debug, Clone, PartialEq)]
pub struct DiceNotation {
    pub count: u32,
    pub sides: u32,
    pub modifier: i32,
}

/// Parse dice notation like `2d6+3`, `1d20`, `4d6-1`, `d8`.
pub fn parse_dice_notation(notation: &str) -> Result<DiceNotation, String> {
    let s = notation.trim().to_lowercase();

    let d_pos = s
        .find('d')
        .ok_or_else(|| format!("Missing 'd' in notation: {notation}"))?;

    // Count (before 'd'), default to 1.
    let count_str = &s[..d_pos];
    let count = if count_str.is_empty() {
        1
    } else {
        count_str
            .parse::<u32>()
            .map_err(|_| format!("Invalid dice count: {count_str}"))?
    };
    if count == 0 {
        return Err("Dice count must be at least 1".into());
    }

    // Sides and optional modifier (after 'd').
    let after_d = &s[d_pos + 1..];
    if after_d.is_empty() {
        return Err("Missing sides after 'd'".into());
    }

    // Find first +/- that isn't at position 0 (sides can't start with +/-).
    let modifier_pos = after_d
        .char_indices()
        .position(|(i, c)| i > 0 && (c == '+' || c == '-'));

    let (sides_str, modifier) = if let Some(pos) = modifier_pos {
        let byte_pos = after_d
            .char_indices()
            .nth(pos)
            .map(|(i, _)| i)
            .unwrap();
        let sides = &after_d[..byte_pos];
        let mod_str = &after_d[byte_pos..];
        let modifier = mod_str
            .parse::<i32>()
            .map_err(|_| format!("Invalid modifier: {mod_str}"))?;
        (sides, modifier)
    } else {
        (after_d, 0)
    };

    let sides = sides_str
        .parse::<u32>()
        .map_err(|_| format!("Invalid sides: {sides_str}"))?;
    if sides == 0 {
        return Err("Dice sides must be at least 1".into());
    }

    Ok(DiceNotation {
        count,
        sides,
        modifier,
    })
}

/// Roll dice according to parsed notation. Returns (individual rolls, total).
pub fn execute_dice_roll(notation: &DiceNotation) -> (Vec<u32>, i32) {
    let mut rng = rand::thread_rng();
    let rolls: Vec<u32> = (0..notation.count)
        .map(|_| rng.gen_range(1..=notation.sides))
        .collect();
    let sum: i32 = rolls.iter().map(|&r| r as i32).sum::<i32>() + notation.modifier;
    (rolls, sum)
}

// ── Tool handlers ───────────────────────────────────────────────────────

fn check_time_handler(_input: &Value) -> ToolOutput {
    let now = chrono::Local::now();
    ToolOutput {
        output: now.to_rfc3339(),
        is_error: false,
    }
}

fn roll_dice_handler(input: &Value) -> ToolOutput {
    let notation_str = match input.get("notation").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return ToolOutput {
                output: "Missing required parameter: notation".into(),
                is_error: true,
            }
        }
    };

    match parse_dice_notation(notation_str) {
        Ok(parsed) => {
            let (rolls, total) = execute_dice_roll(&parsed);
            ToolOutput {
                output: json!({
                    "notation": notation_str,
                    "rolls": rolls,
                    "total": total,
                })
                .to_string(),
                is_error: false,
            }
        }
        Err(e) => ToolOutput {
            output: format!("Invalid dice notation: {e}"),
            is_error: true,
        },
    }
}

// ── Tool registry ───────────────────────────────────────────────────────

/// Tool dispatch table mapping tool names to handler functions and definitions.
pub struct ToolRegistry {
    handlers: HashMap<String, fn(&Value) -> ToolOutput>,
    definitions: Vec<Value>,
}

impl ToolRegistry {
    /// Create a new registry with the appropriate set of tools.
    ///
    /// When `is_private` is true, memory tools are excluded from the registry.
    pub fn new(is_private: bool) -> Self {
        let mut registry = Self {
            handlers: HashMap::new(),
            definitions: Vec::new(),
        };

        // Basic tools — always available.
        registry.register(
            "check_time",
            check_time_handler,
            json!({
                "name": "check_time",
                "description": "Returns the current date and time in ISO 8601 format.",
                "input_schema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }),
        );

        registry.register(
            "roll_dice",
            roll_dice_handler,
            json!({
                "name": "roll_dice",
                "description": "Roll dice using standard dice notation (e.g., '2d6+3', '1d20', 'd8').",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "notation": {
                            "type": "string",
                            "description": "Dice notation: NdS[+/-M] where N=number of dice, S=sides, M=modifier. Examples: '2d6', '1d20+5', '4d6-1'"
                        }
                    },
                    "required": ["notation"]
                }
            }),
        );

        // Memory tools — excluded in private conversations.
        if !is_private {
            // Future: register memory_search, memory_save, etc.
        }

        registry
    }

    fn register(
        &mut self,
        name: &str,
        handler: fn(&Value) -> ToolOutput,
        definition: Value,
    ) {
        self.handlers.insert(name.to_string(), handler);
        self.definitions.push(definition);
    }

    /// Execute a tool by name. Returns an error output if the tool is not found.
    pub fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        match self.handlers.get(name) {
            Some(handler) => handler(input),
            None => ToolOutput {
                output: format!("Unknown tool: {name}"),
                is_error: true,
            },
        }
    }

    /// Tool definitions for inclusion in LLM requests.
    pub fn definitions(&self) -> &[Value] {
        &self.definitions
    }

    /// Check if a tool name is registered.
    pub fn has_tool(&self, name: &str) -> bool {
        self.handlers.contains_key(name)
    }
}

// ── Tool loop ───────────────────────────────────────────────────────────

/// Run the tool use agentic loop.
///
/// If the initial stream result has `finish_reason == "tool_use"`, executes
/// the requested tools, appends results to the request messages, and calls
/// the LLM again. Repeats until `finish_reason != "tool_use"` or
/// `max_iterations` is reached.
pub async fn run_tool_loop(
    client: &LlmClient,
    push_tx: &broadcast::Sender<ServerMessage>,
    request: &mut LlmRequest,
    mut result: StreamResult,
    registry: &ToolRegistry,
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

            let output = registry.execute(&tool_use.name, &tool_use.input);

            // Push ToolResult event to SWP clients.
            let _ = push_tx.send(ServerMessage::ToolResult(SwpToolResult {
                tool_id: tool_use.id.clone(),
                tool_name: tool_use.name.clone(),
                output: output.output.clone(),
                is_error: output.is_error,
            }));

            debug!(
                tool_id = %tool_use.id,
                tool_name = %tool_use.name,
                is_error = output.is_error,
                "Tool completed"
            );

            let mut result_block = json!({
                "type": "tool_result",
                "tool_use_id": tool_use.id,
                "content": output.output,
            });
            if output.is_error {
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
    use crate::llm_client::types::ToolUseEvent;
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixListener;

    // ── Dice notation parsing ───────────────────────────────────────

    #[test]
    fn parse_standard_notation() {
        let r = parse_dice_notation("2d6").unwrap();
        assert_eq!(
            r,
            DiceNotation {
                count: 2,
                sides: 6,
                modifier: 0
            }
        );
    }

    #[test]
    fn parse_with_positive_modifier() {
        let r = parse_dice_notation("1d20+5").unwrap();
        assert_eq!(
            r,
            DiceNotation {
                count: 1,
                sides: 20,
                modifier: 5
            }
        );
    }

    #[test]
    fn parse_with_negative_modifier() {
        let r = parse_dice_notation("4d6-1").unwrap();
        assert_eq!(
            r,
            DiceNotation {
                count: 4,
                sides: 6,
                modifier: -1
            }
        );
    }

    #[test]
    fn parse_implicit_count() {
        let r = parse_dice_notation("d8").unwrap();
        assert_eq!(
            r,
            DiceNotation {
                count: 1,
                sides: 8,
                modifier: 0
            }
        );
    }

    #[test]
    fn parse_large_notation() {
        let r = parse_dice_notation("10d10+100").unwrap();
        assert_eq!(
            r,
            DiceNotation {
                count: 10,
                sides: 10,
                modifier: 100
            }
        );
    }

    #[test]
    fn parse_case_insensitive() {
        let r = parse_dice_notation("2D6").unwrap();
        assert_eq!(
            r,
            DiceNotation {
                count: 2,
                sides: 6,
                modifier: 0
            }
        );
    }

    #[test]
    fn parse_with_whitespace() {
        let r = parse_dice_notation("  3d4+2  ").unwrap();
        assert_eq!(
            r,
            DiceNotation {
                count: 3,
                sides: 4,
                modifier: 2
            }
        );
    }

    #[test]
    fn parse_rejects_missing_d() {
        assert!(parse_dice_notation("26").is_err());
    }

    #[test]
    fn parse_rejects_zero_count() {
        assert!(parse_dice_notation("0d6").is_err());
    }

    #[test]
    fn parse_rejects_zero_sides() {
        assert!(parse_dice_notation("2d0").is_err());
    }

    #[test]
    fn parse_rejects_missing_sides() {
        assert!(parse_dice_notation("2d").is_err());
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_dice_notation("foo").is_err());
    }

    #[test]
    fn dice_roll_within_range() {
        let notation = DiceNotation {
            count: 2,
            sides: 6,
            modifier: 3,
        };
        for _ in 0..100 {
            let (rolls, total) = execute_dice_roll(&notation);
            assert_eq!(rolls.len(), 2);
            for &r in &rolls {
                assert!((1..=6).contains(&r));
            }
            // Min: 1+1+3=5, Max: 6+6+3=15
            assert!((5..=15).contains(&total));
        }
    }

    // ── Tool dispatch ───────────────────────────────────────────────

    #[test]
    fn dispatch_check_time() {
        let registry = ToolRegistry::new(false);
        assert!(registry.has_tool("check_time"));

        let output = registry.execute("check_time", &json!({}));
        assert!(!output.is_error);
        // Should be a valid RFC 3339 datetime.
        assert!(output.output.contains('T'));
    }

    #[test]
    fn dispatch_roll_dice() {
        let registry = ToolRegistry::new(false);
        assert!(registry.has_tool("roll_dice"));

        let output = registry.execute("roll_dice", &json!({"notation": "2d6"}));
        assert!(!output.is_error);

        let parsed: Value = serde_json::from_str(&output.output).unwrap();
        assert_eq!(parsed["notation"], "2d6");
        assert!(parsed["rolls"].is_array());
        assert_eq!(parsed["rolls"].as_array().unwrap().len(), 2);
        assert!(parsed["total"].is_number());
    }

    #[test]
    fn dispatch_roll_dice_invalid_notation() {
        let registry = ToolRegistry::new(false);
        let output = registry.execute("roll_dice", &json!({"notation": "garbage"}));
        assert!(output.is_error);
    }

    #[test]
    fn dispatch_roll_dice_missing_param() {
        let registry = ToolRegistry::new(false);
        let output = registry.execute("roll_dice", &json!({}));
        assert!(output.is_error);
        assert!(output.output.contains("Missing required parameter"));
    }

    #[test]
    fn dispatch_unknown_tool() {
        let registry = ToolRegistry::new(false);
        let output = registry.execute("nonexistent", &json!({}));
        assert!(output.is_error);
        assert!(output.output.contains("Unknown tool"));
    }

    // ── Tool definitions ────────────────────────────────────────────

    #[test]
    fn definitions_include_schema() {
        let registry = ToolRegistry::new(false);
        let defs = registry.definitions();

        assert_eq!(defs.len(), 2);

        let names: Vec<&str> = defs
            .iter()
            .filter_map(|d| d["name"].as_str())
            .collect();
        assert!(names.contains(&"check_time"));
        assert!(names.contains(&"roll_dice"));

        // roll_dice should have notation as required parameter.
        let roll_def = defs.iter().find(|d| d["name"] == "roll_dice").unwrap();
        let required = roll_def["input_schema"]["required"]
            .as_array()
            .unwrap();
        assert!(required.contains(&json!("notation")));
    }

    // ── Private conversation awareness ──────────────────────────────

    #[test]
    fn registry_always_has_basic_tools() {
        // Both private and non-private registries have basic tools.
        let public = ToolRegistry::new(false);
        let private = ToolRegistry::new(true);

        assert!(public.has_tool("check_time"));
        assert!(public.has_tool("roll_dice"));
        assert!(private.has_tool("check_time"));
        assert!(private.has_tool("roll_dice"));
    }

    // ── Tool loop ───────────────────────────────────────────────────

    #[test]
    fn tool_loop_returns_immediately_on_end_turn() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // No mock server needed — the loop should return immediately.
            let client = LlmClient::new("/tmp/unused.sock".into());
            let (push_tx, _rx) = broadcast::channel(16);
            let registry = ToolRegistry::new(false);
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
                &registry,
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

        // Mock LLM server: responds with end_turn after tool results.
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = tokio::io::split(stream);

            // Read the HTTP request.
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
        let registry = ToolRegistry::new(false);
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

        // Simulate initial LLM response requesting check_time tool.
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
            &registry,
            10,
            &cache_ctx,
        )
        .await
        .unwrap();

        assert_eq!(result.finish_reason, "end_turn");
        assert_eq!(result.content, "The current time is shown above.");

        // Verify ToolCall event was pushed.
        let tc = push_rx.try_recv().unwrap();
        match tc {
            ServerMessage::ToolCall(call) => {
                assert_eq!(call.tool_id, "t1");
                assert_eq!(call.tool_name, "check_time");
            }
            other => panic!("Expected ToolCall, got {:?}", other),
        }

        // Verify ToolResult event was pushed.
        let tr = push_rx.try_recv().unwrap();
        match tr {
            ServerMessage::ToolResult(res) => {
                assert_eq!(res.tool_id, "t1");
                assert_eq!(res.tool_name, "check_time");
                assert!(!res.is_error);
                assert!(res.output.contains('T')); // RFC 3339 datetime
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }

        // StreamStart, StreamChunk, StreamEnd from the second LLM call.
        let ss = push_rx.try_recv().unwrap();
        assert!(matches!(ss, ServerMessage::StreamStart(_)));

        let sc = push_rx.try_recv().unwrap();
        assert!(matches!(sc, ServerMessage::StreamChunk(_)));

        let se = push_rx.try_recv().unwrap();
        assert!(matches!(se, ServerMessage::StreamEnd(_)));

        // Request messages should include assistant tool_use + user tool_result.
        assert_eq!(request.messages.len(), 3); // original + assistant + user

        let assistant_msg = &request.messages[1];
        assert_eq!(assistant_msg["role"], "assistant");
        let content = assistant_msg["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_use");
        assert_eq!(content[0]["name"], "check_time");

        let user_msg = &request.messages[2];
        assert_eq!(user_msg["role"], "user");
        let tool_results = user_msg["content"].as_array().unwrap();
        assert_eq!(tool_results[0]["type"], "tool_result");
        assert_eq!(tool_results[0]["tool_use_id"], "t1");

        server.await.unwrap();
    }

    #[tokio::test]
    async fn tool_loop_respects_max_iterations() {
        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("mock-llm-loop.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        // Mock server: always returns tool_use (to test max iterations guard).
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
        let registry = ToolRegistry::new(false);
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

        // Max iterations = 3: initial + 3 loop iterations.
        let result = run_tool_loop(
            &client,
            &push_tx,
            &mut request,
            initial,
            &registry,
            3,
            &cache_ctx,
        )
        .await
        .unwrap();

        // Should have stopped after max iterations, last result still tool_use.
        assert_eq!(result.finish_reason, "tool_use");

        server.await.unwrap();
    }
}
