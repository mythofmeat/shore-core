//! Shared utilities for converting `ContentBlock` values to JSON and
//! extracting structured data from content block sequences.

use serde_json::{json, Value};
use shore_config::models::Sdk;
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

/// Convert a `ContentBlock` to the provider-neutral request JSON Shore passes
/// into `shore-llm` for a specific SDK.
///
/// Anthropic requires signatures on replayed thinking blocks, so it uses the
/// stricter API projection. OpenAI-compatible providers and Z.AI receive the
/// full internal block so their provider adapters can project unsigned
/// reasoning into `reasoning` / `reasoning_content`.
pub fn content_block_to_request_json_for_sdk(block: &ContentBlock, sdk: &Sdk) -> Option<Value> {
    if matches!(sdk, Sdk::Openai | Sdk::Zai) {
        Some(content_block_to_json(block))
    } else {
        content_block_to_api_json(block)
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

/// Whether `block` can be safely replayed to `active_provider`.
///
/// Providers mint opaque, provider-bound data inside `thinking` blocks
/// (signatures) and `redacted_thinking` blocks (encrypted blobs, or
/// OpenRouter's `openrouter.reasoning:` envelopes). Replaying such a block to
/// a provider that did not mint it triggers an HTTP 400 — e.g. Anthropic
/// rejects an OpenRouter-relayed block with `Invalid `data` in
/// `redacted_thinking` block`. Text, tool_use, tool_result, and unsigned
/// thinking carry no provider-bound data and are always portable.
///
/// `minting_provider` is the provider that produced the message this block
/// belongs to ([`shore_protocol::types::Message::provider_key`]), or `None`
/// for messages persisted before provenance tracking existed.
pub fn thinking_block_portable_to(
    block: &ContentBlock,
    minting_provider: Option<&str>,
    active_provider: &str,
) -> bool {
    let carries_opaque_data = match block {
        ContentBlock::Thinking { signature, .. } => signature.is_some(),
        ContentBlock::RedactedThinking { .. } => true,
        _ => false,
    };
    if !carries_opaque_data {
        return true;
    }
    match minting_provider {
        // Known provenance: opaque data is only valid against its minter.
        // Exact match is the safe rule — stripping on a mismatch only loses
        // cache/reasoning continuity (already lost on a provider switch),
        // whereas keeping a foreign block hard-fails the request.
        Some(p) => p == active_provider,
        // Unknown provenance (legacy messages): fall back to the one signal
        // readable off the wire. OpenRouter tags relayed reasoning with an
        // `openrouter.reasoning:` prefix; that envelope is OpenRouter-only.
        // Other legacy opaque blocks are kept, to avoid busting working
        // same-provider histories that predate provenance tracking.
        None => match block {
            ContentBlock::RedactedThinking { data }
                if data.starts_with("openrouter.reasoning:") =>
            {
                active_provider.contains("openrouter")
            }
            _ => true,
        },
    }
}

/// Apply [`strip_thinking_from_assistant_history`] when the user has opted
/// out of preserving prior-turn thinking AND the provider does not require
/// `reasoning_content` to be replayed (DeepSeek V3.1+, Moonshot
/// Kimi-thinking — see [`shore_llm::requires_reasoning_replay`]).
pub fn maybe_strip_prior_thinking(
    messages: &mut [Value],
    preserve_prior_turns: bool,
    provider_key: &str,
) {
    if !preserve_prior_turns && !shore_llm::requires_reasoning_replay(provider_key) {
        strip_thinking_from_assistant_history(messages);
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

    // ── content_block_to_request_json_for_sdk ─────────────────────────

    #[test]
    fn request_json_openai_keeps_unsigned_thinking() {
        let block = ContentBlock::Thinking {
            thinking: "tool reasoning".into(),
            signature: None,
        };
        let result = content_block_to_request_json_for_sdk(&block, &Sdk::Openai).unwrap();
        assert_eq!(result["type"], "thinking");
        assert_eq!(result["thinking"], "tool reasoning");
    }

    #[test]
    fn request_json_anthropic_filters_unsigned_thinking() {
        let block = ContentBlock::Thinking {
            thinking: "unsigned thought".into(),
            signature: None,
        };
        assert!(content_block_to_request_json_for_sdk(&block, &Sdk::Anthropic).is_none());
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

    // ── thinking_block_portable_to ────────────────────────────────────

    #[test]
    fn portable_non_thinking_blocks_always_portable() {
        // Text / tool blocks carry no provider-bound data.
        let text = ContentBlock::Text { text: "hi".into() };
        assert!(thinking_block_portable_to(
            &text,
            Some("openrouter-anthropic"),
            "anthropic"
        ));
    }

    #[test]
    fn portable_unsigned_thinking_is_portable() {
        // No signature → no opaque data to reject.
        let block = ContentBlock::Thinking {
            thinking: "t".into(),
            signature: None,
        };
        assert!(thinking_block_portable_to(
            &block,
            Some("openrouter-anthropic"),
            "anthropic"
        ));
    }

    #[test]
    fn portable_known_provenance_same_provider_kept() {
        let signed = ContentBlock::Thinking {
            thinking: "t".into(),
            signature: Some("sig".into()),
        };
        let redacted = ContentBlock::RedactedThinking { data: "enc".into() };
        assert!(thinking_block_portable_to(
            &signed,
            Some("anthropic"),
            "anthropic"
        ));
        assert!(thinking_block_portable_to(
            &redacted,
            Some("anthropic"),
            "anthropic"
        ));
    }

    #[test]
    fn portable_known_provenance_cross_provider_stripped() {
        // Signed thinking and redacted blobs minted elsewhere must drop.
        let signed = ContentBlock::Thinking {
            thinking: "t".into(),
            signature: Some("sig".into()),
        };
        let redacted = ContentBlock::RedactedThinking { data: "enc".into() };
        assert!(!thinking_block_portable_to(
            &signed,
            Some("openrouter-anthropic"),
            "anthropic"
        ));
        assert!(!thinking_block_portable_to(
            &redacted,
            Some("openrouter-anthropic"),
            "anthropic"
        ));
    }

    #[test]
    fn portable_unknown_provenance_openrouter_prefix_stripped_for_anthropic() {
        // The exact failure mode: an `openrouter.reasoning:`-prefixed blob from
        // a pre-provenance OpenRouter turn, replayed to Anthropic direct.
        let block = ContentBlock::RedactedThinking {
            data: "openrouter.reasoning: signed payload".into(),
        };
        assert!(!thinking_block_portable_to(&block, None, "anthropic"));
        // …but kept when the active provider is still OpenRouter.
        assert!(thinking_block_portable_to(
            &block,
            None,
            "openrouter-anthropic"
        ));
    }

    #[test]
    fn portable_unknown_provenance_plain_blob_kept() {
        // Legacy same-provider blob without the OpenRouter envelope: keep, so
        // we don't bust working histories that predate provenance tracking.
        let block = ContentBlock::RedactedThinking {
            data: "opaque-anthropic-blob".into(),
        };
        assert!(thinking_block_portable_to(&block, None, "anthropic"));
    }
}
