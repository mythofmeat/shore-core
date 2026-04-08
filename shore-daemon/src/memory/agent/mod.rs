//! Memory management agent — interactive and one-shot modes.
//!
//! Provides an LLM-backed agent that queries and mutates the memory database
//! using 9 tools in an inner loop for up to 40 iterations.
//!
//! Ported from V1 `memory_agent.py`.

pub mod prompt;
pub mod tool_handlers;
pub mod tool_loop;
pub mod tool_schemas;
pub mod types;

use serde_json::{json, Value};
use tracing::{debug, info, instrument};

use crate::memory::agent_llm::AgentLlm;
use crate::memory::db::MemoryDB;
use shore_config::models::ResolvedModel;

pub use types::{
    AgentError, AgentIndexer, AgentMode, AgentRag, AgentSearchContext, CallerIdentity,
    ConfirmCallback, ProposedOperation, RagHit, RealAgentIndexer, ToolResult,
};

// ---------------------------------------------------------------------------
// Legacy types — kept for Phase 4 migration compatibility
// ---------------------------------------------------------------------------

/// A structured result from a one-shot memory query (legacy RAG path).
#[derive(Debug, Clone)]
pub struct MemoryQueryResult {
    pub entries: Vec<RetrievedEntry>,
    pub query_text: String,
    pub resolved_query: String,
}

/// A single entry returned from a memory query (legacy RAG path).
#[derive(Debug, Clone)]
pub struct RetrievedEntry {
    pub entry_id: String,
    pub summary_text: String,
    pub memory_type: String,
    pub confidence: f64,
    pub relevance_score: f64,
}

// ---------------------------------------------------------------------------
// Pronoun resolution
// ---------------------------------------------------------------------------

/// Resolve first-person pronouns in a query based on caller identity.
///
/// When the caller is `Char`, "I"/"me"/"my" refer to the character.
/// When the caller is `User`, "I"/"me"/"my" refer to the user.
pub fn resolve_pronouns(query: &str, caller: CallerIdentity, name: &str) -> String {
    let mut result = query.to_string();

    // Replace whole-word first-person pronouns with the caller's name.
    // Both caller variants resolve the same way — we just use the provided name.
    let _ = caller;
    let replacements: &[(&str, &str)] = &[
        ("my ", &format!("{name}'s ")),
        ("My ", &format!("{name}'s ")),
        ("I ", &format!("{name} ")),
        (" me ", &format!(" {name} ")),
        (" me.", &format!(" {name}.")),
        (" me?", &format!(" {name}?")),
        (" me!", &format!(" {name}!")),
        ("myself", name),
    ];

    for &(pattern, replacement) in replacements {
        result = result.replace(pattern, replacement);
    }

    result
}

// ---------------------------------------------------------------------------
// MemoryAgent
// ---------------------------------------------------------------------------

/// LLM-backed agent for memory management.
///
/// Two entry points:
/// - `ask()` — one-shot, no confirmation (used by researcher)
/// - `run_query()` — with history and optional confirmation (used by engine and memory shell)
pub struct MemoryAgent {
    /// Who is calling the agent.
    caller: CallerIdentity,
    /// The name to substitute for first-person pronouns.
    caller_name: String,
    /// Operating mode.
    mode: AgentMode,
    /// Rendered system prompt.
    system_prompt: String,
}

impl MemoryAgent {
    /// Create a new memory agent for one-shot tool calls.
    pub fn one_shot(caller: CallerIdentity, caller_name: &str, user_name: &str) -> Self {
        let (char_name, u_name) = match caller {
            CallerIdentity::Char => (caller_name, user_name),
            CallerIdentity::User => (user_name, caller_name),
        };
        let system_prompt = prompt::render_system_prompt(char_name, u_name);

        Self {
            caller,
            caller_name: caller_name.to_string(),
            mode: AgentMode::OneShot,
            system_prompt,
        }
    }

