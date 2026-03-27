//! Memory researcher — cheap-model tier that synthesizes memory queries.
//!
//! Port of V1's `MemoryResearcher` from `tool_use.py`. Uses a cheap model
//! with a single `ask_memory_agent` tool to query the inner MemoryAgent,
//! then synthesizes results for the character.

use tracing::{info, warn};
use serde_json::{json, Value};

use crate::config::models::ResolvedModel;
use crate::llm_client::types::ContentBlock;
use crate::memory::agent::types::{AgentError, AgentIndexer};
use crate::memory::agent::MemoryAgent;
use crate::memory::agent_llm::AgentLlm;
use crate::memory::db::MemoryDB;

const MAX_RESEARCHER_ITERATIONS: usize = 15;

const NO_RESULTS: &str = "No relevant memories found.";

const RESEARCHER_SYSTEM_PROMPT: &str = "\
You are a memory research agent. You have access to a memory database \
through a natural-language query tool. Your job is to fulfill the \
primary character's memory request by querying the database, chasing \
leads, cross-referencing results, and returning a clear synthesis.

SEARCH PHASE:
- Ask focused questions to find relevant memories.
- If initial results mention related people, events, or topics, ask \
follow-up questions to find those too.
- For requests about current state or latest info, verify there isn't \
a newer entry that supersedes older ones.
- Stop when results are repeating or you've covered the topic.

SAVE/UPDATE REQUESTS:
When the request is to save or update a memory, do BOTH of these:
1. **Search first** — look up existing memories on the same topic \
before saving. This lets the memory agent deduplicate or update \
existing entries instead of blindly creating new ones.
2. **Pass the save/update through** to the memory agent.

RESPONSE PHASE:
Return a plain-text synthesis of what you found. Include entry IDs \
so the caller can reference specific memories.
- For pure lookups: synthesize the results.
- For save/update requests: confirm what was done AND include any \
related prior context on the topic that the caller might find useful. \
The caller does not automatically see what's already in memory, so \
surfacing related existing knowledge helps them stay informed without \
having to issue a separate lookup.
- If nothing relevant was found, say so clearly.

PRONOUN RULES:
In your synthesis, always refer to the <character> and <user> by name, \
never as \"you\". The primary character (your caller) will read your \
response and inject it into a roleplay conversation — ambiguous \"you\" \
causes confusion between the character and the user.";

/// The single tool available to the researcher.
fn ask_memory_agent_tool() -> Value {
    json!({
        "name": "ask_memory_agent",
        "description": "Query the memory database in natural language. The memory agent can search, browse, create, update, and supersede memory entries. Ask it anything about the stored memories.",
        "input_schema": {
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "Your question or request for the memory database"
                }
            },
            "required": ["question"]
        }
    })
}

/// Cheap-model tier that drives the inner MemoryAgent to fulfill memory requests.
pub struct MemoryResearcher {
    char_definition: String,
    user_description: String,
}

impl MemoryResearcher {
    pub fn new(char_definition: String, user_description: String) -> Self {
        Self {
            char_definition,
            user_description,
        }
    }

