pub mod types;
pub mod parser;
pub mod background;

pub use types::*;
pub use parser::{DEFAULT_COMPACT_PROMPT, parse_compaction_response};
pub use background::run_compaction;

use crate::memory::db::{Entry, MemoryDB};
use chrono::Utc;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::time::Duration;

// ---------------------------------------------------------------------------
// CompactionManager
// ---------------------------------------------------------------------------

pub struct CompactionManager {
    config: CompactionConfig,
    activity_notify: Arc<Notify>,
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
    fn find_turn_split(messages: &[ConversationMessage], keep_turns: usize) -> usize {
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
        if let (Some(if_start), Some(endif_pos)) = (
            result.find("{{#if recap}}"),
            result.find("{{/if}}"),
        ) {
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

    /// Generate an entry ID in the standard format: YYYYMMDD_HHMMSS_N
    fn generate_entry_id(index: usize) -> String {
        let now = Utc::now();
        format!("{}_{}", now.format("%Y%m%d_%H%M%S"), index)
    }

    /// Run compaction on a conversation.
    ///
    /// Splits messages into a compacted portion (sent to LLM) and a retained
    /// portion (kept in active.jsonl). The LLM generates both a rolling recap
    /// and memory entries from the compacted messages.
    ///
    /// If `dry_run` is true, returns what would be created without side effects.
    #[allow(clippy::too_many_arguments)]
    pub async fn compact(
        &self,
        conversation_id: &str,
        messages: &[ConversationMessage],
        is_private: bool,
        prompt_template: &str,
        existing_recap: Option<&str>,
        char_name: &str,
        user_name: &str,
        llm: &dyn CompactionLlm,
        db: &MemoryDB,
        indexer: &dyn VectorIndexer,
        conversation_mgr: &dyn ConversationManager,
        dry_run: bool,
    ) -> Result<CompactionOutcome, CompactionError> {
        // Skip private conversations entirely.
        if is_private {
            return Err(CompactionError::PrivateConversation);
        }

        if messages.is_empty() {
            return Err(CompactionError::InsufficientMessages);
        }

        // Split messages: compact the older portion, retain the recent tail.
        let split_at = Self::find_turn_split(messages, self.config.keep_recent_turns);
        if split_at == 0 {
            return Err(CompactionError::InsufficientMessages);
        }
        let compacted_part = &messages[..split_at];

        // Build and send prompt to LLM (only compacted messages, not retained).
        let prompt = Self::build_prompt(prompt_template, compacted_part, existing_recap, char_name, user_name);
        let raw_response = llm.summarize(&prompt).await?;

        // Parse recap + entries from LLM response.
        let (recap, compacted) = parse_compaction_response(&raw_response)?;

        let retained_turns = self.config.keep_recent_turns;

        // Dry run: return preview without side effects.
        if dry_run {
            return Ok(CompactionOutcome::DryRun(DryRunResult {
                would_create_entries: compacted.len(),
                entries_preview: compacted,
                message_count: split_at,
                retained_count: messages.len() - split_at,
                retained_turns,
                recap_preview: recap,
            }));
        }

        // Determine time range from compacted messages.
        let start_timestamp = compacted_part
            .first()
            .map(|m| m.timestamp.clone())
            .unwrap_or_default();
        let end_timestamp = compacted_part
            .last()
            .map(|m| m.timestamp.clone())
            .unwrap_or_default();

        let now_str = Utc::now().to_rfc3339();
        let mut entry_ids = Vec::new();

        for (i, ce) in compacted.iter().enumerate() {
            let entry_id = Self::generate_entry_id(i);

            let entry = Entry {
                id: entry_id.clone(),
                memory_type: ce.memory_type.clone(),
                source: "summary".to_string(),
                reason: "compaction".to_string(),
                status: "active".to_string(),
                confidence: ce.confidence,
                summary_text: ce.summary_text.clone(),
                topic_tags: ce.topic_tags.clone(),
                topic_key: ce.topic_key.clone(),
                start_timestamp: start_timestamp.clone(),
                end_timestamp: end_timestamp.clone(),
                message_count: split_at as i64,
                source_entry_ids: String::new(),
                related_entry_ids: String::new(),
                superseded_by: String::new(),
                created_at: now_str.clone(),
                updated_at: now_str.clone(),
                entry_type: String::new(),
                image_path: String::new(),
                collated_at: String::new(),
            };

            db.create_entry(&entry)
                .map_err(|e| CompactionError::Db(e.to_string()))?;

            // Index to vector store.
            indexer.index_entry(&entry_id, &ce.summary_text).await?;

            // Record changelog.
            let cl_id = db
                .append_changelog(
                    "compaction",
                    &format!(
                        "Compacted conversation {} into entry {}",
                        conversation_id, entry_id
                    ),
                )
                .map_err(|e| CompactionError::Db(e.to_string()))?;

            db.link_changelog_entry(cl_id, &entry_id)
                .map_err(|e| CompactionError::Db(e.to_string()))?;

            entry_ids.push(entry_id);
        }

        // Archive compacted messages, retain recent, write recap.
        let retained = messages.len() - split_at;
        let new_conversation_id = conversation_mgr.archive_and_retain(
            conversation_id,
            RetentionParams {
                keep_last_n: retained,
                recap: recap.clone(),
            },
        )?;

        Ok(CompactionOutcome::Compacted(CompactionResult {
            entries_created: entry_ids,
            conversation_id: conversation_id.to_string(),
            new_conversation_id,
            message_count: split_at,
            retained_count: retained,
            retained_turns,
            recap_generated: recap.is_some(),
        }))
    }

    /// Create an idle timer bound to this manager's activity signal.
    pub fn idle_timer(&self) -> IdleTimer {
        IdleTimer {
            idle_duration: Duration::from_secs(u64::from(self.config.idle_trigger_minutes) * 60),
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
                    return;
                }
                () = self.activity_notify.notified() => {
                    // Activity detected — reset timer by restarting loop.
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex as StdMutex;
    use std::future::Future;
    use std::pin::Pin;

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

    struct MockIndexer {
        indexed: StdMutex<Vec<(String, String)>>,
    }

    impl MockIndexer {
        fn new() -> Self {
            Self {
                indexed: StdMutex::new(Vec::new()),
            }
        }

        fn indexed_entries(&self) -> Vec<(String, String)> {
            self.indexed.lock().unwrap().clone()
        }
    }

    impl VectorIndexer for MockIndexer {
        fn index_entry(
            &self,
            entry_id: &str,
            text: &str,
        ) -> Pin<Box<dyn Future<Output = Result<(), CompactionError>> + Send + '_>> {
            self.indexed
                .lock()
                .unwrap()
                .push((entry_id.to_string(), text.to_string()));
            Box::pin(async { Ok(()) })
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
        ) -> Result<String, CompactionError> {
            self.archived
                .lock()
                .unwrap()
                .push((conversation_id.to_string(), params.keep_last_n));
            Ok(self.next_id.clone())
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
                timestamp: Utc::now().to_rfc3339(),
                is_tool_result_only: false,
            })
            .collect()
    }

    fn make_xml_response() -> String {
        r#"<recap>
The assistant had a pleasant conversation with the user about their day and preferences.
They discussed daily activities and the user's beverage preferences.
</recap>

<entry>
<summary>
- User discussed their day
- They mentioned having a busy morning
</summary>
<topic_tags>daily, personal</topic_tags>
<memory_type>episodic</memory_type>
</entry>

<entry>
<summary>
- User prefers tea over coffee
- This is a stable preference
</summary>
<topic_tags>preference, food</topic_tags>
<memory_type>semantic</memory_type>
</entry>"#
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

        let prompt =
            CompactionManager::build_prompt("Template:\n{{conversation}}", &messages, None, "Char", "User");
        assert!(prompt.contains("[2026-03-25T10:00:00Z] user: Hello!"));
        assert!(prompt.contains("[2026-03-25T10:00:01Z] assistant: Hi there!"));
        assert!(!prompt.contains("{{conversation}}"));
    }

    #[test]
    fn test_build_prompt_with_recap() {
        let messages = make_messages(2);
        let template =
            "Before\n{{#if recap}}RECAP: {{recap}}{{/if}}\nAfter\n{{conversation}}";

        let prompt =
            CompactionManager::build_prompt(template, &messages, Some("Previous events."), "Char", "User");
        assert!(prompt.contains("RECAP: Previous events."));
        assert!(!prompt.contains("{{#if recap}}"));
        assert!(!prompt.contains("{{/if}}"));
    }

    #[test]
    fn test_build_prompt_recap_stripped_when_none() {
        let messages = make_messages(2);
        let template =
            "Before\n{{#if recap}}RECAP: {{recap}}{{/if}}\nAfter\n{{conversation}}";

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
    async fn test_compact_creates_entries() {
        let db = MemoryDB::open_in_memory().unwrap();
        let llm = MockLlm {
            response: make_xml_response(),
        };
        let indexer = MockIndexer::new();
        let conv_mgr = MockConversationMgr::new("new-conv-1");
        let mgr = CompactionManager::new(make_config_with_keep(2));
        let messages = make_messages(10);

        let result = mgr
            .compact(
                "conv-1",
                &messages,
                false,
                DEFAULT_COMPACT_PROMPT,
                None,
                "TestChar",
                "TestUser",
                &llm,
                &db,
                &indexer,
                &conv_mgr,
                false,
            )
            .await
            .unwrap();

        match result {
            CompactionOutcome::Compacted(r) => {
                assert_eq!(r.entries_created.len(), 2);
                assert_eq!(r.conversation_id, "conv-1");
                assert_eq!(r.new_conversation_id, "new-conv-1");
                assert_eq!(r.message_count, 6);
                assert_eq!(r.retained_count, 4);
                assert!(r.recap_generated);

                for id in &r.entries_created {
                    let entry = db.get_entry(id).unwrap().unwrap();
                    assert_eq!(entry.reason, "compaction");
                    assert_eq!(entry.source, "summary");
                    assert_eq!(entry.status, "active");
                    assert_eq!(entry.message_count, 6);
                }
            }
            _ => panic!("Expected Compacted outcome"),
        }
    }

    #[tokio::test]
    async fn test_compact_indexes_to_vector_store() {
        let db = MemoryDB::open_in_memory().unwrap();
        let llm = MockLlm {
            response: make_xml_response(),
        };
        let indexer = MockIndexer::new();
        let conv_mgr = MockConversationMgr::new("new-conv-1");
        let mgr = CompactionManager::new(make_config_with_keep(2));

        mgr.compact(
            "conv-1",
            &make_messages(10),
            false,
            DEFAULT_COMPACT_PROMPT,
            None,
            "TestChar",
            "TestUser",
            &llm,
            &db,
            &indexer,
            &conv_mgr,
            false,
        )
        .await
        .unwrap();

        let indexed = indexer.indexed_entries();
        assert_eq!(indexed.len(), 2);
        assert!(indexed[0].1.contains("User discussed their day"));
        assert!(indexed[1].1.contains("User prefers tea"));
    }

    #[tokio::test]
    async fn test_compact_records_changelog() {
        let db = MemoryDB::open_in_memory().unwrap();
        let llm = MockLlm {
            response: make_xml_response(),
        };
        let indexer = MockIndexer::new();
        let conv_mgr = MockConversationMgr::new("new-conv-1");
        let mgr = CompactionManager::new(make_config_with_keep(2));

        mgr.compact(
            "conv-1",
            &make_messages(10),
            false,
            DEFAULT_COMPACT_PROMPT,
            None,
            "TestChar",
            "TestUser",
            &llm,
            &db,
            &indexer,
            &conv_mgr,
            false,
        )
        .await
        .unwrap();

        let logs = db.get_recent_changelog(10).unwrap();
        assert_eq!(logs.len(), 2);
        assert!(logs.iter().all(|l| l.operation == "compaction"));
        assert!(logs.iter().all(|l| l.description.contains("conv-1")));
    }

    #[tokio::test]
    async fn test_compact_archives_with_retention() {
        let db = MemoryDB::open_in_memory().unwrap();
        let llm = MockLlm {
            response: make_xml_response(),
        };
        let indexer = MockIndexer::new();
        let conv_mgr = MockConversationMgr::new("new-conv-2");
        let mgr = CompactionManager::new(make_config_with_keep(3));

        let result = mgr
            .compact(
                "old-conv",
                &make_messages(10),
                false,
                DEFAULT_COMPACT_PROMPT,
                None,
                "TestChar",
                "TestUser",
                &llm,
                &db,
                &indexer,
                &conv_mgr,
                false,
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
    async fn test_private_conversation_skips_compaction() {
        let db = MemoryDB::open_in_memory().unwrap();
        let llm = MockLlm {
            response: make_xml_response(),
        };
        let indexer = MockIndexer::new();
        let conv_mgr = MockConversationMgr::new("new-conv-1");
        let mgr = CompactionManager::new(CompactionConfig::default());

        let result = mgr
            .compact(
                "private-conv",
                &make_messages(10),
                true,
                DEFAULT_COMPACT_PROMPT,
                None,
                "TestChar",
                "TestUser",
                &llm,
                &db,
                &indexer,
                &conv_mgr,
                false,
            )
            .await;

        assert!(matches!(result, Err(CompactionError::PrivateConversation)));

        assert!(db.get_entries_by_status("active").unwrap().is_empty());
        assert!(indexer.indexed_entries().is_empty());
        assert!(conv_mgr.archived_calls().is_empty());
    }

    #[tokio::test]
    async fn test_compact_dry_run() {
        let db = MemoryDB::open_in_memory().unwrap();
        let llm = MockLlm {
            response: make_xml_response(),
        };
        let indexer = MockIndexer::new();
        let conv_mgr = MockConversationMgr::new("new-conv-1");
        let mgr = CompactionManager::new(make_config_with_keep(2));

        let result = mgr
            .compact(
                "conv-1",
                &make_messages(10),
                false,
                DEFAULT_COMPACT_PROMPT,
                None,
                "TestChar",
                "TestUser",
                &llm,
                &db,
                &indexer,
                &conv_mgr,
                true,
            )
            .await
            .unwrap();

        match result {
            CompactionOutcome::DryRun(r) => {
                assert_eq!(r.would_create_entries, 2);
                assert_eq!(r.message_count, 6);
                assert_eq!(r.retained_count, 4);
                assert_eq!(r.entries_preview.len(), 2);
                assert!(r.recap_preview.is_some());
            }
            _ => panic!("Expected DryRun outcome"),
        }

        assert!(db.get_entries_by_status("active").unwrap().is_empty());
        assert!(indexer.indexed_entries().is_empty());
        assert!(conv_mgr.archived_calls().is_empty());
    }

    #[tokio::test]
    async fn test_compact_empty_messages() {
        let db = MemoryDB::open_in_memory().unwrap();
        let llm = MockLlm {
            response: String::new(),
        };
        let indexer = MockIndexer::new();
        let conv_mgr = MockConversationMgr::new("new-conv-1");
        let mgr = CompactionManager::new(CompactionConfig::default());

        let result = mgr
            .compact(
                "conv-1",
                &[],
                false,
                DEFAULT_COMPACT_PROMPT,
                None,
                "TestChar",
                "TestUser",
                &llm,
                &db,
                &indexer,
                &conv_mgr,
                false,
            )
            .await;

        assert!(matches!(result, Err(CompactionError::InsufficientMessages)));
    }

    #[tokio::test]
    async fn test_compact_fewer_than_keep_recent_turns() {
        let db = MemoryDB::open_in_memory().unwrap();
        let llm = MockLlm {
            response: String::new(),
        };
        let indexer = MockIndexer::new();
        let conv_mgr = MockConversationMgr::new("new-conv-1");
        let mgr = CompactionManager::new(make_config_with_keep(10));

        let result = mgr
            .compact(
                "conv-1",
                &make_messages(5),
                false,
                DEFAULT_COMPACT_PROMPT,
                None,
                "TestChar",
                "TestUser",
                &llm,
                &db,
                &indexer,
                &conv_mgr,
                false,
            )
            .await;

        assert!(matches!(result, Err(CompactionError::InsufficientMessages)));
    }

    // -- Tests: idle timer scheduling logic -----------------------------------

    #[tokio::test]
    async fn test_idle_timer_fires_after_duration() {
        tokio::time::pause();

        let mgr = CompactionManager::new(CompactionConfig {
            idle_trigger_minutes: 5,
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
            idle_trigger_minutes: 5,
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
}
