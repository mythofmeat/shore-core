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
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ImageRef {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    /// Base64-encoded image data for wire transfer. Stripped on disk storage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

impl PartialEq for ImageRef {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.caption == other.caption
    }
}

/// A structured content block within a message.
///
/// Messages can contain a sequence of content blocks representing text,
/// thinking/reasoning, tool invocations, and tool results. This preserves
/// the full fidelity of what happened during generation.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
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
    RedactedThinking {
        data: String,
    },
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<MessageAlternative>,
    pub timestamp: String,
}

/// Stored alternate body for a regenerated assistant message.
///
/// `Message` keeps the currently selected alternative in its top-level
/// `content`/`content_blocks` fields so existing clients and prompt assembly
/// keep reading the active response. `alternatives` stores every selectable
/// candidate, including the active one.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MessageAlternative {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub images: Vec<ImageRef>,
    #[serde(default)]
    pub content_blocks: Vec<ContentBlock>,
    #[serde(default)]
    pub timestamp: String,
}

impl MessageAlternative {
    /// Ensure `content` and `content_blocks` are consistent after
    /// deserialization, matching [`Message::normalize`].
    pub fn normalize(&mut self) {
        if self.content_blocks.is_empty() && !self.content.is_empty() {
            self.content_blocks = vec![ContentBlock::Text {
                text: self.content.clone(),
            }];
        } else if !self.content_blocks.is_empty() {
            self.content = derive_content_from_blocks(&self.content_blocks);
        }
    }
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

        for alt in &mut self.alternatives {
            alt.normalize();
        }
        if !self.alternatives.is_empty() {
            let count = self.alternatives.len() as u32;
            self.alt_count = Some(count);
            let index = self.alt_index.unwrap_or(count.saturating_sub(1));
            self.alt_index = Some(index.min(count.saturating_sub(1)));
        }
    }

    /// Serialize for disk storage, omitting the redundant `content` field
    /// and stripping inline `data` from image refs.
    ///
    /// The wire protocol (History, log command) still includes `content` via
    /// normal serde serialization. This method is only for JSONL persistence.
    pub fn serialize_for_storage(&self) -> Result<String, serde_json::Error> {
        let mut val = serde_json::to_value(self)?;
        if let Some(obj) = val.as_object_mut() {
            obj.remove("content");
            // Strip inline image data — storage uses paths, not embedded bytes.
            if let Some(images) = obj.get_mut("images").and_then(|v| v.as_array_mut()) {
                for img in images {
                    if let Some(obj) = img.as_object_mut() {
                        obj.remove("data");
                    }
                }
            }
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
pub fn derive_content_from_blocks_with(
    blocks: &[ContentBlock],
    include_tool_results: bool,
) -> String {
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

/// Base64-encoded character avatar for clients that cannot read the daemon's
/// local config filesystem.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CharacterAvatar {
    pub mime_type: String,
    pub data: String,
}

/// Information about a character.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CharacterInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<CharacterAvatar>,
}

impl CharacterInfo {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            avatar: None,
        }
    }
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
        assert_eq!(derive_content_from_blocks(&blocks), "2026-03-29T10:00:00Z");
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

    // ── normalize() ──────────────────────────────────────────────────

    fn make_msg(content: &str, blocks: Vec<ContentBlock>) -> Message {
        Message {
            msg_id: "m1".into(),
            role: Role::User,
            content: content.into(),
            images: vec![],
            content_blocks: blocks,
            alt_index: None,
            alt_count: None,
            alternatives: vec![],
            timestamp: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn normalize_legacy_wraps_content_in_text_block() {
        let mut msg = make_msg("hello world", vec![]);
        msg.normalize();
        assert_eq!(msg.content_blocks.len(), 1);
        assert!(
            matches!(&msg.content_blocks[0], ContentBlock::Text { text } if text == "hello world")
        );
        assert_eq!(msg.content, "hello world");
    }

    #[test]
    fn normalize_canonical_derives_content_from_blocks() {
        let mut msg = make_msg(
            "",
            vec![ContentBlock::Text {
                text: "derived".into(),
            }],
        );
        msg.normalize();
        assert_eq!(msg.content, "derived");
        assert_eq!(msg.content_blocks.len(), 1);
    }

    #[test]
    fn normalize_both_empty_is_noop() {
        let mut msg = make_msg("", vec![]);
        msg.normalize();
        assert_eq!(msg.content, "");
        assert!(msg.content_blocks.is_empty());
    }

    // ── serialize_for_storage() ─────────────────────────────────────

    #[test]
    fn serialize_for_storage_omits_content_field() {
        let msg = make_msg(
            "should be removed",
            vec![ContentBlock::Text {
                text: "canonical".into(),
            }],
        );
        let json_str = msg.serialize_for_storage().unwrap();
        let val: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert!(
            val.get("content").is_none(),
            "content field should be omitted"
        );
        assert!(val.get("content_blocks").is_some());
    }

    #[test]
    fn serialize_for_storage_roundtrips_other_fields() {
        let msg = make_msg(
            "ignored",
            vec![ContentBlock::Text {
                text: "hello".into(),
            }],
        );
        let json_str = msg.serialize_for_storage().unwrap();
        let val: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(val["msg_id"], "m1");
        assert_eq!(val["role"], "user");
        assert_eq!(val["timestamp"], "2026-01-01T00:00:00Z");
    }

    // ── derive_content_from_blocks_with ─────────────────────────────

    #[test]
    fn derive_content_excludes_tool_results_when_flag_false() {
        let blocks = vec![
            ContentBlock::Text {
                text: "hello".into(),
            },
            ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: "result".into(),
                is_error: false,
            },
        ];
        assert_eq!(derive_content_from_blocks_with(&blocks, false), "hello");
        assert_eq!(
            derive_content_from_blocks_with(&blocks, true),
            "hello\nresult"
        );
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
