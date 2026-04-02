use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use shore_daemon::autonomy::manager::AutonomyManager;
use shore_daemon::characters::CharacterRegistry;
use shore_daemon::commands::{CommandContext, SessionTokens};
use shore_diagnostics::Diagnostics;
use shore_config::{load_config, load_character_config, load_character_definition, resolve_prompt_template, resolve_user_definition, LoadedConfig};
use shore_daemon::handler::MessageHandler;
use shore_llm_client::LlmClient;
use shore_daemon::notifications::NotificationService;
use shore_daemon::memory::collation::{
    CollationConfig as LibCollationConfig, CollationManager, DEFAULT_REFINE_PROMPT,
};
use shore_daemon::memory::collation_impls::RealCollationLlm;
use shore_daemon::commands::state::resolve_collation_model;
use shore_daemon::memory::compaction::{
    CompactionConfig, CompactionManager, CompactionOutcome, ConversationMessage,
    DEFAULT_COMPACT_PROMPT,
};
use shore_daemon::memory::compaction_impls::{
    resolve_embed_config, RealCompactionLlm, RealConversationManager, RealVectorIndexer,
};
use shore_daemon::memory::db::MemoryDB;
use shore_daemon::memory::vectorstore::VectorStore;
use shore_daemon::server::registry::{InstanceInfo, Registry};
use shore_daemon::server::{Server, ServerConfig};
use shore_protocol::server_msg::ServerMessage;
use shore_protocol::types::ContentBlock;
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
        .unwrap_or_else(|| {
            loaded
                .dirs
                .runtime
                .join(format!("{}.sock", instance_id))
        });

    // Resolve TCP config: config file → SHORE_TCP_ADDR env var fallback.
    let tcp_config = match loaded.app.connections.tcp.clone() {
        Some(tcp) => Some(tcp),
        None => std::env::var("SHORE_TCP_ADDR").ok().map(|addr| {
            shore_config::app::TcpConfig {
                enabled: true,
                addr: Some(addr),
                allowed_hosts: vec![],
            }
        }),
    };

    let tcp_addr = tcp_config.as_ref()
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
                .expect("Failed to listen for SIGTERM");
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

    let mut llm_client = LlmClient::new();
    if loaded.app.advanced.api_payload_logging {
        llm_client.set_payload_log_dir(loaded.dirs.data.clone());
        info!("API payload logging enabled → {}/api_payloads.jsonl", loaded.dirs.data.display());
    }

    // Provide the autonomy manager with resources for interiority/keepalive execution.
    autonomy.set_resources(llm_client.clone(), push_tx.clone(), loaded.clone(), notifier.clone());

    let diagnostics = Arc::new(std::sync::Mutex::new(Diagnostics::default()));
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
    let is_first_after_restart = Arc::new(AtomicBool::new(true));
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
            compaction_task(compaction_rx, config, compaction_llm_client, data_dir, compaction_push_tx, compaction_notifier, compaction_flag, compaction_autonomy).await;
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
async fn compaction_task(
    mut rx: mpsc::Receiver<String>,
    config: LoadedConfig,
    llm_client: LlmClient,
    data_dir: PathBuf,
    push_tx: broadcast::Sender<ServerMessage>,
    notifier: NotificationService,
    compaction_occurred: std::sync::Arc<AtomicBool>,
    autonomy: AutonomyManager,
) {
    while let Some(character) = rx.recv().await {
        info!(character = %character, "Background compaction triggered");

        match run_compaction(&character, &config, &llm_client, &data_dir, &push_tx, &notifier).await {
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

/// Run compaction for a single character (called from the background task).
/// Returns the number of retained turns on success.
async fn run_compaction(
    character: &str,
    config: &LoadedConfig,
    llm_client: &LlmClient,
    data_dir: &std::path::Path,
    _push_tx: &broadcast::Sender<ServerMessage>,
    notifier: &NotificationService,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    let character_dir = data_dir.join(character);
    let active_path = character_dir.join("active.jsonl");

    // Read messages directly from active.jsonl.
    let content = tokio::fs::read_to_string(&active_path).await?;
    let mut messages = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: shore_protocol::types::Message = serde_json::from_str(line)?;
        let is_tool_result_only = msg.role == shore_protocol::types::Role::User
            && !msg.content_blocks.is_empty()
            && msg.content_blocks
                .iter()
                .all(|b| matches!(b, ContentBlock::ToolResult { .. }));
        messages.push(ConversationMessage {
            role: format!("{:?}", msg.role).to_lowercase(),
            content: msg.content,
            timestamp: msg.timestamp,
            is_tool_result_only,
        });
    }

    if messages.is_empty() {
        info!(character = %character, "No messages to compact, skipping");
        return Ok(0);
    }

    // Open memory DB.
    let db_path = character_dir.join("memory").join("memory.db");
    let db = MemoryDB::open(&db_path)
        .map_err(|e| format!("Failed to open memory DB: {e}"))?;

    // Resolve effective config: merge per-character overrides over global.
    let effective = load_character_config(config, character)
        .ok()
        .flatten()
        .unwrap_or_else(|| config.clone());

    // Resolve prompt template.
    let prompt_template = resolve_prompt_template(&effective.dirs.config, character, "compact.md")
        .unwrap_or_else(|| DEFAULT_COMPACT_PROMPT.to_string());

    // Resolve model from effective (character-merged) config.
    let model = effective
        .app
        .defaults
        .model
        .as_deref()
        .and_then(|name| effective.models.find_model(name).ok())
        .ok_or("No default model configured for background compaction")?
        .clone();

    // Resolve embedding config.
    let embed_config = resolve_embed_config(
        effective.app.defaults.embedding.as_deref(),
        &effective.models.embedding,
    )?;

    // Open vector store.
    let vs_path = character_dir.join("memory").join("vectorstore");
    let store = VectorStore::open(&vs_path, embed_config.dimensions).await
        .map_err(|e| format!("Failed to open vector store: {e}"))?;

    // Create trait implementations.
    let llm = RealCompactionLlm::new(llm_client.clone(), model);
    let indexer = RealVectorIndexer::new(store, llm_client.clone(), embed_config);
    let conv_mgr = RealConversationManager::new(&character_dir);

    let app_compaction = &effective.app.memory.compaction;
    let mgr_config = CompactionConfig {
        idle_trigger_minutes: app_compaction.idle_trigger_minutes as u64,
        min_turns: app_compaction.min_turns,
        max_turns: app_compaction.max_turns,
        keep_recent_turns: app_compaction.keep_recent_turns,
    };
    let mgr = CompactionManager::new(mgr_config);

    // Load existing recap for folding.
    let recap_path = character_dir.join("memory").join("recap.md");
    let existing_recap = tokio::fs::read_to_string(&recap_path).await.ok();

    let display_name = effective.app.defaults.resolve_display_name();
    let outcome = mgr
        .compact(
            character,
            &messages,
            false,
            &prompt_template,
            existing_recap.as_deref(),
            character,
            &display_name,
            &llm,
            &db,
            &indexer,
            &conv_mgr,
            false,
        )
        .await?;

    match outcome {
        CompactionOutcome::Compacted(result) => {
            info!(
                character = %character,
                entries = result.entries_created.len(),
                compacted_messages = result.message_count,
                retained_turns = result.retained_turns,
                recap = result.recap_generated,
                "Background compaction completed"
            );

            // Don't broadcast History here — the disk file may be stale
            // relative to the engine's in-memory state (race with concurrent
            // message appends).  The handler will reload the engine on the
            // next message via the compaction_occurred flag, and the TUI
            // re-requests log after StreamEnd as a safety net.

            notifier.notify(
                shore_daemon::notifications::NotificationEvent::CompactionComplete,
                &format!("Shore — {character}"),
                &format!("Compaction complete: {} entries from {} messages", result.entries_created.len(), result.message_count),
            );

            // Run collation after successful compaction if configured.
            if config.app.memory.collation.enabled
                && config.app.memory.collation.auto_run {
                info!(character = %character, "Running auto-collation after compaction");
                match run_collation(character, config, llm_client, data_dir).await {
                    Ok(()) => {
                        notifier.notify(
                            shore_daemon::notifications::NotificationEvent::CollationComplete,
                            &format!("Shore — {character}"),
                            "Collation complete",
                        );
                    }
                    Err(e) => {
                        warn!(
                            character = %character,
                            error = %e,
                            "Auto-collation failed"
                        );
                    }
                }
            }

            Ok(result.retained_turns)
        }
        CompactionOutcome::DryRun(_) => {
            // Should not happen in background mode, but harmless.
            Ok(0)
        }
    }
}

/// Run the collation pipeline for a single character.
///
/// Called after compaction (auto-trigger) or could be invoked independently.
async fn run_collation(
    character: &str,
    config: &LoadedConfig,
    llm_client: &LlmClient,
    data_dir: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let character_dir = data_dir.join(character);

    // Open memory DB.
    let db_path = character_dir.join("memory").join("memory.db");
    let db = MemoryDB::open(&db_path)
        .map_err(|e| format!("Failed to open memory DB: {e}"))?;

    let model = resolve_collation_model(config)
        .ok_or("No model configured")?;

    let llm = RealCollationLlm::new(llm_client.clone(), model);

    // Resolve prompt template.
    let refine_template = resolve_prompt_template(&config.dirs.config, character, "refine.md")
        .unwrap_or_else(|| DEFAULT_REFINE_PROMPT.to_string());

    let mgr = CollationManager::new(LibCollationConfig::default());
    let collation_limit = config.app.memory.collation.batch_limit;

    // Construct vector store + indexer for clustering and indexing (optional).
    let search_ctx = match resolve_embed_config(
        config.app.defaults.embedding.as_deref(),
        &config.models.embedding,
    ) {
        Ok(embed_config) => {
            let vs_path = character_dir.join("memory").join("vectorstore");
            match VectorStore::open(&vs_path, embed_config.dimensions).await {
                Ok(vs) => Some(shore_daemon::memory::agent::AgentSearchContext::new(
                    vs, llm_client.clone(), embed_config,
                )),
                Err(e) => {
                    tracing::warn!("Vector store unavailable for auto-collation: {e}");
                    None
                }
            }
        }
        Err(_) => None,
    };
    let indexer = search_ctx.as_ref().map(|ctx| {
        shore_daemon::memory::agent::RealAgentIndexer::new(ctx)
    });

    let collation_display_name = config.app.defaults.resolve_display_name();
    let mut collation_vars = std::collections::HashMap::new();
    collation_vars.insert("char".to_string(), character.to_string());
    collation_vars.insert("user".to_string(), collation_display_name);
    if let Some(cd) = load_character_definition(&config.dirs.config, character) {
        collation_vars.insert("char_description".to_string(), cd);
    }
    if let Some(ud) = resolve_user_definition(&config.dirs.config, character) {
        collation_vars.insert("user_description".to_string(), ud);
    }

    let outcome = mgr
        .run(
            &db, &llm, &refine_template, &collation_vars,
            indexer.as_ref().map(|i| i as &dyn shore_daemon::memory::agent::AgentIndexer),
            search_ctx.as_ref().map(|ctx| &ctx.vector_store),
            Some(collation_limit),
        )
        .await?;

    info!(
        character = %character,
        refine_merges = outcome.refine_merges,
        refine_splits = outcome.refine_splits,
        refine_updates = outcome.refine_updates,
        entries_decayed = outcome.entries_decayed,
        "Auto-collation completed"
    );

    Ok(())
}

/// Simple epoch-seconds timestamp without pulling in chrono.
fn epoch_timestamp() -> String {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s-since-epoch", duration.as_secs())
}
