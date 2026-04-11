pub(crate) mod atomic;
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
use tracing::{debug, info};

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
        info!(
            character = %character_name,
            dir = %character_dir.display(),
            "initializing conversation engine"
        );
        let messages = MessageStore::load(character_dir.join("active.jsonl"))?;
        let segments = SegmentReader::load(&character_dir)?;
        info!(
            character = %character_name,
            active_messages = messages.message_count(),
            segments = segments.segment_count(),
            total_compacted = segments.total_message_count(),
            "engine loaded"
        );

        Ok(Self {
            character_name,
            character_dir,
            messages,
            segments,
            push_tx,
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

    /// Number of user turns in the active context window.
    pub fn turn_count(&self) -> usize {
        self.messages.turn_count()
    }

    /// Access the segment reader for historical messages.
    pub fn segments(&self) -> &SegmentReader {
        &self.segments
    }

    // ── Message CRUD ────────────────────────────────────────────────────

    /// Append a message to the active conversation and broadcast a `History`
    /// snapshot so all connected clients stay in sync.
    pub fn append_message(&mut self, msg: Message) -> Result<(), EngineError> {
        debug!(character = %self.character_name, msg_id = %msg.msg_id, role = ?msg.role, "appending message");
        self.messages.append(msg)?;
        self.broadcast_history();
        Ok(())
    }

    /// Edit a message in the active conversation.
    pub fn edit_message(&mut self, msg_id: &str, new_content: &str) -> Result<(), EngineError> {
        debug!(character = %self.character_name, msg_id, "editing message");
        self.messages.edit(msg_id, new_content)?;
        self.broadcast_history();
        Ok(())
    }

    /// Delete a message from the active conversation.
    pub fn delete_message(&mut self, msg_id: &str) -> Result<(), EngineError> {
        debug!(character = %self.character_name, msg_id, "deleting message");
        self.messages.delete(msg_id)?;
        self.broadcast_history();
        Ok(())
    }

    /// Remove every message after the last real user turn (for regen).
    pub fn truncate_after_last_user_turn(&mut self) -> Result<usize, EngineError> {
        let removed = self.messages.truncate_after_last_user_turn()?;
        if removed > 0 {
            debug!(character = %self.character_name, removed, "truncated after last user turn");
            self.broadcast_history();
        }
        Ok(removed)
    }

    /// Set swipe state on a message.
    pub fn set_swipe(&mut self, msg_id: &str, index: u32, count: u32) -> Result<(), EngineError> {
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
        info!(character = %self.character_name, "resetting conversation");
        self.messages.clear()?;
        self.broadcast_history();
        Ok(())
    }

    /// Reload messages and segments from disk (e.g. after compaction with retention).
    pub fn reload(&mut self) -> Result<(), EngineError> {
        info!(character = %self.character_name, "reloading engine from disk");
        self.messages = MessageStore::load(self.character_dir.join("active.jsonl"))?;
        self.segments = SegmentReader::load(&self.character_dir)?;
        info!(
            character = %self.character_name,
            active_messages = self.messages.message_count(),
            segments = self.segments.segment_count(),
            "engine reloaded"
        );
        self.broadcast_history();
        Ok(())
    }

    // ── Internal ────────────────────────────────────────────────────────

    /// Broadcast the current History snapshot to all connected clients.
    ///
    /// Tool-loop messages are merged into single assistant turns so clients
    /// receive clean, logical messages rather than raw protocol intermediates.
    pub fn broadcast_history(&self) {
        let merged = shore_protocol::merge::merge_tool_loop_messages(self.messages.messages());
        let history = ServerMessage::History(History {
            messages: merged,
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
        let engine =
            ConversationEngine::new("TestChar".to_string(), tmp.path().to_path_buf(), push_tx)
                .unwrap();
        (engine, push_rx)
    }

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
        let engine =
            ConversationEngine::new("ReloadChar".to_string(), tmp.path().to_path_buf(), push_tx)
                .unwrap();

        assert_eq!(engine.messages().len(), 1);
        assert_eq!(engine.messages()[0].content, "Persisted");
    }
}
