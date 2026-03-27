use shore_protocol::client_msg::{ClientMessage, ClientMessageBody, Command};
use shore_protocol::server_msg::ServerMessage;

/// Routing decision for a Matrix message.
#[derive(Debug, PartialEq)]
pub enum MatrixInput {
    /// Regular text → SWP user message.
    Text(String),
    /// !command [args] → SWP command.
    Command {
        name: String,
        args: serde_json::Value,
    },
    /// Image attachment → SWP message with image path.
    Image {
        path: String,
        caption: Option<String>,
    },
}

/// Parse a Matrix message into a routing decision.
///
/// Messages starting with `!` are treated as commands; everything else
/// is a regular text message forwarded to the daemon.
pub fn parse_matrix_input(text: &str) -> MatrixInput {
    if let Some(rest) = text.strip_prefix('!') {
        let rest = rest.trim();
        let (name, args_text) = match rest.split_once(char::is_whitespace) {
            Some((n, a)) => (n.to_string(), a.trim().to_string()),
            None => (rest.to_string(), String::new()),
        };
        let args = if args_text.is_empty() {
            serde_json::Value::Object(Default::default())
        } else {
            serde_json::json!({ "text": args_text })
        };
        MatrixInput::Command { name, args }
    } else {
        MatrixInput::Text(text.to_string())
    }
}

/// Convert a routing decision to an SWP client message.
pub fn input_to_swp(input: &MatrixInput) -> ClientMessage {
    match input {
        MatrixInput::Text(text) => ClientMessage::Message(ClientMessageBody {
            rid: None,
            text: text.clone(),
            stream: true,
            images: vec![],
            absence_seconds: None,
        }),
        MatrixInput::Command { name, args } => ClientMessage::Command(Command {
            rid: None,
            name: name.clone(),
            args: args.clone(),
        }),
        MatrixInput::Image { path, caption } => ClientMessage::Message(ClientMessageBody {
            rid: None,
            text: caption.clone().unwrap_or_default(),
            stream: true,
            images: vec![path.clone()],
            absence_seconds: None,
        }),
    }
}

/// An image buffered during streaming, to be sent as a Matrix attachment.
pub struct PendingImage {
    pub path: String,
    pub caption: Option<String>,
}

/// Action the bridge should take after processing a daemon message.
pub enum CollectorAction {
    /// Start typing indicator in the active room.
    StartTyping,
    /// Send a text message with optional image attachments.
    SendMessage {
        text: String,
        images: Vec<PendingImage>,
    },
    /// Send a command response.
    SendCommandOutput { name: String, data: String },
    /// Send an error message.
    SendError(String),
    /// Send an autonomous/push message.
    SendPush(String),
    /// No action needed.
    None,
}

/// Collects daemon streaming responses and buffered images.
#[derive(Default)]
pub struct ResponseCollector {
    images: Vec<PendingImage>,
    streaming: bool,
}

