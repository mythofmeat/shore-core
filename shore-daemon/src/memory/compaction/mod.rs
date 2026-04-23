pub mod background;
pub mod parser;
pub mod types;

pub use background::run_compaction;
pub use parser::{parse_compaction_response, MemoryFileOp, DEFAULT_COMPACT_PROMPT};
pub use types::*;

use crate::memory::markdown_query;
use crate::memory::markdown_store::MarkdownMemoryStore;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::time::Duration;
use tracing::{debug, info, instrument, warn};

// ---------------------------------------------------------------------------
// CompactionManager
// ---------------------------------------------------------------------------

pub struct CompactionManager {
    config: CompactionConfig,
    activity_notify: Arc<Notify>,
}

struct CompactionWriteState {
    markdown_path: String,
    previous_markdown: Option<String>,
}

impl CompactionManager {
    pub fn new(config: CompactionConfig) -> Self {
        Self {
            config,
            activity_notify: Arc::new(Notify::new()),
        }
    }

    /// Check if a ConversationMessage is a tool-loop intermediate that
    /// should not be split from its context during compaction.
    fn is_tool_loop_message(msg: &ConversationMessage) -> bool {
        match msg.role.as_str() {
            "user" => msg.is_tool_result_only,
            "assistant" => {
                // Assistant messages in tool loops have empty text content
                // (all their content is tool_use blocks).
                msg.content.is_empty()
            }
            _ => false,
        }
    }

    /// Find the split index that retains `keep_turns` complete user turns
    /// at the tail.  Returns the index of the first retained message.
    /// Returns 0 if there aren't enough messages to compact anything.
    /// When `keep_turns == 0`, returns `messages.len()` so the caller
    /// retains nothing and compacts everything (see QUIRKS.md).
    fn find_turn_split(messages: &[ConversationMessage], keep_turns: usize) -> usize {
        if keep_turns == 0 {
            return messages.len();
        }
        let mut turns_seen = 0usize;
        for i in (0..messages.len()).rev() {
            if messages[i].role == "user" && !Self::is_tool_loop_message(&messages[i]) {
                turns_seen += 1;
                if turns_seen >= keep_turns {
                    return i;
                }
            }
        }
        0
    }

    /// Signal that a new message was received, resetting the idle timer.
    pub fn notify_activity(&self) {
        self.activity_notify.notify_one();
    }

    /// Check if forced compaction should trigger (max turns reached).
    pub fn should_force_compact(&self, turn_count: usize) -> bool {
        self.config.max_turns > 0
            && turn_count >= self.config.max_turns
            && self.has_enough_turns(turn_count)
    }

    /// Check minimum turn gating for any trigger.
    pub fn has_enough_turns(&self, turn_count: usize) -> bool {
        turn_count >= self.config.min_turns
    }

    /// Build a compaction prompt from a template and conversation messages.
    ///
    /// Replaces `{{conversation}}` with formatted messages, handles the
    /// `{{#if recap}}...{{/if}}` conditional block, and substitutes
    /// `{{char}}` / `{{user}}` with the provided names.
    pub fn build_prompt(
        template: &str,
        messages: &[ConversationMessage],
        existing_recap: Option<&str>,
        char_name: &str,
        user_name: &str,
    ) -> String {
        let mut conversation_text = String::new();
        for msg in messages {
            conversation_text.push_str(&format!(
                "[{}] {}: {}\n",
                msg.timestamp, msg.role, msg.content
            ));
        }

        let mut result = template.replace("{{conversation}}", &conversation_text);

        // Handle {{#if recap}}...{{/if}} conditional block.
        if let (Some(if_start), Some(endif_pos)) =
            (result.find("{{#if recap}}"), result.find("{{/if}}"))
        {
            if let Some(recap) = existing_recap.filter(|r| !r.is_empty()) {
                // Keep the block content, strip the tags.
                let block_start = if_start + "{{#if recap}}".len();
                let block_content = &result[block_start..endif_pos];
                let rendered_block = block_content.replace("{{recap}}", recap);
                result = format!(
                    "{}{}{}",
                    &result[..if_start],
                    rendered_block,
                    &result[endif_pos + "{{/if}}".len()..],
                );
            } else {
                // Remove the entire conditional block.
                result = format!(
                    "{}{}",
                    &result[..if_start],
                    &result[endif_pos + "{{/if}}".len()..],
                );
            }
        } else {
            // No conditional block — replace {{recap}} directly if present.
            if let Some(recap) = existing_recap {
                result = result.replace("{{recap}}", recap);
            }
        }

        // Substitute character and user names.
        result = result.replace("{{char}}", char_name);
        result = result.replace("{{user}}", user_name);

        result
    }