    /// Create a new memory agent for an interactive session (stub).
    pub fn interactive(caller: CallerIdentity, caller_name: &str, user_name: &str) -> Self {
        let (char_name, u_name) = match caller {
            CallerIdentity::Char => (caller_name, user_name),
            CallerIdentity::User => (user_name, caller_name),
        };
        let system_prompt = prompt::render_system_prompt(char_name, u_name);

        Self {
            caller,
            caller_name: caller_name.to_string(),
            mode: AgentMode::Interactive,
            system_prompt,
        }
    }

    pub fn caller(&self) -> CallerIdentity {
        self.caller
    }

    pub fn caller_name(&self) -> &str {
        &self.caller_name
    }

    pub fn mode(&self) -> AgentMode {
        self.mode
    }

    /// One-shot mode: answer a question about the memory database.
    ///
    /// No confirmation flow — all writes are auto-accepted.
    #[instrument(skip(self, llm, db, indexer, search_ctx, question), fields(caller = %self.caller_name, model = %model.qualified_name, question_len = question.len()))]
    pub async fn ask(
        &self,
        question: &str,
        llm: &dyn AgentLlm,
        db: &MemoryDB,
        indexer: Option<&dyn AgentIndexer>,
        search_ctx: Option<&AgentSearchContext>,
        model: &ResolvedModel,
    ) -> Result<String, AgentError> {
        info!(caller = %self.caller_name, caller_type = ?self.caller, question_len = question.len(), "Memory agent ask started");
        let messages = vec![json!({"role": "user", "content": question})];
        let (text, _mutations) = tool_loop::run_agent_loop(
            llm,
            db,
            indexer,
            search_ctx,
            model,
            &self.system_prompt,
            messages,
            None, // no confirmation
        )
        .await?;
        debug!(response_len = text.len(), "Memory agent ask complete");
        Ok(text)
    }

