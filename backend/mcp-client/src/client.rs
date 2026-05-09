//! Wrapper around an active rmcp client connection. Owned by the supervisor
//! task; callers reach this via a cloned `rmcp::service::Peer<RoleClient>`
//! held in the registry's shared state.

use crate::error::OutboundClientError;
use rmcp::model::{CallToolRequestParams, Tool};
use rmcp::service::{Peer, RoleClient};
use serde_json::Value;

/// Tool definition discovered from a remote MCP server, in a Shore-native
/// shape that does not leak rmcp types out of this crate.
#[derive(Debug, Clone)]
pub struct RemoteToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub destructive_hint: bool,
    pub read_only_hint: bool,
}

impl RemoteToolDef {
    pub(crate) fn from_rmcp(tool: &Tool) -> Self {
        let description = tool.description.as_deref().unwrap_or("").to_string();
        let input_schema =
            serde_json::to_value(&*tool.input_schema).unwrap_or_else(|_| serde_json::json!({}));
        let (destructive_hint, read_only_hint) = match &tool.annotations {
            Some(ann) => (
                ann.destructive_hint.unwrap_or(false),
                ann.read_only_hint.unwrap_or(false),
            ),
            None => (false, false),
        };
        Self {
            name: tool.name.to_string(),
            description,
            input_schema,
            destructive_hint,
            read_only_hint,
        }
    }
}

/// Cloned handle to a connected MCP server's peer. Calls round-trip over
/// the supervised stdio transport.
#[derive(Clone)]
pub struct Client {
    peer: Peer<RoleClient>,
}

impl Client {
    pub(crate) fn new(peer: Peer<RoleClient>) -> Self {
        Self { peer }
    }

    pub async fn list_tools(&self) -> Result<Vec<RemoteToolDef>, OutboundClientError> {
        let tools = self
            .peer
            .list_all_tools()
            .await
            .map_err(|e| OutboundClientError::CallFailed(format!("list_tools: {e}")))?;
        Ok(tools.iter().map(RemoteToolDef::from_rmcp).collect())
    }

    pub async fn call_tool(&self, name: &str, args: Value) -> Result<Value, OutboundClientError> {
        // rmcp's CallToolRequestParams takes Option<JsonObject> for arguments;
        // accept either a JSON object or null/missing as "no args" and reject
        // anything else explicitly so a misbehaving caller surfaces fast.
        let arguments = match args {
            Value::Null => None,
            Value::Object(map) => Some(map),
            other => {
                return Err(OutboundClientError::CallFailed(format!(
                    "tool args must be a JSON object, got {other:?}"
                )))
            }
        };

        let mut params = CallToolRequestParams::new(name.to_owned());
        params.arguments = arguments;

        let result = self
            .peer
            .call_tool(params)
            .await
            .map_err(|e| OutboundClientError::CallFailed(format!("call_tool: {e}")))?;

        // Forward the full CallToolResult — `content`, `structured_content`,
        // and `is_error` — so the character can reason about partial failures
        // without us having to invent a lossy native error variant.
        serde_json::to_value(&result)
            .map_err(|e| OutboundClientError::CallFailed(format!("serialize CallToolResult: {e}")))
    }
}
