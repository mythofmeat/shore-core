pub mod activity;
pub mod basic;
pub(crate) mod context;
pub mod images;
pub mod memory_tools;
pub mod scratchpad;
pub mod web;
pub mod workspace;

use crate::autonomy::manager::AutonomyManager;
use crate::memory::compaction_impls::ImageGenConfig;
use crate::memory::memory_llm::MemoryLlm;
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
    /// Memory write tools.
    MemoryWrite,
    /// Memory read tools.
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
    fn memory_llm(&self) -> &dyn MemoryLlm;
    fn memory_model(&self) -> &ResolvedModel;
    fn image_dir(&self) -> &str;
    fn llm_client(&self) -> Option<&LlmClient>;
    fn image_gen_config(&self) -> Option<&ImageGenConfig>;

    // Web search configuration
    fn search_config(&self) -> &shore_config::app::SearchConfig;

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

    // Whether memory tools and the workspace `memories/...` namespace may be
    // used in this conversation.
    fn memory_access_allowed(&self) -> bool {
        true
    }
    fn memory_read_allowed(&self) -> bool {
        self.memory_access_allowed()
    }
    fn memory_write_allowed(&self) -> bool {
        self.memory_access_allowed()
    }

    // Config directory for deferred character self-edits
    fn config_dir(&self) -> &str {
        ""
    }

    /// Queue a deferred edit for a protected file (character.md, user.md,
    /// prompts/system.md). Called by the tool dispatch layer after a
    /// successful write or edit to a protected path. The actual copy to
    /// the config dir happens at the next compaction boundary.
    fn defer_edit(&self, _path: &str) {}
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
            let memory_namespace_available =
                workspace_memory_namespace_available(t.name, is_private, toggles);
            let description =
                workspace::description_for_memory_access(t.name, memory_namespace_available)
                    .unwrap_or(t.description);
            serde_json::json!({
                "name": t.name,
                "description": crate::engine::prompt::render_template(description, &vars),
                "input_schema": t.parameters.clone(),
            })
        })
        .collect()
}

/// Returns tool definitions available for the current privacy mode and tool toggles.
pub fn available_tools(is_private: bool, toggles: &shore_config::app::ToolToggles) -> Vec<ToolDef> {
    let exec_can_reach_memory = !is_private && toggles.memory_read() && toggles.memory_write();
    all_tools()
        .into_iter()
        .filter(|t| {
            if is_private && !t.category.allowed_in_private() {
                return false;
            }
            if !exec_can_reach_memory && t.name == "exec" {
                return false;
            }
            toggles.is_enabled(t.name)
        })
        .collect()
}

fn workspace_memory_namespace_available(
    name: &str,
    is_private: bool,
    toggles: &shore_config::app::ToolToggles,
) -> bool {
    if is_private {
        return false;
    }
    match name {
        "read" | "list_files" => toggles.memory_read(),
        "write" => toggles.memory_write(),
        "edit" => toggles.memory_read() && toggles.memory_write(),
        _ => toggles.memory(),
    }
}

fn ensure_memory_read_access(ctx: &dyn ToolContext) -> Result<(), ToolError> {
    if ctx.memory_read_allowed() {
        Ok(())
    } else {
        Err(ToolError::InvalidArgs(
            "memory read access is disabled for this conversation".into(),
        ))
    }
}

fn ensure_memory_write_access(ctx: &dyn ToolContext) -> Result<(), ToolError> {
    if ctx.memory_write_allowed() {
        Ok(())
    } else {
        Err(ToolError::InvalidArgs(
            "memory write access is disabled for this conversation".into(),
        ))
    }
}

