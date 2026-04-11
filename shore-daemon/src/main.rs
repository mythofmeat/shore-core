use std::path::PathBuf;
use std::sync::Arc;

use shore_config::load_config;
use shore_daemon::autonomy::manager::AutonomyManager;
use shore_daemon::characters::CharacterRegistry;
use shore_daemon::commands::{CommandContext, SessionTokens};
use shore_daemon::handler::MessageHandler;
use shore_daemon::notifications::NotificationService;
use shore_daemon_server::registry::{InstanceInfo, Registry};
use shore_daemon_server::{Server, ServerConfig};
use shore_diagnostics::Diagnostics;
use shore_ledger::LedgerClient;
use shore_llm_client::LlmClient;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── Human-readable logging (journalctl already adds timestamps) ──
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(true)
        .without_time()
        .init();

    // ── Load configuration ───────────────────────────────────────────
    let config_path = std::env::args()
        .nth(1)
        .filter(|a| a == "--config")
        .and_then(|_| std::env::args().nth(2))
        .map(PathBuf::from);

    let loaded = load_config(config_path.as_deref())?;
    info!("Configuration loaded");

    // Ensure data and runtime directories exist before anything writes to them.
    std::fs::create_dir_all(&loaded.dirs.data)?;
    std::fs::create_dir_all(&loaded.dirs.runtime)?;

    // ── Notification service ──────────────────────────────────────────
    let notifier = NotificationService::new(loaded.app.notifications.clone());

    let instance_id = uuid::Uuid::new_v4().to_string();

    // Resolve listen address: SHORE_ADDR env → config.
    let addr = std::env::var("SHORE_ADDR").unwrap_or_else(|_| loaded.app.daemon.addr.clone());

    let server_config = ServerConfig {
        addr: addr.clone(),
        allowed_hosts: loaded.app.daemon.allowed_hosts.clone(),
        server_name: "shore-daemon".into(),
    };

    // ── Register instance ────────────────────────────────────────────
    let registry = Registry::default_path();
    let instance_info = InstanceInfo {
        id: instance_id.clone(),
        pid: std::process::id(),
        addr,
        started_at: epoch_timestamp(),
        data_dir: Some(loaded.dirs.data.display().to_string()),
    };
    registry.register(instance_info)?;
    info!(instance_id = %instance_id, "Registered daemon instance");

    // ── Shutdown signal (SIGINT / SIGTERM) ──────────────────────────
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(());
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();

        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = signal(SignalKind::terminate())
                .expect("startup-fatal: failed to listen for SIGTERM");
            tokio::select! {
                _ = ctrl_c => info!("Received SIGINT"),
                _ = sigterm.recv() => info!("Received SIGTERM"),
            }
        }

        #[cfg(not(unix))]
        {
            ctrl_c
                .await
                .expect("startup-fatal: failed to listen for Ctrl+C");
            info!("Received shutdown signal");
        }

        let _ = shutdown_tx.send(());
    });

    // ── Create server and message handler ─────────────────────────────
    let server = Server::new(server_config);
    let push_tx = server.push_sender();
    let route_rx = server.take_route_rx();

    // Create character registry for multi-character management.
    let char_registry = Arc::new(tokio::sync::Mutex::new(CharacterRegistry::new(
        loaded.dirs.config.clone(),
        loaded.dirs.data.clone(),
        push_tx.clone(),
        loaded.clone(),
    )));

    // Create autonomy manager (shared between handler, commands, and per-character tick tasks).
    let mut autonomy = AutonomyManager::new(
        loaded.app.behavior.autonomy.clone(),
        loaded.app.memory.compaction.clone(),
        loaded.dirs.data.clone(),
        shutdown_rx.clone(),
    );

    let mut raw_llm_client = LlmClient::new();
    if loaded.app.advanced.api_payload_logging {
        raw_llm_client.set_payload_log_dir(loaded.dirs.data.clone());
        info!(
            "API payload logging enabled → {}/api_payloads.jsonl",
            loaded.dirs.data.display()
        );
    }

    // Always-on cache forensics — writes to {data_dir}/cache_forensics.jsonl.
    // Logs every Anthropic cache placement + response so cache misses are
    // diagnosable after the fact without needing to reproduce.
    shore_llm_client::cache_forensics::enable(loaded.dirs.data.clone());
    info!(
        "Cache forensics enabled → {}/cache_forensics.jsonl",
        loaded.dirs.data.display()
    );

    let llm_client = LedgerClient::new(raw_llm_client, &loaded.dirs.data.join("ledger.db"))?;

    // Reconstruct cache tracker state from the ledger for each known character.
    // This prevents false-positive anomalies when the cache is still warm from
    // before the restart.
    for character in shore_config::discover_characters(&loaded.dirs.config) {
        llm_client.reconstruct_cache_state(&character, 3600);
    }

    // Provide the autonomy manager with resources for interiority/keepalive execution.
    autonomy.set_resources(
        llm_client.clone(),
        push_tx.clone(),
        loaded.clone(),
        notifier.clone(),
    );
    autonomy.set_registry(char_registry.clone());

    // In-memory diagnostic ring buffers (API calls, tool calls, errors).
    // Writer: generation tasks (handler.rs). Reader: `status`/`diagnostics` commands.
    let diagnostics = Arc::new(std::sync::Mutex::new(Diagnostics::default()));
    // Accumulated token counts for the daemon's lifetime.
    // Writer: generation tasks after each API response. Reader: `status` command.
    let session_tokens = Arc::new(std::sync::Mutex::new(SessionTokens::default()));

    let cmd_ctx = CommandContext {
        config: loaded.clone(),
        push_tx: push_tx.clone(),
        data_dir: loaded.dirs.data.clone(),
        active_model: None,
        session_tokens: session_tokens.clone(),
        autonomy: autonomy.clone(),
        llm_client: llm_client.clone(),
        diagnostics: diagnostics.clone(),
        memory_shell_sessions: std::collections::HashMap::new(),
    };

    let mut msg_handler = MessageHandler {
        registry: char_registry,
        cmd_ctx,
        llm_client,
        push_tx,
        autonomy: autonomy.clone(),
        notifier,
        generation_handle: None,
    };

    // Spawn message handler as a background task.
    let handler_handle = tokio::spawn(async move {
        msg_handler.run(route_rx).await;
    });

    // ── Run server ───────────────────────────────────────────────────
    let result = server.run(shutdown_rx).await;

    // Drop the server so its route_tx is released, unblocking the handler.
    drop(server);

    // ── Wait for handler and autonomy tasks to finish ─────────────────
    let shutdown_timeout = std::time::Duration::from_secs(10);

    let _ = tokio::time::timeout(shutdown_timeout, handler_handle).await;
    let _ = tokio::time::timeout(shutdown_timeout, autonomy.shutdown()).await;

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
