//! Per-server supervised task. Spawns the configured executable, performs
//! the MCP initialize handshake, calls `tools/list`, parks waiting for the
//! running service to terminate, and restarts with exponential backoff on
//! crash. Mirrors the shape of `backend/daemon/src/supervisor.rs` so the
//! restart semantics are familiar.

use crate::client::{Client, RemoteToolDef};
use crate::error::OutboundClientError;
use rmcp::ServiceExt;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::sync::{watch, RwLock};
use tracing::{debug, error, info, warn};

const MAX_CONSECUTIVE_FAILURES: u32 = 5;
const STABLE_RUNTIME_THRESHOLD: Duration = Duration::from_secs(300);

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
    /// Local stop signal — fires when `shutdown()` is called.
    stop_tx: watch::Sender<bool>,
    join: tokio::task::JoinHandle<()>,
}

/// Shared state read by the registry's tool-list and dispatch paths.
struct ServerState {
    /// Connected client peer. `Some` while a server is up; `None` between
    /// restarts.
    client: Option<Client>,
    /// Memoized tool list from the *first* successful handshake. Subsequent
    /// restarts that report a different shape are logged but ignored, so
    /// the in-flight session keeps a stable tool catalog.
    tools: Option<Vec<RemoteToolDef>>,
}

/// Spawn a supervised server task. `parent_shutdown_rx` is the daemon-wide
/// shutdown watch — when it fires, the supervisor stops attempting restarts
/// and tears down its child gracefully.
pub fn spawn_server(
    name: String,
    spec: ServerSpawnSpec,
    parent_shutdown_rx: watch::Receiver<()>,
) -> ServerHandle {
    let state = Arc::new(RwLock::new(ServerState {
        client: None,
        tools: None,
    }));
    let (stop_tx, stop_rx) = watch::channel(false);
    let task_state = state.clone();
    let task_name = name.clone();
    let join = tokio::spawn(async move {
        supervise(task_name, spec, task_state, parent_shutdown_rx, stop_rx).await;
    });

    ServerHandle {
        name,
        state,
        stop_tx,
        join,
    }
}

impl ServerHandle {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Snapshot of the tool list, or `None` if the first handshake hasn't
    /// completed yet. The registry treats `None` as "this server contributes
    /// no tools to the current session" — eager-spawn / render-as-they-arrive
    /// is the established norm.
    pub async fn list_tools(&self) -> Option<Vec<RemoteToolDef>> {
        self.state.read().await.tools.clone()
    }

    /// Dispatch a `tools/call` to this server. Returns `NotReady` when the
    /// server is between restarts.
    pub async fn call(&self, tool: &str, args: Value) -> Result<Value, OutboundClientError> {
        let client = {
            let guard = self.state.read().await;
            guard.client.clone().ok_or(OutboundClientError::NotReady)?
        };
        client.call_tool(tool, args).await
    }

    /// Signal the supervisor to stop and wait up to `timeout` for it.
    pub async fn shutdown(self, timeout: Duration) {
        let _ = self.stop_tx.send(true);
        let _ = tokio::time::timeout(timeout, self.join).await;
    }
}

async fn supervise(
    name: String,
    spec: ServerSpawnSpec,
    state: Arc<RwLock<ServerState>>,
    mut parent_shutdown_rx: watch::Receiver<()>,
    mut stop_rx: watch::Receiver<bool>,
) {
    let mut failures: u32 = 0;

    loop {
        if parent_shutdown_rx.has_changed().unwrap_or(true) || *stop_rx.borrow() {
            return;
        }

        info!(
            server = %name,
            command = %spec.command,
            args = ?spec.args,
            "spawning MCP server"
        );

        let started_at = Instant::now();
        let run_result =
            run_once(&name, &spec, &state, &mut parent_shutdown_rx, &mut stop_rx).await;

        // Always clear the connected client before deciding on restart so a
        // racing call() during the gap fails fast with NotReady.
        {
            let mut guard = state.write().await;
            guard.client = None;
        }

        match run_result {
            RunOutcome::Shutdown => {
                info!(server = %name, "MCP server shut down by signal");
                return;
            }
            RunOutcome::SpawnFailed(e) => {
                error!(server = %name, error = %e, "failed to spawn MCP server");
                failures = failures.saturating_add(1);
            }
            RunOutcome::HandshakeFailed(e) => {
                warn!(server = %name, error = %e, "MCP handshake failed; will retry");
                failures = failures.saturating_add(1);
            }
            RunOutcome::Exited => {
                let runtime = started_at.elapsed();
                if runtime >= STABLE_RUNTIME_THRESHOLD {
                    failures = 0;
                }
                failures = failures.saturating_add(1);
                warn!(
                    server = %name,
                    runtime_secs = runtime.as_secs(),
                    failures,
                    "MCP server exited; will restart with backoff"
                );
            }
        }

        if failures >= MAX_CONSECUTIVE_FAILURES {
            error!(
                server = %name,
                failures,
                "MCP server failed {MAX_CONSECUTIVE_FAILURES} times consecutively; \
                 giving up. Daemon continuing without this server."
            );
            return;
        }

        if !sleep_or_signal(backoff(failures), &mut parent_shutdown_rx, &mut stop_rx).await {
            return;
        }
    }
}