fn ensure_workspace_memory_access(
    name: &str,
    input: &Value,
    ctx: &dyn ToolContext,
) -> Result<(), ToolError> {
    let touches_memories = input
        .get("path")
        .and_then(|v| v.as_str())
        .is_some_and(path_requests_memories_namespace);

    if !touches_memories && name != "exec" {
        return Ok(());
    }

    if name == "exec" {
        if ctx.memory_read_allowed() && ctx.memory_write_allowed() {
            return Ok(());
        }
        return Err(ToolError::InvalidArgs(
            "exec is unavailable when memory access is disabled".into(),
        ));
    }

    let allowed = match name {
        "read" | "list_files" => ctx.memory_read_allowed(),
        "write" => ctx.memory_write_allowed(),
        "edit" => ctx.memory_read_allowed() && ctx.memory_write_allowed(),
        _ => true,
    };

    if !allowed {
        Err(ToolError::InvalidArgs(
            "workspace access to memories/... is disabled for this conversation".into(),
        ))
    } else {
        Ok(())
    }
}

fn path_requests_memories_namespace(path: &str) -> bool {
    let normalized = path.trim().trim_start_matches('/').trim_start_matches('\\');
    normalized == "memories"
        || normalized.starts_with("memories/")
        || normalized.starts_with("memories\\")
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
            "memory" => {
                ensure_memory_read_access(ctx)?;
                memory_tools::handle_memory(input, ctx).await
            }
            "memory_read" => {
                ensure_memory_read_access(ctx)?;
                memory_tools::handle_memory_read(input, ctx).await
            }
            "memory_write" => {
                ensure_memory_write_access(ctx)?;
                memory_tools::handle_memory_write(input, ctx).await
            }
            "memory_search" => {
                ensure_memory_read_access(ctx)?;
                memory_tools::handle_memory_search(input, ctx).await
            }
            "memory_list" => {
                ensure_memory_read_access(ctx)?;
                memory_tools::handle_memory_list(input, ctx).await
            }
            "send_image" => images::handle_send_image(input, ctx).await,
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
            "read" => {
                ensure_workspace_memory_access(name, &input, ctx)?;
                workspace::handle_read(input, ctx.workspace_dir()).await
            }
            "write" => {
                ensure_workspace_memory_access(name, &input, ctx)?;
                let path = input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut result = workspace::handle_write(input, ctx.workspace_dir()).await?;
                if crate::memory::deferred_edits::is_protected_path(&path) {
                    ctx.defer_edit(&path);
                    result["deferred_until_compaction"] = serde_json::json!(true);
                }
                Ok(result)
            }
            "edit" => {
                ensure_workspace_memory_access(name, &input, ctx)?;
                let path = input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut result = workspace::handle_edit(input, ctx.workspace_dir()).await?;
                if crate::memory::deferred_edits::is_protected_path(&path) {
                    ctx.defer_edit(&path);
                    result["deferred_until_compaction"] = serde_json::json!(true);
                }
                Ok(result)
            }
            "list_files" => {
                ensure_workspace_memory_access(name, &input, ctx)?;
                workspace::handle_list_files(input, ctx.workspace_dir()).await
            }
            "exec" => {
                ensure_workspace_memory_access(name, &input, ctx)?;
                workspace::handle_exec(input, ctx.workspace_dir()).await
            }
            // set_next_wake is in the base tool set for cache stability but
            // only handled during heartbeat ticks (intercepted in manager.rs).
            "set_next_wake" => Err(ToolError::InvalidArgs(
                "set_next_wake is only available during heartbeat ticks".into(),
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
        // memory(5) + images(2) + web(2) + activity(1) + basic(3) + scratchpad(4) + workspace(5) = 22
        assert_eq!(tools.len(), 22);
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

        // Web and other tools should remain.
        assert!(private_names.contains(&"web_search"));
        assert!(private_names.contains(&"fetch_url"));
        assert!(private_names.contains(&"activity_heatmap"));
        assert!(private_names.contains(&"send_image"));
        assert!(private_names.contains(&"generate_image"));
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
        assert_eq!(tools.len(), 20); // 22 - 2 disabled
    }

    #[test]
    fn memory_toggle_disables_all_memory_tools_and_exec() {
        let mut toggles = ToolToggles::default();
        toggles.set("memory", false);

        let tools = available_tools(false, &toggles);
        let names: Vec<&str> = tools.iter().map(|t| t.name).collect();

        assert!(!names.contains(&"memory"));
        assert!(!names.contains(&"memory_read"));
        assert!(!names.contains(&"memory_write"));
        assert!(!names.contains(&"memory_search"));
        assert!(!names.contains(&"memory_list"));
        assert!(!names.contains(&"exec"));
        assert!(names.contains(&"read"));
        assert!(names.contains(&"write"));
        assert!(names.contains(&"edit"));
        assert!(names.contains(&"list_files"));
    }

    #[test]
    fn render_tool_defs_hides_memories_namespace_when_memory_disabled() {
        let mut toggles = ToolToggles::default();
        toggles.set("memory", false);

        let defs = render_tool_defs(false, &toggles, "qifei", "ren");
        let read = defs
            .iter()
            .find(|d| d["name"] == "read")
            .expect("read present");
        let desc = read["description"].as_str().unwrap();
        assert!(!desc.contains("memories"));
        assert!(defs.iter().all(|d| d["name"] != "exec"));
    }

    #[test]
    fn granular_memory_write_toggle_hides_memories_namespace_for_writes() {
        let mut toggles = ToolToggles::default();
        toggles.set("memory_write", false);

        let defs = render_tool_defs(false, &toggles, "qifei", "ren");
        let read = defs
            .iter()
            .find(|d| d["name"] == "read")
            .expect("read present");
        let write = defs
            .iter()
            .find(|d| d["name"] == "write")
            .expect("write present");

        assert!(read["description"].as_str().unwrap().contains("memories"));
        assert!(!write["description"].as_str().unwrap().contains("memories"));
        assert!(defs.iter().all(|d| d["name"] != "memory_write"));
        assert!(defs.iter().all(|d| d["name"] != "exec"));
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
    async fn test_dispatch_rejects_memory_when_access_disabled() {
        let ctx = TestToolContext::new().with_memory_access_allowed(false);
        let result =
            dispatch_tool("memory_search", serde_json::json!({"query": "tea"}), &ctx).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidArgs(_)),
            "memory disabled should return InvalidArgs, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_dispatch_rejects_memories_namespace_when_access_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        let ws_str = ws.to_string_lossy().to_string();
        let ctx = TestToolContext::new()
            .with_memory_access_allowed(false)
            .with_workspace_dir(&ws_str);

        let result = dispatch_tool(
            "read",
            serde_json::json!({"path": "memories/people/ren.md"}),
            &ctx,
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidArgs(_)),
            "memories namespace should be blocked, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_dispatch_allows_workspace_when_memory_access_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        let ws_str = ws.to_string_lossy().to_string();
        let ctx = TestToolContext::new()
            .with_memory_access_allowed(false)
            .with_workspace_dir(&ws_str);

        let result = dispatch_tool(
            "write",
            serde_json::json!({"path": "notes.md", "content": "ok"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(result["bytes_written"], 2);
    }

    #[tokio::test]
    async fn test_dispatch_rejects_memory_write_namespace_when_write_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        let ws_str = ws.to_string_lossy().to_string();
        let ctx = TestToolContext::new()
            .with_memory_write_allowed(false)
            .with_workspace_dir(&ws_str);

        let result = dispatch_tool(
            "write",
            serde_json::json!({"path": "memories/people/ren.md", "content": "blocked"}),
            &ctx,
        )
        .await;
        assert!(result.is_err());

        let result = dispatch_tool(
            "read",
            serde_json::json!({"path": "memories/people/ren.md"}),
            &ctx,
        )
        .await;
        assert!(
            !matches!(result, Err(ToolError::InvalidArgs(_))),
            "read access should still be gated independently from write access"
        );
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
