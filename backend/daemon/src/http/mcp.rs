//! MCP-over-HTTP routes for the daemon's listener.
//!
//! Implements the streamable-HTTP transport variant of MCP that
//! `claude` accepts via `--mcp-config '{"mcpServers":{"shore":{
//! "type":"http","url":"<endpoint>"}}}'`. Each chat request
//! allocates a session in [`McpSessionRegistry`] and the CLI calls
//! back to `POST /mcp/<session_id>` with JSON-RPC requests:
//!
//! - `initialize` — one-shot capability advertisement
//! - `notifications/initialized` — fire-and-forget; we 202 it
//! - `tools/list` — return tool defs filtered by `allowed_tools`
//! - `tools/call` — dispatch to `crate::tools::dispatch_tool`,
//!   record on the session ledger, return the wrapped result
//!
//! Unknown methods return JSON-RPC error -32601 (method not found).

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use shore_protocol::types::ContentBlock;
use tracing::{debug, warn};

use crate::engine::mcp_session::{LedgerEntry, McpSession};
use crate::tools::dispatch_tool;

use super::DaemonHttpState;

/// JSON-RPC error codes used by this server.
const RPC_PARSE_ERROR: i32 = -32700;
const RPC_INVALID_REQUEST: i32 = -32600;
const RPC_METHOD_NOT_FOUND: i32 = -32601;
const RPC_INVALID_PARAMS: i32 = -32602;
/// Mount the MCP routes under `/mcp/{session_id}`.
pub(super) fn router() -> Router<Arc<DaemonHttpState>> {
    Router::new().route("/mcp/:session_id", post(handle_mcp_request))
}

async fn handle_mcp_request(
    State(state): State<Arc<DaemonHttpState>>,
    Path(session_id): Path<String>,
    body: String,
) -> impl IntoResponse {
    // Parse the JSON-RPC envelope. We tolerate whitespace.
    let req: Value = match serde_json::from_str(body.trim()) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(rpc_error(Value::Null, RPC_PARSE_ERROR, &e.to_string())),
            )
                .into_response();
        }
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = req.get("method").and_then(Value::as_str) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(rpc_error(id, RPC_INVALID_REQUEST, "missing method")),
        )
            .into_response();
    };
    let params = req.get("params").cloned().unwrap_or_else(|| json!({}));

    // `notifications/*` are fire-and-forget; respond 202 with no
    // body so axum's deserializer doesn't fail downstream.
    if method.starts_with("notifications/") {
        return StatusCode::ACCEPTED.into_response();
    }

    // For everything else we need an active session — except
    // `initialize`, which the CLI sends before the daemon has
    // allocated a session in some clients. We support both: if a
    // session exists at this id we use it; otherwise we still
    // respond to `initialize`.
    let session = state.mcp_sessions.get(&session_id);

    let response = match method {
        "initialize" => Json(rpc_ok(id, initialize_result())).into_response(),
        "tools/list" => match session {
            Some(s) => Json(rpc_ok(id, tools_list_result(&s))).into_response(),
            None => Json(rpc_error(id, RPC_INVALID_REQUEST, "no such session")).into_response(),
        },
        "tools/call" => match session {
            Some(s) => handle_tools_call(&s, id, params).await,
            None => Json(rpc_error(id, RPC_INVALID_REQUEST, "no such session")).into_response(),
        },
        _ => Json(rpc_error(
            id,
            RPC_METHOD_NOT_FOUND,
            &format!("unknown method: {method}"),
        ))
        .into_response(),
    };
    response
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        }
    })
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "shore-daemon",
            "version": env!("CARGO_PKG_VERSION"),
        }
    })
}

fn tools_list_result(session: &McpSession) -> Value {
    // MCP wants `inputSchema` (camelCase) where shore stores
    // `input_schema` (snake_case from the existing tool registry).
    // Translate at the wire boundary.
    let tools: Vec<Value> = session
        .list_tools()
        .into_iter()
        .map(|d| {
            let mut obj = serde_json::Map::new();
            if let Some(name) = d.get("name") {
                obj.insert("name".into(), name.clone());
            }
            if let Some(desc) = d.get("description") {
                obj.insert("description".into(), desc.clone());
            }
            let schema = d
                .get("input_schema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object"}));
            obj.insert("inputSchema".into(), schema);
            Value::Object(obj)
        })
        .collect();
    json!({ "tools": tools })
}

