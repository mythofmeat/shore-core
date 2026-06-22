//! Live MCP connections and the dynamic tool surface they contribute.
//!
//! Each `[mcp.<name>]` config entry is connected at startup (and on hot-reload)
//! via [`shore_mcp_client`]. The tools discovered from every server are flattened into
//! one list, namespaced `mcp__<server>__<tool>`, sorted by that full name, and
//! **pinned for the registry's lifetime**. Pinning is what keeps the outbound
//! tool surface — and therefore the Anthropic cache prefix — stable across
//! turns: a server is listed once at connect, never re-listed mid-session.
//!
//! Unlike the static [`ToolDef`](super::ToolDef) registry (which is `&'static`),
//! MCP tool defs are owned because their names and schemas are only known at
//! runtime.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use shore_config::app::{tool_pattern_matches, McpServerConfig};
use shore_mcp_client::{McpClient, McpServerSpec, Transport};

use super::ToolError;

/// One discovered MCP tool, owned (runtime-resolved, unlike `ToolDef`).
#[derive(Debug, Clone)]
pub(crate) struct McpToolDef {
    /// Namespaced name offered to the model: `mcp__<server>__<tool>`.
    pub(crate) full_name: String,
    pub(crate) description: String,
    pub(crate) input_schema: Value,
    /// The `[mcp.<name>]` key this tool came from.
    pub(crate) server: String,
    /// The bare server-side tool name (used for the actual `tools/call`).
    pub(crate) tool: String,
}

impl McpToolDef {
    /// Render to the outbound LLM `tools` array shape.
    pub(crate) fn to_tool_json(&self) -> Value {
        json!({
            "name": self.full_name,
            "description": self.description,
            "input_schema": self.input_schema,
        })
    }
}

/// Live MCP connections plus the pinned, sorted tool surface they expose.
#[derive(Debug, Default)]
pub struct McpRegistry {
    clients: BTreeMap<String, McpClient>,
    /// Sorted by `full_name`; pinned for the registry's lifetime.
    tools: Vec<McpToolDef>,
    /// The `[mcp.*]` config this registry was built from. Used on hot-reload to
    /// skip reconnecting when the MCP section is unchanged.
    source: BTreeMap<String, McpServerConfig>,
}

impl McpRegistry {
    /// Connect every configured server and discover its tools. A server that
    /// fails to connect or list is logged and skipped — a bad server never
    /// takes the daemon down. Returns an empty registry when `mcp` is empty.
    pub async fn from_config(mcp: &BTreeMap<String, McpServerConfig>) -> Self {
        let mut clients = BTreeMap::new();
        let mut tools = Vec::new();

        for (name, cfg) in mcp {
            let Some(spec) = to_spec(name, cfg) else {
                tracing::warn!(server = %name, "mcp server has no valid transport; skipping");
                continue;
            };
            let client = match McpClient::connect(&spec).await {
                Ok(client) => client,
                Err(e) => {
                    tracing::warn!(server = %name, error = %e, "mcp server connect failed; skipping");
                    continue;
                }
            };
            match client.list_tools().await {
                Ok(discovered) => {
                    for tool in discovered {
                        tools.push(McpToolDef {
                            full_name: format!("mcp__{}__{}", tool.server, tool.name),
                            description: tool.description,
                            input_schema: tool.input_schema,
                            server: tool.server,
                            tool: tool.name,
                        });
                    }
                    let _existing = clients.insert(name.clone(), client);
                }
                Err(e) => {
                    tracing::warn!(server = %name, error = %e, "mcp tools/list failed; skipping");
                    client.shutdown().await;
                }
            }
        }

        tools.sort_by(|a, b| a.full_name.cmp(&b.full_name));
        if !tools.is_empty() {
            tracing::info!(count = tools.len(), "connected MCP tools");
        }
        Self {
            clients,
            tools,
            source: mcp.clone(),
        }
    }

    /// Whether this registry was built from `mcp` (lets hot-reload skip a
    /// needless reconnect when the `[mcp.*]` section is unchanged).
    pub(crate) fn matches_config(&self, mcp: &BTreeMap<String, McpServerConfig>) -> bool {
        &self.source == mcp
    }

    /// Tool defs whose full name matches any allowlist `patterns` (exact or
    /// `mcp__server__*` glob), in pinned sorted order. Shaped for the outbound
    /// LLM `tools` array.
    pub(crate) fn tool_defs_filtered(&self, patterns: &[String]) -> Vec<Value> {
        self.tools
            .iter()
            .filter(|t| {
                patterns
                    .iter()
                    .any(|p| tool_pattern_matches(p, &t.full_name))
            })
            .map(McpToolDef::to_tool_json)
            .collect()
    }

    /// Tools whose full name matches any of `patterns`. Used to expand a
    /// sub-agent's `tools = ["mcp__hue__*"]` grant against the live surface.
    pub(crate) fn names_matching(&self, patterns: &[String]) -> Vec<&McpToolDef> {
        self.tools
            .iter()
            .filter(|t| {
                patterns
                    .iter()
                    .any(|p| tool_pattern_matches(p, &t.full_name))
            })
            .collect()
    }

