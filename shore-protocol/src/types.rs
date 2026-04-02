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

/// A structured content block within a message.
///
/// Messages can contain a sequence of content blocks representing text,
/// thinking/reasoning, tool invocations, and tool results. This preserves
/// the full fidelity of what happened during generation.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    RedactedThinking { data: String },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
}

/// A chat message. One shape everywhere — no polymorphism.
///
/// `content_blocks` is the canonical content representation.
/// `content` is a derived convenience field (human-readable text summary).
/// On disk, only `content_blocks` is stored; `content` is derived on load.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Message {
    pub msg_id: String,
    pub role: Role,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub images: Vec<ImageRef>,
    #[serde(default)]
    pub content_blocks: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt_count: Option<u32>,
    pub timestamp: String,
}

impl Message {
    /// Ensure `content` and `content_blocks` are consistent after deserialization.
    ///
    /// Handles both old format (content only) and new format (content_blocks only):
    /// - Old: wraps `content` in a `Text` block
    /// - New: derives `content` from blocks
    pub fn normalize(&mut self) {
        if self.content_blocks.is_empty() && !self.content.is_empty() {
            // Legacy format: content present but no blocks.
            self.content_blocks = vec![ContentBlock::Text {
                text: self.content.clone(),
            }];
        } else if !self.content_blocks.is_empty() {
            // Canonical: derive content from blocks.
            self.content = derive_content_from_blocks(&self.content_blocks);
        }
    }

    /// Serialize for disk storage, omitting the redundant `content` field.
    ///
    /// The wire protocol (History, log command) still includes `content` via
    /// normal serde serialization. This method is only for JSONL persistence.
    pub fn serialize_for_storage(&self) -> Result<String, serde_json::Error> {
        let mut val = serde_json::to_value(self)?;
        if let Some(obj) = val.as_object_mut() {
            obj.remove("content");
        }
        serde_json::to_string(&val)
    }
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

/// Derive a human-readable text summary from content blocks.
///
/// Joins all `Text` block contents (trimmed), and optionally `ToolResult`
/// contents, skipping thinking, redacted thinking, and tool use blocks
/// which are not user-visible text.
///
/// When `include_tool_results` is true, this is the canonical way to produce
/// `Message.content`. When false, only `Text` blocks contribute (used for
/// merged messages where tool results are already embedded in content_blocks).
pub fn derive_content_from_blocks_with(blocks: &[ContentBlock], include_tool_results: bool) -> String {
    let mut parts: Vec<&str> = Vec::new();

    for block in blocks {
        match block {
            ContentBlock::Text { text } => {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed);
                }
            }
            ContentBlock::ToolResult { content, .. } if include_tool_results => {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed);
                }
            }
            _ => {}
        }
    }

    parts.join("\n")
}

/// Derive a human-readable text summary from content blocks (including tool results).
pub fn derive_content_from_blocks(blocks: &[ContentBlock]) -> String {
    derive_content_from_blocks_with(blocks, true)
}

/// Information about a character.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CharacterInfo {
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_content_empty_blocks() {
        assert_eq!(derive_content_from_blocks(&[]), "");
    }

    #[test]
    fn derive_content_text_only() {
        let blocks = vec![ContentBlock::Text {
            text: "hello world".into(),
        }];
        assert_eq!(derive_content_from_blocks(&blocks), "hello world");
    }

    #[test]
    fn derive_content_trims_whitespace() {
        let blocks = vec![ContentBlock::Text {
            text: "\n\n".into(),
        }];
        assert_eq!(derive_content_from_blocks(&blocks), "");
    }

    #[test]
    fn derive_content_tool_result() {
        let blocks = vec![ContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: "2026-03-29T10:00:00Z".into(),
            is_error: false,
        }];
        assert_eq!(
            derive_content_from_blocks(&blocks),
            "2026-03-29T10:00:00Z"
        );
    }

    #[test]
    fn derive_content_skips_thinking_and_tool_use() {
        let blocks = vec![
            ContentBlock::Thinking {
                thinking: "Let me think...".into(),
                signature: None,
            },
            ContentBlock::ToolUse {
                id: "t1".into(),
                name: "check_time".into(),
                input: serde_json::json!({}),
            },
            ContentBlock::RedactedThinking {
                data: "opaque".into(),
            },
            ContentBlock::Text {
                text: "The answer".into(),
            },
        ];
        assert_eq!(derive_content_from_blocks(&blocks), "The answer");
    }

    #[test]
    fn derive_content_multiple_text_blocks() {
        let blocks = vec![
            ContentBlock::Text {
                text: "first".into(),
            },
            ContentBlock::Text {
                text: "second".into(),
            },
        ];
        assert_eq!(derive_content_from_blocks(&blocks), "first\nsecond");
    }

    #[test]
    fn derive_content_mixed_text_and_tool_result() {
        let blocks = vec![
            ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: "tool output".into(),
                is_error: false,
            },
            ContentBlock::ToolResult {
                tool_use_id: "t2".into(),
                content: "more output".into(),
                is_error: false,
            },
        ];
        assert_eq!(
            derive_content_from_blocks(&blocks),
            "tool output\nmore output"
        );
    }
}
