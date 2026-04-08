use std::path::{Path, PathBuf};

use shore_protocol::types::Message;
use tracing::info;

use super::EngineError;

/// In-memory message store backed by a JSONL file on disk.
///
/// Each line in the JSONL file is one `Message` serialized as JSON.
/// Mutations (append, edit, delete) are reflected both in-memory and on disk.
#[derive(Debug)]
pub struct MessageStore {
    messages: Vec<Message>,
    path: PathBuf,
}

impl MessageStore {
    /// Create a new empty store that will persist to `path`.
    pub fn new(path: PathBuf) -> Self {
        Self {
            messages: Vec::new(),
            path,
        }
    }

    /// Load messages from an existing JSONL file.
    /// If the file doesn't exist, starts with an empty list.
    pub fn load(path: PathBuf) -> Result<Self, EngineError> {
        let messages = if path.exists() {
            let content = std::fs::read_to_string(&path).map_err(|e| EngineError::Io {
                path: path.clone(),
                source: e,
            })?;
            let mut msgs = Vec::new();
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let mut msg: Message =
                    serde_json::from_str(line).map_err(|e| EngineError::JsonParse {
                        path: path.clone(),
                        source: e,
                    })?;
                msg.normalize();
                msgs.push(msg);
            }
            msgs
        } else {
            Vec::new()
        };

