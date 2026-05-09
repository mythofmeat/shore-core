//! rmcp client wrapper. Holds a connected client service and exposes the
//! handful of operations Shore needs: list tools, call a tool, shut down.

use crate::error::OutboundClientError;
use serde_json::Value;

/// Tool definition discovered from a remote MCP server, in a Shore-native
/// shape that does not leak rmcp types into the daemon crate.
#[derive(Debug, Clone)]
pub struct RemoteToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub destructive_hint: bool,
    pub read_only_hint: bool,
}

/// Stub for the rmcp-backed client. The supervisor task owns one of these
/// per server and routes calls into it. Real wiring lands once the rest of
/// the daemon-side plumbing compiles.
pub struct Client {
    _server_name: String,
}

impl Client {
    pub fn new(server_name: String) -> Self {
        Self {
            _server_name: server_name,
        }
    }

    pub async fn list_tools(&self) -> Result<Vec<RemoteToolDef>, OutboundClientError> {
        Err(OutboundClientError::NotReady)
    }

    pub async fn call_tool(
        &self,
        _name: &str,
        _args: Value,
    ) -> Result<Value, OutboundClientError> {
        Err(OutboundClientError::NotReady)
    }
}
