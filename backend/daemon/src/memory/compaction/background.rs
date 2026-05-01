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
pub async fn run_compaction(
    character: &str,
    config: &shore_config::LoadedConfig,
    llm_client: &shore_ledger::LedgerClient,
    data_dir: &std::path::Path,
    notifier: &crate::notifications::NotificationService,
    cached_request: Option<shore_llm::types::LlmRequest>,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    use crate::memory::compaction_impls::{RealCompactionLlm, RealConversationManager};
    use crate::notifications::NotificationEvent;
    use shore_config::{load_character_config, resolve_prompt_template};
    use shore_protocol::types::ContentBlock;
    use tracing::info;

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
        let mut msg: shore_protocol::types::Message = serde_json::from_str(line)?;
        msg.normalize();
        let is_tool_result_only = msg.role == shore_protocol::types::Role::User
            && !msg.content_blocks.is_empty()
            && msg
                .content_blocks
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

    let active_model = crate::runtime_state::load_character_runtime_state(&character_dir)
        .ok()
        .and_then(|state| state.active_model);
    let model =
        crate::commands::state::resolve_compaction_model(&effective, active_model.as_deref())
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
