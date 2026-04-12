use std::path::PathBuf;
use std::sync::Arc;

use shore_config::load_config;
use shore_daemon::autonomy::manager::AutonomyManager;
use shore_daemon::characters::CharacterRegistry;
use shore_daemon::commands::{CommandContext, SessionTokens};
use shore_daemon::handler::MessageHandler;
use shore_daemon::handshake::build_handshake_provider;
use shore_daemon::notifications::NotificationService;
use shore_daemon_server::registry::{InstanceInfo, Registry};
use shore_daemon_server::{Server, ServerConfig};
use shore_diagnostics::Diagnostics;
use shore_ledger::LedgerClient;
use shore_llm_client::LlmClient;
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

    // Ensure data and runtime directories exist before anything writes to them.
    std::fs::create_dir_all(&loaded.dirs.data)?;
    std::fs::create_dir_all(&loaded.dirs.runtime)?;

    // ── Notification service ──────────────────────────────────────────
    let notifier = NotificationService::new(loaded.app.notifications.clone());

    let instance_id = uuid::Uuid::new_v4().to_string();

    // Resolve listen address: SHORE_ADDR env → config.
    let addr = std::env::var("SHORE_ADDR").unwrap_or_else(|_| loaded.app.daemon.addr.clone());
    let remote_access_warnings = validate_remote_access_policy(
        &addr,
        loaded.app.daemon.unsafe_allow_remote_access,
        &loaded.app.daemon.allowed_hosts,
    )
    .map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidInput, message))?;
    for warning in remote_access_warnings {
        warn!(addr = %addr, warning = %warning, "Daemon remote access warning");
    }

    let server_config = ServerConfig {
        addr: addr.clone(),
        allowed_hosts: loaded.app.daemon.allowed_hosts.clone(),
        server_name: "shore-daemon".into(),
        handshake: None,
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
    let mut server = Server::new(server_config);
    let push_tx = server.event_sender();
    let session_router = server.session_router();
    let route_rx = server.take_route_rx();

    // Create character registry for multi-character management.
    let char_registry = Arc::new(tokio::sync::Mutex::new(CharacterRegistry::new(
        loaded.dirs.config.clone(),
        loaded.dirs.data.clone(),
        push_tx.clone(),
        loaded.clone(),
    )));
    server.set_handshake_provider(build_handshake_provider(char_registry.clone()));

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

    let mut msg_handler = MessageHandler::new(
        char_registry,
        cmd_ctx,
        llm_client,
        push_tx,
        session_router,
        autonomy.clone(),
        notifier,
    );

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

fn validate_remote_access_policy(
    addr: &str,
    unsafe_allow_remote_access: bool,
    allowed_hosts: &[String],
) -> Result<Vec<String>, String> {
    if bind_addr_is_loopback(addr)? {
        return Ok(Vec::new());
    }

    if !unsafe_allow_remote_access {
        return Err(format!(
            "Refusing to bind shore-daemon to non-loopback address {addr}. \
Set [daemon].unsafe_allow_remote_access = true to acknowledge unauthenticated remote TCP exposure. \
[daemon].allowed_hosts is only an IP allowlist and does not provide authentication or TLS."
        ));
    }

    let mut warnings = vec![String::from(
        "Remote TCP access is enabled. Shore does not provide authentication or TLS. Restrict Shore to trusted private or overlay networks; [daemon].allowed_hosts only narrows peer IPs and is not a complete security boundary.",
    )];
    if allowed_hosts.is_empty() {
        warnings.push(String::from(
            "Remote TCP access is enabled with an empty [daemon].allowed_hosts list; any host that can reach the port may connect.",
        ));
    }

    Ok(warnings)
}

fn bind_addr_is_loopback(addr: &str) -> Result<bool, String> {
    if let Ok(socket_addr) = addr.parse::<std::net::SocketAddr>() {
        return Ok(socket_addr.ip().is_loopback());
    }

    let host = extract_bind_host(addr).ok_or_else(|| {
        format!("Invalid daemon listen address {addr:?}. Expected HOST:PORT or [IPv6]:PORT.")
    })?;

    Ok(matches!(host, "localhost" | "127.0.0.1" | "::1"))
}

fn extract_bind_host(addr: &str) -> Option<&str> {
    if let Some(rest) = addr.strip_prefix('[') {
        let (host, suffix) = rest.split_once(']')?;
        if suffix.starts_with(':') && !host.is_empty() {
            return Some(host);
        }
        return None;
    }

    let (host, port) = addr.rsplit_once(':')?;
    if host.is_empty() || port.is_empty() {
        return None;
    }
    Some(host)
}

/// Registry timestamps use RFC3339 for human-readable diagnostics.
fn epoch_timestamp() -> String {
    chrono::Local::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::{epoch_timestamp, validate_remote_access_policy};

    #[test]
    fn loopback_bind_does_not_require_remote_opt_in() {
        let warnings = validate_remote_access_policy("127.0.0.1:7320", false, &[]).unwrap();
        assert!(warnings.is_empty());
    }

    #[test]
    fn remote_bind_requires_explicit_opt_in() {
        let err = validate_remote_access_policy("0.0.0.0:7320", false, &[])
            .expect_err("remote bind without opt-in should fail");
        assert!(err.contains("unsafe_allow_remote_access"));
        assert!(err.contains("allowed_hosts"));
    }

    #[test]
    fn opted_in_remote_bind_warns_when_acl_is_empty() {
        let warnings = validate_remote_access_policy("0.0.0.0:7320", true, &[]).unwrap();
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("does not provide authentication or TLS"));
        assert!(warnings[1].contains("any host that can reach the port may connect"));
    }

    #[test]
    fn opted_in_remote_bind_with_acl_only_warns_about_thin_security() {
        let warnings =
            validate_remote_access_policy("0.0.0.0:7320", true, &[String::from("10.0.0.5")])
                .unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("trusted private or overlay networks"));
    }

    #[test]
    fn localhost_name_is_treated_as_loopback() {
        let warnings = validate_remote_access_policy("localhost:7320", false, &[]).unwrap();
        assert!(warnings.is_empty());
    }

    #[test]
    fn invalid_bind_address_fails_validation() {
        let err = validate_remote_access_policy("not-an-address", false, &[])
            .expect_err("invalid address should fail validation");
        assert!(err.contains("Invalid daemon listen address"));
    }

    #[test]
    fn registry_timestamp_uses_rfc3339() {
        let timestamp = epoch_timestamp();
        assert!(
            chrono::DateTime::parse_from_rfc3339(&timestamp).is_ok(),
            "registry timestamps should use RFC3339, got: {timestamp}"
        );
    }
}
