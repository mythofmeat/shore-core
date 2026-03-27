use serde::{Deserialize, Serialize};

/// Role of a message participant.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
}

/// Reference to an image file.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ImageRef {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

/// A chat message. One shape everywhere — no polymorphism.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Message {
    pub msg_id: String,
    pub role: Role,
    pub content: String,
    #[serde(default)]
    pub images: Vec<ImageRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt_count: Option<u32>,
    pub timestamp: String,
}

/// Token usage counts from a generation.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TokenCounts {
    pub input: u32,
    pub output: u32,
    pub cache_read: u32,
    pub cache_write: u32,
}

/// Timing information for a generation.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TimingInfo {
    pub total_ms: u32,
    pub ttft_ms: u32,
}

/// Metadata attached to stream_end.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StreamMetadata {
    pub tokens: TokenCounts,
    pub timing: TimingInfo,
    pub model: String,
}

/// Information about a character.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CharacterInfo {
    pub name: String,
}
