use crate::memory::db::{Entry, MemoryDB};
use chrono::Utc;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::time::Duration;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

const DEFAULT_IDLE_TRIGGER_MINUTES: u64 = 30;
const DEFAULT_MIN_TURNS: usize = 8;
const DEFAULT_MAX_TURNS: usize = 16;
const DEFAULT_KEEP_RECENT_TURNS: usize = 2;

/// Configuration for compaction triggers.
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// Minutes of idle time before proactive compaction fires.
    pub idle_trigger_minutes: u64,
    /// Minimum user turns before any compaction trigger fires.
    pub min_turns: usize,
    /// Force compaction when this user turn count is reached.
    pub max_turns: usize,
    /// User turns retained in active conversation after compaction.
    pub keep_recent_turns: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            idle_trigger_minutes: DEFAULT_IDLE_TRIGGER_MINUTES,
            min_turns: DEFAULT_MIN_TURNS,
            max_turns: DEFAULT_MAX_TURNS,
            keep_recent_turns: DEFAULT_KEEP_RECENT_TURNS,
        }
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A message from a conversation, used as input to compaction.
#[derive(Debug, Clone)]
pub struct ConversationMessage {
    pub role: String,
    pub content: String,
    pub timestamp: String,
    /// True when a user message's content_blocks are ALL ToolResult
    /// (i.e. a tool-loop intermediate, not a real user turn).
    pub is_tool_result_only: bool,
}

/// A memory entry extracted by the LLM during compaction.
#[derive(Debug, Clone)]
pub struct CompactedEntry {
    pub memory_type: String,
    pub summary_text: String,
    pub topic_tags: String,
    pub topic_key: String,
    pub confidence: f64,
}

/// Outcome of a compaction operation.
#[derive(Debug)]
pub enum CompactionOutcome {
    Compacted(CompactionResult),
    DryRun(DryRunResult),
}

/// Result of an actual compaction.
#[derive(Debug)]
pub struct CompactionResult {
    pub entries_created: Vec<String>,
    pub conversation_id: String,
    pub new_conversation_id: String,
    pub message_count: usize,
    pub retained_count: usize,
    pub retained_turns: usize,
    pub recap_generated: bool,
}

/// Result of a dry-run compaction.
#[derive(Debug)]
pub struct DryRunResult {
    pub would_create_entries: usize,
    pub entries_preview: Vec<CompactedEntry>,
    pub message_count: usize,
    pub retained_count: usize,
    pub retained_turns: usize,
    pub recap_preview: Option<String>,
}

/// Parameters for archiving with message retention.
#[derive(Debug)]
pub struct RetentionParams {
    /// Number of messages to keep from the end of active.jsonl.
    pub keep_last_n: usize,
    /// Recap text to write to memory/recap.md (None = leave untouched).
    pub recap: Option<String>,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    #[error("llm: {0}")]
    Llm(String),
    #[error("db: {0}")]
    Db(String),
    #[error("parse: {0}")]
    Parse(String),
    #[error("private conversation: skipped")]
    PrivateConversation,
    #[error("insufficient messages")]
    InsufficientMessages,
    #[error("indexing: {0}")]
    Indexing(String),
    #[error("conversation: {0}")]
    ConversationManager(String),
}

// ---------------------------------------------------------------------------
// Traits for external dependencies
// ---------------------------------------------------------------------------

