//! JSON tool schema definitions for the memory agent's 9 tools.
//!
//! Ported from V1 `memory_agent.py` lines 46-174.

use serde_json::{json, Value};

/// Tools that mutate state and need confirmation in interactive mode.
pub const WRITE_TOOLS: &[&str] = &[
    "update_entry",
    "supersede_entry",
    "create_entry",
    "update_entity",
    "create_flag",
    "merge_entity",
    "resolve_flag",
];

/// Returns true if the tool name is a write (mutating) tool.
pub fn is_write_tool(name: &str) -> bool {
    WRITE_TOOLS.contains(&name)
}

/// All 9 tool definitions for the memory agent LLM calls.
pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "search_entries",
            "description": "Full-text search over memory entries. Uses stemming and relevance ranking — much better than SQL LIKE for finding entries by content, topic, or person. Returns up to 20 results ranked by relevance.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search terms. Supports words, \"quoted phrases\", and boolean operators (AND, OR, NOT). Examples: 'Sam Okafor', '\"golden retriever\"', 'climbing NOT gym'."
                    },
                    "status": {
                        "type": "string",
                        "description": "Filter by entry status. Default: 'active'. Use 'all' to search all statuses.",
                        "enum": ["active", "superseded", "protected", "all"]
                    }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "query_db",
            "description": "Run a read-only SQL SELECT query against the memory database. Maximum 50 rows returned. Use search_entries instead for keyword/content search — this is for structured queries (counts, date ranges, joins, aggregations).",
            "input_schema": {
                "type": "object",
                "properties": {
                    "sql": {
                        "type": "string",
                        "description": "A SELECT SQL query."
                    }
                },
                "required": ["sql"]
            }
        }),
        json!({
            "name": "update_entry",
            "description": "Update fields on an existing memory entry.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "entry_id": { "type": "string" },
                    "summary_text": { "type": "string" },
                    "topic_tags": { "type": "string" },
                    "confidence": { "type": "number" },
                    "memory_type": { "type": "string" },
                    "reason": {
                        "type": "string",
                        "description": "Changelog description for this change."
                    }
                },
                "required": ["entry_id", "reason"]
            }
        }),
        json!({
            "name": "supersede_entry",
            "description": "Mark an entry as superseded (replaced).",
            "input_schema": {
                "type": "object",
                "properties": {
                    "entry_id": { "type": "string" },
                    "superseded_by": {
                        "type": "string",
                        "description": "ID of the replacement entry, if any."
                    },
                    "reason": { "type": "string" }
                },
                "required": ["entry_id", "reason"]
            }
        }),
        json!({
            "name": "create_entry",
            "description": "Create a new memory entry.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "summary_text": { "type": "string" },
                    "topic_tags": { "type": "string" },
                    "memory_type": {
                        "type": "string",
                        "enum": ["episodic", "semantic"]
                    },
                    "confidence": { "type": "number" },
                    "reason": { "type": "string" }
                },
                "required": ["summary_text", "reason"]
            }
        }),
        json!({
            "name": "update_entity",
            "description": "Update an entity's name, type, or description.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "type": { "type": "string" },
                    "description": { "type": "string" }
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "merge_entity",
            "description": "Merge a deprecated/duplicate entity into a canonical one. Re-links all entry associations from the source entity to the target, then removes the old links.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "from_name": {
                        "type": "string",
                        "description": "Name of the deprecated/duplicate entity to merge away."
                    },
                    "to_name": {
                        "type": "string",
                        "description": "Name of the canonical entity to merge into."
                    }
                },
                "required": ["from_name", "to_name"]
            }
        }),
        json!({
            "name": "resolve_flag",
            "description": "Resolve a flag with a resolution description.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "flag_id": { "type": "integer" },
                    "resolution": { "type": "string" }
                },
                "required": ["flag_id", "resolution"]
            }
        }),
        json!({
            "name": "create_flag",
            "description": "Create a new flag on an entry.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "entry_id": { "type": "string" },
                    "flag_type": {
                        "type": "string",
                        "enum": [
                            "contradiction",
                            "entity_conflict",
                            "stale",
                            "consolidation_ambiguity",
                            "ambiguous_tidy"
                        ]
                    },
                    "reason": { "type": "string" }
                },
                "required": ["entry_id", "flag_type", "reason"]
            }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definitions_returns_9_tools() {
        let tools = tool_definitions();
        assert_eq!(tools.len(), 9);

        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"search_entries"));
        assert!(names.contains(&"query_db"));
        assert!(names.contains(&"create_entry"));
        assert!(names.contains(&"update_entry"));
        assert!(names.contains(&"supersede_entry"));
        assert!(names.contains(&"update_entity"));
        assert!(names.contains(&"merge_entity"));
        assert!(names.contains(&"resolve_flag"));
        assert!(names.contains(&"create_flag"));
    }

    #[test]
    fn write_tools_classification() {
        assert!(is_write_tool("create_entry"));
        assert!(is_write_tool("update_entry"));
        assert!(is_write_tool("supersede_entry"));
        assert!(is_write_tool("merge_entity"));
        assert!(is_write_tool("resolve_flag"));
        assert!(!is_write_tool("search_entries"));
        assert!(!is_write_tool("query_db"));
    }
}
