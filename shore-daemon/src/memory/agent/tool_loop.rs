//! The inner agent loop — runs the LLM with 9 tools for up to 40 iterations.
//!
//! Ported from V1 `memory_agent.py::_run_agent_loop()` (lines 386-547).

use serde_json::{json, Value};

use crate::config::models::ResolvedModel;
use crate::llm_client::types::ContentBlock;
use crate::memory::agent_llm::AgentLlm;
use crate::memory::db::MemoryDB;

use super::tool_handlers::execute_tool;
use super::tool_schemas::{is_write_tool, tool_definitions};
use super::types::{AgentError, AgentIndexer, ConfirmCallback, ProposedOperation, ToolResult};

const MAX_ITERATIONS: usize = 40;

const DENIED_MESSAGE: &str =
    "DENIED: The user explicitly rejected this operation. Do NOT retry it. \
     Acknowledge the denial and either ask what they want instead or move on.";

/// Run the memory agent's inner tool loop.
///
/// The LLM is called with the system prompt and 9 tool schemas. On each
/// iteration, if the LLM requests tool calls, they are classified as read
/// (executed immediately) or write (batched for confirmation). The loop
/// continues until the LLM produces a final text response with no tool calls,
/// or the max iteration count is reached.
///
/// Returns `(response_text, mutations)` where mutations is a list of
/// human-readable descriptions of successful write operations.
pub async fn run_agent_loop(
    llm: &dyn AgentLlm,
    db: &MemoryDB,
    indexer: Option<&dyn AgentIndexer>,
    model: &ResolvedModel,
    system_prompt: &str,
    initial_messages: Vec<Value>,
    confirm_callback: Option<&dyn ConfirmCallback>,
) -> Result<(String, Vec<String>), AgentError> {
    let mut conversation = initial_messages;
    let mut mutations: Vec<String> = Vec::new();
    let tools = tool_definitions();

    for _iteration in 0..MAX_ITERATIONS {
        // --- LLM call ---
        let response = llm
            .generate(
                conversation.clone(),
                Some(Value::String(system_prompt.to_string())),
                Some(tools.clone()),
                model,
            )
            .await
            .map_err(|e| AgentError::Llm(e.to_string()))?;

        // --- Extract tool uses ---
        let tool_uses: Vec<(String, String, Value)> = response
            .content_blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
            .collect();

        // --- No tool calls → final response ---
        if tool_uses.is_empty() {
            return Ok((response.text, mutations));
        }

        // --- Classify into read ops and write ops ---
        let mut read_ops: Vec<&(String, String, Value)> = Vec::new();
        let mut write_ops: Vec<&(String, String, Value)> = Vec::new();
        for tu in &tool_uses {
            if is_write_tool(&tu.1) {
                write_ops.push(tu);
            } else {
                read_ops.push(tu);
            }
        }

        let mut tool_results: Vec<ToolResult> = Vec::new();

        // --- Execute read ops immediately ---
        for (id, name, input) in &read_ops {
            let result = execute_tool(name, db, indexer, input).await;
            tool_results.push(ToolResult {
                tool_use_id: id.clone(),
                content: result,
                is_error: false,
            });
        }

        // --- Confirm write ops ---
        let mut denied_ids = std::collections::HashSet::new();

        if !write_ops.is_empty() {
            if let Some(callback) = confirm_callback {
                let proposed: Vec<ProposedOperation> = write_ops
                    .iter()
                    .map(|(id, name, input)| ProposedOperation {
                        tool_use_id: id.clone(),
                        tool_name: name.clone(),
                        args: input.clone(),
                        description: describe_mutation(name, input),
                    })
                    .collect();
                denied_ids = callback.confirm(&proposed).await;
            }
            // If no callback, all writes are auto-accepted.
        }

        // --- Execute or deny write ops ---
        for (id, name, input) in &write_ops {
            if denied_ids.contains(id.as_str()) {
                tool_results.push(ToolResult {
                    tool_use_id: id.clone(),
                    content: DENIED_MESSAGE.to_string(),
                    is_error: true,
                });
                continue;
            }

            let result = execute_tool(name, db, indexer, input).await;
            // Track successful mutations
            if !result.starts_with("Error") {
                mutations.push(result.clone());
            }
            tool_results.push(ToolResult {
                tool_use_id: id.clone(),
                content: result,
                is_error: false,
            });
        }

        // --- Build assistant message from content_blocks ---
        let assistant_content: Vec<Value> = response
            .content_blocks
            .iter()
            .map(|block| match block {
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
                ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                    let mut v = json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": content});
                    if *is_error { v["is_error"] = json!(true); }
                    v
                }
            })
            .collect();

        conversation.push(json!({
            "role": "assistant",
            "content": assistant_content,
        }));

        // --- Build tool results message ---
        let tool_result_blocks: Vec<Value> = tool_results
            .iter()
            .map(|tr| {
                json!({
                    "type": "tool_result",
                    "tool_use_id": tr.tool_use_id,
                    "content": tr.content,
                    "is_error": tr.is_error,
                })
            })
            .collect();

        conversation.push(json!({
            "role": "user",
            "content": tool_result_blocks,
        }));
    }

    // Reached max iterations
    Ok(("Agent loop reached maximum iterations.".to_string(), mutations))
}