async fn handle_tools_call(
    session: &Arc<McpSession>,
    id: Value,
    params: Value,
) -> axum::response::Response {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Json(rpc_error(id, RPC_INVALID_PARAMS, "missing tool name")).into_response();
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    if !session.allows(name) {
        return Json(rpc_error(
            id,
            RPC_INVALID_PARAMS,
            &format!("tool '{name}' is not in the session's allowed list"),
        ))
        .into_response();
    }

    debug!(
        session_id = %session.id,
        tool = %name,
        "MCP tools/call dispatched"
    );

    // Claude Code includes the assistant tool_use id in params._meta;
    // use it so the engine can pair this ledger entry with the
    // stream-json ToolUse block. The JSON-RPC id is only a progress
    // token in current CLI builds.
    let tool_use_id = params
        .get("_meta")
        .and_then(|m| m.get("claudecode/toolUseId"))
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| match &id {
            Value::String(s) => s.clone(),
            Value::Number(n) => format!("rpc-{n}"),
            _ => format!("ledger-{}", uuid::Uuid::new_v4()),
        });

    let dispatch_result = if name == "set_next_wake" {
        match session.tool_ctx.schedule_next_wake(&arguments) {
            Some(result) => result,
            None => dispatch_tool(name, arguments.clone(), session.tool_ctx.as_ref()).await,
        }
    } else {
        dispatch_tool(name, arguments.clone(), session.tool_ctx.as_ref()).await
    };
    let (content_blocks, is_error, response_payload) = match dispatch_result {
        Ok(result) => {
            let text = serialize_tool_value(&result);
            let blocks = vec![ContentBlock::Text { text: text.clone() }];
            let payload = json!({
                "content": [{"type": "text", "text": text}],
                "isError": false,
            });
            (blocks, false, payload)
        }
        Err(e) => {
            let text = format!("{e}");
            warn!(
                session_id = %session.id,
                tool = %name,
                error = %text,
                "tool dispatch returned error"
            );
            let blocks = vec![ContentBlock::Text { text: text.clone() }];
            let payload = json!({
                "content": [{"type": "text", "text": text}],
                "isError": true,
            });
            (blocks, true, payload)
        }
    };

    session
        .record(LedgerEntry {
            tool_use_id,
            name: name.to_string(),
            input: arguments,
            content: content_blocks,
            is_error,
        })
        .await;

    Json(rpc_ok(id, response_payload)).into_response()
}

