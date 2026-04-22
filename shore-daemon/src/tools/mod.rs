pub mod activity;
pub mod basic;
pub(crate) mod context;
pub mod images;
pub mod memory_tools;
pub mod scratchpad;
pub mod web;
pub mod workspace;

use crate::autonomy::manager::AutonomyManager;
use crate::memory::agent::types::{AgentIndexer, AgentSearchContext};
use crate::memory::agent::{AgentError, AgentRag, MemoryAgent};
use crate::memory::agent_llm::AgentLlm;
use crate::memory::compaction_impls::ImageGenConfig;
use crate::memory::db::MemoryDB;
use crate::memory::researcher::MemoryResearcher;
use serde_json::Value;
use shore_config::models::ResolvedModel;
use shore_llm_client::LlmClient;
use std::future::Future;
use std::pin::Pin;

// ---------------------------------------------------------------------------
// Tool category — determines privacy filtering
// ---------------------------------------------------------------------------

/// Tool categories for privacy-based filtering.
///
/// When a conversation is private, memory-related tools are excluded from
/// the tool list so the LLM cannot read or write to memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    /// Memory write tools (memory save, send_image, generate_image).
    MemoryWrite,
    /// Memory read tools (list_images w/ RAG, recall_image).
    MemoryRead,
    /// Web/HTTP tools — always available.
    Web,
    /// Other tools (dice, time, activity) — always available.
    Other,
}

impl ToolCategory {
    /// Whether this category is available in private conversations.
    pub fn allowed_in_private(self) -> bool {
        matches!(self, ToolCategory::Web | ToolCategory::Other)
    }
}

// ---------------------------------------------------------------------------
// Tool definition
// ---------------------------------------------------------------------------

/// Static definition of a tool (name, description, JSON Schema, category).
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
    pub category: ToolCategory,
}

// ---------------------------------------------------------------------------
// Tool error
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("invalid args: {0}")]
    InvalidArgs(String),
    #[error("agent: {0}")]
    Agent(#[from] AgentError),
    #[error("{0}: not yet implemented")]
    NotImplemented(String),
    #[error("io: {0}")]
    Io(String),
    #[error("http: {0}")]
    Http(String),
}

// ---------------------------------------------------------------------------
// Tool context trait — dependency injection for tool handlers
// ---------------------------------------------------------------------------

/// Provides access to shared dependencies needed by tool handlers.
///
/// Requires `Sync` so that `&dyn ToolContext` is `Send`, enabling tool handlers
/// to hold the reference across `.await` points in `Send` futures.
pub trait ToolContext: Sync {
    fn memory_db(&self) -> &MemoryDB;
    fn memory_agent(&self) -> &MemoryAgent;
    fn agent_llm(&self) -> &dyn AgentLlm;
    fn agent_model(&self) -> &ResolvedModel;
    fn researcher_llm(&self) -> Option<&dyn AgentLlm>;
    fn researcher_model(&self) -> Option<&ResolvedModel>;
    fn memory_researcher(&self) -> Option<&MemoryResearcher>;
    fn indexer(&self) -> Option<&dyn AgentIndexer>;
    fn image_dir(&self) -> &str;
    fn llm_client(&self) -> Option<&LlmClient>;
    fn image_gen_config(&self) -> Option<&ImageGenConfig>;

    // Legacy RAG — kept for image tools until they're migrated
    fn rag(&self) -> &dyn AgentRag;

    // Web search configuration
    fn search_config(&self) -> &shore_config::app::SearchConfig;

    // Semantic search context (vector + BM25 + embeddings)
    fn search_context(&self) -> Option<&AgentSearchContext> {
        None
    }

    // Autonomy access — used by activity heatmap tool
    fn autonomy_manager(&self) -> Option<&AutonomyManager> {
        None
    }
    fn character_name(&self) -> &str {
        ""
    }

    // Scratchpad directory for per-character scratch storage
    fn scratchpad_dir(&self) -> &str {
        ""
    }

    // Workspace directory for general filesystem tools
    fn workspace_dir(&self) -> &str {
        ""
    }

    // Markdown memory store for inspectable memory files
    fn markdown_store(&self) -> Option<&crate::memory::markdown_store::MarkdownMemoryStore> {
        None
    }
}

