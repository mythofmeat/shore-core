//! Shared utilities for converting `ContentBlock` values to JSON and
//! extracting structured data from content block sequences.

use serde_json::{json, Value};
use shore_protocol::types::ContentBlock;

/// Convert a `ContentBlock` to its LLM API JSON representation, filtering
/// out blocks the API would reject (unsigned thinking blocks).
///
/// Returns `None` for blocks that should be omitted from API requests.
pub fn content_block_to_api_json(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text { text } => Some(json!({ "type": "text", "text": text })),
        ContentBlock::Thinking {
            thinking,
            signature,
        } => {
            // Require signature — Anthropic API rejects unsigned thinking blocks.
            signature
                .as_ref()
                .map(|sig| json!({ "type": "thinking", "thinking": thinking, "signature": sig }))
        }
        ContentBlock::RedactedThinking { data } => Some(json!({
            "type": "redacted_thinking", "data": data,
        })),
        ContentBlock::ToolUse { id, name, input } => Some(json!({
            "type": "tool_use", "id": id, "name": name, "input": input,
        })),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let mut v = json!({
                "type": "tool_result", "tool_use_id": tool_use_id, "content": content,
            });
            if *is_error {
                v["is_error"] = json!(true);
            }
            Some(v)
        }
    }
}

/// Convert a `ContentBlock` to JSON unconditionally.
///
/// Unlike [`content_block_to_api_json`], this includes all blocks regardless
/// of validity for API submission. Used for internal message reconstruction
/// (e.g. memory agent tool loops, researcher conversations).
pub fn content_block_to_json(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text { text } => json!({"type": "text", "text": text}),
        ContentBlock::ToolUse { id, name, input } => {
            json!({"type": "tool_use", "id": id, "name": name, "input": input})
        }
        ContentBlock::Thinking {
            thinking,
            signature,
        } => {
            let mut block = json!({"type": "thinking", "thinking": thinking});
            if let Some(sig) = signature {
                block["signature"] = json!(sig);
            }
            block
        }
        ContentBlock::RedactedThinking { data } => {
            json!({"type": "redacted_thinking", "data": data})
        }
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let mut v =
                json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": content});
            if *is_error {
                v["is_error"] = json!(true);
            }
            v
        }
    }
}

