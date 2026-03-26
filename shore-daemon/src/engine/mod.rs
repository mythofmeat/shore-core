pub mod messages;
pub mod prompt;
pub mod segments;
pub mod tools;

use std::path::PathBuf;

use messages::MessageStore;
use segments::SegmentReader;
use shore_protocol::server_msg::{History, ServerMessage};
use shore_protocol::types::Message;
use tokio::sync::broadcast;

/// Errors originating from the conversation engine.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to parse {path}: {source}")]
    JsonParse {
        path: PathBuf,
        source: serde_json::Error,
    },

    #[error("failed to serialize {context}: {source}")]
    JsonSerialize {
        context: String,
        source: serde_json::Error,
    },

    #[error("message not found: {0}")]
    MessageNotFound(String),

    #[error("character not found: {0}")]
    CharacterNotFound(String),
}

/// Per-character conversation engine.
///
/// Manages a single continuous conversation per character. Messages in
/// `active.jsonl` form the current context window; frozen history lives
/// in numbered segment files managed by `SegmentReader`.
///
/// State changes are broadcast as `History` messages to all connected clients.
pub struct ConversationEngine {
    character_name: String,
    character_dir: PathBuf,
    messages: MessageStore,
    segments: SegmentReader,
    push_tx: broadcast::Sender<ServerMessage>,
    /// Accumulates pre-tool text fragments that will be prepended to the
    /// final response content.
    pre_tool_buffer: String,
}

impl ConversationEngine {
    /// Create a new engine for the given character.
    ///
    /// `data_dir` is `$XDG_DATA_HOME/shore` (the engine derives the per-character
    /// path from `character_name`).
    pub fn new(
        character_name: String,
        data_dir: PathBuf,
        push_tx: broadcast::Sender<ServerMessage>,
    ) -> Result<Self, EngineError> {
        let character_dir = data_dir.join(&character_name);
        let messages = MessageStore::load(character_dir.join("active.jsonl"))?;
        let segments = SegmentReader::load(&character_dir)?;

        Ok(Self {
            character_name,
            character_dir,
            messages,
            segments,
            push_tx,
            pre_tool_buffer: String::new(),
        })
    }

    /// The character this engine manages.
    pub fn character_name(&self) -> &str {
        &self.character_name
    }

    /// The character data directory.
    pub fn character_dir(&self) -> &PathBuf {
        &self.character_dir
    }

    // ── Message access ───────────────────────────────────────────────────

    /// Get the active conversation messages (context window).
    pub fn messages(&self) -> &[Message] {
        self.messages.messages()
    }

    /// Number of messages in the active context window.
    pub fn message_count(&self) -> usize {
        self.messages.message_count()
    }

    /// Access the segment reader for historical messages.
    pub fn segments(&self) -> &SegmentReader {
        &self.segments
    }

    // ── Message CRUD ────────────────────────────────────────────────────

    /// Append a message to the active conversation.
    pub fn append_message(&mut self, msg: Message) -> Result<(), EngineError> {
        self.messages.append(msg)?;
        self.broadcast_history();
        Ok(())
    }

    /// Edit a message in the active conversation.
    pub fn edit_message(&mut self, msg_id: &str, new_content: &str) -> Result<(), EngineError> {
        self.messages.edit(msg_id, new_content)?;
        self.broadcast_history();
        Ok(())
    }

    /// Delete a message from the active conversation.
    pub fn delete_message(&mut self, msg_id: &str) -> Result<(), EngineError> {
        self.messages.delete(msg_id)?;
        self.broadcast_history();
        Ok(())
    }

    /// Set swipe state on a message.
    pub fn set_swipe(
        &mut self,
        msg_id: &str,
        index: u32,
        count: u32,
    ) -> Result<(), EngineError> {
        self.messages.set_swipe(msg_id, index, count)?;
        self.broadcast_history();
        Ok(())
    }

    /// Add a swipe candidate to a message.
    pub fn add_swipe_candidate(&mut self, msg_id: &str) -> Result<u32, EngineError> {
        let count = self.messages.add_swipe_candidate(msg_id)?;
        self.broadcast_history();
        Ok(count)
    }

    /// Clear all messages from the active conversation and broadcast.
    pub fn reset(&mut self) -> Result<(), EngineError> {
        self.messages.clear()?;
        self.clear_pre_tool_buffer();
        self.broadcast_history();
        Ok(())
    }

    // ── Pre-tool text accumulation ──────────────────────────────────────

    /// Accumulate pre-tool text. Called when partial text arrives before a
    /// tool invocation.
    pub fn accumulate_pre_tool_text(&mut self, text: &str) {
        self.pre_tool_buffer.push_str(text);
    }

    /// Take the accumulated pre-tool text and prepend it to the given
    /// final response content. Clears the buffer.
    pub fn finalize_response(&mut self, final_content: &str) -> String {
        if self.pre_tool_buffer.is_empty() {
            return final_content.to_string();
        }
        let mut result = std::mem::take(&mut self.pre_tool_buffer);
        result.push_str(final_content);
        result
    }

    /// Clear the pre-tool buffer without using it (e.g., on error/reset).
    pub fn clear_pre_tool_buffer(&mut self) {
        self.pre_tool_buffer.clear();
    }

    // ── Internal ────────────────────────────────────────────────────────

