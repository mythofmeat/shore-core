use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;
use shore_config::{config_dir, load_config, ConfigError, LoadedConfig};
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

#[derive(Debug, Parser)]
#[command(name = "shore-daemon", about = "Shore daemon")]
struct Cli {
    /// Config file to load instead of $XDG_CONFIG_HOME/shore/config.toml.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// TCP listen address for this process (overrides SHORE_ADDR and config).
    #[arg(long, value_name = "ADDR")]
    addr: Option<String>,

    /// Pin the registered instance ID in `instances.json`.
    ///
    /// When unset, a fresh UUID is generated on every startup. Set this
    /// to give the daemon a stable, discoverable ID — used by `shore-mcp`
    /// to rediscover a previously-spawned test daemon.
    #[arg(long, value_name = "ID")]
    instance_id: Option<String>,
}

#[derive(Debug)]
struct StartupConfig {
    loaded: LoadedConfig,
    config_path: PathBuf,
    bind_addr: String,
    bind_addr_source: StartupValueSource,
    remote_access_warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupValueSource {
    Cli,
    Env,
    Config,
}

impl std::fmt::Display for StartupValueSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            StartupValueSource::Cli => "--addr",
            StartupValueSource::Env => "SHORE_ADDR",
            StartupValueSource::Config => "[daemon].addr",
        };
        f.write_str(label)
    }
}

#[derive(Debug, thiserror::Error)]
enum StartupError {
    #[error("Invalid --config path {path}: {reason}")]
    InvalidConfigPath { path: PathBuf, reason: String },

    #[error("Failed to load Shore config from {path}: {source}")]
    LoadConfig {
        path: PathBuf,
        #[source]
        source: ConfigError,
    },

    #[error("Refusing startup for daemon address {addr} (from {bind_addr_source}): {message}")]
    RemoteAccessPolicy {
        addr: String,
        bind_addr_source: StartupValueSource,
        message: String,
    },

    #[error("Failed to create {kind} directory {path}: {source}")]
    CreateDir {
        kind: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to register daemon instance in {path}: {source}")]
    RegisterInstance {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to start shore-daemon on {addr}: {source}")]
    ServerRun {
        addr: String,
        #[source]
        source: std::io::Error,
    },
}

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

    let cli_parsed = Cli::parse();
    let instance_id_override = cli_parsed.instance_id.clone();
    let StartupConfig {
        loaded,
        config_path,
        bind_addr: addr,
        bind_addr_source,
        remote_access_warnings,
    } = resolve_startup(cli_parsed, startup_env_addr())?;
    info!(
        config_path = %config_path.display(),
        bind_addr = %addr,
        bind_addr_source = %bind_addr_source,
        "Startup configuration resolved"
    );

    // Ensure data and runtime directories exist before anything writes to them.
    std::fs::create_dir_all(&loaded.dirs.data).map_err(|source| StartupError::CreateDir {
        kind: "data",
        path: loaded.dirs.data.clone(),
        source,
    })?;
    std::fs::create_dir_all(&loaded.dirs.runtime).map_err(|source| StartupError::CreateDir {
        kind: "runtime",
        path: loaded.dirs.runtime.clone(),
        source,
    })?;

    // ── Notification service ──────────────────────────────────────────
    let notifier = NotificationService::new(loaded.app.notifications.clone());

    let instance_id = instance_id_override
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    for warning in remote_access_warnings {
        warn!(
            addr = %addr,
            bind_addr_source = %bind_addr_source,
            warning = %warning,
            "Daemon remote access warning"
        );
    }

    // Pre-bind the TCP listener so we can resolve port-zero binds (e.g.
    // `--addr 127.0.0.1:0`) before recording the addr in the instance
    // registry. Without this, the registry holds the literal `:0` and any
    // discovery client connects to a port that was never actually opened.
    let pre_bind_config = ServerConfig {
        addr: addr.clone(),
        allowed_hosts: loaded.app.daemon.allowed_hosts.clone(),
        server_name: "shore-daemon".into(),
        handshake: None,
    };
    let pre_bind_server = Server::new(pre_bind_config);
    let listener = pre_bind_server
        .bind()
        .await
        .map_err(|source| StartupError::ServerRun {
            addr: addr.clone(),
            source,
        })?;
    let resolved_addr = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| addr.clone());
    drop(pre_bind_server);

    let server_config = ServerConfig {
        addr: resolved_addr.clone(),
        allowed_hosts: loaded.app.daemon.allowed_hosts.clone(),
        server_name: "shore-daemon".into(),
        handshake: None,
    };

