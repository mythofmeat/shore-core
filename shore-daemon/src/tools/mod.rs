pub mod activity;
pub mod images;
pub mod memory_tools;
pub mod web;

use crate::memory::agent::{AgentError, AgentIndexer, AgentRag, MemoryAgent};
use crate::memory::db::MemoryDB;
use serde_json::Value;
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

#[derive(Debug)]
pub enum ToolError {
    InvalidArgs(String),
    Agent(AgentError),
    NotImplemented(String),
    Io(String),
    Http(String),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::InvalidArgs(e) => write!(f, "invalid args: {e}"),
            ToolError::Agent(e) => write!(f, "agent: {e}"),
            ToolError::NotImplemented(name) => write!(f, "{name}: not yet implemented"),
            ToolError::Io(e) => write!(f, "io: {e}"),
            ToolError::Http(e) => write!(f, "http: {e}"),
        }
    }
}

impl std::error::Error for ToolError {}

impl From<AgentError> for ToolError {
    fn from(e: AgentError) -> Self {
        ToolError::Agent(e)
    }
}

// ---------------------------------------------------------------------------
// Tool context trait — dependency injection for tool handlers
// ---------------------------------------------------------------------------

/// Provides access to shared dependencies needed by tool handlers.
/// Not `Send + Sync` because `MemoryDB` (rusqlite) is not `Sync`.
pub trait ToolContext {
    fn memory_db(&self) -> &MemoryDB;
    fn memory_agent(&self) -> &MemoryAgent;
    fn rag(&self) -> &dyn AgentRag;
    fn indexer(&self) -> &dyn AgentIndexer;
    fn image_dir(&self) -> &str;
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
    tools
}

/// Returns tool definitions available for the current privacy mode.
pub fn available_tools(is_private: bool) -> Vec<ToolDef> {
    if is_private {
        all_tools()
            .into_iter()
            .filter(|t| t.category.allowed_in_private())
            .collect()
    } else {
        all_tools()
    }
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

/// Dispatch a tool call by name to its handler.
///
/// Not `Send` because `MemoryDB` (rusqlite) is not `Sync`. Each connection
/// task drives its own tool calls sequentially, so this is fine.
pub fn dispatch_tool<'a>(
    name: &'a str,
    input: Value,
    ctx: &'a dyn ToolContext,
) -> Pin<Box<dyn Future<Output = Result<Value, ToolError>> + 'a>> {
    Box::pin(async move {
        match name {
            // Memory tools
            "memory" => memory_tools::handle_memory(input, ctx).await,
            "send_image" => images::handle_send_image(input, ctx).await,
            "list_images" => images::handle_list_images(input, ctx).await,
            "recall_image" => images::handle_recall_image(input, ctx).await,
            "generate_image" => images::handle_generate_image(input, ctx).await,
            // Web tools
            "web_search" => web::handle_web_search(input).await,
            "fetch_url" => web::handle_fetch_url(input).await,
            "research_web" => web::handle_research_web(input).await,
            // Other
            "activity_heatmap" => activity::handle_activity_heatmap(input).await,
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

    #[test]
    fn test_all_tools_returns_expected_count() {
        let tools = all_tools();
        // memory(1) + images(4) + web(3) + activity(1) = 9
        assert_eq!(tools.len(), 9);
    }

    #[test]
    fn test_available_tools_filters_private() {
        let all = all_tools();
        let private = available_tools(true);
        let public = available_tools(false);

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
        let private = available_tools(true);
        let private_names: Vec<&str> = private.iter().map(|t| t.name).collect();

        // Memory tools should be excluded.
        assert!(!private_names.contains(&"memory"));
        assert!(!private_names.contains(&"send_image"));
        assert!(!private_names.contains(&"list_images"));
        assert!(!private_names.contains(&"recall_image"));
        assert!(!private_names.contains(&"generate_image"));

        // Web and other tools should remain.
        assert!(private_names.contains(&"web_search"));
        assert!(private_names.contains(&"fetch_url"));
        assert!(private_names.contains(&"research_web"));
        assert!(private_names.contains(&"activity_heatmap"));
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
}
