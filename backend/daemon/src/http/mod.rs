//! Daemon-side HTTP listener.
//!
//! Off by default. Currently only used as a callback target for the
//! `claude_code` provider's MCP host: the daemon spawns a `claude`
//! subprocess pointing at `http://<bind_addr>/mcp/<session-id>` and
//! the CLI calls back into this listener for each tool invocation.
//!
//! M2 (this commit) lands the listener scaffold and a `/healthz`
//! probe. M3 mounts the MCP routes on top.
//!
//! Configuration lives at `[daemon.http]` in `config.toml`:
//!
//! ```toml
//! [daemon.http]
//! enabled = true
//! bind_addr = "127.0.0.1:0"   # 0 = ephemeral, resolved at startup
//! ```

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{routing::get, Json, Router};
use serde_json::{json, Value};
use shore_config::app::DaemonHttpConfig;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{error, info};

/// Shared daemon HTTP state surfaced to other components.
///
/// Engine code consults `bind_addr` to construct callback URLs for
/// the `claude_code` provider. M3 will extend this struct with the
/// per-request MCP session registry; M2 only carries the bind address.
#[derive(Debug, Clone)]
pub struct DaemonHttpState {
    /// Resolved bind address. Always concrete — `127.0.0.1:0` configs
    /// are resolved to an ephemeral port before the listener is
    /// returned.
    pub bind_addr: SocketAddr,
}

impl DaemonHttpState {
    /// URL prefix usable for callback URLs. Always `http://<bind>`
    /// (loopback-only by default — see `validate_remote_access_policy`
    /// in `main.rs` for the reasoning).
    pub fn base_url(&self) -> String {
        format!("http://{}", self.bind_addr)
    }
}

/// Spawn the daemon's HTTP listener in the background.
///
/// Returns `Ok(None)` when `[daemon.http].enabled = false` (the default).
/// Returns `Ok(Some((state, handle)))` when the listener is up.
///
/// Bind errors propagate as `std::io::Error` — the caller decides
/// whether to abort startup or fall back gracefully. (For `claude_code`
/// we want the daemon to fail loudly; for other providers the
/// listener is optional.)
pub async fn spawn_listener(
    config: &DaemonHttpConfig,
    shutdown: watch::Receiver<()>,
) -> std::io::Result<Option<(Arc<DaemonHttpState>, JoinHandle<()>)>> {
    if !config.enabled {
        return Ok(None);
    }
    let listener = TcpListener::bind(&config.bind_addr).await?;
    let bind_addr = listener.local_addr()?;
    let state = Arc::new(DaemonHttpState { bind_addr });
    info!(
        bind_addr = %bind_addr,
        "Daemon HTTP listener bound"
    );

    let app = build_router(state.clone());
    let mut shutdown_rx = shutdown.clone();
    let handle = tokio::spawn(async move {
        let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = shutdown_rx.changed().await;
        });
        if let Err(e) = serve.await {
            error!(error = %e, "Daemon HTTP listener exited with error");
        }
    });
    Ok(Some((state, handle)))
}

/// Construct the daemon HTTP router. Public so M3's MCP routes can
/// be plugged in by extending this in-place rather than threading
/// the listener through.
fn build_router(state: Arc<DaemonHttpState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .with_state(state)
}

async fn healthz() -> Json<Value> {
    Json(json!({"ok": true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn cfg(enabled: bool) -> DaemonHttpConfig {
        DaemonHttpConfig {
            enabled,
            bind_addr: "127.0.0.1:0".into(),
        }
    }

    #[tokio::test]
    async fn disabled_returns_none() {
        let (_tx, rx) = watch::channel(());
        let result = spawn_listener(&cfg(false), rx).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn enabled_serves_healthz() {
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        let (state, _handle) = spawn_listener(&cfg(true), shutdown_rx)
            .await
            .unwrap()
            .expect("listener should be present when enabled");

        // Race against startup: poll briefly until the listener is
        // ready. axum::serve binds before the spawn returns, so this
        // is usually a single-shot.
        let url = format!("{}/healthz", state.base_url());
        let client = reqwest::Client::new();
        let mut attempts = 0;
        let response = loop {
            match client.get(&url).send().await {
                Ok(r) => break r,
                Err(_) if attempts < 20 => {
                    attempts += 1;
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(e) => panic!("healthz request failed: {e}"),
            }
        };
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["ok"], true);

        let _ = shutdown_tx.send(());
    }

    #[tokio::test]
    async fn base_url_is_http_loopback() {
        let (_tx, rx) = watch::channel(());
        let (state, _handle) = spawn_listener(&cfg(true), rx).await.unwrap().unwrap();
        assert!(state.base_url().starts_with("http://127.0.0.1:"));
        // Ephemeral port is non-zero after binding.
        assert_ne!(state.bind_addr.port(), 0);
    }
}
