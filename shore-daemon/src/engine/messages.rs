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
                let msg: Message = serde_json::from_str(line).map_err(|e| {
                    EngineError::JsonParse {
                        path: path.clone(),
                        source: e,
                    }
                })?;
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

    /// Number of messages in the store.
    pub fn message_count(&self) -> usize {
        self.messages.len()
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
    pub fn edit(&mut self, msg_id: &str, new_content: &str) -> Result<(), EngineError> {
        let msg = self
            .messages
            .iter_mut()
            .find(|m| m.msg_id == msg_id)
            .ok_or_else(|| EngineError::MessageNotFound(msg_id.to_string()))?;
        info!(msg_id, "Editing message");
        msg.content = new_content.to_string();
        self.persist()
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
        info!(msg_id, alt_index = index, alt_count = count, "Setting swipe");
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
    fn persist(&self) -> Result<(), EngineError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| EngineError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        let mut buf = String::new();
        for msg in &self.messages {
            let line = serde_json::to_string(msg).map_err(|e| EngineError::JsonSerialize {
                context: "message".into(),
                source: e,
            })?;
            buf.push_str(&line);
            buf.push('\n');
        }
        std::fs::write(&self.path, &buf).map_err(|e| EngineError::Io {
            path: self.path.clone(),
            source: e,
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shore_protocol::types::Role;
    use tempfile::TempDir;

    fn make_msg(id: &str, role: Role, content: &str) -> Message {
        Message {
            msg_id: id.to_string(),
            role,
            content: content.to_string(),
            images: vec![],
            content_blocks: vec![],
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

        store
            .append(make_msg("m1", Role::User, "Hello"))
            .unwrap();
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

        store
            .append(make_msg("m1", Role::User, "First"))
            .unwrap();
        store
            .append(make_msg("m2", Role::Assistant, "Second"))
            .unwrap();
        store
            .append(make_msg("m3", Role::User, "Third"))
            .unwrap();

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
}
