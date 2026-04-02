use serde::{Deserialize, Serialize};

/// Client hello — sent once after connect.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ClientHello {
    pub client_type: String,
    pub client_name: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Which character this client wants to talk to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
}

/// One-shot parameter overrides for a single message.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct MessageOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Enable extended thinking with the given budget (in tokens).
    /// `Some(n)` enables thinking with budget `n`; omitted = use model default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u32>,
}

/// Send a user message.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ClientMessageBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    pub text: String,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub images: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub absence_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides: Option<MessageOverrides>,
}

/// Regenerate last response.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Regen {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    #[serde(default)]
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
}

/// Execute a server command.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Command {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    pub name: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

/// Cancel an in-progress generation.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Cancel {}

/// All client → server message types, tagged by "type".
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Hello(ClientHello),
    Message(ClientMessageBody),
    Regen(Regen),
    Command(Command),
    Cancel(Cancel),
}