/// LLM client for compaction. Takes a rendered prompt, returns raw LLM text.
///
/// The library owns the prompt format (XML) and handles parsing the response
/// into recap + entries. The impl just sends text and returns text.
pub trait CompactionLlm: Send + Sync {
    fn summarize(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, CompactionError>> + Send + '_>>;
}

/// Vector indexer for newly created entries.
pub trait VectorIndexer: Send + Sync {
    fn index_entry(
        &self,
        entry_id: &str,
        text: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), CompactionError>> + Send + '_>>;
}

/// Conversation lifecycle management — archive old messages and retain recent ones.
pub trait ConversationManager: Send + Sync {
    fn archive_and_retain(
        &self,
        conversation_id: &str,
        params: RetentionParams,
    ) -> Result<String, CompactionError>;
}

// ---------------------------------------------------------------------------
// Default prompt template
// ---------------------------------------------------------------------------

/// Default compaction prompt template. In production, loaded from `compact.md`.
///
/// Placeholders:
/// - `{{char}}`, `{{user}}` — character and user names
/// - `{{conversation}}` — formatted conversation messages
/// - `{{#if recap}}...{{/if}}` — conditional block for existing recap
/// - `{{recap}}` — existing recap text (inside conditional)
pub const DEFAULT_COMPACT_PROMPT: &str = r#"You are recording what happened in this specific conversation between {{user}} and {{char}}. Write temporal, narrative entries — events, decisions, what was said, emotional shifts — anchored to this conversation. Do not extract timeless facts or stable preferences; those are handled separately.

Preserve:
- Key events and decisions made in this conversation
- Emotional developments and relationship changes
- Ongoing threads or unresolved topics
- Specific details that would be important to remember later
- If {{user}} corrected or updated previously stated information, note the change explicitly

Your response MUST contain two parts, in this order:

1. A single <recap> block — a flowing narrative (2-4 paragraphs) written **about {{char}} in close third person, using {{char}}'s own voice and vocabulary** — not "I" but "{{char}}" / "she" / "he" / "they". Same emotional texture, same interpretive lens, third-person pronouns. Cover what happened, how {{char}} felt about it, what matters to them, and where things stand with {{user}}.

{{#if recap}}
Here is the existing recap from previous compactions. Fold it into your new recap — preserve ongoing threads and relationship developments while incorporating new events. Older details should condense naturally but never disappear entirely:
<previous_recap>
{{recap}}
</previous_recap>
{{/if}}

<recap>
[rolling narrative recap, close third person about {{char}}]
</recap>

2. One or more <entry> blocks (one per topic discussed).

Each entry should be **atomic** — focused on exactly one topic or event. Prefer more entries with fewer bullets (2-4 each) over fewer entries with many bullets. If your bullets cover different subjects, split them into separate entries. Each entry is embedded as a single vector for retrieval, so mixing unrelated topics in one entry makes it harder to find later.

Both parts are required. Begin with the <recap>, then the <entry> blocks.

<entry>
<summary>
- [key fact or event, one per line]
- [preserve names, dates, specifics]
- [include emotional context where relevant]
</summary>
<topic_tags>
[comma separated short tags for this topic]
</topic_tags>
<entities>
- name: [entity name], type: [person/place/organization/concept], relation: [brief description of relation to the conversation]
</entities>
<memory_type>
[episodic or semantic — "episodic" for events, conversations, time-bound happenings; "semantic" for stable facts, preferences, traits, relationships]
</memory_type>
</entry>

If the entire conversation covers only one topic, produce a single <entry> block.

Conversation:
{{conversation}}"#;

// ---------------------------------------------------------------------------
// XML parsing helpers
// ---------------------------------------------------------------------------

/// Extract content between `<tag>` and `</tag>` (first occurrence).
fn extract_xml_tag(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)?;
    let content_start = start + open.len();
    let end = text[content_start..].find(&close)?;
    let content = text[content_start..content_start + end].trim();
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}

/// Extract all occurrences of `<tag>...</tag>` in the text.
fn extract_all_xml_tags(text: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut results = Vec::new();
    let mut search_from = 0;
    while let Some(start) = text[search_from..].find(&open) {
        let abs_start = search_from + start + open.len();
        if let Some(end) = text[abs_start..].find(&close) {
            let content = text[abs_start..abs_start + end].trim();
            if !content.is_empty() {
                results.push(content.to_string());
            }
            search_from = abs_start + end + close.len();
        } else {
            break;
        }
    }
    results
}

/// Parse raw LLM response into recap + entries.
///
/// Expected format: `<recap>...</recap>` followed by one or more `<entry>...</entry>` blocks.
/// Each entry contains `<summary>`, `<topic_tags>`, and `<memory_type>` sub-tags.
pub fn parse_compaction_response(
    raw: &str,
) -> Result<(Option<String>, Vec<CompactedEntry>), CompactionError> {
    let recap = extract_xml_tag(raw, "recap");

    let entry_blocks = extract_all_xml_tags(raw, "entry");
    if entry_blocks.is_empty() {
        return Err(CompactionError::Parse(
            "no <entry> blocks found in LLM response".to_string(),
        ));
    }

    let mut entries = Vec::new();
    for block in &entry_blocks {
        let summary_text = extract_xml_tag(block, "summary").unwrap_or_default();
        let topic_tags = extract_xml_tag(block, "topic_tags").unwrap_or_default();
        let memory_type = extract_xml_tag(block, "memory_type")
            .unwrap_or_else(|| "episodic".to_string());

        // Derive topic_key from the first tag.
        let topic_key = topic_tags
            .split(',')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase()
            .replace(' ', "_");

        entries.push(CompactedEntry {
            memory_type: memory_type.trim().to_string(),
            summary_text,
            topic_tags,
            topic_key,
            confidence: 0.9,
        });
    }

    Ok((recap, entries))
}

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
    ///
    /// Tool-result user messages have content that starts with `[{"type":"tool_result"`
    /// (serialized content_blocks). Tool-use-only assistant messages have content
    /// that is empty (the actual tool_use lives in content_blocks, which is not
    /// in ConversationMessage).  We detect these by checking the role and whether
    /// the content looks like a tool_result block array or is empty for assistants
    /// that appear right before a tool_result user message.
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
        // Count backward by user turns (skipping tool-loop messages) to find
        // the split point, so `keep_recent_turns` whole turns are preserved.
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
            idle_duration: Duration::from_secs(self.config.idle_trigger_minutes * 60),
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

    /// Standard XML response matching the new prompt format.
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

    // -- Tests: XML parsing ---------------------------------------------------

    #[test]
    fn test_extract_xml_tag() {
        let text = "before <recap>hello world</recap> after";
        assert_eq!(extract_xml_tag(text, "recap"), Some("hello world".to_string()));
    }

    #[test]
    fn test_extract_xml_tag_not_found() {
        assert_eq!(extract_xml_tag("no tags here", "recap"), None);
    }

    #[test]
    fn test_extract_xml_tag_empty() {
        assert_eq!(extract_xml_tag("<recap></recap>", "recap"), None);
    }

    #[test]
    fn test_extract_xml_tag_with_whitespace() {
        let text = "<recap>\n  trimmed content  \n</recap>";
        assert_eq!(extract_xml_tag(text, "recap"), Some("trimmed content".to_string()));
    }

    #[test]
    fn test_extract_all_xml_tags() {
        let text = "<entry>first</entry> middle <entry>second</entry>";
        let results = extract_all_xml_tags(text, "entry");
        assert_eq!(results, vec!["first", "second"]);
    }

    #[test]
    fn test_parse_compaction_response() {
        let raw = make_xml_response();
        let (recap, entries) = parse_compaction_response(&raw).unwrap();

        assert!(recap.is_some());
        assert!(recap.unwrap().contains("pleasant conversation"));
        assert_eq!(entries.len(), 2);
        assert!(entries[0].summary_text.contains("User discussed their day"));
        assert_eq!(entries[0].memory_type, "episodic");
        assert_eq!(entries[0].topic_tags, "daily, personal");
        assert!(entries[1].summary_text.contains("User prefers tea"));
        assert_eq!(entries[1].memory_type, "semantic");
    }

    #[test]
    fn test_parse_compaction_response_no_entries() {
        let raw = "<recap>Just a recap</recap>";
        let result = parse_compaction_response(raw);
        assert!(matches!(result, Err(CompactionError::Parse(_))));
    }

    #[test]
    fn test_parse_compaction_response_no_recap() {
        let raw = r#"<entry>
<summary>- Something happened</summary>
<topic_tags>test</topic_tags>
<memory_type>episodic</memory_type>
</entry>"#;
        let (recap, entries) = parse_compaction_response(raw).unwrap();
        assert!(recap.is_none());
        assert_eq!(entries.len(), 1);
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
        // Simulate: user, assistant, tool-result-user, assistant, user, assistant
        // Real user turns: index 0 and index 4.  With keep_turns=1, we should
        // retain from index 4 onward (the last real turn + its assistant reply).
        let messages = vec![
            ConversationMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
                timestamp: "t0".to_string(),
                is_tool_result_only: false,
            },
            ConversationMessage {
                role: "assistant".to_string(),
                content: "".to_string(), // tool_use only
                timestamp: "t1".to_string(),
                is_tool_result_only: false,
            },
            ConversationMessage {
                role: "user".to_string(),
                content: "tool output here".to_string(),
                timestamp: "t2".to_string(),
                is_tool_result_only: true, // tool-result intermediate
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

        // keep 1 turn → split at index 4 (the last real user turn)
        assert_eq!(CompactionManager::find_turn_split(&messages, 1), 4);
        // keep 2 turns → split at index 0 (both real user turns retained)
        assert_eq!(CompactionManager::find_turn_split(&messages, 2), 0);
    }

    #[test]
    fn test_find_turn_split_all_tool_results_returns_zero() {
        // Edge case: only tool-result user messages, no real turns.
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
                assert_eq!(r.message_count, 6); // 10 - 4 retained (2 turns = 4 msgs)
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
                assert_eq!(r.retained_count, 6); // 3 turns × 2 msgs each
            }
            _ => panic!("Expected Compacted outcome"),
        }

        let calls = conv_mgr.archived_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "old-conv");
        assert_eq!(calls[0].1, 6); // keep_last_n (3 turns = 6 raw messages)
    }

    // -- Tests: private conversation skips compaction -------------------------

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

        // No side effects.
        assert!(db.get_entries_by_status("active").unwrap().is_empty());
        assert!(indexer.indexed_entries().is_empty());
        assert!(conv_mgr.archived_calls().is_empty());
    }

    // -- Tests: dry run -------------------------------------------------------

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

        // No side effects.
        assert!(db.get_entries_by_status("active").unwrap().is_empty());
        assert!(indexer.indexed_entries().is_empty());
        assert!(conv_mgr.archived_calls().is_empty());
    }

    // -- Tests: insufficient messages -----------------------------------------

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

        // Only 5 messages but keep_recent_turns=10 — nothing to compact.
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

        // 4 minutes — should NOT have fired.
        tokio::time::advance(Duration::from_secs(4 * 60)).await;
        tokio::task::yield_now().await;
        assert!(!fired.load(Ordering::SeqCst));

        // 1 more minute (total 5) — should fire.
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

        // Advance 4 minutes — should NOT have fired.
        tokio::time::advance(Duration::from_secs(4 * 60)).await;
        tokio::task::yield_now().await;
        assert!(!fired.load(Ordering::SeqCst));

        // Notify activity — resets timer.
        mgr.notify_activity();
        tokio::task::yield_now().await;

        // 4 more minutes since reset — still should NOT have fired.
        tokio::time::advance(Duration::from_secs(4 * 60)).await;
        tokio::task::yield_now().await;
        assert!(!fired.load(Ordering::SeqCst));

        // 1 more minute (5 since reset) — should fire.
        tokio::time::advance(Duration::from_secs(60)).await;
        handle.await.unwrap();
        assert!(fired.load(Ordering::SeqCst));
    }
}