    /// Research a memory request via the cheap model + memory agent.
    ///
    /// The researcher calls the inner agent's `ask()` method through a single
    /// `ask_memory_agent` tool, then synthesizes the results.
    pub async fn research(
        &self,
        request: &str,
        researcher_llm: &dyn AgentLlm,
        researcher_model: &ResolvedModel,
        agent: &MemoryAgent,
        agent_llm: &dyn AgentLlm,
        agent_model: &ResolvedModel,
        db: &MemoryDB,
        indexer: Option<&dyn AgentIndexer>,
    ) -> Result<String, AgentError> {
        // Build the first user message with character/user context.
        let mut context_parts: Vec<String> = Vec::new();
        if !self.char_definition.is_empty() {
            context_parts.push(format!(
                "<character>\n{}\n</character>",
                self.char_definition
            ));
        }
        if !self.user_description.is_empty() {
            context_parts.push(format!("<user>\n{}\n</user>", self.user_description));
        }

        let user_content = if context_parts.is_empty() {
            request.to_string()
        } else {
            format!("{}\n\n{}", context_parts.join("\n"), request)
        };

        let mut messages: Vec<Value> = vec![json!({"role": "user", "content": user_content})];

        let tools = vec![ask_memory_agent_tool()];
        let system = Some(Value::String(RESEARCHER_SYSTEM_PROMPT.to_string()));
        let mut all_tool_outputs: Vec<String> = Vec::new();
        let mut last_text = String::new();

        for iteration in 0..MAX_RESEARCHER_ITERATIONS {
            let response = researcher_llm
                .generate(
                    messages.clone(),
                    system.clone(),
                    Some(tools.clone()),
                    researcher_model,
                )
                .await
                .map_err(|e| AgentError::Llm(e.to_string()))?;

            last_text = response.text.clone();

            // Extract tool_use blocks
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

            // No tool calls → final response
            if tool_uses.is_empty() {
                break;
            }

            // Check for refusal mid-loop
            if !response.text.is_empty() && is_refusal(&response.text) {
                warn!(
                    "Memory researcher refused mid-loop (iteration {}): {}",
                    iteration,
                    &response.text[..response.text.len().min(200)]
                );
                break;
            }

            // Build assistant message from content_blocks
            let assistant_content: Vec<Value> = response
                .content_blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => json!({"type": "text", "text": text}),
                    ContentBlock::ToolUse { id, name, input } => {
                        json!({"type": "tool_use", "id": id, "name": name, "input": input})
                    }
                    ContentBlock::Thinking { thinking } => {
                        json!({"type": "thinking", "thinking": thinking})
                    }
                    ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                        let mut v = json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": content});
                        if *is_error { v["is_error"] = json!(true); }
                        v
                    }
                })
                .collect();

            messages.push(json!({"role": "assistant", "content": assistant_content}));

            // Execute tool calls
            let mut tool_results: Vec<Value> = Vec::new();

            for (tool_id, tool_name, tool_input) in &tool_uses {
                let result_text = if tool_name == "ask_memory_agent" {
                    let question = tool_input["question"].as_str().unwrap_or("");
                    match agent
                        .ask(question, agent_llm, db, indexer, agent_model)
                        .await
                    {
                        Ok(text) => text,
                        Err(e) => format!("Error: {e}"),
                    }
                } else {
                    format!("Error: unknown tool '{tool_name}'.")
                };

                info!(
                    "Memory researcher tool: {}({}) -> {}",
                    tool_name,
                    tool_input,
                    if result_text.len() > 200 {
                        &result_text[..200]
                    } else {
                        &result_text
                    }
                );

                all_tool_outputs.push(result_text.clone());
                tool_results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": tool_id,
                    "content": result_text,
                }));
            }

            messages.push(json!({"role": "user", "content": tool_results}));
        }

        // Return final synthesis
        let final_text = last_text.trim().to_string();
        if !final_text.is_empty() && !is_refusal(&final_text) {
            return Ok(final_text);
        }

        if !final_text.is_empty() && is_refusal(&final_text) {
            warn!(
                "Memory researcher refused, falling back to raw results: {}",
                &final_text[..final_text.len().min(200)]
            );
        }

        // Fall back to raw tool outputs
        if all_tool_outputs.is_empty() {
            return Ok(NO_RESULTS.to_string());
        }

        let parts: Vec<&str> = all_tool_outputs
            .iter()
            .map(|s| s.as_str())
            .filter(|s| !s.is_empty() && *s != NO_RESULTS)
            .collect();

        if parts.is_empty() {
            Ok(NO_RESULTS.to_string())
        } else {
            Ok(parts.join("\n\n"))
        }
    }
}