/// Extract `(id, name, input)` tuples from `ToolUse` blocks in a content
/// block sequence.
pub fn extract_tool_uses(blocks: &[ContentBlock]) -> Vec<(String, String, Value)> {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => {
                Some((id.clone(), name.clone(), input.clone()))
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── content_block_to_api_json ─────────────────────────────────────

    #[test]
    fn api_json_text_block() {
        let block = ContentBlock::Text {
            text: "hello".into(),
        };
        let result = content_block_to_api_json(&block).unwrap();
        assert_eq!(result["type"], "text");
        assert_eq!(result["text"], "hello");
    }

    #[test]
    fn api_json_thinking_with_signature() {
        let block = ContentBlock::Thinking {
            thinking: "let me think".into(),
            signature: Some("sig_abc".into()),
        };
        let result = content_block_to_api_json(&block).unwrap();
        assert_eq!(result["type"], "thinking");
        assert_eq!(result["thinking"], "let me think");
        assert_eq!(result["signature"], "sig_abc");
    }

    #[test]
    fn api_json_thinking_without_signature_returns_none() {
        let block = ContentBlock::Thinking {
            thinking: "unsigned thought".into(),
            signature: None,
        };
        assert!(
            content_block_to_api_json(&block).is_none(),
            "unsigned thinking blocks must be filtered from API requests"
        );
    }

    #[test]
    fn api_json_redacted_thinking() {
        let block = ContentBlock::RedactedThinking {
            data: "opaque".into(),
        };
        let result = content_block_to_api_json(&block).unwrap();
        assert_eq!(result["type"], "redacted_thinking");
        assert_eq!(result["data"], "opaque");
    }

    #[test]
    fn api_json_tool_use() {
        let block = ContentBlock::ToolUse {
            id: "t1".into(),
            name: "web_search".into(),
            input: json!({"query": "cats"}),
        };
        let result = content_block_to_api_json(&block).unwrap();
        assert_eq!(result["type"], "tool_use");
        assert_eq!(result["id"], "t1");
        assert_eq!(result["name"], "web_search");
        assert_eq!(result["input"]["query"], "cats");
    }

    #[test]
    fn api_json_tool_result_with_error() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: "something went wrong".into(),
            is_error: true,
        };
        let result = content_block_to_api_json(&block).unwrap();
        assert_eq!(result["type"], "tool_result");
        assert_eq!(result["tool_use_id"], "t1");
        assert_eq!(result["is_error"], true);
    }

    #[test]
    fn api_json_tool_result_without_error_omits_field() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: "success".into(),
            is_error: false,
        };
        let result = content_block_to_api_json(&block).unwrap();
        assert_eq!(result["type"], "tool_result");
        assert!(
            result.get("is_error").is_none(),
            "is_error should be omitted when false"
        );
    }

    // ── content_block_to_json ─────────────────────────────────────────

    #[test]
    fn json_thinking_without_signature_still_included() {
        let block = ContentBlock::Thinking {
            thinking: "unsigned thought".into(),
            signature: None,
        };
        let result = content_block_to_json(&block);
        assert_eq!(result["type"], "thinking");
        assert_eq!(result["thinking"], "unsigned thought");
        assert!(result.get("signature").is_none());
    }

    #[test]
    fn json_all_variants_produce_valid_json() {
        let blocks = vec![
            ContentBlock::Text { text: "hi".into() },
            ContentBlock::ToolUse {
                id: "t1".into(),
                name: "search".into(),
                input: json!({}),
            },
            ContentBlock::Thinking {
                thinking: "hmm".into(),
                signature: Some("sig".into()),
            },
            ContentBlock::RedactedThinking { data: "enc".into() },
            ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: "ok".into(),
                is_error: false,
            },
        ];
        for block in &blocks {
            let val = content_block_to_json(block);
            assert!(
                val.get("type").is_some(),
                "every block must have a type field"
            );
        }
    }

    #[test]
    fn json_tool_result_without_error_omits_field() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: "ok".into(),
            is_error: false,
        };
        let result = content_block_to_json(&block);
        assert!(result.get("is_error").is_none());
    }

    // ── extract_tool_uses ─────────────────────────────────────────────

    #[test]
    fn extract_tool_uses_empty_input() {
        assert!(extract_tool_uses(&[]).is_empty());
    }

    #[test]
    fn extract_tool_uses_mixed_blocks() {
        let blocks = vec![
            ContentBlock::Text {
                text: "preamble".into(),
            },
            ContentBlock::ToolUse {
                id: "t1".into(),
                name: "check_time".into(),
                input: json!({}),
            },
            ContentBlock::Thinking {
                thinking: "hmm".into(),
                signature: None,
            },
            ContentBlock::ToolUse {
                id: "t2".into(),
                name: "roll_dice".into(),
                input: json!({"notation": "2d6"}),
            },
        ];
        let result = extract_tool_uses(&blocks);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "t1");
        assert_eq!(result[0].1, "check_time");
        assert_eq!(result[1].0, "t2");
        assert_eq!(result[1].1, "roll_dice");
        assert_eq!(result[1].2, json!({"notation": "2d6"}));
    }

    #[test]
    fn extract_tool_uses_no_tool_blocks() {
        let blocks = vec![
            ContentBlock::Text {
                text: "just text".into(),
            },
            ContentBlock::Thinking {
                thinking: "thought".into(),
                signature: None,
            },
        ];
        assert!(extract_tool_uses(&blocks).is_empty());
    }
}