// ---------------------------------------------------------------------------
// Tool registry
// ---------------------------------------------------------------------------

/// Returns all registered tool definitions.
pub fn all_tools() -> Vec<ToolDef> {
    let mut tools = Vec::new();
    tools.extend(memory_tools::tool_defs());
    tools.extend(images::tool_defs());
    tools.extend(web::tool_defs());
    tools.extend(activity::tool_defs());
    tools.extend(basic::tool_defs());
    tools.extend(scratchpad::tool_defs());
    tools.extend(workspace::tool_defs());
    tools
}

/// Build the outbound LLM `tools` array from `available_tools`, rendering
/// `{{char}}` / `{{user}}` placeholders in each tool's description through
/// the same template pipeline the capabilities block uses.
///
/// Centralizes what was previously duplicated at every call site (handler +
/// autonomy manager) and guarantees that `{{user}}` in a description
/// actually substitutes instead of shipping literally to the model.
pub fn render_tool_defs(
    is_private: bool,
    toggles: &shore_config::app::ToolToggles,
    char_name: &str,
    user_name: &str,
) -> Vec<Value> {
    use std::collections::HashMap;
    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert("char".into(), char_name.to_string());
    vars.insert("character_name".into(), char_name.to_string());
    vars.insert("user".into(), user_name.to_string());
    available_tools(is_private, toggles)
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": crate::engine::prompt::render_template(t.description, &vars),
                "input_schema": t.parameters.clone(),
            })
        })
        .collect()
}

