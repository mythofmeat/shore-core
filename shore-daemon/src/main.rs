mod commands;
mod config;
mod engine;
mod server;
mod supervisor;

use std::path::PathBuf;

use config::load_config;
use server::registry::{InstanceInfo, Registry};
use server::{Server, ServerConfig};
use supervisor::Supervisor;
use tracing::{error, info, warn};
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

    // ── Load configuration ───────────────────────────────────────────
    let config_path = std::env::args()
        .nth(1)
        .filter(|a| a == "--config")
        .and_then(|_| std::env::args().nth(2))
        .map(PathBuf::from);

    let loaded = load_config(config_path.as_deref())?;
    info!(character = %loaded.app.character.name, "Configuration loaded");

    // ── Determine socket path ────────────────────────────────────────
    let instance_id = uuid::Uuid::new_v4().to_string();
    let socket_path = loaded
        .app
        .daemon
        .socket_path
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            loaded
                .dirs
                .runtime
                .join(format!("{}.sock", instance_id))
        });

    let tcp_addr = loaded
        .app
        .daemon
        .tcp_addr
        .clone()
        .or_else(|| std::env::var("SHORE_TCP_ADDR").ok());

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

    // ── Start process supervisor ─────────────────────────────────────
    let mut sup = Supervisor::from_config(&loaded.app.services, &loaded.dirs.runtime);
    let has_services = !sup.states().is_empty();
    let llm_ready_rx = sup.llm_ready();

    let supervisor_shutdown_rx = shutdown_rx.clone();
    let supervisor_handle = if has_services {
        let handle = tokio::spawn(async move {
            sup.run(supervisor_shutdown_rx).await;
        });

        // Wait for shore-llm to become ready before accepting SWP connections.
        if sup_has_llm(&loaded.app.services) {
            info!("Waiting for shore-llm to become ready before accepting connections...");
            let mut rx = llm_ready_rx;
            loop {
                if *rx.borrow() {
                    break;
                }
                if rx.changed().await.is_err() {
                    warn!("Supervisor shut down before shore-llm became ready");
                    break;
                }
            }
            info!("shore-llm is ready, starting SWP server");
        }
        Some(handle)
    } else {
        None
    };

    // ── Run server ───────────────────────────────────────────────────
    let server = Server::new(config);
    let result = server.run(shutdown_rx).await;

    // ── Wait for supervisor to finish ────────────────────────────────
    if let Some(handle) = supervisor_handle {
        let _ = handle.await;
    }

    // ── Cleanup ──────────────────────────────────────────────────────
    if let Err(e) = registry.unregister(&instance_id) {
        error!(error = %e, "Failed to unregister instance");
    }
    info!("Daemon shut down cleanly");

    result?;
    Ok(())
}

/// Check if shore-llm is configured as a supervised service.
fn sup_has_llm(services: &config::app::ServicesConfig) -> bool {
    services.llm.enabled && services.llm.command.is_some()
}

/// Simple epoch-seconds timestamp without pulling in chrono.
fn epoch_timestamp() -> String {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s-since-epoch", duration.as_secs())
}
