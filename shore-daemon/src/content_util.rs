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
/// (e.g. memory query tool loops, memory query conversations).
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

/// Convert a `dispatch_tool` result (`Result<Value, ToolError>`) to an
/// `(output_string, is_error)` pair suitable for tool_result messages.
///
/// On success, extracts the string representation (bare string if the
/// Value is a string, otherwise JSON-serialized). On error, uses the
/// Display representation.
pub fn dispatch_result_to_output(result: Result<Value, crate::tools::ToolError>) -> (String, bool) {
    match result {
        Ok(value) => {
            let s = if let Some(s) = value.as_str() {
                s.to_string()
            } else {
                serde_json::to_string(&value).unwrap_or_default()
            };
            (s, false)
        }
        Err(e) => (e.to_string(), true),
    }
}

/// Remove `thinking` and `redacted_thinking` blocks from every assistant
/// message in an already-serialized request body. Used when
/// `[memory.thinking] preserve_prior_turns` is false to avoid re-sending
/// signed thinking blocks from completed prior turns on every subsequent
/// request — they consume input/cache tokens but Anthropic's Claude 4.x
/// models do not attend to prior-turn thinking (only to thinking within
/// an in-progress tool-use loop, which is appended via a different code
/// path and not touched by this helper).
///
/// Expects each element of `messages` to be an object with `role` and an
/// array-typed `content` field (the format produced by `build_llm_messages`
/// and by the tool-loop continuation paths). Non-conforming entries are
/// left untouched.
pub fn strip_thinking_from_assistant_history(messages: &mut [Value]) {
    for msg in messages.iter_mut() {
        let Some(role) = msg.get("role").and_then(|r| r.as_str()) else {
            continue;
        };
        if role != "assistant" {
            continue;
        }
        let Some(content) = msg.get_mut("content") else {
            continue;
        };
        let Some(arr) = content.as_array_mut() else {
            continue;
        };
        arr.retain(|block| {
            block
                .get("type")
                .and_then(|t| t.as_str())
                .is_none_or(|t| t != "thinking" && t != "redacted_thinking")
        });
    }
}

/// Build a `tool_result` JSON value for the LLM request payload.
///
/// The `is_error` field is only included when `true`, matching the
/// Anthropic API convention.
pub fn build_tool_result_json(tool_use_id: &str, content: &str, is_error: bool) -> Value {
    let mut v = json!({
        "type": "tool_result",
        "tool_use_id": tool_use_id,
        "content": content,
    });
    if is_error {
        v["is_error"] = json!(true);
    }
    v
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

    // ── strip_thinking_from_assistant_history ─────────────────────────

    #[test]
    fn strip_removes_thinking_from_assistant() {
        let mut msgs = vec![json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "hmm", "signature": "sig"},
                {"type": "text", "text": "hello"},
            ],
        })];
        strip_thinking_from_assistant_history(&mut msgs);
        let blocks = msgs[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
    }

    #[test]
    fn strip_removes_redacted_thinking_from_assistant() {
        let mut msgs = vec![json!({
            "role": "assistant",
            "content": [
                {"type": "redacted_thinking", "data": "opaque"},
                {"type": "text", "text": "final"},
            ],
        })];
        strip_thinking_from_assistant_history(&mut msgs);
        let blocks = msgs[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
    }

    #[test]
    fn strip_preserves_tool_use_and_text() {
        let mut msgs = vec![json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "x", "signature": "s"},
                {"type": "text", "text": "checking..."},
                {"type": "tool_use", "id": "t1", "name": "check_time", "input": {}},
            ],
        })];
        strip_thinking_from_assistant_history(&mut msgs);
        let blocks = msgs[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "tool_use");
    }

    #[test]
    fn strip_leaves_user_messages_untouched() {
        // Defensive — user messages shouldn't have thinking blocks, but the
        // helper must not touch them even if one sneaks in.
        let mut msgs = vec![json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "hi"},
                {"type": "thinking", "thinking": "bogus", "signature": "x"},
            ],
        })];
        strip_thinking_from_assistant_history(&mut msgs);
        let blocks = msgs[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn strip_tolerates_string_content() {
        // Legacy/simple messages whose `content` is a bare string (not an
        // array of blocks) must pass through unchanged.
        let mut msgs = vec![json!({"role": "assistant", "content": "plain text"})];
        strip_thinking_from_assistant_history(&mut msgs);
        assert_eq!(msgs[0]["content"], "plain text");
    }

    #[test]
    fn strip_tolerates_missing_fields() {
        let mut msgs = vec![json!({"role": "assistant"}), json!({"content": []})];
        strip_thinking_from_assistant_history(&mut msgs);
        // Just asserting no panic; structure untouched.
        assert_eq!(msgs[0].get("content"), None);
    }

    #[test]
    fn strip_across_multiple_assistant_messages() {
        let mut msgs = vec![
            json!({
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "one", "signature": "s1"},
                    {"type": "text", "text": "a"},
                ],
            }),
            json!({
                "role": "user",
                "content": [{"type": "text", "text": "q"}],
            }),
            json!({
                "role": "assistant",
                "content": [
                    {"type": "redacted_thinking", "data": "d"},
                    {"type": "text", "text": "b"},
                    {"type": "thinking", "thinking": "two", "signature": "s2"},
                ],
            }),
        ];
        strip_thinking_from_assistant_history(&mut msgs);
        assert_eq!(msgs[0]["content"].as_array().unwrap().len(), 1);
        assert_eq!(msgs[1]["content"].as_array().unwrap().len(), 1); // user untouched
        assert_eq!(msgs[2]["content"].as_array().unwrap().len(), 1);
        assert_eq!(msgs[2]["content"][0]["type"], "text");
        assert_eq!(msgs[2]["content"][0]["text"], "b");
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
