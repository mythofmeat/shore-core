use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use shore_config::{load_config, LoadedConfig};
use shore_daemon::autonomy::manager::AutonomyManager;
use shore_daemon::characters::CharacterRegistry;
use shore_daemon::commands::{CommandContext, SessionTokens};
use shore_daemon::handler::MessageHandler;
use shore_daemon::notifications::NotificationService;
use shore_daemon::server::registry::{InstanceInfo, Registry};
use shore_daemon::server::{Server, ServerConfig};
use shore_diagnostics::Diagnostics;
use shore_ledger::LedgerClient;
use shore_llm_client::LlmClient;
use shore_protocol::server_msg::ServerMessage;
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};
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

    // ── Notification service ──────────────────────────────────────────
    let notifier = NotificationService::new(loaded.app.notifications.clone());

    // ── Determine socket path ────────────────────────────────────────
    let instance_id = uuid::Uuid::new_v4().to_string();
    let socket_path = loaded
        .app
        .daemon
        .socket_path
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| loaded.dirs.runtime.join(format!("{}.sock", instance_id)));

    // Resolve TCP config: config file → SHORE_TCP_ADDR env var fallback.
    let tcp_config = match loaded.app.connections.tcp.clone() {
        Some(tcp) => Some(tcp),
        None => std::env::var("SHORE_TCP_ADDR")
            .ok()
            .map(|addr| shore_config::app::TcpConfig {
                enabled: true,
                addr: Some(addr),
                allowed_hosts: vec![],
            }),
    };

    let tcp_addr = tcp_config
        .as_ref()
        .filter(|t| t.enabled)
        .and_then(|t| t.addr.clone());

    let server_config = ServerConfig {
        socket_path: socket_path.clone(),
        tcp: tcp_config,
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
            let mut sigterm =
                signal(SignalKind::terminate()).expect("Failed to listen for SIGTERM");
            tokio::select! {
                _ = ctrl_c => info!("Received SIGINT"),
                _ = sigterm.recv() => info!("Received SIGTERM"),
            }
        }

        #[cfg(not(unix))]
        {
            ctrl_c.await.expect("Failed to listen for Ctrl+C");
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
    let (mut autonomy, compaction_rx) = AutonomyManager::new(
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

    // Shared flag: compaction task sets this after successful compaction so the
    // message handler knows the next message is the first after compaction
    // (expected cache miss, not an invalidation).
    let compaction_occurred = Arc::new(AtomicBool::new(false));
    // True at startup, cleared by the first successful generation. Suppresses
    // cache-invalidation warning on the first message (expected cache miss).
    // Writer/reader: generation task (handler.rs).
    let is_first_after_restart = Arc::new(AtomicBool::new(true));
    // Set on first API response with cache_read_tokens > 0. Distinguishes
    // "cache never populated" from "cache invalidated". Writer/reader: generation task.
    let has_seen_cache_read = Arc::new(AtomicBool::new(false));

    // Spawn background compaction task driven by autonomy idle triggers.
    let compaction_handle = {
        let config = loaded.clone();
        let compaction_llm_client = llm_client.clone();
        let data_dir = loaded.dirs.data.clone();
        let compaction_push_tx = push_tx.clone();
        let compaction_notifier = notifier.clone();
        let compaction_flag = compaction_occurred.clone();
        let compaction_autonomy = autonomy.clone();
        tokio::spawn(async move {
            compaction_task(
                compaction_rx,
                config,
                compaction_llm_client,
                data_dir,
                compaction_push_tx,
                compaction_notifier,
                compaction_flag,
                compaction_autonomy,
            )
            .await;
        })
    };

    let mut msg_handler = MessageHandler {
        registry: char_registry,
        cmd_ctx,
        llm_client,
        push_tx,
        is_first_after_restart,
        has_seen_cache_read,
        compaction_occurred,
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

    // ── Wait for handler, autonomy tasks, and supervisor to finish ───
    // Use timeouts to prevent indefinite hangs during shutdown.
    // Order matters: autonomy must shut down before compaction, because
    // autonomy holds the compaction channel sender — dropping it unblocks
    // the compaction task's recv().
    let shutdown_timeout = std::time::Duration::from_secs(10);

    let _ = tokio::time::timeout(shutdown_timeout, handler_handle).await;
    let _ = tokio::time::timeout(shutdown_timeout, autonomy.shutdown()).await;
    // Drop the autonomy manager so its compaction_tx sender is released,
    // allowing the compaction task's recv() to return None and exit.
    drop(autonomy);
    let _ = tokio::time::timeout(shutdown_timeout, compaction_handle).await;

    // ── Cleanup ──────────────────────────────────────────────────────
    if let Err(e) = registry.unregister(&instance_id) {
        error!(error = %e, "Failed to unregister instance");
    }
    info!("Daemon shut down cleanly");

    result?;
    Ok(())
}

/// Background task that processes compaction triggers from the autonomy system.
///
/// Reads character names from the channel and runs compaction for each.
/// Reads `active.jsonl` directly (no engine access needed).
#[allow(clippy::too_many_arguments)]
async fn compaction_task(
    mut rx: mpsc::Receiver<String>,
    config: LoadedConfig,
    llm_client: LedgerClient,
    data_dir: PathBuf,
    push_tx: broadcast::Sender<ServerMessage>,
    notifier: NotificationService,
    compaction_occurred: std::sync::Arc<AtomicBool>,
    autonomy: AutonomyManager,
) {
    while let Some(character) = rx.recv().await {
        info!(character = %character, "Background compaction triggered");

        match shore_daemon::memory::compaction::run_compaction(
            &character,
            &config,
            &llm_client,
            &data_dir,
            &push_tx,
            &notifier,
        )
        .await
        {
            Ok(retained_count) => {
                compaction_occurred.store(true, std::sync::atomic::Ordering::Release);
                autonomy.notify_compaction_complete(&character, retained_count);
            }
            Err(e) => {
                warn!(
                    character = %character,
                    error = %e,
                    "Background compaction failed"
                );
                autonomy.notify_compaction_failed(&character);
            }
        }
    }
    info!("Background compaction task shutting down");
}

/// Simple epoch-seconds timestamp without pulling in chrono.
fn epoch_timestamp() -> String {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s-since-epoch", duration.as_secs())
}
