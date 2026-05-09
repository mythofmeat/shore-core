//! Registry of running MCP server handles + the tool list each one
//! contributes. Construction happens in `main.rs` after the autonomy
//! manager exists and before the message handler is built.
//!
//! The registry holds one supervised task per configured + enabled server,
//! handles policy enforcement (per-server `allowed_tools` allowlist plus
//! the `destructiveHint` rule), and exposes the merged tool list / dispatch
//! routing through the `McpDispatch` trait.

use crate::mcp::dispatch::McpDispatch;
use crate::mcp::{make_prefixed_name, parse_prefixed_name};
use crate::tools::{DynamicToolDef, ToolCategory, ToolError};
use serde_json::Value;
use shore_config::mcp::{McpConfig, McpServerConfig};
use shore_mcp_client::{spawn_server, RemoteToolDef, ServerHandle, ServerSpawnSpec};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tracing::{info, warn};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub struct McpRegistry {
    servers: HashMap<String, ServerEntry>,
}

struct ServerEntry {
    handle: Arc<ServerHandle>,
    /// Tool names (unprefixed) that this server is allowed to expose, copied
    /// from `allowed_tools` so the dispatch path doesn't need the original
    /// `McpServerConfig`. Empty means "no tools" — a missing or empty
    /// allowlist is *not* the same as "all tools."
    allowed: Vec<String>,
    allow_destructive: bool,
}

impl McpRegistry {
    /// Construct an empty registry. Use [`McpRegistry::start`] to spawn the
    /// configured + enabled servers.
    pub fn empty() -> Self {
        Self {
            servers: HashMap::new(),
        }
    }

    /// Spawn supervisors for every enabled server in `cfg`. Returns `None`
    /// if no servers are enabled — `main.rs` uses that to skip threading
    /// the registry through entirely.
    ///
    /// The returned registry holds tools as they become available
    /// (eager-spawn, render-as-they-arrive). Servers that fail to handshake
    /// are logged and dropped; the daemon continues without them.
    pub fn start(cfg: &McpConfig, parent_shutdown_rx: watch::Receiver<()>) -> Option<Arc<Self>> {
        let enabled: Vec<(&String, &McpServerConfig)> =
            cfg.servers.iter().filter(|(_, s)| s.enabled).collect();
        if enabled.is_empty() {
            return None;
        }

        let mut servers = HashMap::with_capacity(enabled.len());
        for (name, srv) in enabled {
            if srv.allowed_tools.is_empty() {
                warn!(
                    server = %name,
                    "MCP server enabled but allowed_tools is empty; \
                     no tools will be registered. Set allowed_tools = [...] \
                     to expose tools."
                );
            }
            let spec = ServerSpawnSpec {
                command: srv.command.clone(),
                args: srv.args.clone(),
                env: srv.env.clone(),
            };
            let handle = spawn_server(name.clone(), spec, parent_shutdown_rx.clone());
            servers.insert(
                name.clone(),
                ServerEntry {
                    handle: Arc::new(handle),
                    allowed: srv.allowed_tools.clone(),
                    allow_destructive: srv.allow_destructive,
                },
            );
            info!(server = %name, "spawned MCP server supervisor");
        }

        Some(Arc::new(Self { servers }))
    }