/// Simple refusal detection — matches common safety refusal patterns.
fn is_refusal(text: &str) -> bool {
    let lower = text.to_lowercase();
    let refusal_phrases = [
        "i can't assist",
        "i cannot assist",
        "i'm not able to",
        "i am not able to",
        "i can't help with",
        "i cannot help with",
        "i'm unable to",
        "i am unable to",
        "as an ai",
        "as a language model",
    ];
    refusal_phrases.iter().any(|phrase| lower.contains(phrase))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::agent::CallerIdentity;
    use crate::memory::agent_llm::{AgentLlmResponse, MockAgentLlm};
    use crate::memory::db::MemoryDB;
    use crate::test_support::test_model;

    /// Single ask_memory_agent call → synthesis.
    #[tokio::test]
    async fn single_query_synthesis() {
        // Researcher LLM: calls ask_memory_agent, then synthesizes
        let researcher_mock = MockAgentLlm::new(vec![
            // First: request ask_memory_agent
            AgentLlmResponse {
                text: String::new(),
                content_blocks: vec![ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "ask_memory_agent".into(),
                    input: json!({"question": "What does Alice like?"}),
                }],
                finish_reason: "tool_use".into(),
            },
            // Second: synthesize
            AgentLlmResponse {
                text: "Based on the memory database, Alice likes chocolate (entry e1).".into(),
                content_blocks: vec![ContentBlock::Text {
                    text: "Based on the memory database, Alice likes chocolate (entry e1).".into(),
                }],
                finish_reason: "end_turn".into(),
            },
        ]);

        // Agent LLM: returns search results when asked
        let agent_mock = MockAgentLlm::new(vec![AgentLlmResponse {
            text: "Alice likes chocolate according to entry e1.".into(),
            content_blocks: vec![ContentBlock::Text {
                text: "Alice likes chocolate according to entry e1.".into(),
            }],
            finish_reason: "end_turn".into(),
        }]);

        let db = MemoryDB::open_in_memory().unwrap();
        let agent = MemoryAgent::one_shot(CallerIdentity::Char, "Alice", "Bob");
        let researcher = MemoryResearcher::new(String::new(), String::new());

        let result = researcher
            .research(
                "What does Alice like?",
                &researcher_mock,
                &test_model(),
                &agent,
                &agent_mock,
                &test_model(),
                &db,
                None,
            )
            .await
            .unwrap();

        assert!(result.contains("chocolate"));
        assert_eq!(researcher_mock.call_count(), 2);
        assert_eq!(agent_mock.call_count(), 1);
    }

    /// Multiple ask_memory_agent calls (chasing leads).
    #[tokio::test]
    async fn multiple_queries_synthesis() {
        let researcher_mock = MockAgentLlm::new(vec![
            // First: ask about food
            AgentLlmResponse {
                text: String::new(),
                content_blocks: vec![ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "ask_memory_agent".into(),
                    input: json!({"question": "What food does Alice like?"}),
                }],
                finish_reason: "tool_use".into(),
            },
            // Second: follow-up about chocolate specifically
            AgentLlmResponse {
                text: String::new(),
                content_blocks: vec![ContentBlock::ToolUse {
                    id: "tu_2".into(),
                    name: "ask_memory_agent".into(),
                    input: json!({"question": "Tell me more about Alice and chocolate"}),
                }],
                finish_reason: "tool_use".into(),
            },
            // Third: synthesize
            AgentLlmResponse {
                text: "Alice likes dark chocolate, especially from Belgium.".into(),
                content_blocks: vec![ContentBlock::Text {
                    text: "Alice likes dark chocolate, especially from Belgium.".into(),
                }],
                finish_reason: "end_turn".into(),
            },
        ]);

        let agent_mock = MockAgentLlm::new(vec![
            AgentLlmResponse {
                text: "Alice likes chocolate.".into(),
                content_blocks: vec![ContentBlock::Text {
                    text: "Alice likes chocolate.".into(),
                }],
                finish_reason: "end_turn".into(),
            },
            AgentLlmResponse {
                text: "Alice prefers dark chocolate from Belgium.".into(),
                content_blocks: vec![ContentBlock::Text {
                    text: "Alice prefers dark chocolate from Belgium.".into(),
                }],
                finish_reason: "end_turn".into(),
            },
        ]);

        let db = MemoryDB::open_in_memory().unwrap();
        let agent = MemoryAgent::one_shot(CallerIdentity::Char, "Alice", "Bob");
        let researcher = MemoryResearcher::new(String::new(), String::new());

        let result = researcher
            .research(
                "What food does Alice like?",
                &researcher_mock,
                &test_model(),
                &agent,
                &agent_mock,
                &test_model(),
                &db,
                None,
            )
            .await
            .unwrap();

        assert!(result.contains("dark chocolate"));
        assert!(result.contains("Belgium"));
        assert_eq!(researcher_mock.call_count(), 3);
        assert_eq!(agent_mock.call_count(), 2);
    }

    /// Refusal → falls back to raw outputs.
    #[tokio::test]
    async fn refusal_falls_back_to_raw() {
        let researcher_mock = MockAgentLlm::new(vec![
            // First: ask memory agent
            AgentLlmResponse {
                text: String::new(),
                content_blocks: vec![ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "ask_memory_agent".into(),
                    input: json!({"question": "test query"}),
                }],
                finish_reason: "tool_use".into(),
            },
            // Second: refuse
            AgentLlmResponse {
                text: "I can't assist with that request.".into(),
                content_blocks: vec![ContentBlock::Text {
                    text: "I can't assist with that request.".into(),
                }],
                finish_reason: "end_turn".into(),
            },
        ]);

        let agent_mock = MockAgentLlm::new(vec![AgentLlmResponse {
            text: "Raw agent output about the topic.".into(),
            content_blocks: vec![ContentBlock::Text {
                text: "Raw agent output about the topic.".into(),
            }],
            finish_reason: "end_turn".into(),
        }]);

        let db = MemoryDB::open_in_memory().unwrap();
        let agent = MemoryAgent::one_shot(CallerIdentity::Char, "Alice", "Bob");
        let researcher = MemoryResearcher::new(String::new(), String::new());

        let result = researcher
            .research(
                "test",
                &researcher_mock,
                &test_model(),
                &agent,
                &agent_mock,
                &test_model(),
                &db,
                None,
            )
            .await
            .unwrap();

        // Should fall back to the raw agent output
        assert!(result.contains("Raw agent output"));
    }

    /// No tool calls at all → returns text directly.
    #[tokio::test]
    async fn immediate_text_response() {
        let researcher_mock = MockAgentLlm::new(vec![AgentLlmResponse {
            text: "I don't need to query the database for this.".into(),
            content_blocks: vec![ContentBlock::Text {
                text: "I don't need to query the database for this.".into(),
            }],
            finish_reason: "end_turn".into(),
        }]);

        let agent_mock = MockAgentLlm::new(vec![]);

        let db = MemoryDB::open_in_memory().unwrap();
        let agent = MemoryAgent::one_shot(CallerIdentity::Char, "Alice", "Bob");
        let researcher = MemoryResearcher::new(String::new(), String::new());

        let result = researcher
            .research(
                "test",
                &researcher_mock,
                &test_model(),
                &agent,
                &agent_mock,
                &test_model(),
                &db,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result, "I don't need to query the database for this.");
        assert_eq!(agent_mock.call_count(), 0);
    }

    #[test]
    fn test_refusal_detection() {
        assert!(is_refusal("I can't assist with that request."));
        assert!(is_refusal("As an AI, I cannot do that."));
        assert!(is_refusal("I'm not able to help with this."));
        assert!(!is_refusal("Here are the search results."));
        assert!(!is_refusal("No relevant memories found."));
    }

    #[test]
    fn test_context_prepended() {
        let researcher = MemoryResearcher::new(
            "Alice is a kind person".into(),
            "Bob is the user".into(),
        );
        assert!(!researcher.char_definition.is_empty());
        assert!(!researcher.user_description.is_empty());
    }
}
