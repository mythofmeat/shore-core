//! Trait that the tool dispatch layer uses to reach the MCP registry without
//! depending on its concrete type. Kept separate so `ToolContext` can hold a
//! `&dyn McpDispatch` without dragging in the registry's whole transitive
//! graph at the trait-object call site.

use crate::tools::{DynamicToolDef, ToolError};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;

/// Read-only view onto the MCP registry exposed to tool dispatch + per-turn
/// rendering.
pub trait McpDispatch: Sync + Send {
    /// Currently-discovered MCP tools, ready to merge into the LLM's tool
    /// list. Names are already prefixed (`mcp__<server>__<tool>`); allowlist
    /// and `destructiveHint` filtering have already been applied.
    fn dynamic_tool_defs(&self) -> Vec<DynamicToolDef>;

    /// Route an MCP `tools/call`. The caller has already verified the name
    /// starts with `mcp__`; this method is responsible for any further
    /// validation and the actual subprocess round-trip.
    fn dispatch<'a>(
        &'a self,
        name: &'a str,
        input: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send + 'a>>;
}
