use serde::{Deserialize, Serialize};

use crate::error::ErrorCode;
use crate::types::{CharacterInfo, Message, StreamMetadata};

/// Server hello — sent after client connects.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ServerHello {
    pub v: u32,
    pub server_name: String,
    #[serde(default)]
    pub characters: Vec<CharacterInfo>,
}

/// Full state snapshot.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct History {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_character: Option<String>,
    #[serde(default)]
    pub revision: u64,
}

/// Server shutting down.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Shutdown {}

/// Keepalive.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Ping {}

/// Command result.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CommandOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    pub name: String,
    pub data: serde_json::Value,
}

/// Error response.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Error {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    pub code: ErrorCode,
    pub message: String,
}

/// Begin streaming.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StreamStart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    #[serde(default)]
    pub regen: bool,
}

/// Partial content chunk.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StreamChunk {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    pub text: String,
    #[serde(default = "default_content_type")]
    pub content_type: String,
}

fn default_content_type() -> String {
    "text".to_string()
}

/// Done streaming.
///
/// A single `send`/`regen` can emit multiple `StreamEnd` frames when the
/// daemon is running a tool loop: one per LLM turn, so clients can render
/// tool calls as they happen. Only the frame with `is_final = true` marks
/// the end of the whole generation — clients that want the final aggregated
/// result (e.g. `collect_stream`) must keep reading until they see it.
/// Older servers that predate the field will serialize nothing; `serde`'s
/// default treats missing as `true`, preserving pre-tool-loop semantics.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StreamEnd {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    /// Persisted assistant message id for terminal stream ends.
    ///
    /// Present only after the final assistant message has been appended and
    /// persisted. Intermediate tool-use boundaries and older servers omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    /// Durable history revision containing `msg_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    pub content: String,
    pub metadata: StreamMetadata,
    /// Why the model stopped: "end_turn", "tool_use", "max_tokens", etc.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub finish_reason: String,
    /// Whether this is the final `StreamEnd` for the generation. Intermediate
    /// tool-loop boundaries set this to `false`; the terminal StreamEnd sets
    /// it to `true`. Defaults to `true` so pre-field daemon frames are treated
    /// as terminal (matching historical single-turn behavior).
    #[serde(default = "default_true")]
    pub is_final: bool,
}

fn default_true() -> bool {
    true
}

/// Generation phase change.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Phase {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Autonomous message arrived.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NewMessage {
    #[serde(default)]
    pub revision: u64,
    #[serde(flatten)]
    pub message: Message,
}

/// Tool invoked during generation.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ToolCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    pub tool_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
}

/// Tool completed.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ToolResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    pub tool_id: String,
    pub tool_name: String,
    pub output: String,
    #[serde(default)]
    pub is_error: bool,
}

/// Server-generated image ready.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SendImage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    /// Base64-encoded image data for wire transfer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

/// Unexpected cache invalidation warning.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CacheWarning {
    pub expected_tokens: u32,
    pub message: String,
}

/// TTS audio stream starting.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AudioStart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    pub msg_id: String,
    pub sample_rate: u32,
    pub channels: u16,
}

/// TTS audio data chunk (base64-encoded PCM).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AudioChunk {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    pub data: String,
}

/// TTS audio stream complete.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AudioEnd {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
}

/// TTS error.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AudioError {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    pub message: String,
}

/// All server → client message types, tagged by "type".
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Hello(ServerHello),
    History(History),
    Shutdown(Shutdown),
    Ping(Ping),
    CommandOutput(CommandOutput),
    Error(Error),
    StreamStart(StreamStart),
    StreamChunk(StreamChunk),
    StreamEnd(StreamEnd),
    Phase(Phase),
    NewMessage(NewMessage),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    SendImage(SendImage),
    CacheWarning(CacheWarning),
    AudioStart(AudioStart),
    AudioChunk(AudioChunk),
    AudioEnd(AudioEnd),
    AudioError(AudioError),
}

impl ServerMessage {
    /// Attach a request ID to request-scoped responses.
    ///
    /// Unsolicited push/broadcast messages intentionally ignore `rid`.
    pub fn with_rid(mut self, rid: Option<String>) -> Self {
        match &mut self {
            ServerMessage::History(msg) => msg.rid = rid.clone(),
            ServerMessage::CommandOutput(msg) => msg.rid = rid.clone(),
            ServerMessage::Error(msg) => msg.rid = rid.clone(),
            ServerMessage::StreamStart(msg) => msg.rid = rid.clone(),
            ServerMessage::StreamChunk(msg) => msg.rid = rid.clone(),
            ServerMessage::StreamEnd(msg) => msg.rid = rid.clone(),
            ServerMessage::Phase(msg) => msg.rid = rid.clone(),
            ServerMessage::ToolCall(msg) => msg.rid = rid.clone(),
            ServerMessage::ToolResult(msg) => msg.rid = rid.clone(),
            ServerMessage::SendImage(msg) => msg.rid = rid.clone(),
            ServerMessage::AudioStart(msg) => msg.rid = rid.clone(),
            ServerMessage::AudioChunk(msg) => msg.rid = rid.clone(),
            ServerMessage::AudioEnd(msg) => msg.rid = rid.clone(),
            ServerMessage::AudioError(msg) => msg.rid = rid.clone(),
            ServerMessage::Hello(_)
            | ServerMessage::Shutdown(_)
            | ServerMessage::Ping(_)
            | ServerMessage::NewMessage(_)
            | ServerMessage::CacheWarning(_) => {}
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_start_roundtrip() {
        let msg = ServerMessage::AudioStart(AudioStart {
            rid: Some("r1".into()),
            msg_id: "msg-123".into(),
            sample_rate: 24000,
            channels: 1,
        });
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "audio_start");
        assert_eq!(json["sample_rate"], 24000);
        assert_eq!(json["channels"], 1);

        let roundtrip: ServerMessage = serde_json::from_value(json).unwrap();
        assert!(matches!(roundtrip, ServerMessage::AudioStart(_)));
    }

    #[test]
    fn audio_chunk_roundtrip() {
        let msg = ServerMessage::AudioChunk(AudioChunk {
            rid: None,
            data: "AQID".into(),
        });
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "audio_chunk");

        let roundtrip: ServerMessage = serde_json::from_value(json).unwrap();
        assert!(matches!(roundtrip, ServerMessage::AudioChunk(_)));
    }

    #[test]
    fn audio_end_roundtrip() {
        let msg = ServerMessage::AudioEnd(AudioEnd { rid: None });
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "audio_end");

        let roundtrip: ServerMessage = serde_json::from_value(json).unwrap();
        assert!(matches!(roundtrip, ServerMessage::AudioEnd(_)));
    }

    #[test]
    fn audio_error_roundtrip() {
        let msg = ServerMessage::AudioError(AudioError {
            rid: None,
            message: "voice not found".into(),
        });
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "audio_error");
        assert_eq!(json["message"], "voice not found");

        let roundtrip: ServerMessage = serde_json::from_value(json).unwrap();
        assert!(matches!(roundtrip, ServerMessage::AudioError(_)));
    }
}
