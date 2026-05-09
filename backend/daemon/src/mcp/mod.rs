//! Outbound MCP client surface — the daemon-side glue between Shore's
//! tool dispatch and the `shore-mcp-client` crate.
//!
//! Responsibilities:
//! - Hold the registry of running MCP server handles + their discovered tools.
//! - Render `DynamicToolDef`s for the per-turn tool list.
//! - Dispatch `mcp__<server>__<tool>` calls to the right server.
//! - Enforce policy: per-server allowlist + `destructiveHint` rule.
//!
//! Subprocess lifecycle (spawn, restart-with-backoff, handshake) lives in
//! `shore-mcp-client`. This module is policy + routing only.

pub mod dispatch;
pub mod registry;

pub use dispatch::McpDispatch;
pub use registry::McpRegistry;

/// Compose a Shore-namespaced tool name from a server name and the remote
/// tool name as the server reports it.
pub fn make_prefixed_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

/// Reverse of `make_prefixed_name`. Returns `(server, tool)` or `None` if
/// the input doesn't have the expected `mcp__<server>__<tool>` shape.
pub fn parse_prefixed_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    let sep = rest.find("__")?;
    let server = &rest[..sep];
    let tool = &rest[sep + 2..];
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server, tool))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_prefixed_name() {
        let s = make_prefixed_name("beets", "recently_played");
        assert_eq!(s, "mcp__beets__recently_played");
        assert_eq!(parse_prefixed_name(&s), Some(("beets", "recently_played")));
    }

    #[test]
    fn parse_rejects_native_tool_name() {
        assert_eq!(parse_prefixed_name("web_search"), None);
        assert_eq!(parse_prefixed_name("mcp__"), None);
        assert_eq!(parse_prefixed_name("mcp__only"), None);
        assert_eq!(parse_prefixed_name("mcp__server__"), None);
        assert_eq!(parse_prefixed_name("mcp____tool"), None);
    }
}
