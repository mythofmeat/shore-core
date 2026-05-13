//! Conversation history search tool.
//!
//! Searches both compacted segment files and the current active conversation
//! window for the character. This is intentionally separate from filesystem
//! search: history is transcript data, not workspace files.

use std::path::PathBuf;

use chrono::{DateTime, FixedOffset};
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
                    "description": "Optional keyword or phrase to search for (case-insensitive). Omit this to return messages by time range only."
                },
                "start_time": {
                    "type": "string",
                    "description": "Optional inclusive lower timestamp bound in RFC3339 format, for example 2026-05-13T09:00:00+10:00."
                },
                "end_time": {
                    "type": "string",
                    "description": "Optional inclusive upper timestamp bound in RFC3339 format, for example 2026-05-13T17:00:00+10:00."
                },
                "max_results": {
                    "type": "number",
                    "description": "Maximum matching messages to return. Defaults to 20, maximum 100."
                }
            },
            "required": []
        }),
        category: ToolCategory::Other,
    }]
}

#[derive(Debug, Clone)]
struct TimeRange {
    start: Option<DateTime<FixedOffset>>,
    end: Option<DateTime<FixedOffset>>,
}

impl TimeRange {
    fn is_empty(&self) -> bool {
        self.start.is_none() && self.end.is_none()
    }

    fn contains(&self, timestamp: DateTime<FixedOffset>) -> bool {
        if let Some(start) = self.start {
            if timestamp < start {
                return false;
            }
        }
        if let Some(end) = self.end {
            if timestamp > end {
                return false;
            }
        }
        true
    }
}

struct SearchFilters<'a> {
    query: Option<&'a str>,
    query_lower: Option<&'a str>,
    range: &'a TimeRange,
}

#[derive(Default)]
struct SearchStats {
    skipped_invalid_timestamps: usize,
}

fn optional_trimmed_string(input: &Value, field: &str) -> Result<Option<String>, ToolError> {
    let Some(value) = input.get(field) else {
        return Ok(None);
    };
    let Some(s) = value.as_str() else {
        return Err(ToolError::InvalidArgs(format!("{field} must be a string")));
    };
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(trimmed.to_string()))
}

fn parse_time_bound(
    input: &Value,
    field: &str,
) -> Result<Option<DateTime<FixedOffset>>, ToolError> {
    optional_trimmed_string(input, field)?
        .map(|raw| {
            DateTime::parse_from_rfc3339(&raw).map_err(|e| {
                ToolError::InvalidArgs(format!("{field} must be an RFC3339 timestamp: {e}"))
            })
        })
        .transpose()
}

