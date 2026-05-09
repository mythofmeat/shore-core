//! Per-server supervisor task. Skeleton; real subprocess + rmcp wiring lands
//! after the daemon-side integration compiles end-to-end.

use crate::client::{Client, RemoteToolDef};
use crate::error::OutboundClientError;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, RwLock};

/// Spec passed in by the daemon to spawn a server. Mirrors the relevant
/// fields of the user's `[mcp.servers.<name>]` config but does not depend on
/// `shore-config` types — keeps this crate slim.
#[derive(Debug, Clone)]
pub struct ServerSpawnSpec {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// Handle returned to the daemon. The supervisor task lives behind it.
pub struct ServerHandle {
    name: String,
    state: Arc<RwLock<ServerState>>,
    _shutdown_tx: watch::Sender<()>,
    join: tokio::task::JoinHandle<()>,
}

struct ServerState {
    client: Option<Client>,
    tools: Option<Vec<RemoteToolDef>>,
}

/// Spawn a supervised server task. Returns a handle the daemon can poll
/// for tool list (eager-spawn, render-as-they-arrive) and route calls
/// through.
pub fn spawn_server(
    name: String,
    _spec: ServerSpawnSpec,
    shutdown_rx: watch::Receiver<()>,
) -> ServerHandle {
    let (shutdown_tx, _) = watch::channel(());
    let state = Arc::new(RwLock::new(ServerState {
        client: None,
        tools: None,
    }));
    let task_state = state.clone();
    let task_name = name.clone();
    let join = tokio::spawn(async move {
        // Skeleton: real spawn, handshake, restart-with-backoff lands later.
        let mut rx = shutdown_rx;
        let _ = rx.changed().await;
        tracing::info!(server = %task_name, "mcp-client supervisor exiting");
        let _ = task_state;
    });

    ServerHandle {
        name,
        state,
        _shutdown_tx: shutdown_tx,
        join,
    }
}

impl ServerHandle {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Snapshot of the discovered tool list, or `None` if handshake hasn't
    /// completed yet. The registry is responsible for memoizing the first
    /// successful list so a later restart with a different shape doesn't
    /// disturb a running session.
    pub async fn list_tools(&self) -> Option<Vec<RemoteToolDef>> {
        self.state.read().await.tools.clone()
    }

    pub async fn call(&self, tool: &str, args: Value) -> Result<Value, OutboundClientError> {
        let guard = self.state.read().await;
        let client = guard.client.as_ref().ok_or(OutboundClientError::NotReady)?;
        client.call_tool(tool, args).await
    }

    pub async fn shutdown(self, timeout: Duration) {
        let _ = tokio::time::timeout(timeout, self.join).await;
    }
}
