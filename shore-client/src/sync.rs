use shore_protocol::server_msg::ServerMessage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDecision {
    Deliver,
    DropStale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncState {
    latest_revision: u64,
}

impl SyncState {
    pub fn new(initial_revision: u64) -> Self {
        Self {
            latest_revision: initial_revision,
        }
    }

    pub fn latest_revision(&self) -> u64 {
        self.latest_revision
    }

    pub fn observe(&mut self, msg: &ServerMessage) -> SyncDecision {
        match msg {
            ServerMessage::History(history) => {
                if history.revision < self.latest_revision {
                    SyncDecision::DropStale
                } else {
                    self.latest_revision = history.revision;
                    SyncDecision::Deliver
                }
            }
            ServerMessage::NewMessage(message) => {
                if message.revision <= self.latest_revision {
                    SyncDecision::DropStale
                } else {
                    SyncDecision::Deliver
                }
            }
            _ => SyncDecision::Deliver,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shore_protocol::server_msg::{History, NewMessage};
    use shore_protocol::types::{Message, Role};

    fn message(id: &str) -> Message {
        Message {
            msg_id: id.into(),
            role: Role::Assistant,
            content: "hello".into(),
            images: vec![],
            content_blocks: vec![],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn drops_stale_history_snapshots() {
        let mut sync = SyncState::new(5);
        let stale = ServerMessage::History(History {
            messages: vec![message("m1")],
            config: serde_json::json!({}),
            selected_character: Some("alice".into()),
            revision: 4,
        });

        assert_eq!(sync.observe(&stale), SyncDecision::DropStale);
        assert_eq!(sync.latest_revision(), 5);
    }

    #[test]
    fn accepts_newer_history_snapshots() {
        let mut sync = SyncState::new(5);
        let newer = ServerMessage::History(History {
            messages: vec![message("m1")],
            config: serde_json::json!({}),
            selected_character: Some("alice".into()),
            revision: 6,
        });

        assert_eq!(sync.observe(&newer), SyncDecision::Deliver);
        assert_eq!(sync.latest_revision(), 6);
    }

    #[test]
    fn drops_new_message_when_snapshot_already_covers_it() {
        let mut sync = SyncState::new(6);
        let message = ServerMessage::NewMessage(NewMessage {
            revision: 6,
            message: message("m2"),
        });

        assert_eq!(sync.observe(&message), SyncDecision::DropStale);
    }
}
