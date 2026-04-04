use super::parser::DEFAULT_COMPACT_PROMPT;
use super::types::{CompactionOutcome, ConversationMessage};
use super::CompactionManager;

/// Run compaction for a single character (called from the background task).
/// Returns the number of retained turns on success.
pub async fn run_compaction(
    character: &str,
    config: &shore_config::LoadedConfig,
    llm_client: &shore_llm_client::LlmClient,
    data_dir: &std::path::Path,
    _push_tx: &tokio::sync::broadcast::Sender<shore_protocol::server_msg::ServerMessage>,
    notifier: &crate::notifications::NotificationService,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    use crate::memory::compaction_impls::{
        resolve_embed_config, RealCompactionLlm, RealConversationManager, RealVectorIndexer,
    };
    use crate::memory::db::MemoryDB;
    use crate::memory::vectorstore::VectorStore;
    use crate::notifications::NotificationEvent;
    use shore_config::{load_character_config, resolve_prompt_template};
    use shore_protocol::types::ContentBlock;
    use tracing::{info, warn};

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

    // Open memory DB.
    let db_path = character_dir.join("memory").join("memory.db");
    let db = MemoryDB::open(&db_path).map_err(|e| format!("Failed to open memory DB: {e}"))?;

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
    let store = VectorStore::open(&vs_path, embed_config.dimensions)
        .await
        .map_err(|e| format!("Failed to open vector store: {e}"))?;

    // Create trait implementations.
    let llm = RealCompactionLlm::new(llm_client.clone(), model);
    let indexer = RealVectorIndexer::new(store, llm_client.clone(), embed_config);
    let conv_mgr = RealConversationManager::new(&character_dir);

    let mgr = CompactionManager::new(effective.app.memory.compaction.clone());

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

            notifier.notify(
                NotificationEvent::CompactionComplete,
                &format!("Shore — {character}"),
                &format!(
                    "Compaction complete: {} entries from {} messages",
                    result.entries_created.len(),
                    result.message_count
                ),
            );

            // Run collation after successful compaction if configured.
            if config.app.memory.collation.enabled && config.app.memory.collation.auto_run {
                info!(character = %character, "Running auto-collation after compaction");
                match crate::memory::collation::run_collation(
                    character, config, llm_client, data_dir,
                )
                .await
                {
                    Ok(()) => {
                        notifier.notify(
                            NotificationEvent::CollationComplete,
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
