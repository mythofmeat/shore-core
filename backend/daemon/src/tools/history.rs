//! Conversation history search tool.
//!
//! Searches both compacted segment files and the current active conversation
//! window for the character. This is intentionally separate from filesystem
//! search: history is transcript data, not workspace files.

use std::path::PathBuf;

use serde_json::{json, Value};
use shore_protocol::types::{Message, Role};

use crate::engine::messages::MessageStore;
use crate::engine::segments::SegmentReader;

use super::{ToolCategory, ToolContext, ToolDef, ToolError};

const DEFAULT_MAX_RESULTS: usize = 20;
const MAX_RESULTS: usize = 100;
const EXCERPT_CHARS: usize = 360;

pub fn tool_defs() -> Vec<ToolDef> {
    vec![ToolDef {
        name: "search_history",
        description: crate::include_prompt!("../../prompts/tools/history/search_history.md"),
        parameters: json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keyword or phrase to search for (case-insensitive)."
                },
                "max_results": {
                    "type": "number",
                    "description": "Maximum matching messages to return. Defaults to 20, maximum 100."
                }
            },
            "required": ["query"]
        }),
        category: ToolCategory::Other,
    }]
}

fn query_from(input: &Value) -> Result<String, ToolError> {
    let query = input
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing required field: query".into()))?
        .trim();
    if query.is_empty() {
        return Err(ToolError::InvalidArgs("query must not be empty".into()));
    }
    Ok(query.to_string())
}

fn max_results_from(input: &Value) -> usize {
    input
        .get("max_results")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(DEFAULT_MAX_RESULTS)
        .clamp(1, MAX_RESULTS)
}

fn role_label(role: &Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
    }
}

fn excerpt_for(content: &str, query: &str) -> String {
    let content_lower = content.to_lowercase();
    let query_lower = query.to_lowercase();
    let Some(byte_idx) = content_lower.find(&query_lower) else {
        return content.chars().take(EXCERPT_CHARS).collect();
    };

    let start_char = content
        .get(..byte_idx)
        .map(|prefix| prefix.chars().count().saturating_sub(80))
        .unwrap_or(0);
    let mut excerpt: String = content
        .chars()
        .skip(start_char)
        .take(EXCERPT_CHARS)
        .collect();
    if start_char > 0 {
        excerpt = format!("...{excerpt}");
    }
    if content.chars().count() > start_char + EXCERPT_CHARS {
        excerpt.push_str("...");
    }
    excerpt
}

fn push_matches(
    results: &mut Vec<Value>,
    messages: &[Message],
    source: &str,
    query: &str,
    query_lower: &str,
    max_results: usize,
) {
    for message in messages {
        if results.len() >= max_results {
            return;
        }
        if !message.content.to_lowercase().contains(query_lower) {
            continue;
        }
        results.push(json!({
            "msg_id": message.msg_id,
            "role": role_label(&message.role),
            "timestamp": message.timestamp,
            "source": source,
            "excerpt": excerpt_for(&message.content, query),
        }));
    }
}

pub async fn handle_search_history(
    input: Value,
    ctx: &dyn ToolContext,
) -> Result<Value, ToolError> {
    let character_data_dir = ctx.character_data_dir();
    if character_data_dir.is_empty() {
        return Err(ToolError::InvalidArgs(
            "conversation history is not configured".into(),
        ));
    }

    let query = query_from(&input)?;
    let query_lower = query.to_lowercase();
    let max_results = max_results_from(&input);
    let character_dir = PathBuf::from(character_data_dir);

    let mut results = Vec::new();
    let mut searched_messages = 0usize;

    let segments = SegmentReader::load(&character_dir).map_err(|e| ToolError::Io(e.to_string()))?;
    for index in 0..segments.segment_count() {
        if results.len() >= max_results {
            break;
        }
        let messages = segments
            .read_segment(index)
            .map_err(|e| ToolError::Io(e.to_string()))?;
        searched_messages += messages.len();
        push_matches(
            &mut results,
            &messages,
            &format!("segment:{index}"),
            &query,
            &query_lower,
            max_results,
        );
    }

    if results.len() < max_results {
        let active_path = character_dir.join("active.jsonl");
        let active = MessageStore::load(active_path).map_err(|e| ToolError::Io(e.to_string()))?;
        searched_messages += active.message_count();
        push_matches(
            &mut results,
            active.messages(),
            "active",
            &query,
            &query_lower,
            max_results,
        );
    }

    let count = results.len();
    Ok(json!({
        "query": query,
        "results": results,
        "count": count,
        "searched_messages": searched_messages,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestToolContext;
    use shore_protocol::types::ContentBlock;

    fn msg(id: &str, role: Role, content: &str) -> Message {
        Message {
            msg_id: id.to_string(),
            role,
            content: content.to_string(),
            images: vec![],
            content_blocks: vec![ContentBlock::Text {
                text: content.to_string(),
            }],
            alt_index: None,
            alt_count: None,
            alternatives: vec![],
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[tokio::test]
    async fn search_history_finds_active_and_segment_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let character_dir = tmp.path();
        let segments_dir = character_dir.join("segments");
        std::fs::create_dir_all(&segments_dir).unwrap();

        let old = msg("old", Role::User, "We talked about tea last winter.");
        let old_line = old.serialize_for_storage().unwrap();
        std::fs::write(segments_dir.join("0001.jsonl"), format!("{old_line}\n")).unwrap();
        std::fs::write(
            character_dir.join("compaction.json"),
            r#"{
              "segments": [{
                "file": "0001.jsonl",
                "message_count": 1,
                "compacted_at": "2026-01-01T00:00:00Z"
              }],
              "total_compacted_messages": 1
            }"#,
        )
        .unwrap();

        let active = msg("active", Role::Assistant, "Tea came up again today.");
        std::fs::write(
            character_dir.join("active.jsonl"),
            format!("{}\n", active.serialize_for_storage().unwrap()),
        )
        .unwrap();

        let ctx = TestToolContext::new().with_character_data_dir(character_dir.to_str().unwrap());
        let result = handle_search_history(json!({"query": "tea"}), &ctx)
            .await
            .unwrap();
        let hits = result["results"].as_array().unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0]["msg_id"], "old");
        assert_eq!(hits[1]["msg_id"], "active");
    }
}
