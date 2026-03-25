use serde::{Deserialize, Serialize};

/// Client hello — sent once after connect.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ClientHello {
    pub client_type: String,
    pub client_name: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
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

/// All client → server message types, tagged by "type".
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Hello(ClientHello),
    Message(ClientMessageBody),
    Regen(Regen),
    Command(Command),
}