    fn dedupe_file_ops(file_ops: Vec<MemoryFileOp>) -> Vec<MemoryFileOp> {
        let mut deduped: Vec<MemoryFileOp> = Vec::new();
        for op in file_ops {
            if let Some(existing_idx) = deduped.iter().position(|existing| existing.path == op.path)
            {
                deduped.remove(existing_idx);
            }
            deduped.push(op);
        }
        deduped
    }

    /// Generate an entry ID with a compaction prefix: c_YYYYMMDD_HHMMSS_N
    ///
    /// The `c_` prefix marks compaction-generated entries.
    /// when both run in the same second.
    /// Run compaction on a conversation.
    ///
    /// Splits messages into a compacted portion (sent to LLM) and a retained
    /// portion (kept in active.jsonl). The LLM generates both a rolling recap
    /// and memory entries from the compacted messages.
    ///
    /// If `dry_run` is true, returns what would be created without side effects.
    #[instrument(skip(self, messages, active_content, prompt_template, existing_recap, llm, conversation_mgr, markdown_store), fields(char = char_name, user = user_name, msg_count = messages.len(), dry_run))]
    #[allow(clippy::too_many_arguments)]
    pub async fn compact(
        &self,
        conversation_id: &str,
        messages: &[ConversationMessage],
        active_content: &str,
        is_private: bool,
        prompt_template: &str,
        existing_recap: Option<&str>,
        char_name: &str,
        user_name: &str,
        llm: &dyn CompactionLlm,
        conversation_mgr: &dyn ConversationManager,
        markdown_store: Option<&MarkdownMemoryStore>,
        dry_run: bool,
        keep_turns_override: Option<usize>,
    ) -> Result<CompactionOutcome, CompactionError> {
        let compaction_started = std::time::Instant::now();
        info!(
            conversation_id,
            messages = messages.len(),
            char_name,
            user_name,
            dry_run,
            "Compaction started"
        );

        // Skip private conversations entirely.
        if is_private {
            return Err(CompactionError::PrivateConversation);
        }

        if messages.is_empty() {
            return Err(CompactionError::InsufficientMessages);
        }

        // Split messages: compact the older portion, retain the recent tail.
        let keep_turns = keep_turns_override.unwrap_or(self.config.keep_recent_turns);
        let split_at = Self::find_turn_split(messages, keep_turns);
        if split_at == 0 {
            return Err(CompactionError::InsufficientMessages);
        }
        let compacted_part = &messages[..split_at];
        debug!(
            compacted = split_at,
            retained = messages.len() - split_at,
            "Conversation split for compaction"
        );

        // Build and send prompt to LLM (only compacted messages, not retained).
        let prompt = Self::build_prompt(
            prompt_template,
            compacted_part,
            existing_recap,
            char_name,
            user_name,
        );
        let raw_response = llm.summarize(&prompt).await?;

        // Parse recap + memory file operations from LLM response.
        let (recap, raw_file_ops) = parse_compaction_response(&raw_response)?;
        let file_ops = Self::dedupe_file_ops(raw_file_ops);
        debug!(
            ops = file_ops.len(),
            has_recap = recap.is_some(),
            "LLM compaction response parsed"
        );

        let retained_turns = keep_turns;

        // Build markdown previews for dry run.
        let markdown_preview: Vec<String> = if dry_run {
            file_ops.iter().map(|op| op.path.clone()).collect()
        } else {
            Vec::new()
        };

        // Dry run: return preview without side effects.
        if dry_run {
            return Ok(CompactionOutcome::DryRun(DryRunResult {
                would_create_entries: file_ops.len(),
                file_ops_preview: file_ops,
                message_count: split_at,
                retained_count: messages.len() - split_at,
                retained_turns,
                recap_preview: recap,
                markdown_preview,
            }));
        }

        // Track created resources for compensating deletes on failure.
        let store = markdown_store.ok_or_else(|| {
            CompactionError::MarkdownStore("markdown memory store not available".to_string())
        })?;

        let mut created: Vec<CompactionWriteState> = Vec::new();
        let mut markdown_elapsed = std::time::Duration::ZERO;

        for op in &file_ops {
            let previous_markdown = match store.read(&op.path).await {
                Ok(entry) => Some(entry.content),
                Err(crate::memory::markdown_store::MarkdownStoreError::NotFound(_)) => None,
                Err(e) => return Err(CompactionError::MarkdownStore(e.to_string())),
            };
            created.push(CompactionWriteState {
                markdown_path: op.path.clone(),
                previous_markdown,
            });

            let md_started = std::time::Instant::now();
            if let Err(e) = store.write(&op.path, &op.content).await {
                Self::rollback_compaction(&created, store).await;
                return Err(CompactionError::MarkdownStore(e.to_string()));
            }
            debug!(path = %op.path, elapsed = ?md_started.elapsed(), "compaction: markdown entry written");
            markdown_elapsed += md_started.elapsed();
        }

        // Archive compacted messages, retain recent, write recap.
        let retained = messages.len() - split_at;
        let archive_started = std::time::Instant::now();
        let new_conversation_id = match conversation_mgr
            .archive_and_retain(
                conversation_id,
                RetentionParams {
                    keep_last_n: retained,
                    recap: recap.clone(),
                    active_content: active_content.to_string(),
                },
            )
            .await
        {
            Ok(id) => id,
            Err(e) => {
                Self::rollback_compaction(&created, store).await;
                return Err(e);
            }
        };
        debug!(
            retained,
            elapsed = ?archive_started.elapsed(),
            "compaction: archive/retain done"
        );

        let markdown_paths: Vec<String> = created.iter().map(|state| state.markdown_path.clone()).collect();
        let dream_body = format!(
            "Compacted {} messages from `{conversation_id}`.\n\nUpdated memory files:\n{}",
            split_at,
            markdown_paths
                .iter()
                .map(|path| format!("- `{path}`"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        if let Err(e) = markdown_query::append_dream_entry(
            store,
            chrono::Local::now().fixed_offset(),
            "compaction",
            &dream_body,
        )
        .await
        {
            warn!(error = %e, "compaction: failed to append DREAMS.md entry");
        }

        info!(
            entries_created = markdown_paths.len(),
            markdown_files = markdown_paths.len(),
            conversation_id,
            retained,
            elapsed = ?compaction_started.elapsed(),
            "Compaction complete"
        );
        Ok(CompactionOutcome::Compacted(CompactionResult {
            memory_files_written: markdown_paths.clone(),
            conversation_id: conversation_id.to_string(),
            new_conversation_id,
            message_count: split_at,
            retained_count: retained,
            retained_turns,
            recap_generated: recap.is_some(),
            markdown_paths,
        }))
    }

    /// Compensating-delete rollback for a failed compaction.
    ///
    /// Iterates the created list in reverse and removes each resource:
    /// - changelog rows (SQLite)
    /// - entry rows (SQLite, including FK cleanup)
    /// - vector index entries (LanceDB, best-effort)
    ///
    /// Errors during cleanup are logged at WARN level and skipped so that
    /// rollback continues regardless of individual failures.
    async fn rollback_compaction(
        created: &[CompactionWriteState],
        markdown_store: &MarkdownMemoryStore,
    ) {
        use tracing::warn;
        for state in created.iter().rev() {
            match &state.previous_markdown {
                Some(previous) => {
                    if let Err(e) = markdown_store.write(&state.markdown_path, previous).await {
                        warn!(path = %state.markdown_path, error = %e, "rollback: failed to restore markdown entry");
                    }
                }
                None => match markdown_store.delete(&state.markdown_path).await {
                    Ok(()) => {}
                    Err(crate::memory::markdown_store::MarkdownStoreError::NotFound(_)) => {}
                    Err(e) => {
                        warn!(path = %state.markdown_path, error = %e, "rollback: failed to delete markdown entry");
                    }
                }
            }
        }
    }

    /// Create an idle timer bound to this manager's activity signal.
    pub fn idle_timer(&self) -> IdleTimer {
        IdleTimer {
            idle_duration: self.config.idle_trigger.as_duration(),
            activity_notify: Arc::clone(&self.activity_notify),
        }
    }
}

// ---------------------------------------------------------------------------
// IdleTimer
// ---------------------------------------------------------------------------

/// A timer that waits for an idle period to elapse without activity.
/// Activity notifications (via `CompactionManager::notify_activity`) reset it.
pub struct IdleTimer {
    idle_duration: Duration,
    activity_notify: Arc<Notify>,
}

impl IdleTimer {
    /// Wait until the full idle period elapses without any activity.
    /// Returns when compaction should be triggered.
    pub async fn wait_for_idle(&self) {
        loop {
            tokio::select! {
                () = tokio::time::sleep(self.idle_duration) => {
                    debug!("Idle period elapsed, compaction ready");
                    return;
                }
                () = self.activity_notify.notified() => {
                    // Activity detected — reset timer by restarting loop.
                    debug!("Idle timer reset by activity");
                    continue;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Local;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::Mutex as StdMutex;
    use tokio::sync::oneshot;

    // -- Mock implementations ------------------------------------------------

    struct MockLlm {
        response: String,
    }

    impl CompactionLlm for MockLlm {
        fn summarize(
            &self,
            _prompt: &str,
        ) -> Pin<Box<dyn Future<Output = Result<String, CompactionError>> + Send + '_>> {
            let result = Ok(self.response.clone());
            Box::pin(async move { result })
        }
    }

    struct MockConversationMgr {
        archived: StdMutex<Vec<(String, usize)>>,
        next_id: String,
    }

    impl MockConversationMgr {
        fn new(next_id: &str) -> Self {
            Self {
                archived: StdMutex::new(Vec::new()),
                next_id: next_id.to_string(),
            }
        }

        fn archived_calls(&self) -> Vec<(String, usize)> {
            self.archived.lock().unwrap().clone()
        }
    }

    impl ConversationManager for MockConversationMgr {
        fn archive_and_retain(
            &self,
            conversation_id: &str,
            params: RetentionParams,
        ) -> Pin<Box<dyn Future<Output = Result<String, CompactionError>> + Send + '_>> {
            self.archived
                .lock()
                .unwrap()
                .push((conversation_id.to_string(), params.keep_last_n));
            let next_id = self.next_id.clone();
            Box::pin(async move { Ok(next_id) })
        }
    }

    struct BlockingConversationMgr {
        entered_tx: StdMutex<Option<oneshot::Sender<()>>>,
        release_rx: StdMutex<Option<mpsc::Receiver<()>>>,
        next_id: String,
    }

    impl BlockingConversationMgr {
        fn new(next_id: &str) -> (Self, oneshot::Receiver<()>, mpsc::Sender<()>) {
            let (entered_tx, entered_rx) = oneshot::channel();
            let (release_tx, release_rx) = mpsc::channel();
            (
                Self {
                    entered_tx: StdMutex::new(Some(entered_tx)),
                    release_rx: StdMutex::new(Some(release_rx)),
                    next_id: next_id.to_string(),
                },
                entered_rx,
                release_tx,
            )
        }
    }

    impl ConversationManager for BlockingConversationMgr {
        fn archive_and_retain(
            &self,
            _conversation_id: &str,
            _params: RetentionParams,
        ) -> Pin<Box<dyn Future<Output = Result<String, CompactionError>> + Send + '_>> {
            let entered_tx = self.entered_tx.lock().unwrap().take();
            let release_rx = self
                .release_rx
                .lock()
                .unwrap()
                .take()
                .expect("test setup should install a release receiver");
            let next_id = self.next_id.clone();

            Box::pin(async move {
                tokio::task::spawn_blocking(move || {
                    if let Some(tx) = entered_tx {
                        let _ = tx.send(());
                    }
                    release_rx.recv().map_err(|_| {
                        CompactionError::ConversationManager(
                            "test release signal dropped before archive completed".to_string(),
                        )
                    })?;
                    Ok(next_id)
                })
                .await
                .map_err(|err| {
                    CompactionError::ConversationManager(format!(
                        "blocking archive task failed: {err}"
                    ))
                })?
            })
        }
    }

    struct FailingConversationMgr;

    impl ConversationManager for FailingConversationMgr {
        fn archive_and_retain(
            &self,
            _conversation_id: &str,
            _params: RetentionParams,
        ) -> Pin<Box<dyn Future<Output = Result<String, CompactionError>> + Send + '_>> {
            Box::pin(async {
                Err(CompactionError::ConversationManager(
                    "simulated archive failure".to_string(),
                ))
            })
        }
    }

    // -- Helpers --------------------------------------------------------------

    fn make_messages(count: usize) -> Vec<ConversationMessage> {
        (0..count)
            .map(|i| ConversationMessage {
                role: if i % 2 == 0 {
                    "user".to_string()
                } else {
                    "assistant".to_string()
                },
                content: format!("Message {i}"),
                timestamp: Local::now().to_rfc3339(),
                is_tool_result_only: false,
            })
            .collect()
    }

    fn make_xml_response() -> String {
        r#"<recap>
The assistant had a pleasant conversation with the user about their day and preferences.
They discussed daily activities and the user's beverage preferences.
</recap>

<memory>
<write path="daily/2026-03-25.md">
# Conversation on 2026-03-25

- User discussed their day
- They mentioned having a busy morning
</write>

<write path="preferences/beverages.md">
# Beverage Preferences

- User prefers tea over coffee
- This is a stable preference
</write>
</memory>"#
            .to_string()
    }

    fn make_config_with_keep(keep_recent_turns: usize) -> CompactionConfig {
        CompactionConfig {
            keep_recent_turns,
            ..Default::default()
        }
    }

    // -- Tests: prompt building -----------------------------------------------

    #[test]
    fn test_build_prompt_no_recap() {
        let messages = vec![
            ConversationMessage {
                role: "user".to_string(),
                content: "Hello!".to_string(),
                timestamp: "2026-03-25T10:00:00Z".to_string(),
                is_tool_result_only: false,
            },
            ConversationMessage {
                role: "assistant".to_string(),
                content: "Hi there!".to_string(),
                timestamp: "2026-03-25T10:00:01Z".to_string(),
                is_tool_result_only: false,
            },
        ];

        let prompt = CompactionManager::build_prompt(
            "Template:\n{{conversation}}",
            &messages,
            None,
            "Char",
            "User",
        );
        assert!(prompt.contains("[2026-03-25T10:00:00Z] user: Hello!"));
        assert!(prompt.contains("[2026-03-25T10:00:01Z] assistant: Hi there!"));
        assert!(!prompt.contains("{{conversation}}"));
    }

    #[test]
    fn test_build_prompt_with_recap() {
        let messages = make_messages(2);
        let template = "Before\n{{#if recap}}RECAP: {{recap}}{{/if}}\nAfter\n{{conversation}}";

        let prompt = CompactionManager::build_prompt(
            template,
            &messages,
            Some("Previous events."),
            "Char",
            "User",
        );
        assert!(prompt.contains("RECAP: Previous events."));
        assert!(!prompt.contains("{{#if recap}}"));
        assert!(!prompt.contains("{{/if}}"));
    }

    #[test]
    fn test_build_prompt_recap_stripped_when_none() {
        let messages = make_messages(2);
        let template = "Before\n{{#if recap}}RECAP: {{recap}}{{/if}}\nAfter\n{{conversation}}";

        let prompt = CompactionManager::build_prompt(template, &messages, None, "Char", "User");
        assert!(!prompt.contains("RECAP"));
        assert!(!prompt.contains("{{#if recap}}"));
        assert!(prompt.contains("Before"));
        assert!(prompt.contains("After"));
    }

    // -- Tests: helper methods ------------------------------------------------

    #[test]
    fn test_should_force_compact() {
        let mgr = CompactionManager::new(CompactionConfig {
            max_turns: 60,
            min_turns: 20,
            keep_recent_turns: 2,
            ..Default::default()
        });

        assert!(!mgr.should_force_compact(0));
        assert!(!mgr.should_force_compact(19)); // below min
        assert!(!mgr.should_force_compact(59)); // below max
        assert!(mgr.should_force_compact(60));
        assert!(mgr.should_force_compact(100));
    }

    #[test]
    fn test_should_force_compact_disabled() {
        let mgr = CompactionManager::new(CompactionConfig {
            max_turns: 0,
            ..Default::default()
        });
        assert!(!mgr.should_force_compact(1000));
    }

    #[test]
    fn test_has_enough_turns() {
        let mgr = CompactionManager::new(CompactionConfig {
            min_turns: 20,
            keep_recent_turns: 2,
            ..Default::default()
        });

        assert!(!mgr.has_enough_turns(0));
        assert!(!mgr.has_enough_turns(19));
        assert!(mgr.has_enough_turns(20));
        assert!(mgr.has_enough_turns(100));
    }

    // -- Tests: find_turn_split with tool-result messages ----------------------

    #[test]
    fn test_find_turn_split_skips_tool_result_messages() {
        let messages = vec![
            ConversationMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
                timestamp: "t0".to_string(),
                is_tool_result_only: false,
            },
            ConversationMessage {
                role: "assistant".to_string(),
                content: "".to_string(),
                timestamp: "t1".to_string(),
                is_tool_result_only: false,
            },
            ConversationMessage {
                role: "user".to_string(),
                content: "tool output here".to_string(),
                timestamp: "t2".to_string(),
                is_tool_result_only: true,
            },
            ConversationMessage {
                role: "assistant".to_string(),
                content: "Based on the tool result...".to_string(),
                timestamp: "t3".to_string(),
                is_tool_result_only: false,
            },
            ConversationMessage {
                role: "user".to_string(),
                content: "Thanks!".to_string(),
                timestamp: "t4".to_string(),
                is_tool_result_only: false,
            },
            ConversationMessage {
                role: "assistant".to_string(),
                content: "You're welcome!".to_string(),
                timestamp: "t5".to_string(),
                is_tool_result_only: false,
            },
        ];

        assert_eq!(CompactionManager::find_turn_split(&messages, 1), 4);
        assert_eq!(CompactionManager::find_turn_split(&messages, 2), 0);
    }

    #[test]
    fn test_find_turn_split_keep_zero_returns_full_length() {
        // All-user, mixed, and tool-loop-interleaved shapes should all
        // return messages.len() so the caller retains nothing.
        let all_user = vec![
            ConversationMessage {
                role: "user".to_string(),
                content: "a".to_string(),
                timestamp: "t0".to_string(),
                is_tool_result_only: false,
            },
            ConversationMessage {
                role: "user".to_string(),
                content: "b".to_string(),
                timestamp: "t1".to_string(),
                is_tool_result_only: false,
            },
        ];
        assert_eq!(CompactionManager::find_turn_split(&all_user, 0), 2);

        let mixed = vec![
            ConversationMessage {
                role: "user".to_string(),
                content: "hi".to_string(),
                timestamp: "t0".to_string(),
                is_tool_result_only: false,
            },
            ConversationMessage {
                role: "assistant".to_string(),
                content: "hey".to_string(),
                timestamp: "t1".to_string(),
                is_tool_result_only: false,
            },
        ];
        assert_eq!(CompactionManager::find_turn_split(&mixed, 0), 2);

        let with_tool_loop = vec![
            ConversationMessage {
                role: "user".to_string(),
                content: "do a thing".to_string(),
                timestamp: "t0".to_string(),
                is_tool_result_only: false,
            },
            ConversationMessage {
                role: "assistant".to_string(),
                content: "".to_string(),
                timestamp: "t1".to_string(),
                is_tool_result_only: false,
            },
            ConversationMessage {
                role: "user".to_string(),
                content: "tool output".to_string(),
                timestamp: "t2".to_string(),
                is_tool_result_only: true,
            },
            ConversationMessage {
                role: "assistant".to_string(),
                content: "done".to_string(),
                timestamp: "t3".to_string(),
                is_tool_result_only: false,
            },
        ];
        assert_eq!(CompactionManager::find_turn_split(&with_tool_loop, 0), 4);

        let empty: Vec<ConversationMessage> = vec![];
        assert_eq!(CompactionManager::find_turn_split(&empty, 0), 0);
    }

    #[test]
    fn test_find_turn_split_all_tool_results_returns_zero() {
        let messages = vec![
            ConversationMessage {
                role: "user".to_string(),
                content: "tool output".to_string(),
                timestamp: "t0".to_string(),
                is_tool_result_only: true,
            },
            ConversationMessage {
                role: "assistant".to_string(),
                content: "response".to_string(),
                timestamp: "t1".to_string(),
                is_tool_result_only: false,
            },
        ];

        assert_eq!(CompactionManager::find_turn_split(&messages, 1), 0);
    }

    // -- Tests: compaction with retention -------------------------------------

    #[tokio::test]
    async fn test_compact_writes_markdown_files_and_dream_entry() {
        let llm = MockLlm {
            response: make_xml_response(),
        };
        let conv_mgr = MockConversationMgr::new("new-conv-1");
        let mgr = CompactionManager::new(make_config_with_keep(2));
        let messages = make_messages(10);
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();

        let result = mgr
            .compact(
                "conv-1",
                &messages,
                "",
                false,
                DEFAULT_COMPACT_PROMPT,
                None,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                None,
            )
            .await
            .unwrap();

        match result {
            CompactionOutcome::Compacted(r) => {
                assert_eq!(r.memory_files_written.len(), 2);
                assert_eq!(r.conversation_id, "conv-1");
                assert_eq!(r.new_conversation_id, "new-conv-1");
                assert_eq!(r.message_count, 6);
                assert_eq!(r.retained_count, 4);
                assert!(r.recap_generated);
            }
            _ => panic!("Expected Compacted outcome"),
        }

        assert!(store.read("daily/2026-03-25.md").await.is_ok());
        assert!(store.read("preferences/beverages.md").await.is_ok());
        let dreams = store.read("DREAMS.md").await.unwrap();
        assert!(dreams.content.contains("Compacted 6 messages"));
    }

    #[tokio::test]
    async fn test_compact_archives_with_retention() {
        let llm = MockLlm {
            response: make_xml_response(),
        };
        let conv_mgr = MockConversationMgr::new("new-conv-2");
        let mgr = CompactionManager::new(make_config_with_keep(3));
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();

        let result = mgr
            .compact(
                "old-conv",
                &make_messages(10),
                "",
                false,
                DEFAULT_COMPACT_PROMPT,
                None,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                None,
            )
            .await
            .unwrap();

        match result {
            CompactionOutcome::Compacted(r) => {
                assert_eq!(r.new_conversation_id, "new-conv-2");
                assert_eq!(r.retained_count, 6);
            }
            _ => panic!("Expected Compacted outcome"),
        }

        let calls = conv_mgr.archived_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "old-conv");
        assert_eq!(calls[0].1, 6);
    }

    #[tokio::test]
    async fn test_compact_with_keep_turns_zero_retains_nothing() {
        let llm = MockLlm {
            response: make_xml_response(),
        };
        let conv_mgr = MockConversationMgr::new("new-conv-zero");
        let mgr = CompactionManager::new(make_config_with_keep(2));
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();

        let result = mgr
            .compact(
                "conv-1",
                &make_messages(10),
                "",
                false,
                DEFAULT_COMPACT_PROMPT,
                None,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                Some(0),
            )
            .await
            .unwrap();

        match result {
            CompactionOutcome::Compacted(r) => {
                assert_eq!(r.message_count, 10);
                assert_eq!(r.retained_count, 0);
                assert_eq!(r.retained_turns, 0);
                assert_eq!(r.memory_files_written.len(), 2);
            }
            _ => panic!("Expected Compacted outcome"),
        }
    }

    #[tokio::test]
    async fn test_compact_keep_turns_override_beats_config() {
        let llm = MockLlm {
            response: make_xml_response(),
        };
        let conv_mgr = MockConversationMgr::new("new-conv-override");
        let mgr = CompactionManager::new(make_config_with_keep(2));
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();

        let result = mgr
            .compact(
                "conv-1",
                &make_messages(10),
                "",
                false,
                DEFAULT_COMPACT_PROMPT,
                None,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                Some(3),
            )
            .await
            .unwrap();

        match result {
            CompactionOutcome::Compacted(r) => {
                assert_eq!(r.retained_count, 6);
                assert_eq!(r.retained_turns, 3);
            }
            _ => panic!("Expected Compacted outcome"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_compaction_archive_boundary_keeps_executor_responsive() {
        let llm = MockLlm {
            response: make_xml_response(),
        };
        let (conv_mgr, entered_rx, release_tx) = BlockingConversationMgr::new("new-conv-3");
        let mgr = CompactionManager::new(make_config_with_keep(2));
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();

        let compaction = tokio::spawn(async move {
            mgr.compact(
                "conv-1",
                &make_messages(10),
                "",
                false,
                DEFAULT_COMPACT_PROMPT,
                None,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                None,
            )
            .await
        });

        tokio::time::timeout(Duration::from_millis(250), entered_rx)
            .await
            .expect("blocking archive boundary should start promptly")
            .expect("blocking archive boundary should signal entry");

        let sibling_ran = Arc::new(AtomicBool::new(false));
        let sibling_ran_clone = Arc::clone(&sibling_ran);
        let sibling_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            sibling_ran_clone.store(true, Ordering::SeqCst);
        });

        tokio::time::timeout(Duration::from_millis(250), sibling_task)
            .await
            .expect("sibling task should stay responsive during compaction")
            .unwrap();
        assert!(sibling_ran.load(Ordering::SeqCst));

        release_tx.send(()).unwrap();

        let result = compaction.await.unwrap().unwrap();
        match result {
            CompactionOutcome::Compacted(r) => {
                assert_eq!(r.new_conversation_id, "new-conv-3");
                assert_eq!(r.memory_files_written.len(), 2);
            }
            _ => panic!("Expected Compacted outcome"),
        }
    }

    #[tokio::test]
    async fn test_private_conversation_skips_compaction() {
        let llm = MockLlm {
            response: make_xml_response(),
        };
        let conv_mgr = MockConversationMgr::new("new-conv-1");
        let mgr = CompactionManager::new(CompactionConfig::default());
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();

        let result = mgr
            .compact(
                "private-conv",
                &make_messages(10),
                "",
                true,
                DEFAULT_COMPACT_PROMPT,
                None,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                None,
            )
            .await;

        assert!(matches!(result, Err(CompactionError::PrivateConversation)));
        assert!(conv_mgr.archived_calls().is_empty());
    }

    #[tokio::test]
    async fn test_compact_dry_run() {
        let llm = MockLlm {
            response: make_xml_response(),
        };
        let conv_mgr = MockConversationMgr::new("new-conv-1");
        let mgr = CompactionManager::new(make_config_with_keep(2));
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();

        let result = mgr
            .compact(
                "conv-1",
                &make_messages(10),
                "",
                false,
                DEFAULT_COMPACT_PROMPT,
                None,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                true,
                None,
            )
            .await
            .unwrap();

        match result {
            CompactionOutcome::DryRun(r) => {
                assert_eq!(r.would_create_entries, 2);
                assert_eq!(r.message_count, 6);
                assert_eq!(r.retained_count, 4);
                assert_eq!(r.file_ops_preview.len(), 2);
                assert!(r.recap_preview.is_some());
            }
            _ => panic!("Expected DryRun outcome"),
        }

        assert!(store.read("daily/2026-03-25.md").await.is_err());
        assert!(conv_mgr.archived_calls().is_empty());
    }

    #[tokio::test]
    async fn test_compact_empty_messages() {
        let llm = MockLlm {
            response: String::new(),
        };
        let conv_mgr = MockConversationMgr::new("new-conv-1");
        let mgr = CompactionManager::new(CompactionConfig::default());
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();

        let result = mgr
            .compact(
                "conv-1",
                &[],
                "",
                false,
                DEFAULT_COMPACT_PROMPT,
                None,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                None,
            )
            .await;

        assert!(matches!(result, Err(CompactionError::InsufficientMessages)));
    }

    #[tokio::test]
    async fn test_compact_fewer_than_keep_recent_turns() {
        let llm = MockLlm {
            response: String::new(),
        };
        let conv_mgr = MockConversationMgr::new("new-conv-1");
        let mgr = CompactionManager::new(make_config_with_keep(10));
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();

        let result = mgr
            .compact(
                "conv-1",
                &make_messages(5),
                "",
                false,
                DEFAULT_COMPACT_PROMPT,
                None,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                None,
            )
            .await;

        assert!(matches!(result, Err(CompactionError::InsufficientMessages)));
    }

    // -- Tests: idle timer scheduling logic -----------------------------------

    #[tokio::test]
    async fn test_idle_timer_fires_after_duration() {
        tokio::time::pause();

        let mgr = CompactionManager::new(CompactionConfig {
            idle_trigger: shore_config::ConfigDuration::from_secs(300),
            ..Default::default()
        });

        let timer = mgr.idle_timer();
        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = Arc::clone(&fired);

        let handle = tokio::spawn(async move {
            timer.wait_for_idle().await;
            fired_clone.store(true, Ordering::SeqCst);
        });

        tokio::time::advance(Duration::from_secs(4 * 60)).await;
        tokio::task::yield_now().await;
        assert!(!fired.load(Ordering::SeqCst));

        tokio::time::advance(Duration::from_secs(60)).await;
        handle.await.unwrap();
        assert!(fired.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_idle_timer_resets_on_activity() {
        tokio::time::pause();

        let mgr = CompactionManager::new(CompactionConfig {
            idle_trigger: shore_config::ConfigDuration::from_secs(300),
            ..Default::default()
        });

        let timer = mgr.idle_timer();
        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = Arc::clone(&fired);

        let handle = tokio::spawn(async move {
            timer.wait_for_idle().await;
            fired_clone.store(true, Ordering::SeqCst);
        });

        tokio::time::advance(Duration::from_secs(4 * 60)).await;
        tokio::task::yield_now().await;
        assert!(!fired.load(Ordering::SeqCst));

        mgr.notify_activity();
        tokio::task::yield_now().await;

        tokio::time::advance(Duration::from_secs(4 * 60)).await;
        tokio::task::yield_now().await;
        assert!(!fired.load(Ordering::SeqCst));

        tokio::time::advance(Duration::from_secs(60)).await;
        handle.await.unwrap();
        assert!(fired.load(Ordering::SeqCst));
    }

    // -- Tests: rollback on failure -------------------------------------------

    #[tokio::test]
    async fn test_compact_rollback_restores_overwritten_markdown() {
        let llm = MockLlm {
            response: make_xml_response(),
        };
        let conv_mgr = FailingConversationMgr;
        let mgr = CompactionManager::new(make_config_with_keep(2));
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();

        let original = "# Beverage Preferences\n\n- User prefers coffee on weekends\n";
        store
            .write("preferences/beverages.md", original)
            .await
            .unwrap();

        let result = mgr
            .compact(
                "conv-1",
                &make_messages(10),
                "",
                false,
                DEFAULT_COMPACT_PROMPT,
                None,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                None,
            )
            .await;

        assert!(matches!(result, Err(CompactionError::ConversationManager(_))));

        let restored = store.read("preferences/beverages.md").await.unwrap();
        assert_eq!(restored.content, original);
    }
}