/// Render an arbitrary `serde_json::Value` as the MCP text payload.
/// Strings are passed through; everything else is JSON-stringified.
fn serialize_tool_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::mcp_session::McpSessionRegistry;
    use std::collections::HashSet;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;
    use tokio::sync::watch;

    /// Test ToolContext that records dispatched calls and returns
    /// scripted responses for known tool names.
    #[derive(Default)]
    struct ScriptedCtx {
        config: shore_config::app::SearchConfig,
        wake_calls: Option<Arc<StdMutex<Vec<Value>>>>,
    }

    impl crate::tools::ToolContext for ScriptedCtx {
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
            &self.config
        }
        fn schedule_next_wake(
            &self,
            input: &Value,
        ) -> Option<Result<Value, crate::tools::ToolError>> {
            let calls = self.wake_calls.as_ref()?;
            calls.lock().unwrap().push(input.clone());
            Some(Ok(json!("Scheduled next moment in 2.0 hours.")))
        }
    }

    async fn spawn_test_listener(
        registry: McpSessionRegistry,
    ) -> (
        Arc<DaemonHttpState>,
        tokio::task::JoinHandle<()>,
        watch::Sender<()>,
    ) {
        let (tx, rx) = watch::channel(());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind_addr = listener.local_addr().unwrap();
        let state = Arc::new(DaemonHttpState {
            bind_addr,
            mcp_sessions: registry,
        });
        let app = super::super::build_router(state.clone());
        let mut shutdown_rx = rx;
        let handle = tokio::spawn(async move {
            let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
                let _ = shutdown_rx.changed().await;
            });
            let _ = serve.await;
        });
        (state, handle, tx)
    }

    fn tool_defs_for_test() -> Vec<Value> {
        vec![
            json!({"name": "roll_dice", "description": "Roll dice", "input_schema": {"type": "object", "properties": {"sides": {"type": "integer"}, "count": {"type": "integer"}}}}),
            json!({"name": "check_time", "description": "Get the current time", "input_schema": {"type": "object"}}),
            json!({"name": "memory", "description": "Memory ops", "input_schema": {"type": "object"}}),
        ]
    }

    fn allowed(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    async fn rpc(
        client: &reqwest::Client,
        url: &str,
        method: &str,
        id: Value,
        params: Value,
    ) -> Value {
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let res = client.post(url).json(&body).send().await.unwrap();
        assert_eq!(res.status(), 200, "method {method} should 200");
        res.json::<Value>().await.unwrap()
    }

    #[tokio::test]
    async fn initialize_returns_capabilities() {
        let registry = McpSessionRegistry::new();
        let (state, _h, _tx) = spawn_test_listener(registry.clone()).await;
        // Wait for listener readiness.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let url = format!("{}/mcp/any-session-id", state.base_url());
        let client = reqwest::Client::new();
        let resp = rpc(&client, &url, "initialize", json!(1), json!({})).await;
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        assert_eq!(resp["result"]["serverInfo"]["name"], "shore-daemon");
    }

    #[tokio::test]
    async fn tools_list_filters_to_allowed() {
        let registry = McpSessionRegistry::new();
        let _g = registry.allocate(
            "s-list".into(),
            allowed(&["roll_dice", "check_time"]),
            tool_defs_for_test(),
            Arc::new(ScriptedCtx::default()),
        );
        let (state, _h, _tx) = spawn_test_listener(registry.clone()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let url = format!("{}/mcp/s-list", state.base_url());
        let client = reqwest::Client::new();
        let resp = rpc(&client, &url, "tools/list", json!(2), json!({})).await;
        let tools = resp["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"roll_dice"));
        assert!(names.contains(&"check_time"));
        assert!(!names.contains(&"memory"));
        // inputSchema must be present in the camelCase form MCP wants.
        assert!(tools[0]["inputSchema"].is_object());
    }

    #[tokio::test]
    async fn tools_call_dispatches_known_tool_and_records_ledger() {
        let registry = McpSessionRegistry::new();
        let guard = registry.allocate(
            "s-call".into(),
            allowed(&["check_time"]),
            tool_defs_for_test(),
            Arc::new(ScriptedCtx::default()),
        );
        let (state, _h, _tx) = spawn_test_listener(registry.clone()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let url = format!("{}/mcp/s-call", state.base_url());
        let client = reqwest::Client::new();
        let resp = rpc(
            &client,
            &url,
            "tools/call",
            json!("call-1"),
            json!({"name": "check_time", "arguments": {}}),
        )
        .await;
        assert_eq!(resp["jsonrpc"], "2.0");
        assert!(resp["result"].is_object(), "got {resp}");
        assert_eq!(resp["result"]["isError"], false);
        let content = resp["result"]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert!(!content[0]["text"].as_str().unwrap().is_empty());

        // Ledger captured the call.
        let drained = guard.drain().await;
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].tool_use_id, "call-1");
        assert_eq!(drained[0].name, "check_time");
        assert!(!drained[0].is_error);
    }

    #[tokio::test]
    async fn tools_call_routes_set_next_wake_through_heartbeat_context() {
        let registry = McpSessionRegistry::new();
        let wake_calls = Arc::new(StdMutex::new(Vec::new()));
        let guard = registry.allocate(
            "s-wake".into(),
            allowed(&["set_next_wake"]),
            tool_defs_for_test(),
            Arc::new(ScriptedCtx {
                config: shore_config::app::SearchConfig::default(),
                wake_calls: Some(wake_calls.clone()),
            }),
        );
        let (state, _h, _tx) = spawn_test_listener(registry.clone()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let url = format!("{}/mcp/s-wake", state.base_url());
        let client = reqwest::Client::new();
        let resp = rpc(
            &client,
            &url,
            "tools/call",
            json!("call-wake"),
            json!({
                "name": "set_next_wake",
                "arguments": {"hours_from_now": 2.0, "reason": "continue later"}
            }),
        )
        .await;

        assert_eq!(resp["result"]["isError"], false);
        assert_eq!(
            resp["result"]["content"][0]["text"],
            "Scheduled next moment in 2.0 hours."
        );
        assert_eq!(wake_calls.lock().unwrap().len(), 1);

        let drained = guard.drain().await;
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].tool_use_id, "call-wake");
        assert_eq!(drained[0].name, "set_next_wake");
        assert!(!drained[0].is_error);
    }

    #[tokio::test]
    async fn tools_call_prefers_claude_code_tool_use_id_meta() {
        let registry = McpSessionRegistry::new();
        let guard = registry.allocate(
            "s-meta".into(),
            allowed(&["check_time"]),
            tool_defs_for_test(),
            Arc::new(ScriptedCtx::default()),
        );
        let (state, _h, _tx) = spawn_test_listener(registry.clone()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let url = format!("{}/mcp/s-meta", state.base_url());
        let client = reqwest::Client::new();
        let _ = rpc(
            &client,
            &url,
            "tools/call",
            json!(2),
            json!({
                "name": "check_time",
                "arguments": {},
                "_meta": {"claudecode/toolUseId": "toolu_actual"}
            }),
        )
        .await;

        let drained = guard.drain().await;
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].tool_use_id, "toolu_actual");
    }

    #[tokio::test]
    async fn tools_call_disallowed_tool_returns_invalid_params() {
        let registry = McpSessionRegistry::new();
        let _guard = registry.allocate(
            "s-deny".into(),
            allowed(&["check_time"]),
            tool_defs_for_test(),
            Arc::new(ScriptedCtx::default()),
        );
        let (state, _h, _tx) = spawn_test_listener(registry.clone()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let url = format!("{}/mcp/s-deny", state.base_url());
        let client = reqwest::Client::new();
        let resp = rpc(
            &client,
            &url,
            "tools/call",
            json!(7),
            json!({"name": "memory", "arguments": {}}),
        )
        .await;
        assert!(resp["error"].is_object(), "got {resp}");
        assert_eq!(resp["error"]["code"], RPC_INVALID_PARAMS);
        let msg = resp["error"]["message"].as_str().unwrap();
        assert!(msg.contains("memory"));
        assert!(msg.contains("allowed"));
    }

    #[tokio::test]
    async fn unknown_session_for_tools_list_returns_invalid_request() {
        let registry = McpSessionRegistry::new();
        let (state, _h, _tx) = spawn_test_listener(registry.clone()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let url = format!("{}/mcp/no-such-session", state.base_url());
        let client = reqwest::Client::new();
        let resp = rpc(&client, &url, "tools/list", json!(99), json!({})).await;
        assert_eq!(resp["error"]["code"], RPC_INVALID_REQUEST);
    }

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let registry = McpSessionRegistry::new();
        let (state, _h, _tx) = spawn_test_listener(registry.clone()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let url = format!("{}/mcp/x", state.base_url());
        let client = reqwest::Client::new();
        let resp = rpc(&client, &url, "weird/method", json!(0), json!({})).await;
        assert_eq!(resp["error"]["code"], RPC_METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn notifications_initialized_returns_202_with_no_body() {
        let registry = McpSessionRegistry::new();
        let (state, _h, _tx) = spawn_test_listener(registry.clone()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let url = format!("{}/mcp/y", state.base_url());
        let client = reqwest::Client::new();
        let body = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        });
        let res = client.post(&url).json(&body).send().await.unwrap();
        assert_eq!(res.status(), 202);
    }

    #[tokio::test]
    async fn malformed_json_returns_400_with_parse_error() {
        let registry = McpSessionRegistry::new();
        let (state, _h, _tx) = spawn_test_listener(registry.clone()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let url = format!("{}/mcp/z", state.base_url());
        let client = reqwest::Client::new();
        let res = client
            .post(&url)
            .body("not json")
            .header("content-type", "application/json")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 400);
        let body: Value = res.json().await.unwrap();
        assert_eq!(body["error"]["code"], RPC_PARSE_ERROR);
    }
}
