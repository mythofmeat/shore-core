use super::parser::{DEFAULT_COMPACT_PROMPT, DEFAULT_COMPACT_SYSTEM};
use super::types::{CompactionOutcome, ConversationMessage};
use super::CompactionManager;
use std::sync::Arc;

/// Run compaction for a single character (called from the background task).
/// Returns the number of retained turns on success.
///
/// `cached_request` is the live conversation's cached LLM request (typically
/// from `AutonomyManager::cached_last_request`). When provided, compaction
/// reuses the cached prefix instead of building a fresh request, preserving
/// the Anthropic prompt cache for the compaction call itself.
///
/// The data directory comes from `config.dirs.data` — callers should not
/// thread a separate `data_dir` argument; the two would have to stay
/// manually in sync.
pub async fn run_compaction(
    character: &str,
    config: &shore_config::LoadedConfig,
    llm_client: &shore_ledger::LedgerClient,
    notifier: &crate::notifications::NotificationService,
    cached_request: Option<shore_llm::types::LlmRequest>,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    use crate::engine::messages::MessageStore;
    use crate::memory::compaction_impls::{RealCompactionLlm, RealConversationManager};
    use crate::notifications::NotificationEvent;
    use shore_config::{
        character_active_jsonl, character_data_dir, load_character_config, resolve_prompt_template,
    };
    use tracing::{info, warn};

    let data_dir = config.dirs.data.as_path();
    let _compaction_guard = super::try_begin_compaction(data_dir, character).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!("Compaction already running for {character}"),
        )
    })?;

    let character_dir = character_data_dir(data_dir, character);
    let active_path = character_active_jsonl(data_dir, character);

    // Single read: parse messages + capture raw bytes for segment archival.
    // Prior to this we did `MessageStore::load(...)` followed by a separate
    // `tokio::fs::read_to_string(...)`, which read the same potentially
    // multi-MB file twice and briefly blocked the runtime on the second
    // (sync) read inside `load`.
    let (store, content) = MessageStore::load_with_raw(active_path.clone())?;
    let messages: Vec<ConversationMessage> = store
        .messages()
        .iter()
        .map(|msg| ConversationMessage {
            role: format!("{:?}", msg.role).to_lowercase(),
            content: msg.content.clone(),
            timestamp: msg.timestamp.clone(),
            is_tool_result_only: msg.is_tool_result_only(),
        })
        .collect();

    if messages.is_empty() {
        info!(character = %character, "No messages to compact, skipping");
        return Ok(0);
    }

    // Resolve effective config: merge per-character overrides over global.
    let effective = load_character_config(config, character)
        .ok()
        .flatten()
        .unwrap_or_else(|| config.clone());

    // Resolve prompt templates.
    let system_template =
        resolve_prompt_template(&effective.dirs.config, character, "compact_system.md")
            .unwrap_or_else(|| DEFAULT_COMPACT_SYSTEM.to_string());
    let prompt_template = resolve_prompt_template(&effective.dirs.config, character, "compact.md")
        .unwrap_or_else(|| DEFAULT_COMPACT_PROMPT.to_string());

    let model = crate::preferences::resolve_background_model(
        &effective,
        shore_config::app::BackgroundTask::Compaction,
        character,
    )
    .ok_or("No model configured for background compaction")?;

    // Create trait implementations.
    let llm = RealCompactionLlm::new(
        llm_client.clone(),
        model,
        effective.providers.clone(),
        character.to_string(),
    );
    let conv_mgr = RealConversationManager::new(&character_dir);

    let mgr = CompactionManager::new(effective.app.memory.compaction.clone());

    let display_name = effective.app.defaults.resolve_display_name();

    // Open markdown memory store for existing-memory context and file writes.
    let markdown_store = crate::memory::markdown_store::MarkdownMemoryStore::open(
        shore_config::character_memory_dir(&effective.dirs.config, character),
    )
    .await
    .ok();

    // Build the canonical SharedToolContext for the compaction tool loop.
    // Mirrors the heartbeat/librarian wiring so write/edit dispatch
    // resolves paths, queues prompt-visible refreshes, and routes search
    // through the configured embedder.
    let tool_ctx = build_compaction_tool_context(&effective, data_dir, llm_client, character).await;

    let outcome = mgr
        .compact(
            character,
            &messages,
            &content,
            false,
            &system_template,
            &prompt_template,
            character,
            &display_name,
            &llm,
            &conv_mgr,
            markdown_store.as_ref(),
            false,
            None,
            cached_request,
            Some(data_dir),
            tool_ctx.as_ref(),
        )
        .await?;

    match outcome {
        CompactionOutcome::Compacted(result) => {
            info!(
                character = %character,
                entries = result.memory_files_written.len(),
                compacted_turns = result.compacted_turns,
                retained_turns = result.retained_turns,
                tool_rounds = result.tool_rounds,
                "Background compaction completed"
            );

            notifier.notify(
                NotificationEvent::CompactionComplete,
                &format!("Shore — {character}"),
                &format!(
                    "Compaction complete: {} entries from {} turns",
                    result.memory_files_written.len(),
                    result.compacted_turns
                ),
            );

            Ok(result.retained_turns)
        }
        CompactionOutcome::NoMemoryWrites(result) => {
            // Compaction ran but the model produced no allowed memory
            // writes. Leave active.jsonl intact and surface diagnostics
            // so an operator can investigate (often: a model that
            // ignored the tool prompt, hit max rounds, or only tried
            // disallowed paths). The next idle/forced trigger will
            // retry.
            warn!(
                character = %character,
                tool_rounds = result.tool_rounds,
                rejected = result.rejected_paths.len(),
                max_rounds_hit = result.max_rounds_hit,
                tools_called = ?result.tools_called,
                "Background compaction produced no memory writes — conversation NOT archived"
            );
            notifier.notify(
                NotificationEvent::CompactionComplete,
                &format!("Shore — {character}"),
                &format!(
                    "Compaction ran but wrote no memory ({} tool round{}). Conversation kept; will retry on next trigger.",
                    result.tool_rounds,
                    if result.tool_rounds == 1 { "" } else { "s" },
                ),
            );
            Ok(0)
        }
        CompactionOutcome::DryRun(_) => {
            // Should not happen in background mode (dry_run is hard-coded
            // to false above), but harmless.
            Ok(0)
        }
    }
}