        Ok(Self { messages, path })
    }

    /// All messages in order.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Number of raw messages in the store.
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Number of user turns in the store.
    ///
    /// A turn is a real user message — one that is NOT purely tool_result
    /// content.  Tool exchanges (assistant tool_use + user tool_result) are
    /// part of the same turn as the preceding real user message.
    pub fn turn_count(&self) -> usize {
        use shore_protocol::types::{ContentBlock, Role};
        self.messages
            .iter()
            .filter(|m| {
                if m.role != Role::User {
                    return false;
                }
                // A user message whose content_blocks are ALL ToolResult is a
                // tool-loop message, not a real turn.
                if !m.content_blocks.is_empty()
                    && m.content_blocks
                        .iter()
                        .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
                {
                    return false;
                }
                true
            })
            .count()
    }

    /// Clear all messages and truncate the backing file.
    pub fn clear(&mut self) -> Result<(), EngineError> {
        self.messages.clear();
        self.persist()
    }

    /// The file path this store persists to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append a message and persist.
    pub fn append(&mut self, msg: Message) -> Result<(), EngineError> {
        info!(msg_id = %msg.msg_id, role = ?msg.role, "Appending message");
        self.messages.push(msg);
        self.persist()
    }

    /// Edit the content of a message by `msg_id`. Returns an error if not found.
    ///
    /// Updates both `content` and `content_blocks` to keep them in sync.
    pub fn edit(&mut self, msg_id: &str, new_content: &str) -> Result<(), EngineError> {
        use shore_protocol::types::ContentBlock;
        let msg = self
            .messages
            .iter_mut()
            .find(|m| m.msg_id == msg_id)
            .ok_or_else(|| EngineError::MessageNotFound(msg_id.to_string()))?;
        info!(msg_id, "Editing message");
        msg.content = new_content.to_string();
        msg.content_blocks = vec![ContentBlock::Text {
            text: new_content.to_string(),
        }];
        self.persist()
    }

    /// Remove every message after the last real user turn.
    ///
    /// A "real user turn" is a User message that is NOT purely tool_result
    /// content.  This wipes assistant replies, tool-use exchanges, and
    /// tool-result messages that followed the last genuine user input.
    /// Returns the number of messages removed.
    pub fn truncate_after_last_user_turn(&mut self) -> Result<usize, EngineError> {
        use shore_protocol::types::{ContentBlock, Role};
        let keep = self
            .messages
            .iter()
            .rposition(|m| {
                m.role == Role::User
                    && (m.content_blocks.is_empty()
                        || !m
                            .content_blocks
                            .iter()
                            .all(|b| matches!(b, ContentBlock::ToolResult { .. })))
            })
            .map_or(0, |i| i + 1);
        let removed = self.messages.len() - keep;
        if removed > 0 {
            info!(removed, "Truncating messages after last user turn");
            self.messages.truncate(keep);
            self.persist()?;
        }
        Ok(removed)
    }

    /// Delete a message by `msg_id`. Returns an error if not found.
    pub fn delete(&mut self, msg_id: &str) -> Result<(), EngineError> {
        let idx = self
            .messages
            .iter()
            .position(|m| m.msg_id == msg_id)
            .ok_or_else(|| EngineError::MessageNotFound(msg_id.to_string()))?;
        info!(msg_id, "Deleting message");
        self.messages.remove(idx);
        self.persist()
    }

    /// Set the active swipe alternative for a message.
    /// Updates `alt_index` to `index` and ensures `alt_count >= index + 1`.
    pub fn set_swipe(&mut self, msg_id: &str, index: u32, count: u32) -> Result<(), EngineError> {
        let msg = self
            .messages
            .iter_mut()
            .find(|m| m.msg_id == msg_id)
            .ok_or_else(|| EngineError::MessageNotFound(msg_id.to_string()))?;
        info!(
            msg_id,
            alt_index = index,
            alt_count = count,
            "Setting swipe"
        );
        msg.alt_index = Some(index);
        msg.alt_count = Some(count);
        self.persist()
    }

    /// Increment the swipe alt_count for a message and return the new count.
    pub fn add_swipe_candidate(&mut self, msg_id: &str) -> Result<u32, EngineError> {
        let msg = self
            .messages
            .iter_mut()
            .find(|m| m.msg_id == msg_id)
            .ok_or_else(|| EngineError::MessageNotFound(msg_id.to_string()))?;
        let new_count = msg.alt_count.unwrap_or(1) + 1;
        msg.alt_count = Some(new_count);
        // Point to the newest candidate.
        msg.alt_index = Some(new_count - 1);
        info!(msg_id, alt_count = new_count, "Added swipe candidate");
        self.persist()?;
        Ok(new_count)
    }

    /// Write all messages to the JSONL file (full rewrite).
    ///
    /// Uses `serialize_for_storage()` to omit the derived `content` field.
    fn persist(&self) -> Result<(), EngineError> {
        let mut buf = String::new();
        for msg in &self.messages {
            let line = msg
                .serialize_for_storage()
                .map_err(|e| EngineError::JsonSerialize {
                    context: "message".into(),
                    source: e,
                })?;
            buf.push_str(&line);
            buf.push('\n');
        }
        super::atomic::atomic_write(&self.path, buf.as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shore_protocol::types::Role;
    use tempfile::TempDir;

    fn make_msg(id: &str, role: Role, content: &str) -> Message {
        use shore_protocol::types::ContentBlock;
        Message {
            msg_id: id.to_string(),
            role,
            content: content.to_string(),
            images: vec![],
            content_blocks: if content.is_empty() {
                vec![]
            } else {
                vec![ContentBlock::Text {
                    text: content.to_string(),
                }]
            },
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn append_and_read_back() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("conv.jsonl");
        let mut store = MessageStore::new(path.clone());

        store.append(make_msg("m1", Role::User, "Hello")).unwrap();
        store
            .append(make_msg("m2", Role::Assistant, "Hi there"))
            .unwrap();

        assert_eq!(store.messages().len(), 2);
        assert_eq!(store.messages()[0].content, "Hello");
        assert_eq!(store.messages()[1].content, "Hi there");

        // Reload from disk.
        let reloaded = MessageStore::load(path).unwrap();
        assert_eq!(reloaded.messages().len(), 2);
        assert_eq!(reloaded.messages()[0].msg_id, "m1");
        assert_eq!(reloaded.messages()[1].msg_id, "m2");
    }

    #[test]
    fn edit_message() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("conv.jsonl");
        let mut store = MessageStore::new(path.clone());

        store
            .append(make_msg("m1", Role::User, "Original"))
            .unwrap();
        store.edit("m1", "Edited").unwrap();

        assert_eq!(store.messages()[0].content, "Edited");

        // Persisted.
        let reloaded = MessageStore::load(path).unwrap();
        assert_eq!(reloaded.messages()[0].content, "Edited");
    }

    #[test]
    fn edit_nonexistent_returns_error() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("conv.jsonl");
        let mut store = MessageStore::new(path);

        let result = store.edit("nope", "content");
        assert!(matches!(result, Err(EngineError::MessageNotFound(_))));
    }

    #[test]
    fn delete_message() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("conv.jsonl");
        let mut store = MessageStore::new(path.clone());

        store.append(make_msg("m1", Role::User, "First")).unwrap();
        store
            .append(make_msg("m2", Role::Assistant, "Second"))
            .unwrap();
        store.append(make_msg("m3", Role::User, "Third")).unwrap();

        store.delete("m2").unwrap();

        assert_eq!(store.messages().len(), 2);
        assert_eq!(store.messages()[0].msg_id, "m1");
        assert_eq!(store.messages()[1].msg_id, "m3");

        // Persisted.
        let reloaded = MessageStore::load(path).unwrap();
        assert_eq!(reloaded.messages().len(), 2);
    }

    #[test]
    fn delete_nonexistent_returns_error() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("conv.jsonl");
        let mut store = MessageStore::new(path);

        let result = store.delete("nope");
        assert!(matches!(result, Err(EngineError::MessageNotFound(_))));
    }

    #[test]
    fn swipe_candidate_management() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("conv.jsonl");
        let mut store = MessageStore::new(path.clone());

        store
            .append(make_msg("m1", Role::Assistant, "Response A"))
            .unwrap();

        // Set initial swipe state.
        store.set_swipe("m1", 0, 1).unwrap();
        assert_eq!(store.messages()[0].alt_index, Some(0));
        assert_eq!(store.messages()[0].alt_count, Some(1));

        // Add a swipe candidate.
        let count = store.add_swipe_candidate("m1").unwrap();
        assert_eq!(count, 2);
        assert_eq!(store.messages()[0].alt_index, Some(1));
        assert_eq!(store.messages()[0].alt_count, Some(2));

        // Switch back to first.
        store.set_swipe("m1", 0, 2).unwrap();
        assert_eq!(store.messages()[0].alt_index, Some(0));

        // Persisted.
        let reloaded = MessageStore::load(path).unwrap();
        assert_eq!(reloaded.messages()[0].alt_index, Some(0));
        assert_eq!(reloaded.messages()[0].alt_count, Some(2));
    }

    #[test]
    fn load_nonexistent_file_gives_empty_store() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("does_not_exist.jsonl");

        let store = MessageStore::load(path).unwrap();
        assert!(store.messages().is_empty());
    }

    // ── JSONL resilience tests ���───────────────────────────────────────

    #[test]
    fn load_skips_empty_lines() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("conv.jsonl");

        // Create a store with one message, then manually inject blank lines.
        let mut store = MessageStore::new(path.clone());
        store.append(make_msg("m1", Role::User, "Hello")).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let with_blanks = format!("\n\n{}\n\n", content.trim());
        std::fs::write(&path, with_blanks).unwrap();

        let reloaded = MessageStore::load(path).unwrap();
        assert_eq!(reloaded.messages().len(), 1);
        assert_eq!(reloaded.messages()[0].msg_id, "m1");
    }

    #[test]
    fn load_rejects_invalid_json_line() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("conv.jsonl");

        std::fs::write(&path, "{broken json\n").unwrap();
        let result = MessageStore::load(path);
        assert!(
            matches!(result, Err(EngineError::JsonParse { .. })),
            "should return JsonParse error for malformed JSON"
        );
    }

    #[test]
    fn load_handles_trailing_newline() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("conv.jsonl");

        let mut store = MessageStore::new(path.clone());
        store.append(make_msg("m1", Role::User, "Hello")).unwrap();

        // Append extra trailing newlines (simulating editor artifact).
        let mut content = std::fs::read_to_string(&path).unwrap();
        content.push_str("\n\n\n");
        std::fs::write(&path, content).unwrap();

        let reloaded = MessageStore::load(path).unwrap();
        assert_eq!(reloaded.messages().len(), 1);
    }

    #[test]
    fn load_normalizes_legacy_format() {
        use shore_protocol::types::ContentBlock;

        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("conv.jsonl");

        // Write a legacy message with content but no content_blocks.
        let legacy = serde_json::json!({
            "msg_id": "legacy1",
            "role": "user",
            "content": "old format message",
            "content_blocks": [],
            "images": [],
            "timestamp": "2026-01-01T00:00:00Z"
        });
        std::fs::write(&path, format!("{}\n", legacy)).unwrap();

        let store = MessageStore::load(path).unwrap();
        assert_eq!(store.messages().len(), 1);
        let msg = &store.messages()[0];
        // normalize() should populate content_blocks from content.
        assert!(
            !msg.content_blocks.is_empty(),
            "normalize should populate content_blocks from content"
        );
        assert!(
            matches!(&msg.content_blocks[0], ContentBlock::Text { text } if text == "old format message")
        );
    }

    #[test]
    fn persist_roundtrip_special_characters() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("conv.jsonl");
        let mut store = MessageStore::new(path.clone());

        let special = "Unicode: \u{1F600} \u{00E9}\nNewline in content\t\"Quoted\"\\Backslash";
        store.append(make_msg("m1", Role::User, special)).unwrap();

        let reloaded = MessageStore::load(path).unwrap();
        assert_eq!(reloaded.messages()[0].content, special);
    }

    #[test]
    fn turn_count_excludes_tool_result_messages() {
        use shore_protocol::types::ContentBlock;

        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("conv.jsonl");
        let mut store = MessageStore::new(path);

        // Real user turn.
        store.append(make_msg("m1", Role::User, "Hello")).unwrap();
        // Assistant with tool use.
        store
            .append(make_msg("m2", Role::Assistant, "Let me search"))
            .unwrap();
        // Tool result (NOT a real user turn).
        let mut tool_msg = Message {
            msg_id: "m3".to_string(),
            role: Role::User,
            content: String::new(),
            images: vec![],
            content_blocks: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".to_string(),
                content: "5 results found".to_string(),
                is_error: false,
            }],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        };
        tool_msg.content = "5 results found".to_string();
        store.append(tool_msg).unwrap();
        // Another real user turn.
        store.append(make_msg("m4", Role::User, "Thanks")).unwrap();

        assert_eq!(store.message_count(), 4);
        assert_eq!(
            store.turn_count(),
            2,
            "tool-result message should not count as a turn"
        );
    }

    // ── JSONL corruption / edge-case tests ─────────────────────────────

    #[test]
    fn load_truncated_json_line() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("conv.jsonl");

        // Write one valid message, then a truncated JSON line (simulating power loss).
        let mut store = MessageStore::new(path.clone());
        store.append(make_msg("m1", Role::User, "Hello")).unwrap();

        let mut content = std::fs::read_to_string(&path).unwrap();
        content.push_str(r#"{"msg_id":"m2","role":"user","content":"trunc"#);
        content.push('\n');
        std::fs::write(&path, content).unwrap();

        let result = MessageStore::load(path);
        assert!(
            matches!(result, Err(EngineError::JsonParse { .. })),
            "truncated JSON line should produce JsonParse error"
        );
    }

    #[test]
    fn load_skips_whitespace_only_lines() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("conv.jsonl");

        let mut store = MessageStore::new(path.clone());
        store.append(make_msg("m1", Role::User, "Hello")).unwrap();

        let valid_line = std::fs::read_to_string(&path).unwrap();
        let with_ws = format!("   \t  \n{}\n  \t\n", valid_line.trim());
        std::fs::write(&path, with_ws).unwrap();

        let reloaded = MessageStore::load(path).unwrap();
        assert_eq!(reloaded.messages().len(), 1);
        assert_eq!(reloaded.messages()[0].msg_id, "m1");
    }

    #[test]
    fn load_large_file_1000_messages() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("conv.jsonl");
        let mut store = MessageStore::new(path.clone());

        for i in 0..1000 {
            let role = if i % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            };
            store
                .append(make_msg(&format!("m{i}"), role, &format!("Message {i}")))
                .unwrap();
        }

        assert_eq!(store.messages().len(), 1000);
        assert_eq!(store.turn_count(), 500);

        let reloaded = MessageStore::load(path).unwrap();
        assert_eq!(reloaded.messages().len(), 1000);
        assert_eq!(reloaded.messages()[0].msg_id, "m0");
        assert_eq!(reloaded.messages()[999].msg_id, "m999");
    }
}
