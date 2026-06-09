use shore_protocol::server_msg::ServerMessage;

#[derive(Debug, Clone)]
pub struct CollectedToolCall {
    pub name: String,
    pub id: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Default)]
pub struct CollectedResponse {
    pub text: String,
    pub tool_calls: Vec<CollectedToolCall>,
    pub raw_messages: Vec<ServerMessage>,
    pub stream_ended: bool,
}

impl CollectedResponse {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a message into the collector. Returns true if the stream is complete.
    pub fn push(&mut self, msg: ServerMessage) -> bool {
        match &msg {
            ServerMessage::StreamChunk(chunk) => {
                self.text.push_str(&chunk.text);
                self.raw_messages.push(msg);
                false
            }
            ServerMessage::ToolCall(tc) => {
                self.tool_calls.push(CollectedToolCall {
                    name: tc.tool_name.clone(),
                    id: tc.tool_id.clone(),
                    input: tc.input.clone(),
                });
                self.raw_messages.push(msg);
                false
            }
            ServerMessage::StreamEnd(_) => {
                self.stream_ended = true;
                self.raw_messages.push(msg);
                true
            }
            ServerMessage::Error(_) => {
                self.raw_messages.push(msg);
                true
            }
            ServerMessage::Hello(_)
            | ServerMessage::History(_)
            | ServerMessage::Shutdown(_)
            | ServerMessage::Ping(_)
            | ServerMessage::CommandOutput(_)
            | ServerMessage::StreamStart(_)
            | ServerMessage::Phase(_)
            | ServerMessage::NewMessage(_)
            | ServerMessage::ToolResult(_)
            | ServerMessage::SendImage(_)
            | ServerMessage::CacheWarning(_)
            | ServerMessage::ProviderFallbackWarning(_)
            | ServerMessage::UsageWarning(_)
            | ServerMessage::Unknown => {
                self.raw_messages.push(msg);
                false
            }
        }
    }

    pub fn assert_text_contains(&self, expected: &str) {
        assert!(
            self.text.contains(expected),
            "Expected response text to contain {:?}, but got: {:?}",
            expected,
            self.text,
        );
    }

    pub fn assert_tool_call_count(&self, n: usize) {
        assert_eq!(
            self.tool_calls.len(),
            n,
            "Expected {} tool call(s), but got {}: {:?}",
            n,
            self.tool_calls.len(),
            self.tool_calls
                .iter()
                .map(|tc| &tc.name)
                .collect::<Vec<_>>(),
        );
    }
}