    /// Invoke `full_name` (an `mcp__server__tool`) with `args`. Resolves the
    /// server/tool via the pinned tool list (robust to `__` inside names) and
    /// routes to the live client.
    pub(crate) async fn call(&self, full_name: &str, args: Value) -> Result<Value, ToolError> {
        let def = self
            .tools
            .iter()
            .find(|t| t.full_name == full_name)
            .ok_or_else(|| ToolError::NotImplemented(full_name.to_owned()))?;
        let client = self
            .clients
            .get(&def.server)
            .ok_or_else(|| ToolError::NotImplemented(full_name.to_owned()))?;
        client
            .call(&def.tool, args)
            .await
            .map_err(|e| ToolError::Http(e.to_string()))
    }

    /// Shut down every connection (drains stdio child processes).
    pub(crate) async fn shutdown(self) {
        for (_name, client) in self.clients {
            client.shutdown().await;
        }
    }
}

/// Convert a config entry into a connection spec. Returns `None` if no transport
/// is set (config validation rejects this, but stay defensive).
fn to_spec(name: &str, cfg: &McpServerConfig) -> Option<McpServerSpec> {
    let transport = if let Some(command) = &cfg.command {
        Transport::Stdio {
            command: command.clone(),
            args: cfg.args.clone(),
            env: cfg.env.clone(),
        }
    } else if let Some(url) = &cfg.url {
        Transport::Http { url: url.clone() }
    } else {
        return None;
    };
    Some(McpServerSpec {
        name: name.to_owned(),
        transport,
    })
}

#[cfg(test)]
impl McpToolDef {
    /// Build a tool def for tests (no live connection).
    pub(crate) fn new_for_test(server: &str, tool: &str) -> Self {
        Self {
            full_name: format!("mcp__{server}__{tool}"),
            description: format!("{tool} tool"),
            input_schema: json!({"type": "object"}),
            server: server.to_owned(),
            tool: tool.to_owned(),
        }
    }
}

#[cfg(test)]
impl McpRegistry {
    /// Build a registry from hand-made tool defs (no live clients), sorted like
    /// the real `from_config`.
    pub(crate) fn from_tools_for_test(mut tools: Vec<McpToolDef>) -> Self {
        tools.sort_by(|a, b| a.full_name.cmp(&b.full_name));
        Self {
            clients: BTreeMap::new(),
            tools,
            source: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> McpRegistry {
        McpRegistry::from_tools_for_test(vec![
            McpToolDef::new_for_test("hue", "on"),
            McpToolDef::new_for_test("hue", "off"),
            McpToolDef::new_for_test("nanoleaf", "scene"),
        ])
    }

    #[test]
    fn tool_defs_filtered_globs_and_stays_sorted() {
        let r = registry();
        let names: Vec<String> = r
            .tool_defs_filtered(&["mcp__hue__*".to_owned()])
            .iter()
            .map(|d| d["name"].as_str().unwrap().to_owned())
            .collect();
        // Sorted by full name (off < on); the nanoleaf tool is excluded.
        assert_eq!(names, vec!["mcp__hue__off", "mcp__hue__on"]);
    }

    #[test]
    fn tool_defs_filtered_exact_wildcard_and_empty() {
        let r = registry();
        assert_eq!(r.tool_defs_filtered(&["mcp__hue__on".to_owned()]).len(), 1);
        assert_eq!(r.tool_defs_filtered(&["mcp__*".to_owned()]).len(), 3);
        assert!(r.tool_defs_filtered(&[]).is_empty());
    }

    #[test]
    fn tool_defs_filtered_is_stable_across_calls() {
        let r = registry();
        let a = r.tool_defs_filtered(&["mcp__*".to_owned()]);
        let b = r.tool_defs_filtered(&["mcp__*".to_owned()]);
        assert_eq!(a, b, "tool surface must be byte-stable for cache reuse");
    }

    #[test]
    fn names_matching_expands_server_glob() {
        let r = registry();
        let matched = r.names_matching(&["mcp__hue__*".to_owned()]);
        assert_eq!(matched.len(), 2);
        assert!(matched.iter().all(|t| t.server == "hue"));
    }

    #[tokio::test]
    async fn call_unknown_or_unconnected_tool_is_not_implemented() {
        let r = registry();
        // Name not in the surface at all.
        assert!(matches!(
            r.call("mcp__hue__missing", json!({})).await,
            Err(ToolError::NotImplemented(_))
        ));
        // Known def but no live client (test registry has none).
        assert!(matches!(
            r.call("mcp__hue__on", json!({})).await,
            Err(ToolError::NotImplemented(_))
        ));
    }

    #[test]
    fn matches_config_detects_changes() {
        let r = registry();
        assert!(r.matches_config(&BTreeMap::new()));
        let mut changed = BTreeMap::new();
        let _ = changed.insert(
            "hue".to_owned(),
            McpServerConfig {
                command: Some("node".to_owned()),
                args: vec![],
                env: BTreeMap::new(),
                url: None,
            },
        );
        assert!(!r.matches_config(&changed));
    }
}