    /// Process a user query with conversation history.
    ///
    /// Used by the researcher and engine. Returns a mutations summary string.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_query(
        &self,
        content: &str,
        history: &mut Vec<Value>,
        llm: &dyn AgentLlm,
        db: &MemoryDB,
        indexer: Option<&dyn AgentIndexer>,
        search_ctx: Option<&AgentSearchContext>,
        model: &ResolvedModel,
        confirm_callback: Option<&dyn ConfirmCallback>,
    ) -> Result<String, AgentError> {
        debug!(caller = %self.caller_name, mode = ?self.mode, content_len = content.len(), "Memory agent run_query started");
        history.push(json!({"role": "user", "content": content}));

        let (text, mutations) = tool_loop::run_agent_loop(
            llm,
            db,
            indexer,
            search_ctx,
            model,
            &self.system_prompt,
            history.clone(),
            confirm_callback,
        )
        .await?;

        debug!(mutations = mutations.len(), response_len = text.len(), "Memory agent run_query complete");
        history.push(json!({"role": "assistant", "content": text}));

        if mutations.is_empty() {
            Ok(String::new())
        } else {
            Ok(mutations.join("; "))
        }
    }

    // ------------------------------------------------------------------
    // Legacy RAG-only path — used by current tool handlers until Phase 4
    // ------------------------------------------------------------------

    /// Legacy one-shot query via RAG. Will be removed in Phase 4.
    pub async fn query(
        &self,
        request: &str,
        rag: &dyn AgentRag,
        db: &MemoryDB,
    ) -> Result<MemoryQueryResult, AgentError> {
        if self.mode == AgentMode::Interactive {
            return Err(AgentError::Llm(
                "legacy query() not available in interactive mode".to_string(),
            ));
        }

        let resolved = resolve_pronouns(request, self.caller, &self.caller_name);
        debug!(caller = %self.caller_name, original = request, resolved = %resolved, "Legacy RAG query");
        let hits = rag.query(&resolved, 32).await?;

        let mut entries = Vec::new();
        for hit in &hits {
            if let Some(entry) = db
                .get_entry(&hit.entry_id)
                .map_err(|e| AgentError::Db(e.to_string()))?
            {
                entries.push(RetrievedEntry {
                    entry_id: entry.id,
                    summary_text: entry.summary_text,
                    memory_type: entry.memory_type,
                    confidence: entry.confidence,
                    relevance_score: hit.score,
                });
            }
        }

        debug!(hits = hits.len(), entries = entries.len(), "Legacy RAG query complete");
        Ok(MemoryQueryResult {
            entries,
            query_text: request.to_string(),
            resolved_query: resolved,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Tests: caller identity -----------------------------------------------

    #[test]
    fn test_caller_identity_char_mode() {
        let agent = MemoryAgent::one_shot(CallerIdentity::Char, "Alice", "Bob");
        assert_eq!(agent.caller(), CallerIdentity::Char);
        assert_eq!(agent.caller_name(), "Alice");
        assert_eq!(agent.mode(), AgentMode::OneShot);
    }

    #[test]
    fn test_caller_identity_user_mode() {
        let agent = MemoryAgent::interactive(CallerIdentity::User, "Bob", "Alice");
        assert_eq!(agent.caller(), CallerIdentity::User);
        assert_eq!(agent.caller_name(), "Bob");
        assert_eq!(agent.mode(), AgentMode::Interactive);
    }

    // -- Tests: pronoun resolution --------------------------------------------

    #[test]
    fn test_pronoun_resolution_char_caller() {
        let resolved = resolve_pronouns("What do I like to eat?", CallerIdentity::Char, "Alice");
        assert_eq!(resolved, "What do Alice like to eat?");
    }

    #[test]
    fn test_pronoun_resolution_user_caller() {
        let resolved = resolve_pronouns("What do I like to eat?", CallerIdentity::User, "Bob");
        assert_eq!(resolved, "What do Bob like to eat?");
    }

    #[test]
    fn test_pronoun_resolution_my() {
        let resolved = resolve_pronouns("my favorite color", CallerIdentity::Char, "Alice");
        assert_eq!(resolved, "Alice's favorite color");
    }

    #[test]
    fn test_pronoun_resolution_me() {
        let resolved = resolve_pronouns("tell me about me.", CallerIdentity::User, "Bob");
        assert_eq!(resolved, "tell Bob about Bob.");
    }

    #[test]
    fn test_pronoun_resolution_no_pronouns() {
        let resolved = resolve_pronouns("What does Alice like?", CallerIdentity::Char, "Alice");
        assert_eq!(resolved, "What does Alice like?");
    }

    #[test]
    fn test_pronoun_resolution_myself() {
        let resolved = resolve_pronouns("things about myself", CallerIdentity::User, "Bob");
        assert_eq!(resolved, "things about Bob");
    }

    // -- Tests: one-shot ask with mock LLM ------------------------------------

    #[tokio::test]
    async fn test_ask_returns_text() {
        use crate::memory::agent_llm::{AgentLlmResponse, MockAgentLlm};
        use shore_config::models::Sdk;
        use shore_llm_client::types::ContentBlock;

        let mock = MockAgentLlm::new(vec![AgentLlmResponse {
            text: "No relevant memories found.".into(),
            content_blocks: vec![ContentBlock::Text {
                text: "No relevant memories found.".into(),
            }],
            finish_reason: "end_turn".into(),
        }]);

        let db = MemoryDB::open_in_memory().unwrap();
        let model = ResolvedModel {
            name: "test".into(),
            qualified_name: "chat.test".into(),
            category: "chat".into(),
            provider_key: "anthropic".into(),
            sdk: Sdk::Anthropic,
            model_id: "claude-test".into(),
            api_key_env: Some("TEST_KEY".into()),
            base_url: None,
            max_context_tokens: None,
            max_tokens: Some(4096),
            temperature: Some(0.7),
            top_p: None,
            reasoning_effort: None,
            budget_tokens: None,
            cache_ttl: None,
            keepalive_enabled: None,
            keepalive_ttl: None,
            keepalive_max_pings: None,
            openrouter_provider: None,
            vertex_project: None,
            vertex_location: None,
            gemini_generation: None,
            gemini_web_search: None,
            zai_clear_thinking: None,
            zai_subscription: None,
        };

        let agent = MemoryAgent::one_shot(CallerIdentity::Char, "Alice", "Bob");
        let result = agent
            .ask(
                "What do I know about chocolate?",
                &mock,
                &db,
                None,
                None,
                &model,
            )
            .await
            .unwrap();

        assert_eq!(result, "No relevant memories found.");
    }
}
