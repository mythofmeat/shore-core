use super::parser::{DEFAULT_COMPACT_PROMPT, DEFAULT_COMPACT_SYSTEM};
use super::types::{CompactionOutcome, ConversationMessage};
use super::CompactionManager;

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
    http: Option<std::sync::Arc<crate::http::DaemonHttpState>>,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    use crate::engine::messages::MessageStore;
    use crate::memory::compaction_impls::{RealCompactionLlm, RealConversationManager};
    use crate::notifications::NotificationEvent;
    use shore_config::{
        character_active_jsonl, character_data_dir, load_character_config, resolve_prompt_template,
    };
    use tracing::info;

    let data_dir = config.dirs.data.as_path();
    let character_dir = character_data_dir(data_dir, character);
    let active_path = character_active_jsonl(data_dir, character);

    // Same canonical load + normalize path the engine itself uses.
    let store = MessageStore::load(active_path.clone())?;
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

    // The `compact()` call below still wants the raw on-disk content for
    // segment archival; read it once here.
    let content = tokio::fs::read_to_string(&active_path).await?;

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
        http,
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
        )
        .await?;

    match outcome {
        CompactionOutcome::Compacted(result) => {
            info!(
                character = %character,
                entries = result.memory_files_written.len(),
                compacted_messages = result.message_count,
                retained_turns = result.retained_turns,
                "Background compaction completed"
            );

            notifier.notify(
                NotificationEvent::CompactionComplete,
                &format!("Shore — {character}"),
                &format!(
                    "Compaction complete: {} entries from {} messages",
                    result.memory_files_written.len(),
                    result.message_count
                ),
            );

            Ok(result.retained_turns)
        }
        CompactionOutcome::DryRun(_) => {
            // Should not happen in background mode, but harmless.
            Ok(0)
        }
    }
}