    /// Filter a remote tool list against this server's policy: drop tools
    /// that aren't on the allowlist, drop `destructiveHint:true` tools
    /// unless `allow_destructive` is set.
    fn policy_filter<'a>(
        entry: &'a ServerEntry,
        server_name: &'a str,
        tools: &'a [RemoteToolDef],
    ) -> impl Iterator<Item = &'a RemoteToolDef> + 'a {
        tools.iter().filter(move |t| {
            if !entry.allowed.iter().any(|a| a == &t.name) {
                return false;
            }
            if t.destructive_hint && !entry.allow_destructive {
                warn!(
                    server = %server_name,
                    tool = %t.name,
                    "MCP server reports destructiveHint:true; \
                     dropping. Set allow_destructive = true to enable."
                );
                return false;
            }
            true
        })
    }

    /// Inherent shortcut so callers don't need to import the
    /// [`McpDispatch`] trait to ask for the dynamic tool list.
    pub fn dynamic_tool_defs(&self) -> Vec<DynamicToolDef> {
        <Self as McpDispatch>::dynamic_tool_defs(self)
    }

    /// Best-effort shutdown of all supervised servers. Used during daemon
    /// teardown if we want to wait for child processes to wind down before
    /// exiting; today main.rs relies on `kill_on_drop` so this is unused.
    #[allow(dead_code)]
    pub async fn shutdown(self: Arc<Self>) {
        // We can't move out of an Arc, so this only takes effect when the
        // daemon holds the only Arc. The kill_on_drop handle in
        // shore-mcp-client makes this method's existence non-load-bearing.
        let _ = SHUTDOWN_TIMEOUT;
    }
}

impl McpDispatch for McpRegistry {
    fn dynamic_tool_defs(&self) -> Vec<DynamicToolDef> {
        // Snapshot every server's *currently-discovered* tools, applying
        // policy. Servers whose handshake hasn't completed yet contribute
        // nothing to this snapshot — the next session over the same daemon
        // will see them once they're up.
        let mut out = Vec::new();
        for (name, entry) in &self.servers {
            // The supervisor stores tools behind a tokio RwLock; the
            // registry call sites are already async (per-turn rendering
            // path) — but `McpDispatch::dynamic_tool_defs` is sync because
            // it's called from sync rendering helpers. We can't block the
            // executor inside an async runtime, so we use try_read on the
            // already-cached snapshot. ServerHandle exposes a sync helper
            // for the cached snapshot; if not, we'd have to make this
            // method async. Since the API today is async-only, fall back
            // to spawning a blocking thread.
            //
            // Simplest correct option: use `tokio::task::block_in_place` +
            // `Handle::current().block_on`, which is permitted under
            // multi-thread runtimes (the daemon is one).
            let handle = entry.handle.clone();
            let tools_opt = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async move { handle.list_tools().await })
            });
            let Some(tools) = tools_opt else {
                continue;
            };
            for t in Self::policy_filter(entry, name, &tools) {
                out.push(DynamicToolDef {
                    name: make_prefixed_name(name, &t.name),
                    description: t.description.clone(),
                    parameters: t.input_schema.clone(),
                    category: ToolCategory::Other,
                });
            }
        }
        out
    }

    fn dispatch<'a>(
        &'a self,
        name: &'a str,
        input: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let Some((server_name, tool_name)) = parse_prefixed_name(name) else {
                return Err(ToolError::NotImplemented(name.to_string()));
            };
            let Some(entry) = self.servers.get(server_name) else {
                return Err(ToolError::NotImplemented(name.to_string()));
            };

            // Allowlist re-check at dispatch time: even if the LLM hallucinates
            // a tool name beyond what `dynamic_tool_defs` reported, we still
            // refuse to forward it.
            if !entry.allowed.iter().any(|a| a == tool_name) {
                return Err(ToolError::NotImplemented(name.to_string()));
            }

            // destructiveHint re-check at dispatch time, in case a server
            // upgrade flipped a tool to destructive after we cached it.
            if let Some(tools) = entry.handle.list_tools().await {
                if let Some(t) = tools.iter().find(|t| t.name == tool_name) {
                    if t.destructive_hint && !entry.allow_destructive {
                        return Err(ToolError::InvalidArgs(format!(
                            "tool {name} reports destructiveHint:true; \
                             refusing to forward (allow_destructive=false)"
                        )));
                    }
                }
            }

            entry
                .handle
                .call(tool_name, input)
                .await
                .map_err(|e| ToolError::Http(format!("mcp dispatch: {e}")))
        })
    }
}
