//! Registry of running MCP server handles + the tool list each one
//! contributes. Construction happens in `main.rs` after the autonomy
//! manager exists and before the message handler is built.
//!
//! Skeleton — empty registry only. Real spawn-and-discover wiring lands
//! after the daemon-side integration compiles end-to-end.

use crate::mcp::dispatch::McpDispatch;
use crate::tools::{DynamicToolDef, ToolError};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;

pub struct McpRegistry {
    // Real fields land in a later step:
    //   - per-server `ServerHandle` from shore-mcp-client
    //   - memoized first-successful tool list per server
    //   - shutdown sender
}

impl McpRegistry {
    /// Construct an empty registry. Spawning configured servers happens
    /// during a separate `start` step once the rest of the daemon plumbing
    /// is in place.
    pub fn empty() -> Self {
        Self {}
    }

    /// Inherent shortcut so callers don't need to import the
    /// [`McpDispatch`] trait to ask for the dynamic tool list.
    pub fn dynamic_tool_defs(&self) -> Vec<DynamicToolDef> {
        <Self as McpDispatch>::dynamic_tool_defs(self)
    }
}

impl McpDispatch for McpRegistry {
    fn dynamic_tool_defs(&self) -> Vec<DynamicToolDef> {
        Vec::new()
    }

    fn dispatch<'a>(
        &'a self,
        name: &'a str,
        _input: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send + 'a>> {
        let owned = name.to_string();
        Box::pin(async move { Err(ToolError::NotImplemented(owned)) })
    }
}