/// Returns tool definitions available for the current privacy mode and tool toggles.
pub fn available_tools(is_private: bool, toggles: &shore_config::app::ToolToggles) -> Vec<ToolDef> {
    all_tools()
        .into_iter()
        .filter(|t| {
            if is_private && !t.category.allowed_in_private() {
                return false;
            }
            toggles.is_enabled(t.name)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

/// Dispatch a tool call by name to its handler.
pub fn dispatch_tool<'a>(
    name: &'a str,
    input: Value,
    ctx: &'a dyn ToolContext,
) -> Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send + 'a>> {
    Box::pin(async move {
        match name {
            // Memory tools
            "memory" => memory_tools::handle_memory(input, ctx).await,
            "memory_read" => memory_tools::handle_memory_read(input, ctx).await,
            "memory_write" => memory_tools::handle_memory_write(input, ctx).await,
            "memory_search" => memory_tools::handle_memory_search(input, ctx).await,
            "memory_list" => memory_tools::handle_memory_list(input, ctx).await,
            "send_image" => images::handle_send_image(input, ctx).await,
            "list_images" => images::handle_list_images(input, ctx).await,
            "recall_image" => images::handle_recall_image(input, ctx).await,
            "remember_image" => images::handle_remember_image(input, ctx).await,
            "generate_image" => images::handle_generate_image(input, ctx).await,
            // Web tools
            "web_search" => web::handle_web_search(input, ctx).await,
            "fetch_url" => web::handle_fetch_url(input).await,
            // Basic tools
            "check_time" => basic::handle_check_time(input).await,
            "roll_dice" => basic::handle_roll_dice(input).await,
            // Other
            "activity_heatmap" => activity::handle_activity_heatmap(input, ctx).await,
            // Scratchpad tools
            "scratchpad_list" => {
                scratchpad::handle_scratchpad_list(input, ctx.scratchpad_dir()).await
            }
            "scratchpad_read" => {
                scratchpad::handle_scratchpad_read(input, ctx.scratchpad_dir()).await
            }
            "scratchpad_write" => {
                scratchpad::handle_scratchpad_write(input, ctx.scratchpad_dir()).await
            }
            "scratchpad_delete" => {
                scratchpad::handle_scratchpad_delete(input, ctx.scratchpad_dir()).await
            }
            // Workspace tools
            "read" => workspace::handle_read(input, ctx.workspace_dir()).await,
            "write" => workspace::handle_write(input, ctx.workspace_dir()).await,
            "edit" => workspace::handle_edit(input, ctx.workspace_dir()).await,
            "list_files" => workspace::handle_list_files(input, ctx.workspace_dir()).await,
            "exec" => workspace::handle_exec(input, ctx.workspace_dir()).await,
            // set_next_wake is in the base tool set for cache stability but
            // only handled during interiority ticks (intercepted in manager.rs).
            "set_next_wake" => Err(ToolError::InvalidArgs(
                "set_next_wake is only available during interiority ticks".into(),
            )),
            _ => Err(ToolError::NotImplemented(name.to_string())),
        }
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestToolContext;
    use shore_config::app::ToolToggles;

    #[test]
    fn render_tool_defs_substitutes_user_placeholder() {
        // {{user}} appears in scratchpad_delete and must resolve, not ship
        // literal to the model.
        let toggles = ToolToggles::default();
        let defs = render_tool_defs(false, &toggles, "qifei", "ren");
        let delete = defs
            .iter()
            .find(|d| d["name"] == "scratchpad_delete")
            .expect("scratchpad_delete present");
        let desc = delete["description"].as_str().unwrap();
        assert!(
            !desc.contains("{{user}}"),
            "{{{{user}}}} must be substituted, got: {desc}"
        );
        assert!(
            desc.contains("ren"),
            "substituted name 'ren' must appear in description, got: {desc}"
        );
    }

    #[test]
    fn render_tool_defs_substitutes_char_placeholder() {
        // Future-proof guard: {{char}} also resolves through render_tool_defs.
        let toggles = ToolToggles::default();
        let defs = render_tool_defs(false, &toggles, "qifei", "ren");
        for def in &defs {
            let desc = def["description"].as_str().unwrap();
            assert!(
                !desc.contains("{{char}}") && !desc.contains("{{user}}"),
                "tool {} has unsubstituted placeholder: {desc}",
                def["name"]
            );
        }
    }

    #[test]
    fn test_all_tools_returns_expected_count() {
        let tools = all_tools();
        // memory(5) + images(5) + web(2) + activity(1) + basic(2) + scratchpad(4) + workspace(5) = 25
        assert_eq!(tools.len(), 25);
    }

    #[test]
    fn test_available_tools_filters_private() {
        let toggles = ToolToggles::default();
        let all = all_tools();
        let private = available_tools(true, &toggles);
        let public = available_tools(false, &toggles);

        assert_eq!(public.len(), all.len());
        assert!(private.len() < public.len());

        // All private tools should be Web or Other category.
        for tool in &private {
            assert!(
                tool.category.allowed_in_private(),
                "tool {} should not be available in private mode",
                tool.name
            );
        }
    }

    #[test]
    fn test_private_excludes_memory_tools() {
        let toggles = ToolToggles::default();
        let private = available_tools(true, &toggles);
        let private_names: Vec<&str> = private.iter().map(|t| t.name).collect();

        // Memory tools should be excluded.
        assert!(!private_names.contains(&"memory"));
        assert!(!private_names.contains(&"memory_read"));
        assert!(!private_names.contains(&"memory_write"));
        assert!(!private_names.contains(&"memory_search"));
        assert!(!private_names.contains(&"memory_list"));
        assert!(!private_names.contains(&"send_image"));
        assert!(!private_names.contains(&"list_images"));
        assert!(!private_names.contains(&"recall_image"));
        assert!(!private_names.contains(&"generate_image"));
        assert!(!private_names.contains(&"remember_image"));

        // Web and other tools should remain.
        assert!(private_names.contains(&"web_search"));
        assert!(private_names.contains(&"fetch_url"));
        assert!(private_names.contains(&"activity_heatmap"));
    }

    #[test]
    fn test_tool_toggles_filter() {
        let mut toggles = ToolToggles::default();
        toggles.set("roll_dice", false);
        toggles.set("web_search", false);

        let tools = available_tools(false, &toggles);
        let names: Vec<&str> = tools.iter().map(|t| t.name).collect();

        assert!(!names.contains(&"roll_dice"));
        assert!(!names.contains(&"web_search"));
        assert!(names.contains(&"memory"));
        assert!(names.contains(&"check_time"));
        assert_eq!(tools.len(), 23); // 25 - 2 disabled
    }

    #[test]
    fn test_tool_category_allowed_in_private() {
        assert!(!ToolCategory::MemoryWrite.allowed_in_private());
        assert!(!ToolCategory::MemoryRead.allowed_in_private());
        assert!(ToolCategory::Web.allowed_in_private());
        assert!(ToolCategory::Other.allowed_in_private());
    }

    #[test]
    fn test_tool_names_unique() {
        let tools = all_tools();
        let mut names: Vec<&str> = tools.iter().map(|t| t.name).collect();
        let original_len = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), original_len, "duplicate tool names found");
    }

    // ── dispatch_tool tests ───────────────────────────────────────────

    #[tokio::test]
    async fn test_dispatch_check_time() {
        let ctx = TestToolContext::new();
        let result = dispatch_tool("check_time", serde_json::json!({}), &ctx).await;
        assert!(result.is_ok(), "check_time should succeed");
        let val = result.unwrap();
        // Should contain a datetime string.
        assert!(val.get("time").is_some() || val.is_string());
    }

    #[tokio::test]
    async fn test_dispatch_roll_dice() {
        let ctx = TestToolContext::new();
        let result = dispatch_tool("roll_dice", serde_json::json!({"notation": "2d6"}), &ctx).await;
        assert!(result.is_ok(), "roll_dice should succeed");
    }

    #[tokio::test]
    async fn test_dispatch_unknown_tool() {
        let ctx = TestToolContext::new();
        let result = dispatch_tool("nonexistent_tool", serde_json::json!({}), &ctx).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ToolError::NotImplemented(_)),
            "unknown tool should return NotImplemented, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_dispatch_send_image_invalid_args() {
        let ctx = TestToolContext::new();
        // Missing required "path" arg — handler should return InvalidArgs, not NotImplemented.
        let result = dispatch_tool("send_image", serde_json::json!({}), &ctx).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            !matches!(err, ToolError::NotImplemented(_)),
            "send_image with bad args should reach handler, not return NotImplemented"
        );
    }

    #[tokio::test]
    async fn test_dispatch_fetch_url_routes_correctly() {
        let ctx = TestToolContext::new();
        // Invalid URL — handler should return an error (not NotImplemented).
        let result = dispatch_tool(
            "fetch_url",
            serde_json::json!({"url": "not-a-valid-url"}),
            &ctx,
        )
        .await;
        // May succeed or fail, but should NOT be NotImplemented.
        if let Err(ref e) = result {
            assert!(
                !matches!(e, ToolError::NotImplemented(_)),
                "fetch_url should reach handler"
            );
        }
    }

    #[tokio::test]
    async fn test_dispatch_memory_routes_correctly() {
        let ctx = TestToolContext::new();
        let result = dispatch_tool(
            "memory",
            serde_json::json!({"request": "search for test"}),
            &ctx,
        )
        .await;
        // The handler may return Ok or an error from the agent, but NOT NotImplemented.
        if let Err(ref e) = result {
            assert!(
                !matches!(e, ToolError::NotImplemented(_)),
                "memory should reach handler, got: {e}"
            );
        }
    }

    #[tokio::test]
    async fn test_dispatch_all_registered_names_route() {
        let ctx = TestToolContext::new();
        let tools = all_tools();

        for tool in &tools {
            let result = dispatch_tool(tool.name, serde_json::json!({}), &ctx).await;
            // Every registered tool name should route to a handler.
            // The handler may succeed or fail with InvalidArgs/Agent/etc,
            // but must NOT return NotImplemented.
            if let Err(ref e) = result {
                assert!(
                    !matches!(e, ToolError::NotImplemented(_)),
                    "registered tool '{}' returned NotImplemented — dispatch arm missing",
                    tool.name
                );
            }
        }
    }

    #[tokio::test]
    async fn test_dispatch_scratchpad_routes_correctly() {
        let ctx = TestToolContext::new();
        // scratchpad_dir is "" by default, which the handler rejects — but that
        // proves the dispatch routed to the handler (not NotImplemented).
        let result = dispatch_tool("scratchpad_list", serde_json::json!({}), &ctx).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            !matches!(err, ToolError::NotImplemented(_)),
            "scratchpad_list should reach handler"
        );
    }
}