fn filters_from(input: &Value) -> Result<(Option<String>, TimeRange), ToolError> {
    let query = optional_trimmed_string(input, "query")?;
    let range = TimeRange {
        start: parse_time_bound(input, "start_time")?,
        end: parse_time_bound(input, "end_time")?,
    };

    if let (Some(start), Some(end)) = (range.start, range.end) {
        if start > end {
            return Err(ToolError::InvalidArgs(
                "start_time must be before or equal to end_time".into(),
            ));
        }
    }

    if query.is_none() && range.is_empty() {
        return Err(ToolError::InvalidArgs(
            "provide query, start_time, end_time, or a combination".into(),
        ));
    }

    Ok((query, range))
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

fn excerpt_for(content: &str, query: Option<&str>) -> String {
    let Some(query) = query else {
        let mut excerpt: String = content.chars().take(EXCERPT_CHARS).collect();
        if content.chars().count() > EXCERPT_CHARS {
            excerpt.push_str("...");
        }
        return excerpt;
    };

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

fn matches_query(content: &str, query_lower: Option<&str>) -> bool {
    query_lower
        .map(|query| content.to_lowercase().contains(query))
        .unwrap_or(true)
}

fn matches_time_range(
    timestamp: &str,
    range: &TimeRange,
    skipped_invalid_timestamps: &mut usize,
) -> bool {
    if range.is_empty() {
        return true;
    }

    match DateTime::parse_from_rfc3339(timestamp) {
        Ok(parsed) => range.contains(parsed),
        Err(_) => {
            *skipped_invalid_timestamps += 1;
            false
        }
    }
}

fn push_matches(
    results: &mut Vec<Value>,
    messages: &[Message],
    source: &str,
    filters: &SearchFilters<'_>,
    max_results: usize,
    stats: &mut SearchStats,
) {
    for message in messages {
        if results.len() >= max_results {
            return;
        }

        if matches_query(&message.content, filters.query_lower)
            && matches_time_range(
                &message.timestamp,
                filters.range,
                &mut stats.skipped_invalid_timestamps,
            )
        {
            results.push(json!({
                "msg_id": message.msg_id,
                "role": role_label(&message.role),
                "timestamp": message.timestamp,
                "source": source,
                "excerpt": excerpt_for(&message.content, filters.query),
            }));
        }

        for (index, alternative) in message.alternatives.iter().enumerate() {
            if results.len() >= max_results {
                return;
            }
            let timestamp = if alternative.timestamp.is_empty() {
                &message.timestamp
            } else {
                &alternative.timestamp
            };
            if alternative.content == message.content
                || !matches_query(&alternative.content, filters.query_lower)
                || !matches_time_range(
                    timestamp,
                    filters.range,
                    &mut stats.skipped_invalid_timestamps,
                )
            {
                continue;
            }
            let alt_source = format!("{source}:alt:{index}");
            results.push(json!({
                "msg_id": message.msg_id,
                "role": role_label(&message.role),
                "timestamp": timestamp,
                "source": alt_source,
                "alternative_index": index,
                "alternative_count": message.alternatives.len(),
                "excerpt": excerpt_for(&alternative.content, filters.query),
            }));
        }
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

    let (query, range) = filters_from(&input)?;
    let query_lower = query.as_ref().map(|q| q.to_lowercase());
    let filters = SearchFilters {
        query: query.as_deref(),
        query_lower: query_lower.as_deref(),
        range: &range,
    };
    let max_results = max_results_from(&input);
    let character_dir = PathBuf::from(character_data_dir);

    let mut results = Vec::new();
    let mut searched_messages = 0usize;
    let mut stats = SearchStats::default();

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
            &filters,
            max_results,
            &mut stats,
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
            &filters,
            max_results,
            &mut stats,
        );
    }

    let count = results.len();
    Ok(json!({
        "query": query,
        "time_range": {
            "start_time": range.start.map(|ts| ts.to_rfc3339()),
            "end_time": range.end.map(|ts| ts.to_rfc3339()),
            "inclusive": true,
        },
        "results": results,
        "count": count,
        "searched_messages": searched_messages,
        "skipped_invalid_timestamps": stats.skipped_invalid_timestamps,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestToolContext;
    use shore_protocol::types::{ContentBlock, MessageAlternative};

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

    fn msg_at(id: &str, role: Role, content: &str, timestamp: &str) -> Message {
        let mut message = msg(id, role, content);
        message.timestamp = timestamp.to_string();
        message
    }

    fn write_active(character_dir: &std::path::Path, messages: &[Message]) {
        let lines = messages
            .iter()
            .map(|message| message.serialize_for_storage().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(character_dir.join("active.jsonl"), format!("{lines}\n")).unwrap();
    }

    #[test]
    fn search_history_schema_exposes_optional_time_range() {
        let defs = tool_defs();
        let schema = &defs[0].parameters;
        assert!(schema["properties"].get("query").is_some());
        assert!(schema["properties"].get("start_time").is_some());
        assert!(schema["properties"].get("end_time").is_some());
        assert!(schema["required"].as_array().unwrap().is_empty());
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

    #[tokio::test]
    async fn search_history_finds_stored_alternatives() {
        let tmp = tempfile::tempdir().unwrap();
        let character_dir = tmp.path();

        let mut active = msg("active", Role::Assistant, "Tea came up again today.");
        active.alt_index = Some(0);
        active.alt_count = Some(2);
        active.alternatives = vec![
            MessageAlternative {
                content: active.content.clone(),
                images: vec![],
                content_blocks: active.content_blocks.clone(),
                timestamp: active.timestamp.clone(),
            },
            MessageAlternative {
                content: "Coffee came up in a regenerated reply.".to_string(),
                images: vec![],
                content_blocks: vec![ContentBlock::Text {
                    text: "Coffee came up in a regenerated reply.".to_string(),
                }],
                timestamp: "2026-01-01T00:01:00Z".to_string(),
            },
        ];
        std::fs::write(
            character_dir.join("active.jsonl"),
            format!("{}\n", active.serialize_for_storage().unwrap()),
        )
        .unwrap();

        let ctx = TestToolContext::new().with_character_data_dir(character_dir.to_str().unwrap());
        let result = handle_search_history(json!({"query": "coffee"}), &ctx)
            .await
            .unwrap();
        let hits = result["results"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["msg_id"], "active");
        assert_eq!(hits[0]["source"], "active:alt:1");
        assert_eq!(hits[0]["alternative_index"], 1);
    }

    #[tokio::test]
    async fn search_history_finds_messages_by_time_range_without_query() {
        let tmp = tempfile::tempdir().unwrap();
        let character_dir = tmp.path();
        let segments_dir = character_dir.join("segments");
        std::fs::create_dir_all(&segments_dir).unwrap();

        let old = msg_at(
            "old",
            Role::User,
            "This is before the requested window.",
            "2026-05-13T08:59:59+10:00",
        );
        let segment_match = msg_at(
            "segment_match",
            Role::Assistant,
            "This compacted message is inside the window.",
            "2026-05-13T09:15:00+10:00",
        );
        let segment_lines = [old, segment_match]
            .iter()
            .map(|message| message.serialize_for_storage().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(
            segments_dir.join("0001.jsonl"),
            format!("{segment_lines}\n"),
        )
        .unwrap();
        std::fs::write(
            character_dir.join("compaction.json"),
            r#"{
              "segments": [{
                "file": "0001.jsonl",
                "message_count": 2,
                "compacted_at": "2026-05-13T10:00:00+10:00"
              }],
              "total_compacted_messages": 2
            }"#,
        )
        .unwrap();

        let active_match = msg_at(
            "active_match",
            Role::User,
            "This active message is also inside the window.",
            "2026-05-13T10:30:00+10:00",
        );
        let late = msg_at(
            "late",
            Role::Assistant,
            "This is after the requested window.",
            "2026-05-13T11:00:01+10:00",
        );
        write_active(character_dir, &[active_match, late]);

        let ctx = TestToolContext::new().with_character_data_dir(character_dir.to_str().unwrap());
        let result = handle_search_history(
            json!({
                "start_time": "2026-05-13T09:00:00+10:00",
                "end_time": "2026-05-13T10:59:59+10:00"
            }),
            &ctx,
        )
        .await
        .unwrap();

        let hits = result["results"].as_array().unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0]["msg_id"], "segment_match");
        assert_eq!(hits[1]["msg_id"], "active_match");
        assert_eq!(result["query"], Value::Null);
        assert_eq!(
            result["time_range"]["start_time"],
            "2026-05-13T09:00:00+10:00"
        );
        assert_eq!(result["time_range"]["inclusive"], true);
    }

    #[tokio::test]
    async fn search_history_combines_query_and_time_range() {
        let tmp = tempfile::tempdir().unwrap();
        let character_dir = tmp.path();
        write_active(
            character_dir,
            &[
                msg_at(
                    "too_early",
                    Role::User,
                    "Tea was mentioned before breakfast.",
                    "2026-05-13T08:30:00+10:00",
                ),
                msg_at(
                    "match",
                    Role::Assistant,
                    "Tea came up during the requested hour.",
                    "2026-05-13T09:30:00+10:00",
                ),
                msg_at(
                    "wrong_query",
                    Role::User,
                    "Coffee came up during the requested hour.",
                    "2026-05-13T09:45:00+10:00",
                ),
            ],
        );

        let ctx = TestToolContext::new().with_character_data_dir(character_dir.to_str().unwrap());
        let result = handle_search_history(
            json!({
                "query": "tea",
                "start_time": "2026-05-13T09:00:00+10:00",
                "end_time": "2026-05-13T10:00:00+10:00"
            }),
            &ctx,
        )
        .await
        .unwrap();

        let hits = result["results"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["msg_id"], "match");
    }

    #[tokio::test]
    async fn search_history_time_range_uses_stored_alternative_timestamp() {
        let tmp = tempfile::tempdir().unwrap();
        let character_dir = tmp.path();

        let mut active = msg_at(
            "active",
            Role::Assistant,
            "This selected reply is outside the requested window.",
            "2026-05-13T08:00:00+10:00",
        );
        active.alt_index = Some(0);
        active.alt_count = Some(2);
        active.alternatives = vec![
            MessageAlternative {
                content: active.content.clone(),
                images: vec![],
                content_blocks: active.content_blocks.clone(),
                timestamp: active.timestamp.clone(),
            },
            MessageAlternative {
                content: "Tea appeared in a regenerated reply.".to_string(),
                images: vec![],
                content_blocks: vec![ContentBlock::Text {
                    text: "Tea appeared in a regenerated reply.".to_string(),
                }],
                timestamp: "2026-05-13T09:30:00+10:00".to_string(),
            },
        ];
        write_active(character_dir, &[active]);

        let ctx = TestToolContext::new().with_character_data_dir(character_dir.to_str().unwrap());
        let result = handle_search_history(
            json!({
                "query": "tea",
                "start_time": "2026-05-13T09:00:00+10:00",
                "end_time": "2026-05-13T10:00:00+10:00"
            }),
            &ctx,
        )
        .await
        .unwrap();

        let hits = result["results"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["source"], "active:alt:1");
        assert_eq!(hits[0]["timestamp"], "2026-05-13T09:30:00+10:00");
    }

    #[tokio::test]
    async fn search_history_requires_query_or_time_range() {
        let ctx = TestToolContext::new().with_character_data_dir("/tmp");
        let result = handle_search_history(json!({}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn search_history_rejects_invalid_time_range() {
        let ctx = TestToolContext::new().with_character_data_dir("/tmp");

        let invalid = handle_search_history(
            json!({
                "start_time": "not-a-timestamp"
            }),
            &ctx,
        )
        .await;
        assert!(matches!(invalid, Err(ToolError::InvalidArgs(_))));

        let reversed = handle_search_history(
            json!({
                "start_time": "2026-05-13T10:00:00+10:00",
                "end_time": "2026-05-13T09:00:00+10:00"
            }),
            &ctx,
        )
        .await;
        assert!(matches!(reversed, Err(ToolError::InvalidArgs(_))));
    }
}