    /// Broadcast the current History snapshot to all connected clients.
    pub fn broadcast_history(&self) {
        let history = ServerMessage::History(History {
            messages: self.messages.messages().to_vec(),
            config: serde_json::json!({}),
        });

        // Ignore send errors — means no receivers are listening.
        let _ = self.push_tx.send(history);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shore_protocol::types::Role;
    use tempfile::TempDir;

    fn make_engine(tmp: &TempDir) -> (ConversationEngine, broadcast::Receiver<ServerMessage>) {
        let (push_tx, push_rx) = broadcast::channel(16);
        let engine = ConversationEngine::new(
            "TestChar".to_string(),
            tmp.path().to_path_buf(),
            push_tx,
        )
        .unwrap();
        (engine, push_rx)
    }

    fn make_msg(id: &str, role: Role, content: &str) -> Message {
        Message {
            msg_id: id.to_string(),
            role,
            content: content.to_string(),
            images: vec![],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn engine_starts_with_empty_messages() {
        let tmp = TempDir::new().unwrap();
        let (engine, _rx) = make_engine(&tmp);
        assert!(engine.messages().is_empty());
        assert_eq!(engine.message_count(), 0);
    }

    #[test]
    fn append_and_read_messages() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, _rx) = make_engine(&tmp);

        engine
            .append_message(make_msg("m1", Role::User, "Hello"))
            .unwrap();
        assert_eq!(engine.messages().len(), 1);
        assert_eq!(engine.messages()[0].content, "Hello");
        assert_eq!(engine.message_count(), 1);
    }

    #[test]
    fn state_changes_broadcast_history() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut rx) = make_engine(&tmp);

        // append_message broadcasts.
        engine
            .append_message(make_msg("m1", Role::User, "Hi"))
            .unwrap();
        let msg = rx.try_recv().unwrap();
        match msg {
            ServerMessage::History(h) => {
                assert_eq!(h.messages.len(), 1);
                assert_eq!(h.messages[0].content, "Hi");
            }
            other => panic!("Expected History, got {:?}", other),
        }

        // edit_message broadcasts.
        engine.edit_message("m1", "Hello").unwrap();
        let msg = rx.try_recv().unwrap();
        match msg {
            ServerMessage::History(h) => {
                assert_eq!(h.messages[0].content, "Hello");
            }
            other => panic!("Expected History, got {:?}", other),
        }

        // delete_message broadcasts.
        engine.delete_message("m1").unwrap();
        let msg = rx.try_recv().unwrap();
        match msg {
            ServerMessage::History(h) => assert!(h.messages.is_empty()),
            other => panic!("Expected History, got {:?}", other),
        }
    }

    #[test]
    fn reset_clears_messages_and_broadcasts() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut rx) = make_engine(&tmp);

        engine
            .append_message(make_msg("m1", Role::User, "Hello"))
            .unwrap();
        let _ = rx.try_recv(); // drain append broadcast

        engine.reset().unwrap();
        assert!(engine.messages().is_empty());

        let msg = rx.try_recv().unwrap();
        match msg {
            ServerMessage::History(h) => assert!(h.messages.is_empty()),
            other => panic!("Expected History, got {:?}", other),
        }
    }

    #[test]
    fn pre_tool_text_accumulation() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, _rx) = make_engine(&tmp);

        // No accumulated text — returns final content as-is.
        assert_eq!(engine.finalize_response("Final"), "Final");

        // Accumulate some pre-tool text.
        engine.accumulate_pre_tool_text("Before tool. ");
        engine.accumulate_pre_tool_text("More text. ");
        let result = engine.finalize_response("After tool.");
        assert_eq!(result, "Before tool. More text. After tool.");

        // Buffer is cleared after finalize.
        assert_eq!(engine.finalize_response("Clean"), "Clean");
    }

    #[test]
    fn clear_pre_tool_buffer() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, _rx) = make_engine(&tmp);

        engine.accumulate_pre_tool_text("Should be cleared");
        engine.clear_pre_tool_buffer();
        assert_eq!(engine.finalize_response("Final"), "Final");
    }

    #[test]
    fn data_persisted_to_active_jsonl() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, _rx) = make_engine(&tmp);

        engine
            .append_message(make_msg("m1", Role::User, "Hello"))
            .unwrap();

        // Verify active.jsonl exists at the expected path.
        let active_path = tmp.path().join("TestChar").join("active.jsonl");
        assert!(active_path.exists());
    }

    #[test]
    fn engine_reloads_messages() {
        let tmp = TempDir::new().unwrap();

        // First engine instance — add messages.
        {
            let (push_tx, _) = broadcast::channel(16);
            let mut engine = ConversationEngine::new(
                "ReloadChar".to_string(),
                tmp.path().to_path_buf(),
                push_tx,
            )
            .unwrap();
            engine
                .append_message(make_msg("m1", Role::User, "Persisted"))
                .unwrap();
        }

        // Second engine instance — should reload.
        let (push_tx, _) = broadcast::channel(16);
        let engine = ConversationEngine::new(
            "ReloadChar".to_string(),
            tmp.path().to_path_buf(),
            push_tx,
        )
        .unwrap();

        assert_eq!(engine.messages().len(), 1);
        assert_eq!(engine.messages()[0].content, "Persisted");
    }
}