/// Build the canonical `SharedToolContext` for the compaction tool loop.
/// Pulls the same dependencies the heartbeat/librarian wiring relies on so
/// that compaction sees an identical view of the workspace, memory store,
/// embedder, and image-gen config. Returned as `Arc` so the caller can
/// hand out a `&dyn ToolContext` whose lifetime is bound to the function.
async fn build_compaction_tool_context(
    effective: &shore_config::LoadedConfig,
    data_dir: &std::path::Path,
    llm_client: &shore_ledger::LedgerClient,
    character: &str,
) -> Arc<crate::tools::context::SharedToolContext> {
    use shore_config::{character_data_dir, character_memory_dir, character_workspace_dir};

    let character_data_dir_path = character_data_dir(data_dir, character);
    let image_gen_config = crate::memory::compaction_impls::resolve_image_gen_config(
        effective.app.defaults.image_generation.as_deref(),
        &effective.models.image_generation,
    )
    .ok();
    let embedder = crate::memory::retrieval::resolve_embedder(
        effective.app.defaults.embedding.as_deref(),
        &effective.models.embedding,
        llm_client.inner().http_client(),
    )
    .ok();

    Arc::new(crate::tools::context::SharedToolContext {
        image_dir_val: character_data_dir_path
            .join("images")
            .to_string_lossy()
            .into_owned(),
        llm_client_val: llm_client.inner().clone(),
        image_gen_config_val: image_gen_config,
        search_config_val: effective.app.behavior.tool_use.search.clone(),
        character_name_val: character.to_string(),
        workspace_dir_val: character_workspace_dir(&effective.dirs.config, character)
            .to_string_lossy()
            .into_owned(),
        markdown_store_val: crate::memory::markdown_store::MarkdownMemoryStore::open_sync(
            character_memory_dir(&effective.dirs.config, character),
        )
        .ok(),
        memory_retrieval_config_val: effective.app.memory.retrieval.clone(),
        embedder_val: embedder,
        memory_index_path_val: crate::memory::workspace_index::index_path(
            &effective.dirs.cache,
            character,
        ),
        config_dir_val: effective.dirs.config.to_string_lossy().into_owned(),
        character_data_dir_val: character_data_dir_path.to_string_lossy().into_owned(),
    })
}
