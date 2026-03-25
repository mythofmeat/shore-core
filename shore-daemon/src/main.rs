mod server;

use std::path::PathBuf;

use server::registry::{InstanceInfo, Registry};
use server::{Server, ServerConfig};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── Structured JSON logging with rid propagation ─────────────────
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(true)
        .with_thread_ids(true)
        .init();

    // ── Determine socket path ────────────────────────────────────────
    let instance_id = uuid::Uuid::new_v4().to_string();
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(dirs::runtime_dir)
        .unwrap_or_else(std::env::temp_dir);
    let socket_path = runtime_dir.join("shore").join(format!("{}.sock", instance_id));

    let tcp_addr = std::env::var("SHORE_TCP_ADDR").ok();

    let config = ServerConfig {
        socket_path: socket_path.clone(),
        tcp_addr: tcp_addr.clone(),
        server_name: "shore-daemon".into(),
    };

    // ── Register instance ────────────────────────────────────────────
    let registry = Registry::default_path();
    let instance_info = InstanceInfo {
        id: instance_id.clone(),
        pid: std::process::id(),
        socket_path: socket_path.display().to_string(),
        tcp_addr,
        started_at: epoch_timestamp(),
    };
    registry.register(instance_info)?;
    info!(instance_id = %instance_id, "Registered daemon instance");

    // ── Shutdown signal (Ctrl+C) ─────────────────────────────────────
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(());
    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for Ctrl+C");
        info!("Received shutdown signal");
        let _ = shutdown_tx.send(());
    });

    // ── Run server ───────────────────────────────────────────────────
    let server = Server::new(config);
    let result = server.run(shutdown_rx).await;

    // ── Cleanup ──────────────────────────────────────────────────────
    if let Err(e) = registry.unregister(&instance_id) {
        error!(error = %e, "Failed to unregister instance");
    }
    info!("Daemon shut down cleanly");

    result?;
    Ok(())
}

/// Simple epoch-seconds timestamp without pulling in chrono.
fn epoch_timestamp() -> String {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s-since-epoch", duration.as_secs())
}
