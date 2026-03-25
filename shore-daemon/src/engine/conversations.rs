use std::path::{Path, PathBuf};

use shore_protocol::types::ConversationInfo;
use tracing::info;

use super::EngineError;

/// Manifest tracking all conversations for a character.
///
/// Persisted at `$XDG_DATA_HOME/shore/{character}/conversations/manifest.json`.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize, Clone)]
pub struct Manifest {
    pub conversations: Vec<ConversationInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_id: Option<String>,
}

/// Manages conversation lifecycle for one character.
///
/// Handles new/switch/list/private operations and persists the manifest.
#[derive(Debug)]
pub struct ConversationManager {
    manifest: Manifest,
    /// Directory: `$XDG_DATA_HOME/shore/{character}/conversations/`
    conversations_dir: PathBuf,
}

impl ConversationManager {
    /// Create a new manager. Loads the manifest from disk if it exists.
    pub fn load(conversations_dir: PathBuf) -> Result<Self, EngineError> {
        let manifest_path = conversations_dir.join("manifest.json");
        let manifest = if manifest_path.exists() {
            let content =
                std::fs::read_to_string(&manifest_path).map_err(|e| EngineError::Io {
                    path: manifest_path.clone(),
                    source: e,
                })?;
            serde_json::from_str(&content).map_err(|e| EngineError::JsonParse {
                path: manifest_path,
                source: e,
            })?
        } else {
            Manifest::default()
        };

        Ok(Self {
            manifest,
            conversations_dir,
        })
    }

    /// The conversations directory path.
    pub fn conversations_dir(&self) -> &Path {
        &self.conversations_dir
    }

    /// The current manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The active conversation ID, if any.
    pub fn active_id(&self) -> Option<&str> {
        self.manifest.active_id.as_deref()
    }

    /// List all conversations.
    pub fn list(&self) -> &[ConversationInfo] {
        &self.manifest.conversations
    }

    /// Create a new conversation. Returns the new conversation's ID.
    pub fn new_conversation(&mut self, title: &str) -> Result<String, EngineError> {
        let id = uuid::Uuid::new_v4().to_string();
        let info = ConversationInfo {
            id: id.clone(),
            title: title.to_string(),
            private: false,
        };
        info!(conv_id = %id, title, "Creating new conversation");
        self.manifest.conversations.push(info);
        self.manifest.active_id = Some(id.clone());
        self.persist()?;
        Ok(id)
    }

    /// Switch to an existing conversation. Returns an error if not found.
    pub fn switch(&mut self, conv_id: &str) -> Result<(), EngineError> {
        if !self.manifest.conversations.iter().any(|c| c.id == conv_id) {
            return Err(EngineError::ConversationNotFound(conv_id.to_string()));
        }
        info!(conv_id, "Switching conversation");
        self.manifest.active_id = Some(conv_id.to_string());
        self.persist()
    }

    /// Toggle the private flag on a conversation.
    pub fn set_private(&mut self, conv_id: &str, private: bool) -> Result<(), EngineError> {
        let conv = self
            .manifest
            .conversations
            .iter_mut()
            .find(|c| c.id == conv_id)
            .ok_or_else(|| EngineError::ConversationNotFound(conv_id.to_string()))?;
        info!(conv_id, private, "Setting private flag");
        conv.private = private;
        self.persist()
    }

    /// Get the JSONL file path for a conversation.
    pub fn conversation_path(&self, conv_id: &str) -> PathBuf {
        self.conversations_dir.join(format!("{conv_id}.jsonl"))
    }

