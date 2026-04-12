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
    pub name: String,
    pub data: serde_json::Value,
}

/// Error response.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
}

/// Begin streaming.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StreamStart {
    #[serde(default)]
    pub regen: bool,
}

/// Partial content chunk.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StreamChunk {
    pub text: String,
    #[serde(default = "default_content_type")]
    pub content_type: String,
}

fn default_content_type() -> String {
    "text".to_string()
}

/// Done streaming.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StreamEnd {
    pub content: String,
    pub metadata: StreamMetadata,
    /// Why the model stopped: "end_turn", "tool_use", "max_tokens", etc.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub finish_reason: String,
}

/// Generation phase change.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Phase {
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
    pub tool_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
}

/// Tool completed.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ToolResult {
    pub tool_id: String,
    pub tool_name: String,
    pub output: String,
    #[serde(default)]
    pub is_error: bool,
}

/// Server-generated image ready.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SendImage {
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
}
