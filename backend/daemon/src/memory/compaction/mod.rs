pub mod background;
pub mod parser;
pub mod types;

pub use background::run_compaction;
pub use parser::{
    parse_compaction_response, MemoryFileOp, DEFAULT_COMPACT_PROMPT, DEFAULT_COMPACT_SYSTEM,
};
pub use types::*;

use crate::memory::markdown_store::MarkdownMemoryStore;
use shore_config::character_data_dir;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::time::Duration;
use tracing::{debug, info, instrument, warn};

const EXISTING_MEMORY_CONTEXT_MAX_FILES: usize = 24;
const EXISTING_MEMORY_CONTEXT_MAX_CHARS_PER_FILE: usize = 1_800;

// ---------------------------------------------------------------------------
// CompactionManager
// ---------------------------------------------------------------------------

pub struct CompactionManager {
    config: CompactionConfig,
    activity_notify: Arc<Notify>,
}

struct CompactionWriteState {
    display_path: String,
    target: CompactionWriteTarget,
    previous_content: Option<String>,
}

enum CompactionWriteTarget {
    /// Resolved absolute path inside the character workspace. Both
    /// memory entries (memory/...) and the workspace-root MEMORY.md
    /// land here.
    WorkspaceFile { path: PathBuf },
}

fn truncate_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn escape_attr(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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
    /// retains nothing and compacts everything.
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

    /// Render the system prompt template, substituting `{{char}}` and `{{user}}`.
    pub fn build_system(template: &str, char_name: &str, user_name: &str) -> String {
        template
            .replace("{{char}}", char_name)
            .replace("{{user}}", user_name)
    }

    /// Render the final compaction user message. The template supports:
    /// - `{{existing_memories}}` — bounded snapshot of current markdown memories
    /// - `{{char}}` / `{{user}}` — character and user names
    /// - legacy `{{#if recap}}...{{/if}}` and `{{recap}}` placeholders, which
    ///   are stripped (recaps are no longer generated).
    pub fn build_final_message(
        final_message_template: &str,
        existing_memories: Option<&str>,
        char_name: &str,
        user_name: &str,
    ) -> String {
        let existing_memories_text = existing_memories
            .filter(|m| !m.trim().is_empty())
            .unwrap_or("No existing memory files were available.");

        let mut final_msg = final_message_template
            .replace("{{existing_memories}}", existing_memories_text)
            .replace("{{char}}", char_name)
            .replace("{{user}}", user_name);

        while let (Some(if_start), Some(endif_pos)) =
            (final_msg.find("{{#if recap}}"), final_msg.find("{{/if}}"))
        {
            final_msg = format!(
                "{}{}",
                &final_msg[..if_start],
                &final_msg[endif_pos + "{{/if}}".len()..],
            );
        }
        final_msg.replace("{{recap}}", "")
    }

    /// Build the structured messages array for a fresh-prefix compaction LLM
    /// call. Returns the conversation messages as role/content JSON objects
    /// followed by a final user message rendered from `final_message_template`.
    ///
    /// In cache-preserving mode, the conversation slice is already part of the
    /// cached request prefix; only [`build_final_message`] is needed.
    pub fn build_messages(
        final_message_template: &str,
        messages: &[ConversationMessage],
        existing_memories: Option<&str>,
        char_name: &str,
        user_name: &str,
    ) -> Vec<serde_json::Value> {
        let mut result: Vec<serde_json::Value> = messages
            .iter()
            .map(|msg| serde_json::json!({"role": msg.role, "content": msg.content}))
            .collect();
        let final_msg = Self::build_final_message(
            final_message_template,
            existing_memories,
            char_name,
            user_name,
        );
        result.push(serde_json::json!({"role": "user", "content": final_msg}));
        result
    }

    /// Build a compaction prompt from a template and conversation messages.
    ///
    /// Legacy helper used by tests that check the flattened prompt string.
    /// Replaces `{{conversation}}` with formatted messages, replaces
    /// `{{existing_memories}}` with a bounded markdown-memory snapshot, and
    /// substitutes `{{char}}` / `{{user}}` with the provided names.
    #[cfg(test)]
    pub fn build_prompt(
        template: &str,
        messages: &[ConversationMessage],
        existing_memories: Option<&str>,
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
        let existing_memories_text = existing_memories
            .filter(|m| !m.trim().is_empty())
            .unwrap_or("No existing memory files were available.");
        result = result.replace("{{existing_memories}}", existing_memories_text);

        while let (Some(if_start), Some(endif_pos)) =
            (result.find("{{#if recap}}"), result.find("{{/if}}"))
        {
            result = format!(
                "{}{}",
                &result[..if_start],
                &result[endif_pos + "{{/if}}".len()..],
            );
        }
        result = result.replace("{{recap}}", "");

        result = result.replace("{{char}}", char_name);
        result = result.replace("{{user}}", user_name);

        result
    }

    async fn build_existing_memory_context(markdown_store: Option<&MarkdownMemoryStore>) -> String {
        let Some(store) = markdown_store else {
            return "No existing memory files were available.".to_string();
        };

        let entries = match store.list_all().await {
            Ok(entries) => entries,
            Err(e) => {
                warn!(error = %e, "compaction: failed to read existing memory files");
                return format!("Existing memory files could not be loaded: {e}");
            }
        };

        let mut entries: Vec<_> = entries
            .into_iter()
            .filter(|entry| entry.path != "DREAMS.md")
            .collect();
        entries.sort_by(|a, b| a.path.cmp(&b.path));

        if entries.is_empty() {
            return "No existing memory files yet.".to_string();
        }

        let total = entries.len();
        let mut context = String::new();
        for entry in entries.into_iter().take(EXISTING_MEMORY_CONTEXT_MAX_FILES) {
            context.push_str(&format!(
                "<file path=\"memory/{}\">\n",
                escape_attr(&entry.path)
            ));
            context.push_str(&truncate_chars(
                &entry.content,
                EXISTING_MEMORY_CONTEXT_MAX_CHARS_PER_FILE,
            ));
            if entry.content.chars().count() > EXISTING_MEMORY_CONTEXT_MAX_CHARS_PER_FILE {
                context.push_str("\n...[truncated]");
            }
            context.push_str("\n</file>\n\n");
        }

        if total > EXISTING_MEMORY_CONTEXT_MAX_FILES {
            context.push_str(&format!(
                "{} additional memory files omitted from this snapshot.\n",
                total - EXISTING_MEMORY_CONTEXT_MAX_FILES
            ));
        }

        context.trim_end().to_string()
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

    fn write_allowed_path(path: &str) -> bool {
        let normalized = path.trim().trim_start_matches("./").replace('\\', "/");
        let lower = normalized.to_lowercase();

        // MEMORY.md (workspace root) is intentionally allowed: compaction's
        // job includes updating the conversational throughline.
        if lower == "memory.md" {
            return true;
        }

        // All other compaction writes must live under memory/. This keeps the
        // protected workspace-root files (SOUL.md, USER.md, AGENTS.md, etc.)
        // out of compaction's reach.
        let Some(rest) = normalized.strip_prefix("memory/") else {
            return false;
        };
        if rest.is_empty() {
            return false;
        }

        // Block dreaming-generated artifacts so compaction doesn't stomp on
        // them. The filter applies to the path inside memory/.
        let rest_lower = rest.to_lowercase();
        !(rest_lower == "dreams.md"
            || rest_lower == "dreams"
            || rest_lower == "dreams/"
            || rest_lower.starts_with(".dreams/")
            || rest_lower.starts_with("dreaming/"))
    }

    fn filter_file_ops(file_ops: Vec<MemoryFileOp>) -> Vec<MemoryFileOp> {
        file_ops
            .into_iter()
            .filter(|op| {
                let allowed = Self::write_allowed_path(&op.path);
                if !allowed {
                    warn!(
                        path = %op.path,
                        "compaction: refusing to write generated memory/index path"
                    );
                }
                allowed
            })
            .collect()
    }

    fn is_memory_index_path(path: &str) -> bool {
        crate::memory::deferred_edits::normalize_prompt_visible_path(path).as_deref()
            == Some(crate::memory::deferred_edits::MEMORY_INDEX_DEFERRED_PATH)
    }

    fn workspace_dir_from_store(store: &MarkdownMemoryStore) -> Result<PathBuf, CompactionError> {
        store
            .base_dir()
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                CompactionError::MarkdownStore(format!(
                    "memory store has no workspace parent: {}",
                    store.base_dir().display()
                ))
            })
    }

    fn workspace_memory_index_path(
        store: &MarkdownMemoryStore,
    ) -> Result<PathBuf, CompactionError> {
        let workspace_dir = Self::workspace_dir_from_store(store)?;
        Ok(workspace_dir.join(crate::memory::deferred_edits::MEMORY_INDEX_FILE))
    }

    async fn read_optional_workspace_file(path: &Path) -> Result<Option<String>, CompactionError> {
        match tokio::fs::read_to_string(path).await {
            Ok(content) => Ok(Some(content)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(CompactionError::MarkdownStore(e.to_string())),
        }
    }

    async fn write_workspace_file(path: &Path, content: &str) -> Result<(), CompactionError> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| CompactionError::MarkdownStore(e.to_string()))?;
        }
        tokio::fs::write(path, content)
            .await
            .map_err(|e| CompactionError::MarkdownStore(e.to_string()))
    }

    /// Splits messages into a compacted portion (sent to LLM) and a retained
    /// portion (kept in active.jsonl). The LLM generates markdown memory file
    /// operations from the compacted messages.
    ///
    /// If `dry_run` is true, returns what would be created without side effects.
    #[instrument(skip(self, messages, active_content, system_template, prompt_template, llm, conversation_mgr, markdown_store), fields(char = char_name, user = user_name, msg_count = messages.len(), dry_run))]
    #[allow(clippy::too_many_arguments)]
    pub async fn compact(
        &self,
        conversation_id: &str,
        messages: &[ConversationMessage],
        active_content: &str,
        is_private: bool,
        system_template: &str,
        prompt_template: &str,
        char_name: &str,
        user_name: &str,
        llm: &dyn CompactionLlm,
        conversation_mgr: &dyn ConversationManager,
        markdown_store: Option<&MarkdownMemoryStore>,
        dry_run: bool,
        keep_turns_override: Option<usize>,
        cached_request: Option<shore_llm::types::LlmRequest>,
        data_dir: Option<&std::path::Path>,
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

        if !dry_run && markdown_store.is_none() {
            return Err(CompactionError::MarkdownStore(
                "markdown memory store not available".to_string(),
            ));
        }

        let existing_memory_context = Self::build_existing_memory_context(markdown_store).await;

        // Build system prompt (stable instructions, cacheable). In cache-
        // preserving mode the cached request prefix already contains the
        // conversation, so only the final user instruction needs to be
        // appended; the trailing inline `system` message is wrapped to a
        // user `<system_instruction>` turn by the Anthropic provider so the
        // top-level system parameter stays untouched and the cache prefix
        // remains valid.
        let system = Self::build_system(system_template, char_name, user_name);
        let llm_messages = if cached_request.is_some() {
            let final_msg = Self::build_final_message(
                prompt_template,
                Some(&existing_memory_context),
                char_name,
                user_name,
            );
            vec![serde_json::json!({"role": "user", "content": final_msg})]
        } else {
            Self::build_messages(
                prompt_template,
                compacted_part,
                Some(&existing_memory_context),
                char_name,
                user_name,
            )
        };
        let raw_response = llm.summarize(&system, llm_messages, cached_request).await?;

        // Parse memory file operations from LLM response.
        let raw_file_ops = parse_compaction_response(&raw_response)?;
        let file_ops = Self::filter_file_ops(Self::dedupe_file_ops(raw_file_ops));
        debug!(ops = file_ops.len(), "LLM compaction response parsed");

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
                would_write_files: file_ops.len(),
                file_ops_preview: file_ops,
                message_count: split_at,
                retained_count: messages.len() - split_at,
                retained_turns,
                markdown_preview,
            }));
        }

        // Track created resources for compensating deletes on failure.
        let store = markdown_store.ok_or_else(|| {
            CompactionError::MarkdownStore("markdown memory store not available".to_string())
        })?;

        let mut created: Vec<CompactionWriteState> = Vec::new();
        let mut markdown_elapsed = std::time::Duration::ZERO;
        let mut memory_index_updated = false;

        let workspace_dir = Self::workspace_dir_from_store(store)?;
        let workspace_dir_str = workspace_dir.to_string_lossy().into_owned();

        for op in &file_ops {
            let is_index = Self::is_memory_index_path(&op.path);
            let resolved = if is_index {
                Self::workspace_memory_index_path(store)?
            } else {
                crate::tools::workspace::resolve_path(&workspace_dir_str, &op.path)
                    .map_err(|e| CompactionError::MarkdownStore(e.to_string()))?
            };

            let previous_content = Self::read_optional_workspace_file(&resolved).await?;
            let display_path = if is_index {
                crate::memory::deferred_edits::MEMORY_INDEX_FILE.to_string()
            } else {
                op.path.clone()
            };
            created.push(CompactionWriteState {
                display_path: display_path.clone(),
                target: CompactionWriteTarget::WorkspaceFile {
                    path: resolved.clone(),
                },
                previous_content,
            });

            let md_started = std::time::Instant::now();
            if let Err(e) = Self::write_workspace_file(&resolved, &op.content).await {
                Self::rollback_compaction(&created).await;
                return Err(e);
            }
            if is_index {
                memory_index_updated = true;
                debug!(path = %resolved.display(), elapsed = ?md_started.elapsed(), "compaction: workspace memory index written");
            } else {
                info!(path = %op.path, resolved = %resolved.display(), bytes = op.content.len(), "compaction: wrote memory entry");
                debug!(path = %op.path, elapsed = ?md_started.elapsed(), "compaction: memory entry written");
            }
            markdown_elapsed += md_started.elapsed();
        }

        // Archive compacted messages and retain recent context.
        let retained = messages.len() - split_at;
        let archive_started = std::time::Instant::now();
        let new_conversation_id = match conversation_mgr
            .archive_and_retain(
                conversation_id,
                RetentionParams {
                    keep_last_n: retained,
                    active_content: active_content.to_string(),
                },
            )
            .await
        {
            Ok(id) => id,
            Err(e) => {
                Self::rollback_compaction(&created).await;
                return Err(e);
            }
        };
        debug!(
            retained,
            elapsed = ?archive_started.elapsed(),
            "compaction: archive/retain done"
        );

        let markdown_paths: Vec<String> = created
            .iter()
            .map(|state| state.display_path.clone())
            .collect();
        if memory_index_updated {
            if let Some(data_dir) = data_dir {
                if let Err(e) = crate::memory::deferred_edits::note_memory_index_deferred(
                    &character_data_dir(data_dir, char_name),
                ) {
                    warn!(
                        error = %e,
                        "compaction: failed to queue MEMORY.md prompt refresh"
                    );
                }
            } else {
                warn!("compaction: MEMORY.md updated but data_dir was unavailable for prompt refresh queue");
            }
        }
        let dream_body = format!(
            "Compacted {} messages from `{conversation_id}`.\n\nUpdated memory files:\n{}",
            split_at,
            markdown_paths
                .iter()
                .map(|path| format!("- `{path}`"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let _ = store;
        if let Some(data_dir) = data_dir {
            if let Err(e) = crate::memory::dreams_log::append_dream_entry(
                data_dir,
                char_name,
                chrono::Local::now().fixed_offset(),
                "compaction",
                &dream_body,
            )
            .await
            {
                warn!(error = %e, "compaction: failed to append dreams log entry");
            }
        }

        info!(
            memory_files_written = markdown_paths.len(),
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
            markdown_paths,
        }))
    }

    /// Compensating-delete rollback for a failed compaction.
    ///
    /// Iterates the created list in reverse: restores prior content if the
    /// file existed before compaction, otherwise deletes the file. Errors
    /// during cleanup are logged at WARN level and skipped so rollback
    /// continues regardless of individual failures.
    async fn rollback_compaction(created: &[CompactionWriteState]) {
        use tracing::warn;
        for state in created.iter().rev() {
            let CompactionWriteTarget::WorkspaceFile { path } = &state.target;
            match &state.previous_content {
                Some(previous) => {
                    if let Err(e) = Self::write_workspace_file(path, previous).await {
                        warn!(path = %path.display(), display = %state.display_path, error = %e, "rollback: failed to restore compaction write");
                    }
                }
                None => match tokio::fs::remove_file(path).await {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        warn!(path = %path.display(), display = %state.display_path, error = %e, "rollback: failed to delete compaction write");
                    }
                },
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
            _system: &str,
            _messages: Vec<serde_json::Value>,
            _cached_request: Option<shore_llm::types::LlmRequest>,
        ) -> Pin<Box<dyn Future<Output = Result<String, CompactionError>> + Send + '_>> {
            let result = Ok(self.response.clone());
            Box::pin(async move { result })
        }
    }

    struct CapturingLlm {
        response: String,
        /// Captures the content of the last user message from the messages array.
        last_user_message: StdMutex<Option<String>>,
        /// Captures the message count passed in for each call.
        last_message_count: StdMutex<Option<usize>>,
        /// Captures whether a cached request was provided.
        cached_request_provided: StdMutex<bool>,
    }

    impl CapturingLlm {
        fn new(response: String) -> Self {
            Self {
                response,
                last_user_message: StdMutex::new(None),
                last_message_count: StdMutex::new(None),
                cached_request_provided: StdMutex::new(false),
            }
        }

        fn prompt(&self) -> Option<String> {
            self.last_user_message.lock().unwrap().clone()
        }

        fn message_count(&self) -> Option<usize> {
            *self.last_message_count.lock().unwrap()
        }

        fn saw_cached_request(&self) -> bool {
            *self.cached_request_provided.lock().unwrap()
        }
    }

    impl CompactionLlm for CapturingLlm {
        fn summarize(
            &self,
            _system: &str,
            messages: Vec<serde_json::Value>,
            cached_request: Option<shore_llm::types::LlmRequest>,
        ) -> Pin<Box<dyn Future<Output = Result<String, CompactionError>> + Send + '_>> {
            let captured = messages
                .iter()
                .rev()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            *self.last_user_message.lock().unwrap() = captured;
            *self.last_message_count.lock().unwrap() = Some(messages.len());
            *self.cached_request_provided.lock().unwrap() = cached_request.is_some();
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
        r#"<memory>
<write path="memory/daily/2026-03-25.md">
# Conversation on 2026-03-25

- User discussed their day
- They mentioned having a busy morning
</write>

<write path="memory/preferences/beverages.md">
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
    fn test_build_prompt_strips_legacy_recap_block() {
        let messages = make_messages(2);
        let template = "Before\n{{#if recap}}RECAP: {{recap}}{{/if}}\nAfter\n{{conversation}}";

        let prompt = CompactionManager::build_prompt(template, &messages, None, "Char", "User");
        assert!(!prompt.contains("RECAP"));
        assert!(!prompt.contains("{{#if recap}}"));
        assert!(!prompt.contains("{{/if}}"));
        assert!(prompt.contains("Before"));
        assert!(prompt.contains("After"));
    }

    #[test]
    fn test_build_prompt_includes_existing_memories() {
        let messages = make_messages(2);
        let template = "Existing:\n{{existing_memories}}\nConversation:\n{{conversation}}";

        let prompt = CompactionManager::build_prompt(
            template,
            &messages,
            Some("<file path=\"people/User.md\">\n# User\n</file>"),
            "Char",
            "User",
        );

        assert!(prompt.contains("people/User.md"));
        assert!(!prompt.contains("{{existing_memories}}"));
    }

    #[tokio::test]
    async fn test_build_existing_memory_context_reads_markdown_files() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memory"))
            .await
            .unwrap();
        store
            .write("people/User.md", "# User\n\n- Likes tea.")
            .await
            .unwrap();
        store.write("DREAMS.md", "# Dreams").await.unwrap();

        let context = CompactionManager::build_existing_memory_context(Some(&store)).await;

        assert!(context.contains("people/User.md"));
        assert!(context.contains("Likes tea"));
        assert!(!context.contains("DREAMS.md"));
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
        let store = MarkdownMemoryStore::open(tmp.path().join("memory"))
            .await
            .unwrap();
        let data_dir = tmp.path().join("data");

        let result = mgr
            .compact(
                "conv-1",
                &messages,
                "",
                false,
                DEFAULT_COMPACT_SYSTEM,
                DEFAULT_COMPACT_PROMPT,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                None,
                None,
                Some(&data_dir),
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
            }
            _ => panic!("Expected Compacted outcome"),
        }

        assert!(store.read("daily/2026-03-25.md").await.is_ok());
        assert!(store.read("preferences/beverages.md").await.is_ok());
        let dreams = crate::memory::dreams_log::read_dreams_log(&data_dir, "TestChar")
            .await
            .unwrap()
            .expect("dreams log should be written by compaction");
        assert!(dreams.contains("Compacted 6 messages"));
    }

    #[tokio::test]
    async fn test_compact_writes_workspace_rooted_paths_without_double_nesting() {
        // Regression: a model that emits <write path="memory/people/foo.md">
        // must produce <workspace>/memory/people/foo.md, NOT
        // <workspace>/memory/memory/people/foo.md.
        let llm = MockLlm {
            response: r#"<memory>
<write path="memory/people/foo.md"># Foo

- Likes tea.
</write>
</memory>"#
                .to_string(),
        };
        let conv_mgr = MockConversationMgr::new("new-conv-rooted");
        let mgr = CompactionManager::new(make_config_with_keep(2));
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memory"))
            .await
            .unwrap();

        let result = mgr
            .compact(
                "conv-rooted",
                &make_messages(10),
                "",
                false,
                DEFAULT_COMPACT_SYSTEM,
                DEFAULT_COMPACT_PROMPT,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let CompactionOutcome::Compacted(result) = result else {
            panic!("Expected Compacted outcome");
        };
        assert!(result
            .memory_files_written
            .iter()
            .any(|p| p == "memory/people/foo.md"));

        // The file must land at <workspace>/memory/people/foo.md.
        assert!(tmp
            .path()
            .join("memory")
            .join("people")
            .join("foo.md")
            .is_file());
        // It must NOT land at the double-nested path that the bug produced.
        assert!(!tmp
            .path()
            .join("memory")
            .join("memory")
            .join("people")
            .join("foo.md")
            .exists());
        // The store-relative read uses the path as it sits inside memory/.
        assert!(store.read("people/foo.md").await.is_ok());
    }

    #[tokio::test]
    async fn test_compact_writes_memory_index_but_refuses_generated_paths() {
        let llm = MockLlm {
            response: r#"<memory>
<write path="MEMORY.md"># Memory Index

## Throughline
- Carry-forward note from compaction.
</write>
<write path="DREAMS.md"># Bad dream diary overwrite</write>
<write path="memory/.dreams/candidates.md">bad staged output</write>
<write path="memory/dreaming/rem/today.md">bad phase report</write>
<write path="SOUL.md"># Bad protected-file overwrite</write>
<write path="workspace/USER.md"># Bad protected-file overwrite</write>
<write path="topics/foo.md"># Bare path with no memory/ prefix</write>
<write path="memory/notes/ok.md"># OK

- Real note
</write>
</memory>"#
                .to_string(),
        };
        let conv_mgr = MockConversationMgr::new("new-conv-filter");
        let mgr = CompactionManager::new(make_config_with_keep(2));
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memory"))
            .await
            .unwrap();
        let data_dir = tmp.path().join("data");

        let result = mgr
            .compact(
                "conv-filter",
                &make_messages(10),
                "",
                false,
                DEFAULT_COMPACT_SYSTEM,
                DEFAULT_COMPACT_PROMPT,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                None,
                None,
                Some(&data_dir),
            )
            .await
            .unwrap();

        let CompactionOutcome::Compacted(result) = result else {
            panic!("Expected Compacted outcome");
        };
        assert!(result.memory_files_written.iter().any(|p| p == "MEMORY.md"));
        assert!(result
            .memory_files_written
            .iter()
            .any(|p| p == "memory/notes/ok.md"));
        // Rejected ops must not appear in the written list.
        for rejected in [
            "DREAMS.md",
            "memory/.dreams/candidates.md",
            "memory/dreaming/rem/today.md",
            "SOUL.md",
            "workspace/USER.md",
            "topics/foo.md",
        ] {
            assert!(
                !result.memory_files_written.iter().any(|p| p == rejected),
                "expected {rejected} to be filtered out of compaction writes"
            );
        }
        let memory = std::fs::read_to_string(tmp.path().join("MEMORY.md")).unwrap();
        assert!(memory.contains("Carry-forward note"));
        assert!(
            store.read("MEMORY.md").await.is_err(),
            "MEMORY.md must live at the workspace root, not workspace/memory"
        );
        let pending =
            crate::memory::deferred_edits::pending_deferred_edit_paths(&data_dir.join("TestChar"))
                .unwrap();
        assert_eq!(
            pending,
            vec![crate::memory::deferred_edits::MEMORY_INDEX_FILE.to_string()]
        );
        assert!(store.read(".dreams/candidates.md").await.is_err());
        assert!(store.read("dreaming/rem/today.md").await.is_err());
        // DREAMS.md must not be created in the workspace memory store; the
        // daemon-controlled dreams log lives in data_dir.
        assert!(store.read("DREAMS.md").await.is_err());
        // Bare/protected paths must not have written to the workspace either.
        let workspace_dir = tmp.path();
        assert!(!workspace_dir.join("SOUL.md").exists());
        assert!(!workspace_dir.join("USER.md").exists());
        assert!(!workspace_dir.join("topics/foo.md").exists());
        // The accepted memory-rooted note lands at workspace/memory/notes/ok.md.
        assert!(store.read("notes/ok.md").await.is_ok());
    }

    #[tokio::test]
    async fn test_compact_prompt_includes_existing_markdown_context() {
        let llm = CapturingLlm::new(make_xml_response());
        let conv_mgr = MockConversationMgr::new("new-conv-context");
        let mgr = CompactionManager::new(make_config_with_keep(2));
        let messages = make_messages(10);
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memory"))
            .await
            .unwrap();
        store
            .write(
                "people/TestUser.md",
                "# TestUser\n\n- Already likes green tea.",
            )
            .await
            .unwrap();

        mgr.compact(
            "conv-context",
            &messages,
            "",
            false,
            DEFAULT_COMPACT_SYSTEM,
            DEFAULT_COMPACT_PROMPT,
            "TestChar",
            "TestUser",
            &llm,
            &conv_mgr,
            Some(&store),
            false,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        let prompt = llm.prompt().expect("prompt captured");
        assert!(prompt.contains("people/TestUser.md"));
        assert!(prompt.contains("Already likes green tea"));
        assert!(!prompt.contains("{{existing_memories}}"));
    }

    #[tokio::test]
    async fn test_compact_cache_preserving_passes_only_final_message() {
        // When a cached request is provided, the conversation slice is already
        // part of the cached prefix; only the final user instruction should be
        // appended in the messages array passed to summarize().
        let llm = CapturingLlm::new(make_xml_response());
        let conv_mgr = MockConversationMgr::new("new-conv-cached");
        let mgr = CompactionManager::new(make_config_with_keep(2));
        let messages = make_messages(10);
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memory"))
            .await
            .unwrap();

        let cached = shore_llm::types::LlmRequest {
            sdk: shore_config::models::Sdk::Anthropic,
            model: "test-model".into(),
            api_key: "k".into(),
            base_url: None,
            messages: vec![
                serde_json::json!({"role": "user", "content": "hi"}),
                serde_json::json!({"role": "assistant", "content": "hello"}),
            ],
            system: Some(serde_json::json!("sys")),
            tools: None,
            max_tokens: 1024,
            temperature: None,
            top_p: None,
            provider_options: None,
            provider_key: None,
            rid: None,
            forensic_character: None,
            system_suffix: None,
        };

        mgr.compact(
            "conv-cached",
            &messages,
            "",
            false,
            DEFAULT_COMPACT_SYSTEM,
            DEFAULT_COMPACT_PROMPT,
            "TestChar",
            "TestUser",
            &llm,
            &conv_mgr,
            Some(&store),
            false,
            None,
            Some(cached),
            None,
        )
        .await
        .unwrap();

        assert!(
            llm.saw_cached_request(),
            "cached request must reach summarize()"
        );
        assert_eq!(
            llm.message_count(),
            Some(1),
            "cache-preserving mode must pass only the final user message"
        );
        let prompt = llm.prompt().expect("final user message captured");
        assert!(prompt.contains("Existing memory files:"));
    }

    #[tokio::test]
    async fn test_compact_archives_with_retention() {
        let llm = MockLlm {
            response: make_xml_response(),
        };
        let conv_mgr = MockConversationMgr::new("new-conv-2");
        let mgr = CompactionManager::new(make_config_with_keep(3));
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memory"))
            .await
            .unwrap();

        let result = mgr
            .compact(
                "old-conv",
                &make_messages(10),
                "",
                false,
                DEFAULT_COMPACT_SYSTEM,
                DEFAULT_COMPACT_PROMPT,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                None,
                None,
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
        let store = MarkdownMemoryStore::open(tmp.path().join("memory"))
            .await
            .unwrap();

        let result = mgr
            .compact(
                "conv-1",
                &make_messages(10),
                "",
                false,
                DEFAULT_COMPACT_SYSTEM,
                DEFAULT_COMPACT_PROMPT,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                Some(0),
                None,
                None,
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
        let store = MarkdownMemoryStore::open(tmp.path().join("memory"))
            .await
            .unwrap();

        let result = mgr
            .compact(
                "conv-1",
                &make_messages(10),
                "",
                false,
                DEFAULT_COMPACT_SYSTEM,
                DEFAULT_COMPACT_PROMPT,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                Some(3),
                None,
                None,
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
        let store = MarkdownMemoryStore::open(tmp.path().join("memory"))
            .await
            .unwrap();

        let compaction = tokio::spawn(async move {
            mgr.compact(
                "conv-1",
                &make_messages(10),
                "",
                false,
                DEFAULT_COMPACT_SYSTEM,
                DEFAULT_COMPACT_PROMPT,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                None,
                None,
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
        let store = MarkdownMemoryStore::open(tmp.path().join("memory"))
            .await
            .unwrap();

        let result = mgr
            .compact(
                "private-conv",
                &make_messages(10),
                "",
                true,
                DEFAULT_COMPACT_SYSTEM,
                DEFAULT_COMPACT_PROMPT,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                None,
                None,
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
        let store = MarkdownMemoryStore::open(tmp.path().join("memory"))
            .await
            .unwrap();

        let result = mgr
            .compact(
                "conv-1",
                &make_messages(10),
                "",
                false,
                DEFAULT_COMPACT_SYSTEM,
                DEFAULT_COMPACT_PROMPT,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                true,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        match result {
            CompactionOutcome::DryRun(r) => {
                assert_eq!(r.would_write_files, 2);
                assert_eq!(r.message_count, 6);
                assert_eq!(r.retained_count, 4);
                assert_eq!(r.file_ops_preview.len(), 2);
                assert!(
                    r.file_ops_preview
                        .iter()
                        .all(|op| op.path.starts_with("memory/")),
                    "dry run preview paths should reflect the workspace-rooted path scheme"
                );
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
        let store = MarkdownMemoryStore::open(tmp.path().join("memory"))
            .await
            .unwrap();

        let result = mgr
            .compact(
                "conv-1",
                &[],
                "",
                false,
                DEFAULT_COMPACT_SYSTEM,
                DEFAULT_COMPACT_PROMPT,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                None,
                None,
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
        let store = MarkdownMemoryStore::open(tmp.path().join("memory"))
            .await
            .unwrap();

        let result = mgr
            .compact(
                "conv-1",
                &make_messages(5),
                "",
                false,
                DEFAULT_COMPACT_SYSTEM,
                DEFAULT_COMPACT_PROMPT,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                None,
                None,
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
        let store = MarkdownMemoryStore::open(tmp.path().join("memory"))
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
                DEFAULT_COMPACT_SYSTEM,
                DEFAULT_COMPACT_PROMPT,
                "TestChar",
                "TestUser",
                &llm,
                &conv_mgr,
                Some(&store),
                false,
                None,
                None,
                None,
            )
            .await;

        assert!(matches!(
            result,
            Err(CompactionError::ConversationManager(_))
        ));

        let restored = store.read("preferences/beverages.md").await.unwrap();
        assert_eq!(restored.content, original);
    }
}