impl ResponseCollector {
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(dead_code)]
    pub fn is_streaming(&self) -> bool {
        self.streaming
    }

    /// Feed a server message and return the action to take.
    pub fn feed(&mut self, msg: &ServerMessage) -> CollectorAction {
        match msg {
            ServerMessage::StreamStart(_) => {
                self.streaming = true;
                self.images.clear();
                CollectorAction::StartTyping
            }
            ServerMessage::StreamChunk(_) => {
                // Chunks accumulate server-side; we just maintain the typing indicator.
                CollectorAction::None
            }
            ServerMessage::StreamEnd(end) => {
                self.streaming = false;
                let images = std::mem::take(&mut self.images);
                CollectorAction::SendMessage {
                    text: end.content.clone(),
                    images,
                }
            }
            ServerMessage::SendImage(img) => {
                self.images.push(PendingImage {
                    path: img.path.clone(),
                    caption: img.caption.clone(),
                });
                CollectorAction::None
            }
            ServerMessage::CommandOutput(out) => {
                let data = serde_json::to_string_pretty(&out.data)
                    .unwrap_or_else(|_| format!("{:?}", out.data));
                CollectorAction::SendCommandOutput {
                    name: out.name.clone(),
                    data,
                }
            }
            ServerMessage::Error(err) => {
                CollectorAction::SendError(format!("{:?}: {}", err.code, err.message))
            }
            ServerMessage::NewMessage(new_msg) => {
                CollectorAction::SendPush(new_msg.message.content.clone())
            }
            _ => CollectorAction::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shore_protocol::server_msg::*;
    use shore_protocol::types::*;

    #[test]
    fn parse_text_message() {
        let input = parse_matrix_input("hello world");
        assert_eq!(input, MatrixInput::Text("hello world".to_string()));
    }

    #[test]
    fn parse_command_no_args() {
        let input = parse_matrix_input("!status");
        assert!(matches!(input, MatrixInput::Command { ref name, .. } if name == "status"));
        if let MatrixInput::Command { args, .. } = &input {
            assert!(args.as_object().unwrap().is_empty());
        }
    }

    #[test]
    fn parse_command_with_args() {
        let input = parse_matrix_input("!switch Alice");
        if let MatrixInput::Command { name, args } = &input {
            assert_eq!(name, "switch");
            assert_eq!(args["text"], "Alice");
        } else {
            panic!("expected Command");
        }
    }

    #[test]
    fn parse_command_extra_whitespace() {
        let input = parse_matrix_input("!  switch   Alice  ");
        if let MatrixInput::Command { name, args } = &input {
            assert_eq!(name, "switch");
            assert_eq!(args["text"], "Alice");
        } else {
            panic!("expected Command");
        }
    }

    #[test]
    fn text_to_swp_message() {
        let input = MatrixInput::Text("hi".to_string());
        let msg = input_to_swp(&input);
        if let ClientMessage::Message(body) = msg {
            assert_eq!(body.text, "hi");
            assert!(body.stream);
        } else {
            panic!("expected Message");
        }
    }

    #[test]
    fn command_to_swp_command() {
        let input = MatrixInput::Command {
            name: "status".into(),
            args: serde_json::json!({}),
        };
        let msg = input_to_swp(&input);
        if let ClientMessage::Command(cmd) = msg {
            assert_eq!(cmd.name, "status");
        } else {
            panic!("expected Command");
        }
    }

    #[test]
    fn collector_stream_lifecycle() {
        let mut c = ResponseCollector::new();

        let action = c.feed(&ServerMessage::StreamStart(StreamStart { regen: false }));
        assert!(matches!(action, CollectorAction::StartTyping));
        assert!(c.is_streaming());

        let action = c.feed(&ServerMessage::StreamChunk(StreamChunk {
            text: "hello".into(),
            content_type: "text".into(),
        }));
        assert!(matches!(action, CollectorAction::None));

        let action = c.feed(&ServerMessage::StreamEnd(StreamEnd {
            content: "hello world".into(),
            metadata: StreamMetadata {
                tokens: TokenCounts {
                    input: 10,
                    output: 5,
                    cache_read: 0,
                    cache_write: 0,
                },
                timing: TimingInfo {
                    total_ms: 100,
                    ttft_ms: 50,
                },
                model: "test".into(),
            },
            finish_reason: "end_turn".into(),
        }));
        if let CollectorAction::SendMessage { text, images } = action {
            assert_eq!(text, "hello world");
            assert!(images.is_empty());
        } else {
            panic!("expected SendMessage");
        }
        assert!(!c.is_streaming());
    }

    #[test]
    fn collector_buffers_images() {
        let mut c = ResponseCollector::new();

        c.feed(&ServerMessage::StreamStart(StreamStart { regen: false }));

        c.feed(&ServerMessage::SendImage(SendImage {
            path: "/tmp/img.png".into(),
            caption: Some("test image".into()),
        }));
        c.feed(&ServerMessage::SendImage(SendImage {
            path: "/tmp/img2.png".into(),
            caption: None,
        }));

        let action = c.feed(&ServerMessage::StreamEnd(StreamEnd {
            content: "here are images".into(),
            metadata: StreamMetadata {
                tokens: TokenCounts {
                    input: 10,
                    output: 5,
                    cache_read: 0,
                    cache_write: 0,
                },
                timing: TimingInfo {
                    total_ms: 100,
                    ttft_ms: 50,
                },
                model: "test".into(),
            },
            finish_reason: "end_turn".into(),
        }));

        if let CollectorAction::SendMessage { text, images } = action {
            assert_eq!(text, "here are images");
            assert_eq!(images.len(), 2);
            assert_eq!(images[0].path, "/tmp/img.png");
            assert_eq!(images[0].caption.as_deref(), Some("test image"));
            assert_eq!(images[1].path, "/tmp/img2.png");
            assert!(images[1].caption.is_none());
        } else {
            panic!("expected SendMessage");
        }
    }

    #[test]
    fn collector_command_output() {
        let mut c = ResponseCollector::new();
        let action = c.feed(&ServerMessage::CommandOutput(CommandOutput {
            name: "status".into(),
            data: serde_json::json!({"active": true}),
        }));
        if let CollectorAction::SendCommandOutput { name, data } = action {
            assert_eq!(name, "status");
            assert!(data.contains("active"));
        } else {
            panic!("expected SendCommandOutput");
        }
    }

    #[test]
    fn collector_error() {
        let mut c = ResponseCollector::new();
        let action = c.feed(&ServerMessage::Error(Error {
            code: shore_protocol::error::ErrorCode::NotFound,
            message: "not found".into(),
        }));
        if let CollectorAction::SendError(err) = action {
            assert!(err.contains("NotFound"));
            assert!(err.contains("not found"));
        } else {
            panic!("expected SendError");
        }
    }

    #[test]
    fn collector_new_message() {
        let mut c = ResponseCollector::new();
        let action = c.feed(&ServerMessage::NewMessage(NewMessage {
            message: Message {
                msg_id: "1".into(),
                role: Role::Assistant,
                content: "autonomous hello".into(),
                images: vec![],
                content_blocks: vec![],
                alt_index: None,
                alt_count: None,
                timestamp: "2026-01-01T00:00:00Z".into(),
            },
        }));
        if let CollectorAction::SendPush(text) = action {
            assert_eq!(text, "autonomous hello");
        } else {
            panic!("expected SendPush");
        }
    }

    #[test]
    fn collector_ignores_unrelated_messages() {
        let mut c = ResponseCollector::new();
        let action = c.feed(&ServerMessage::Ping(Ping {}));
        assert!(matches!(action, CollectorAction::None));
    }

    #[test]
    fn image_to_swp_message() {
        let input = MatrixInput::Image {
            path: "/tmp/photo.jpg".into(),
            caption: Some("sunset".into()),
        };
        let msg = input_to_swp(&input);
        if let ClientMessage::Message(body) = msg {
            assert_eq!(body.text, "sunset");
            assert_eq!(body.images, vec!["/tmp/photo.jpg"]);
            assert!(body.stream);
        } else {
            panic!("expected Message");
        }
    }

    #[test]
    fn image_to_swp_no_caption() {
        let input = MatrixInput::Image {
            path: "/tmp/photo.jpg".into(),
            caption: None,
        };
        let msg = input_to_swp(&input);
        if let ClientMessage::Message(body) = msg {
            assert_eq!(body.text, "");
            assert_eq!(body.images, vec!["/tmp/photo.jpg"]);
        } else {
            panic!("expected Message");
        }
    }

    #[test]
    fn collector_images_cleared_on_new_stream() {
        let mut c = ResponseCollector::new();

        // First stream with images
        c.feed(&ServerMessage::StreamStart(StreamStart { regen: false }));
        c.feed(&ServerMessage::SendImage(SendImage {
            path: "/old.png".into(),
            caption: None,
        }));
        c.feed(&ServerMessage::StreamEnd(StreamEnd {
            content: "first".into(),
            metadata: StreamMetadata {
                tokens: TokenCounts {
                    input: 0,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                },
                timing: TimingInfo {
                    total_ms: 0,
                    ttft_ms: 0,
                },
                model: "test".into(),
            },
            finish_reason: "end_turn".into(),
        }));

        // Second stream should start clean
        c.feed(&ServerMessage::StreamStart(StreamStart { regen: false }));
        let action = c.feed(&ServerMessage::StreamEnd(StreamEnd {
            content: "second".into(),
            metadata: StreamMetadata {
                tokens: TokenCounts {
                    input: 0,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                },
                timing: TimingInfo {
                    total_ms: 0,
                    ttft_ms: 0,
                },
                model: "test".into(),
            },
            finish_reason: "end_turn".into(),
        }));

        if let CollectorAction::SendMessage { images, .. } = action {
            assert!(images.is_empty());
        } else {
            panic!("expected SendMessage");
        }
    }
}