/// Generate a human-readable description of a proposed write operation.
fn describe_mutation(tool_name: &str, input: &Value) -> String {
    match tool_name {
        "create_entry" => {
            let text = input["summary_text"].as_str().unwrap_or("");
            let truncated = if text.len() > 80 {
                format!("{}...", &text[..80])
            } else {
                text.to_string()
            };
            format!("Create entry: {truncated}")
        }
        "update_entry" => {
            let eid = input["entry_id"].as_str().unwrap_or("?");
            format!("Update entry {eid}")
        }
        "supersede_entry" => {
            let eid = input["entry_id"].as_str().unwrap_or("?");
            format!("Supersede entry {eid}")
        }
        "update_entity" => {
            let name = input["name"].as_str().unwrap_or("?");
            format!("Update entity '{name}'")
        }
        "merge_entity" => {
            let from = input["from_name"].as_str().unwrap_or("?");
            let to = input["to_name"].as_str().unwrap_or("?");
            format!("Merge entity '{from}' → '{to}'")
        }
        "resolve_flag" => {
            let fid = input["flag_id"].as_i64().map(|n| n.to_string()).unwrap_or("?".into());
            format!("Resolve flag #{fid}")
        }
        "create_flag" => {
            let ftype = input["flag_type"].as_str().unwrap_or("?");
            let eid = input["entry_id"].as_str().unwrap_or("?");
            format!("Create {ftype} flag on {eid}")
        }
        _ => format!("{tool_name}: {input}"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::agent_llm::{AgentLlmResponse, MockAgentLlm};
    use crate::memory::db::MemoryDB;
    use crate::test_support::test_model;

    fn test_db() -> MemoryDB {
        MemoryDB::open_in_memory().unwrap()
    }

    /// LLM returns text immediately → returns text, empty mutations.
    #[tokio::test]
    async fn text_only_response() {
        let mock = MockAgentLlm::new(vec![AgentLlmResponse {
            text: "I found nothing relevant.".into(),
            content_blocks: vec![ContentBlock::Text {
                text: "I found nothing relevant.".into(),
            }],
            finish_reason: "end_turn".into(),
        }]);

        let db = test_db();
        let model = test_model();

        let (text, mutations) = run_agent_loop(
            &mock,
            &db,
            None,
            &model,
            "You are a memory agent.",
            vec![json!({"role": "user", "content": "What do I know?"})],
            None,
        )
        .await
        .unwrap();

        assert_eq!(text, "I found nothing relevant.");
        assert!(mutations.is_empty());
        assert_eq!(mock.call_count(), 1);
    }

    /// LLM requests a read tool (search_entries) then produces text.
    #[tokio::test]
    async fn read_tool_then_text() {
        let mock = MockAgentLlm::new(vec![
            // First response: request search_entries
            AgentLlmResponse {
                text: String::new(),
                content_blocks: vec![ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "search_entries".into(),
                    input: json!({"query": "chocolate"}),
                }],
                finish_reason: "tool_use".into(),
            },
            // Second response: final text
            AgentLlmResponse {
                text: "No memories about chocolate found.".into(),
                content_blocks: vec![ContentBlock::Text {
                    text: "No memories about chocolate found.".into(),
                }],
                finish_reason: "end_turn".into(),
            },
        ]);

        let db = test_db();
        let model = test_model();

        let (text, mutations) = run_agent_loop(
            &mock,
            &db,
            None,
            &model,
            "You are a memory agent.",
            vec![json!({"role": "user", "content": "What about chocolate?"})],
            None,
        )
        .await
        .unwrap();

        assert_eq!(text, "No memories about chocolate found.");
        assert!(mutations.is_empty()); // search is a read op, no mutations
        assert_eq!(mock.call_count(), 2);

        // Verify the tool result was passed back in the conversation
        let calls = mock.calls.lock().unwrap();
        let second_call_messages = &calls[1].messages;
        // Should have: original user msg, assistant with tool_use, user with tool_result
        assert_eq!(second_call_messages.len(), 3);
        assert_eq!(second_call_messages[2]["content"][0]["type"], "tool_result");
    }

    /// LLM requests create_entry (write) → auto-confirmed, mutation recorded.
    #[tokio::test]
    async fn write_tool_auto_confirmed() {
        let mock = MockAgentLlm::new(vec![
            // Request create_entry
            AgentLlmResponse {
                text: String::new(),
                content_blocks: vec![ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "create_entry".into(),
                    input: json!({
                        "summary_text": "Alice likes chocolate",
                        "topic_tags": "food",
                        "reason": "user said so"
                    }),
                }],
                finish_reason: "tool_use".into(),
            },
            // Final response
            AgentLlmResponse {
                text: "I've saved that memory.".into(),
                content_blocks: vec![ContentBlock::Text {
                    text: "I've saved that memory.".into(),
                }],
                finish_reason: "end_turn".into(),
            },
        ]);

        let db = test_db();
        let model = test_model();

        let (text, mutations) = run_agent_loop(
            &mock,
            &db,
            None,
            &model,
            "You are a memory agent.",
            vec![json!({"role": "user", "content": "Remember: Alice likes chocolate"})],
            None,
        )
        .await
        .unwrap();

        assert_eq!(text, "I've saved that memory.");
        assert_eq!(mutations.len(), 1);
        assert!(mutations[0].starts_with("Created entry "));

        // Verify entry exists in DB
        let entries = db.get_entries_by_status("active").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].summary_text, "Alice likes chocolate");
    }

    /// Confirm callback denies a write → DENIED message sent, no DB change.
    #[tokio::test]
    async fn write_tool_denied() {
        use std::collections::HashSet;
        use std::pin::Pin;

        struct DenyAll;
        impl ConfirmCallback for DenyAll {
            fn confirm(
                &self,
                operations: &[ProposedOperation],
            ) -> Pin<Box<dyn std::future::Future<Output = HashSet<String>> + Send + '_>> {
                let denied: HashSet<String> =
                    operations.iter().map(|op| op.tool_use_id.clone()).collect();
                Box::pin(async move { denied })
            }
        }

        let mock = MockAgentLlm::new(vec![
            // Request create_entry
            AgentLlmResponse {
                text: String::new(),
                content_blocks: vec![ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "create_entry".into(),
                    input: json!({
                        "summary_text": "Should not be saved",
                        "reason": "test"
                    }),
                }],
                finish_reason: "tool_use".into(),
            },
            // LLM acknowledges denial
            AgentLlmResponse {
                text: "Understood, I won't save that.".into(),
                content_blocks: vec![ContentBlock::Text {
                    text: "Understood, I won't save that.".into(),
                }],
                finish_reason: "end_turn".into(),
            },
        ]);

        let db = test_db();
        let model = test_model();
        let deny_all = DenyAll;

        let (text, mutations) = run_agent_loop(
            &mock,
            &db,
            None,
            &model,
            "You are a memory agent.",
            vec![json!({"role": "user", "content": "Save something"})],
            Some(&deny_all),
        )
        .await
        .unwrap();

        assert_eq!(text, "Understood, I won't save that.");
        assert!(mutations.is_empty()); // Denied, nothing saved

        // Verify nothing in DB
        let entries = db.get_entries_by_status("active").unwrap();
        assert!(entries.is_empty());

        // Verify DENIED message was sent to LLM
        let calls = mock.calls.lock().unwrap();
        let second_messages = &calls[1].messages;
        let tool_result = &second_messages[2]["content"][0];
        assert_eq!(tool_result["is_error"], true);
        assert!(tool_result["content"].as_str().unwrap().contains("DENIED"));
    }

    /// Mixed read and write in one turn.
    #[tokio::test]
    async fn mixed_read_write_in_one_turn() {
        let mock = MockAgentLlm::new(vec![
            // Search + create in same turn
            AgentLlmResponse {
                text: String::new(),
                content_blocks: vec![
                    ContentBlock::ToolUse {
                        id: "tu_read".into(),
                        name: "search_entries".into(),
                        input: json!({"query": "chocolate"}),
                    },
                    ContentBlock::ToolUse {
                        id: "tu_write".into(),
                        name: "create_entry".into(),
                        input: json!({
                            "summary_text": "New fact about chocolate",
                            "reason": "test"
                        }),
                    },
                ],
                finish_reason: "tool_use".into(),
            },
            // Final text
            AgentLlmResponse {
                text: "Done.".into(),
                content_blocks: vec![ContentBlock::Text {
                    text: "Done.".into(),
                }],
                finish_reason: "end_turn".into(),
            },
        ]);

        let db = test_db();
        let model = test_model();

        let (text, mutations) = run_agent_loop(
            &mock,
            &db,
            None,
            &model,
            "system",
            vec![json!({"role": "user", "content": "test"})],
            None,
        )
        .await
        .unwrap();

        assert_eq!(text, "Done.");
        assert_eq!(mutations.len(), 1);

        // Both tool results should be in the conversation
        let calls = mock.calls.lock().unwrap();
        let second_messages = &calls[1].messages;
        let tool_results = &second_messages[2]["content"];
        assert_eq!(tool_results.as_array().unwrap().len(), 2);
    }
}