    // ── Register instance ────────────────────────────────────────────
    let registry = Registry::default_path();
    let instance_info = InstanceInfo {
        id: instance_id.clone(),
        pid: std::process::id(),
        addr: resolved_addr.clone(),
        started_at: epoch_timestamp(),
        data_dir: Some(loaded.dirs.data.display().to_string()),
    };
    registry
        .register(instance_info)
        .map_err(|source| StartupError::RegisterInstance {
            path: registry.path().to_path_buf(),
            source,
        })?;
    info!(
        instance_id = %instance_id,
        registry_path = %registry.path().display(),
        addr = %resolved_addr,
        data_dir = %loaded.dirs.data.display(),
        "Registered daemon instance"
    );

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

    let cache_forensics_path = loaded.dirs.data.join("cache_forensics.jsonl");
    if loaded.app.advanced.cache_forensics {
        shore_llm_client::cache_forensics::enable(loaded.dirs.data.clone());
        info!(
            path = %cache_forensics_path.display(),
            "Cache forensics enabled"
        );
    } else {
        info!(
            path = %cache_forensics_path.display(),
            "Cache forensics disabled; set [advanced].cache_forensics = true to enable"
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
    let result = server
        .run_with_listener(listener, shutdown_rx)
        .await
        .map_err(|source| StartupError::ServerRun {
            addr: resolved_addr.clone(),
            source,
        });
    if let Err(error) = &result {
        error!(addr = %resolved_addr, error = %error, "Daemon server exited with error");
    }

    // Drop the server so its route_tx is released, unblocking the handler.
    drop(server);

    // ── Wait for handler and autonomy tasks to finish ─────────────────
    let shutdown_timeout = std::time::Duration::from_secs(10);

    let _ = tokio::time::timeout(shutdown_timeout, handler_handle).await;
    let _ = tokio::time::timeout(shutdown_timeout, autonomy.shutdown()).await;

    // ── Cleanup ──────────────────────────────────────────────────────
    if let Err(e) = registry.unregister(&instance_id) {
        error!(
            instance_id = %instance_id,
            registry_path = %registry.path().display(),
            error = %e,
            "Failed to unregister daemon instance"
        );
    } else {
        info!(
            instance_id = %instance_id,
            registry_path = %registry.path().display(),
            "Unregistered daemon instance"
        );
    }
    info!("Daemon shut down cleanly");

    result?;
    Ok(())
}

fn resolve_startup(cli: Cli, env_addr: Option<String>) -> Result<StartupConfig, StartupError> {
    let explicit_config_path = resolve_explicit_config_path(cli.config.as_deref())?;
    let config_path_for_errors = explicit_config_path
        .clone()
        .unwrap_or_else(default_config_path);
    let loaded = load_config(explicit_config_path.as_deref()).map_err(|source| {
        StartupError::LoadConfig {
            path: config_path_for_errors.clone(),
            source,
        }
    })?;
    let (bind_addr, bind_addr_source) = resolve_listen_addr(cli.addr, env_addr, &loaded);
    let remote_access_warnings = validate_remote_access_policy(
        &bind_addr,
        loaded.app.daemon.unsafe_allow_remote_access,
        &loaded.app.daemon.allowed_hosts,
    )
    .map_err(|message| StartupError::RemoteAccessPolicy {
        addr: bind_addr.clone(),
        bind_addr_source,
        message,
    })?;

    Ok(StartupConfig {
        loaded,
        config_path: explicit_config_path.unwrap_or_else(default_config_path),
        bind_addr,
        bind_addr_source,
        remote_access_warnings,
    })
}

fn resolve_explicit_config_path(
    config_path: Option<&Path>,
) -> Result<Option<PathBuf>, StartupError> {
    let Some(path) = config_path else {
        return Ok(None);
    };

    if !path.exists() {
        return Err(StartupError::InvalidConfigPath {
            path: path.to_path_buf(),
            reason: String::from("file does not exist"),
        });
    }

    if path.is_dir() {
        return Err(StartupError::InvalidConfigPath {
            path: path.to_path_buf(),
            reason: String::from("expected a config.toml file, not a directory"),
        });
    }

    Ok(Some(path.to_path_buf()))
}

fn resolve_listen_addr(
    cli_addr: Option<String>,
    env_addr: Option<String>,
    loaded: &LoadedConfig,
) -> (String, StartupValueSource) {
    if let Some(addr) = cli_addr {
        return (addr, StartupValueSource::Cli);
    }

    if let Some(addr) = env_addr.filter(|value| !value.trim().is_empty()) {
        return (addr, StartupValueSource::Env);
    }

    (loaded.app.daemon.addr.clone(), StartupValueSource::Config)
}

fn startup_env_addr() -> Option<String> {
    std::env::var("SHORE_ADDR")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn default_config_path() -> PathBuf {
    config_dir().join("config.toml")
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
    use std::path::{Path, PathBuf};

    use super::{
        epoch_timestamp, resolve_explicit_config_path, resolve_listen_addr, resolve_startup,
        validate_remote_access_policy, Cli, StartupError, StartupValueSource,
    };
    use clap::Parser;
    use tempfile::TempDir;

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

    #[test]
    fn cli_parses_startup_flags() {
        let cli = Cli::try_parse_from([
            "shore-daemon",
            "--config",
            "/tmp/shore/config.toml",
            "--addr",
            "127.0.0.1:9000",
        ])
        .unwrap();

        assert_eq!(cli.config, Some(PathBuf::from("/tmp/shore/config.toml")));
        assert_eq!(cli.addr.as_deref(), Some("127.0.0.1:9000"));
    }

    #[test]
    fn cli_parses_instance_id_flag() {
        let cli = Cli::try_parse_from([
            "shore-daemon",
            "--instance-id",
            "shore-mcp-test",
        ])
        .unwrap();
        assert_eq!(cli.instance_id.as_deref(), Some("shore-mcp-test"));
    }

    #[test]
    fn cli_instance_id_defaults_to_none() {
        let cli = Cli::try_parse_from(["shore-daemon"]).unwrap();
        assert!(cli.instance_id.is_none());
    }

    #[test]
    fn explicit_config_path_must_exist() {
        let err = resolve_explicit_config_path(Some(Path::new("/definitely/missing.toml")))
            .expect_err("missing explicit config path should fail");
        assert!(matches!(err, StartupError::InvalidConfigPath { .. }));
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn explicit_config_path_rejects_directories() {
        let tmp = TempDir::new().unwrap();
        let err = resolve_explicit_config_path(Some(tmp.path()))
            .expect_err("directory should not be accepted as config file");
        assert!(matches!(err, StartupError::InvalidConfigPath { .. }));
        assert!(err.to_string().contains("expected a config.toml file"));
    }

    #[test]
    fn listen_addr_precedence_is_cli_then_env_then_config() {
        let tmp = TempDir::new().unwrap();
        let loaded = shore_config::LoadedConfig::new_for_test(
            shore_config::app::AppConfig::default(),
            shore_config::models::ModelCatalog::default(),
            shore_config::ShoreDirs {
                config: tmp.path().join("config"),
                data: tmp.path().join("data"),
                runtime: tmp.path().join("runtime"),
                cache: tmp.path().join("cache"),
            },
        );

        let (addr, source) = resolve_listen_addr(
            Some("127.0.0.1:9000".into()),
            Some("127.0.0.1:8000".into()),
            &loaded,
        );
        assert_eq!(addr, "127.0.0.1:9000");
        assert_eq!(source, StartupValueSource::Cli);

        let (addr, source) = resolve_listen_addr(None, Some("127.0.0.1:8000".into()), &loaded);
        assert_eq!(addr, "127.0.0.1:8000");
        assert_eq!(source, StartupValueSource::Env);

        let (addr, source) = resolve_listen_addr(None, None, &loaded);
        assert_eq!(addr, "127.0.0.1:7320");
        assert_eq!(source, StartupValueSource::Config);
    }

    #[test]
    fn resolve_startup_uses_single_precedence_model() {
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let config_path = config_dir.join("config.toml");
        std::fs::write(
            &config_path,
            r#"
[daemon]
addr = "127.0.0.1:7000"
unsafe_allow_remote_access = true
"#,
        )
        .unwrap();

        let startup = resolve_startup(
            Cli {
                config: Some(config_path.clone()),
                addr: Some("0.0.0.0:9000".into()),
                instance_id: None,
            },
            Some("127.0.0.1:8000".into()),
        )
        .unwrap();

        assert_eq!(startup.config_path, config_path);
        assert_eq!(startup.bind_addr, "0.0.0.0:9000");
        assert_eq!(startup.bind_addr_source, StartupValueSource::Cli);
        assert!(!startup.remote_access_warnings.is_empty());
    }

    #[test]
    fn resolve_startup_reports_env_remote_access_source() {
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let config_path = config_dir.join("config.toml");
        std::fs::write(&config_path, "").unwrap();

        let err = resolve_startup(
            Cli {
                config: Some(config_path),
                addr: None,
                instance_id: None,
            },
            Some("0.0.0.0:9000".into()),
        )
        .expect_err("non-loopback SHORE_ADDR should still enforce remote-access policy");

        match err {
            StartupError::RemoteAccessPolicy {
                bind_addr_source, ..
            } => {
                assert_eq!(bind_addr_source, StartupValueSource::Env);
            }
            other => panic!("expected remote access policy error, got {other:?}"),
        }
    }
}
