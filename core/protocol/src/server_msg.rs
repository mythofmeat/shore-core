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
    /// Index of the first message that is still in the active prompt context.
    ///
    /// Normal push/handshake snapshots contain only active context and leave
    /// this at zero. Bounded log/history responses may include durable archive
    /// scrollback before this index; those messages are useful for humans but
    /// are no longer part of the model's active conversation context.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub active_start: usize,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_character: Option<String>,
    #[serde(default)]
    pub revision: u64,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
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
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageOrigin {
    UserInput,
    AssistantReply,
    Autonomous,
}

/// Conversation message appended by the daemon.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NewMessage {
    #[serde(default)]
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<MessageOrigin>,
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

/// The daemon rotated from one configured provider key to another mid-request
/// because the previous key reported a credential-scoped failure (missing,
/// invalid, exhausted quota or budget, account-scoped rate limit).
///
/// Emitted only when the previous key had `warn_on_fallback = true`. The
/// payload intentionally never carries the env var value or the API key
/// itself — only the provider key, the friendly key names, the failure
/// classification, and a sanitized human-readable reason.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProviderFallbackWarning {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    /// Provider this fallback applies to (e.g. `"openrouter"`).
    pub provider: String,
    /// Friendly name of the key being abandoned.
    pub from_key: String,
    /// Friendly name of the key now in use.
    pub to_key: String,
    /// Stable failure tag from `CredentialFailureKind::as_str()`. Stable
    /// across releases so client-side rendering can branch on it.
    pub kind: String,
    /// HTTP status when the failure was a status-shaped error; `None` for
    /// missing-key / network / classified-by-body cases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Sanitized human-readable summary. Never contains secrets.
    pub message: String,
}

/// A configured usage budget crossed one or more warning thresholds.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UsageWarning {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    /// Configured budget name.
    pub budget: String,
    /// Human-readable warning text.
    pub message: String,
    /// Current spend for the budget window.
    pub current_cost: f64,
    /// Configured budget limit.
    pub cost_limit: f64,
    /// Fraction used, e.g. 0.8 for 80%.
    pub percent_used: f64,
    /// Newly crossed warning thresholds, as fractions.
    pub crossed_warn_at: Vec<f64>,
    /// Calendar period name.
    pub period: String,
    /// RFC3339 period start.
    pub period_start: String,
    /// RFC3339 reset/end time.
    pub reset_at: String,
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
    ProviderFallbackWarning(ProviderFallbackWarning),
    UsageWarning(UsageWarning),
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
            ServerMessage::ProviderFallbackWarning(msg) => msg.rid = rid.clone(),
            ServerMessage::UsageWarning(msg) => msg.rid = rid.clone(),
            ServerMessage::Hello(_)
            | ServerMessage::Shutdown(_)
            | ServerMessage::Ping(_)
            | ServerMessage::NewMessage(_)
            | ServerMessage::CacheWarning(_) => {}
        }
        self
    }
}