    /// Persist the manifest to disk.
    fn persist(&self) -> Result<(), EngineError> {
        std::fs::create_dir_all(&self.conversations_dir).map_err(|e| EngineError::Io {
            path: self.conversations_dir.clone(),
            source: e,
        })?;
        let manifest_path = self.conversations_dir.join("manifest.json");
        let json = serde_json::to_string_pretty(&self.manifest).map_err(|e| {
            EngineError::JsonSerialize {
                context: "manifest".into(),
                source: e,
            }
        })?;
        std::fs::write(&manifest_path, &json).map_err(|e| EngineError::Io {
            path: manifest_path,
            source: e,
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn new_conversation_and_list() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("conversations");
        let mut mgr = ConversationManager::load(dir.clone()).unwrap();

        assert!(mgr.list().is_empty());

        let id = mgr.new_conversation("Hello world").unwrap();
        assert_eq!(mgr.list().len(), 1);
        assert_eq!(mgr.list()[0].title, "Hello world");
        assert_eq!(mgr.list()[0].id, id);
        assert!(!mgr.list()[0].private);
        assert_eq!(mgr.active_id(), Some(id.as_str()));
    }

    #[test]
    fn switch_conversation() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("conversations");
        let mut mgr = ConversationManager::load(dir).unwrap();

        let id1 = mgr.new_conversation("First").unwrap();
        let id2 = mgr.new_conversation("Second").unwrap();

        assert_eq!(mgr.active_id(), Some(id2.as_str()));

        mgr.switch(&id1).unwrap();
        assert_eq!(mgr.active_id(), Some(id1.as_str()));
    }

    #[test]
    fn switch_nonexistent_returns_error() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("conversations");
        let mut mgr = ConversationManager::load(dir).unwrap();

        let result = mgr.switch("nonexistent");
        assert!(matches!(result, Err(EngineError::ConversationNotFound(_))));
    }

    #[test]
    fn private_flag_toggle() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("conversations");
        let mut mgr = ConversationManager::load(dir.clone()).unwrap();

        let id = mgr.new_conversation("Secret chat").unwrap();
        assert!(!mgr.list()[0].private);

        mgr.set_private(&id, true).unwrap();
        assert!(mgr.list()[0].private);

        mgr.set_private(&id, false).unwrap();
        assert!(!mgr.list()[0].private);

        // Persisted.
        let reloaded = ConversationManager::load(dir).unwrap();
        assert!(!reloaded.list()[0].private);
    }

    #[test]
    fn manifest_persistence_round_trip() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("conversations");
        let mut mgr = ConversationManager::load(dir.clone()).unwrap();

        let id1 = mgr.new_conversation("Chat 1").unwrap();
        let _id2 = mgr.new_conversation("Chat 2").unwrap();
        mgr.set_private(&id1, true).unwrap();

        // Reload from disk.
        let reloaded = ConversationManager::load(dir).unwrap();
        assert_eq!(reloaded.list().len(), 2);
        assert_eq!(reloaded.list()[0].title, "Chat 1");
        assert!(reloaded.list()[0].private);
        assert_eq!(reloaded.list()[1].title, "Chat 2");
        assert!(!reloaded.list()[1].private);
    }

    #[test]
    fn manifest_serialization_includes_private_field() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("conversations");
        let mut mgr = ConversationManager::load(dir.clone()).unwrap();

        let id = mgr.new_conversation("Test").unwrap();
        mgr.set_private(&id, true).unwrap();

        // Read raw JSON to verify structure.
        let manifest_path = dir.join("manifest.json");
        let raw = std::fs::read_to_string(manifest_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();

        let convs = parsed["conversations"].as_array().unwrap();
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0]["private"], serde_json::json!(true));
        assert_eq!(convs[0]["title"], serde_json::json!("Test"));
        assert!(convs[0]["id"].is_string());
    }

    #[test]
    fn conversation_path_format() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("conversations");
        let mgr = ConversationManager::load(dir.clone()).unwrap();

        let path = mgr.conversation_path("abc-123");
        assert_eq!(path, dir.join("abc-123.jsonl"));
    }

    #[test]
    fn load_nonexistent_dir_gives_empty_manifest() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("does_not_exist");
        let mgr = ConversationManager::load(dir).unwrap();

        assert!(mgr.list().is_empty());
        assert!(mgr.active_id().is_none());
    }
}
