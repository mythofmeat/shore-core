//! Configuration for outbound MCP servers — external processes Shore spawns
//! and exposes to characters as namespaced tools (`mcp__<server>__<tool>`).
//!
//! Servers are defined globally in `config.toml`; per-character enablement
//! is achieved by setting `enabled = true` at a character config layer that
//! deep-merges over the global one.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    /// Map of server name → spawn + policy spec. The key is used as the
    /// server's identifier in tool names: `mcp__<key>__<tool>`.
    #[serde(default)]
    pub servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    /// Executable to spawn (e.g. `python3`, an absolute path, or a binary
    /// on PATH).
    pub command: String,

    /// Arguments passed to the executable.
    #[serde(default)]
    pub args: Vec<String>,

    /// Environment variables to set on the spawned process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,

    /// Allowlist of tool names (unprefixed, as the server reports them) that
    /// Shore will register. **Empty allowlist registers no tools** —
    /// safety net so adopting a third-party server can't surprise you with
    /// new destructive tools.
    #[serde(default)]
    pub allowed_tools: Vec<String>,

    /// If false (default), tools whose MCP `destructiveHint` annotation is
    /// `true` are refused at registration time. Setting this to `true`
    /// requires explicit per-server opt-in.
    #[serde(default)]
    pub allow_destructive: bool,

    /// Whether Shore should spawn this server. Default off; flip to `true`
    /// at whichever config layer (global or per-character) you want the
    /// server active.
    #[serde(default)]
    pub enabled: bool,
}
