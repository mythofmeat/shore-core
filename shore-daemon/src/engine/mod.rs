pub mod conversations;
pub mod messages;

use std::path::PathBuf;

use conversations::ConversationManager;
use messages::MessageStore;
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

    #[error("conversation not found: {0}")]
    ConversationNotFound(String),

    #[error("no active conversation")]
    NoActiveConversation,
}

/// Per-character conversation engine.
///
/// Manages the conversation lifecycle (create, switch, list) and message
/// CRUD for the currently active conversation. State changes are broadcast
/// as `History` messages to all connected clients.
pub struct ConversationEngine {
    character_name: String,
    conversations: ConversationManager,
    current_messages: Option<MessageStore>,
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
        let conversations_dir = data_dir
            .join(&character_name)
            .join("conversations");
        let conversations = ConversationManager::load(conversations_dir)?;

        // If there's an active conversation, load its messages.
        let current_messages = if let Some(id) = conversations.active_id() {
            let path = conversations.conversation_path(id);
            Some(MessageStore::load(path)?)
        } else {
            None
        };

        Ok(Self {
            character_name,
            conversations,
            current_messages,
            push_tx,
            pre_tool_buffer: String::new(),
        })
    }

    /// The character this engine manages.
    pub fn character_name(&self) -> &str {
        &self.character_name
    }

    // ── Conversation lifecycle ──────────────────────────────────────────

    /// Create a new conversation and switch to it.
    pub fn new_conversation(&mut self, title: &str) -> Result<String, EngineError> {
        let id = self.conversations.new_conversation(title)?;
        let path = self.conversations.conversation_path(&id);
        self.current_messages = Some(MessageStore::new(path));
        self.broadcast_history();
        Ok(id)
    }

    /// Switch to an existing conversation by ID.
    pub fn switch_conversation(&mut self, conv_id: &str) -> Result<(), EngineError> {
        self.conversations.switch(conv_id)?;
        let path = self.conversations.conversation_path(conv_id);
        self.current_messages = Some(MessageStore::load(path)?);
        self.broadcast_history();
        Ok(())
    }

    /// List all conversations.
    pub fn list_conversations(&self) -> &[shore_protocol::types::ConversationInfo] {
        self.conversations.list()
    }

    /// Set the private flag on a conversation.
    pub fn set_private(&mut self, conv_id: &str, private: bool) -> Result<(), EngineError> {
        self.conversations.set_private(conv_id, private)
    }

    /// The active conversation ID.
    pub fn active_conversation_id(&self) -> Option<&str> {
        self.conversations.active_id()
    }

    // ── Message CRUD ────────────────────────────────────────────────────

    /// Get the current conversation's messages.
    pub fn messages(&self) -> Result<&[Message], EngineError> {
        self.current_messages
            .as_ref()
            .map(|s| s.messages())
            .ok_or(EngineError::NoActiveConversation)
    }

    /// Append a message to the current conversation.
    pub fn append_message(&mut self, msg: Message) -> Result<(), EngineError> {
        let store = self
            .current_messages
            .as_mut()
            .ok_or(EngineError::NoActiveConversation)?;
        store.append(msg)?;
        self.broadcast_history();
        Ok(())
    }

    /// Edit a message in the current conversation.
    pub fn edit_message(&mut self, msg_id: &str, new_content: &str) -> Result<(), EngineError> {
        let store = self
            .current_messages
            .as_mut()
            .ok_or(EngineError::NoActiveConversation)?;
        store.edit(msg_id, new_content)?;
        self.broadcast_history();
        Ok(())
    }

    /// Delete a message from the current conversation.
    pub fn delete_message(&mut self, msg_id: &str) -> Result<(), EngineError> {
        let store = self
            .current_messages
            .as_mut()
            .ok_or(EngineError::NoActiveConversation)?;
        store.delete(msg_id)?;
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
        let store = self
            .current_messages
            .as_mut()
            .ok_or(EngineError::NoActiveConversation)?;
        store.set_swipe(msg_id, index, count)?;
        self.broadcast_history();
        Ok(())
    }

    /// Add a swipe candidate to a message.
    pub fn add_swipe_candidate(&mut self, msg_id: &str) -> Result<u32, EngineError> {
        let store = self
            .current_messages
            .as_mut()
            .ok_or(EngineError::NoActiveConversation)?;
        let count = store.add_swipe_candidate(msg_id)?;
        self.broadcast_history();
        Ok(count)
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
        let messages = self
            .current_messages
            .as_ref()
            .map(|s| s.messages().to_vec())
            .unwrap_or_default();

        let history = ServerMessage::History(History {
            messages,
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
    fn new_conversation_and_append() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, _rx) = make_engine(&tmp);

        let id = engine.new_conversation("Test chat").unwrap();
        assert_eq!(engine.active_conversation_id(), Some(id.as_str()));

        engine
            .append_message(make_msg("m1", Role::User, "Hello"))
            .unwrap();
        assert_eq!(engine.messages().unwrap().len(), 1);
    }

    #[test]
    fn switch_conversation_loads_messages() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, _rx) = make_engine(&tmp);

        let id1 = engine.new_conversation("Chat 1").unwrap();
        engine
            .append_message(make_msg("m1", Role::User, "In chat 1"))
            .unwrap();

        let id2 = engine.new_conversation("Chat 2").unwrap();
        engine
            .append_message(make_msg("m2", Role::User, "In chat 2"))
            .unwrap();

        // Switch back to chat 1.
        engine.switch_conversation(&id1).unwrap();
        assert_eq!(engine.messages().unwrap().len(), 1);
        assert_eq!(engine.messages().unwrap()[0].content, "In chat 1");

        // Switch to chat 2.
        engine.switch_conversation(&id2).unwrap();
        assert_eq!(engine.messages().unwrap().len(), 1);
        assert_eq!(engine.messages().unwrap()[0].content, "In chat 2");
    }

    #[test]
    fn no_active_conversation_returns_error() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, _rx) = make_engine(&tmp);

        let result = engine.append_message(make_msg("m1", Role::User, "Hi"));
        assert!(matches!(result, Err(EngineError::NoActiveConversation)));

        let result = engine.messages();
        assert!(matches!(result, Err(EngineError::NoActiveConversation)));
    }

    #[test]
    fn state_changes_broadcast_history() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut rx) = make_engine(&tmp);

        // new_conversation broadcasts.
        engine.new_conversation("Chat").unwrap();
        let msg = rx.try_recv().unwrap();
        match msg {
            ServerMessage::History(h) => assert!(h.messages.is_empty()),
            other => panic!("Expected History, got {:?}", other),
        }

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
    fn conversation_data_persisted_to_correct_path() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, _rx) = make_engine(&tmp);

        let id = engine.new_conversation("Persistent").unwrap();
        engine
            .append_message(make_msg("m1", Role::User, "Hello"))
            .unwrap();

        // Verify files exist at expected paths.
        let conv_dir = tmp.path().join("TestChar").join("conversations");
        assert!(conv_dir.join("manifest.json").exists());
        assert!(conv_dir.join(format!("{id}.jsonl")).exists());
    }

    #[test]
    fn engine_reloads_active_conversation() {
        let tmp = TempDir::new().unwrap();

        // First engine instance — create conversation and add messages.
        {
            let (push_tx, _) = broadcast::channel(16);
            let mut engine = ConversationEngine::new(
                "ReloadChar".to_string(),
                tmp.path().to_path_buf(),
                push_tx,
            )
            .unwrap();
            engine.new_conversation("Reload test").unwrap();
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

        assert_eq!(engine.messages().unwrap().len(), 1);
        assert_eq!(engine.messages().unwrap()[0].content, "Persisted");
    }
}
