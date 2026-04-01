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
        ContentBlock::Thinking { thinking, signature } => {
            // Require signature — Anthropic API rejects unsigned thinking blocks.
            signature.as_ref().map(|sig| {
                json!({ "type": "thinking", "thinking": thinking, "signature": sig })
            })
        }
        ContentBlock::RedactedThinking { data } => Some(json!({
            "type": "redacted_thinking", "data": data,
        })),
        ContentBlock::ToolUse { id, name, input } => Some(json!({
            "type": "tool_use", "id": id, "name": name, "input": input,
        })),
        ContentBlock::ToolResult { tool_use_id, content, is_error } => {
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
        ContentBlock::Thinking { thinking, signature } => {
            let mut block = json!({"type": "thinking", "thinking": thinking});
            if let Some(sig) = signature {
                block["signature"] = json!(sig);
            }
            block
        }
        ContentBlock::RedactedThinking { data } => {
            json!({"type": "redacted_thinking", "data": data})
        }
        ContentBlock::ToolResult { tool_use_id, content, is_error } => {
            let mut v = json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": content});
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