enum RunOutcome {
    /// External shutdown signal received during run.
    Shutdown,
    /// `tokio::process::Command::spawn` (or rmcp's wrapper) failed.
    SpawnFailed(OutboundClientError),
    /// Spawned, but rmcp initialize handshake or first `tools/list` failed.
    HandshakeFailed(OutboundClientError),
    /// Was running successfully; child or transport exited.
    Exited,
}

/// One spawn-handshake-serve cycle. Returns when the server exits, fails,
/// or shutdown is requested.
async fn run_once(
    name: &str,
    spec: &ServerSpawnSpec,
    state: &Arc<RwLock<ServerState>>,
    parent_shutdown_rx: &mut watch::Receiver<()>,
    stop_rx: &mut watch::Receiver<bool>,
) -> RunOutcome {
    // Build the tokio Command, configure stdio, env, and kill-on-drop.
    let mut cmd = Command::new(&spec.command);
    cmd.args(&spec.args);
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    cmd.kill_on_drop(true);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    // Inherit stderr so server diagnostics surface in the daemon's logs
    // unmodified — the user's existing `RUST_LOG` discipline applies.
    cmd.stderr(std::process::Stdio::inherit());

    let transport = match rmcp::transport::TokioChildProcess::new(cmd) {
        Ok(t) => t,
        Err(e) => return RunOutcome::SpawnFailed(OutboundClientError::Spawn(e.to_string())),
    };

    let pid = transport.id();

    // The simplest valid `ClientHandler` is `()`. We don't currently
    // implement any inbound notifications (sampling, elicitation, etc.).
    let running = match ().serve(transport).await {
        Ok(s) => s,
        Err(e) => {
            return RunOutcome::HandshakeFailed(OutboundClientError::Handshake(e.to_string()));
        }
    };

    let peer = running.peer().clone();
    let client = Client::new(peer);

    // Discover tools. First successful list is memoized; subsequent
    // restarts use the original to keep tool catalogs stable across
    // sessions.
    let tools = match client.list_tools().await {
        Ok(t) => t,
        Err(e) => {
            warn!(
                server = %name,
                error = %e,
                "MCP tools/list failed after handshake; treating as handshake failure"
            );
            return RunOutcome::HandshakeFailed(e);
        }
    };

    info!(
        server = %name,
        pid = pid.unwrap_or(0),
        tool_count = tools.len(),
        "MCP server ready"
    );

    {
        let mut guard = state.write().await;
        guard.client = Some(client);
        match &guard.tools {
            None => guard.tools = Some(tools),
            Some(existing) => {
                let names_match = existing.len() == tools.len()
                    && existing
                        .iter()
                        .zip(tools.iter())
                        .all(|(a, b)| a.name == b.name);
                if !names_match {
                    warn!(
                        server = %name,
                        old_count = existing.len(),
                        new_count = tools.len(),
                        "MCP server reported a different tool set after restart; \
                         keeping the original list to preserve session cache stability"
                    );
                }
                debug!(server = %name, "post-restart tools/list complete");
            }
        }
    }

    // Grab a cancellation handle before `waiting()` consumes the running
    // service, so the local-stop arm can ask rmcp to wind down cleanly.
    let cancel_token = running.cancellation_token();

    // Park until either the running service exits (server crashed,
    // protocol error) or a shutdown signal arrives.
    tokio::select! {
        wait_result = running.waiting() => {
            match wait_result {
                Ok(_) => debug!(server = %name, "rmcp running service exited cleanly"),
                Err(e) => warn!(server = %name, error = %e, "rmcp running service errored"),
            }
            RunOutcome::Exited
        }
        _ = parent_shutdown_rx.changed() => {
            // The daemon is shutting down — best-effort. Dropping the
            // running service cancels its task; `kill_on_drop` on the child
            // kills the process.
            info!(server = %name, "MCP server shutting down (parent signal)");
            cancel_token.cancel();
            RunOutcome::Shutdown
        }
        _ = stop_rx.changed() => {
            info!(server = %name, "MCP server shutting down (local stop)");
            cancel_token.cancel();
            // Brief grace period lets the cancellation propagate before the
            // outer task lets `running` drop.
            tokio::time::sleep(Duration::from_millis(500)).await;
            RunOutcome::Shutdown
        }
    }
}

/// Exponential backoff: 1s, 2s, 4s, 8s, 16s, capped at 32s.
fn backoff(failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(5);
    Duration::from_secs(1u64 << shift)
}

/// Sleep for `dur` or return early if either shutdown channel signals.
/// Returns `true` if the sleep completed normally, `false` on signal.
async fn sleep_or_signal(
    dur: Duration,
    parent: &mut watch::Receiver<()>,
    stop: &mut watch::Receiver<bool>,
) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(dur) => true,
        _ = parent.changed() => false,
        _ = stop.changed() => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_progression_matches_matrix_supervisor() {
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(2));
        assert_eq!(backoff(3), Duration::from_secs(4));
        assert_eq!(backoff(4), Duration::from_secs(8));
        assert_eq!(backoff(5), Duration::from_secs(16));
        assert_eq!(backoff(6), Duration::from_secs(32));
        assert_eq!(backoff(100), Duration::from_secs(32));
    }
}
