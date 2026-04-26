pub mod activity;
pub mod basic;
pub(crate) mod context;
pub mod history;
pub mod images;
pub mod web;
pub mod workspace;

use crate::autonomy::manager::AutonomyManager;
use crate::memory::compaction_impls::ImageGenConfig;
use crate::memory::retrieval::EmbeddingConfig;
use serde_json::Value;
use shore_config::app::RetrievalConfig;
use shore_llm::LlmClient;
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

    // Workspace directory for general filesystem tools
    fn workspace_dir(&self) -> &str {
        ""
    }

    // Character data directory for conversation history search.
    fn character_data_dir(&self) -> &str {
        ""
    }

    // Markdown memory store for inspectable memory files
    fn markdown_store(&self) -> Option<&crate::memory::markdown_store::MarkdownMemoryStore> {
        None
    }

    // Memory retrieval configuration and optional embedding profile. The
    // embedding index is non-authoritative; markdown files remain the source
    // of truth.
    fn memory_retrieval_config(&self) -> &RetrievalConfig {
        static DEFAULT: std::sync::OnceLock<RetrievalConfig> = std::sync::OnceLock::new();
        DEFAULT.get_or_init(RetrievalConfig::default)
    }
    fn embedding_config(&self) -> Option<&EmbeddingConfig> {
        None
    }
    fn memory_index_path(&self) -> Option<&std::path::Path> {
        None
    }

    // Whether memory tools and the workspace `memory/...` namespace may be
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

    /// Queue a deferred edit for a protected workspace bootstrap file
    /// (SOUL.md, USER.md, AGENTS.md, TOOLS.md, HEARTBEAT.md). Called by the tool dispatch layer after a
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
    tools.extend(images::tool_defs());
    tools.extend(web::tool_defs());
    tools.extend(activity::tool_defs());
    tools.extend(basic::tool_defs());
    tools.extend(workspace::tool_defs());
    tools.extend(history::tool_defs());
    tools
}

/// Build the outbound LLM `tools` array from `available_tools`, rendering
/// `{{char}}` / `{{user}}` placeholders in each tool's description through
/// the same template pipeline the system prompt uses.
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
            if t.category == ToolCategory::MemoryRead && !toggles.memory_read() {
                return false;
            }
            if t.category == ToolCategory::MemoryWrite && !toggles.memory_write() {
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
        "read" | "list_files" | "search" => toggles.memory_read(),
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

fn ensure_workspace_memory_access(
    name: &str,
    input: &Value,
    ctx: &dyn ToolContext,
) -> Result<(), ToolError> {
    let touches_memory = input
        .get("path")
        .and_then(|v| v.as_str())
        .is_some_and(path_requests_memory_namespace);

    if !touches_memory && name != "exec" {
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
        "read" | "list_files" | "search" => ctx.memory_read_allowed(),
        "write" => ctx.memory_write_allowed(),
        "edit" => ctx.memory_read_allowed() && ctx.memory_write_allowed(),
        _ => true,
    };

    if !allowed {
        Err(ToolError::InvalidArgs(
            "workspace access to memory/... is disabled for this conversation".into(),
        ))
    } else {
        Ok(())
    }
}

fn path_requests_memory_namespace(path: &str) -> bool {
    let normalized = path
        .trim()
        .trim_start_matches(['/', '\\'])
        .replace('\\', "/");
    let mut parts = Vec::new();
    for component in std::path::Path::new(&normalized).components() {
        match component {
            std::path::Component::Normal(part) => {
                parts.push(part.to_string_lossy().to_string());
            }
            std::path::Component::CurDir => {}
            _ => return false,
        }
    }

    match parts.as_slice() {
        [first, ..] if first == "memory" => true,
        [first, second, ..] if first == "workspace" && second == "memory" => true,
        _ => false,
    }
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
            "search_history" => {
                ensure_memory_read_access(ctx)?;
                history::handle_search_history(input, ctx).await
            }
            "generate_image" => images::handle_generate_image(input, ctx).await,
            // Web tools
            "web_search" => web::handle_web_search(input, ctx).await,
            "fetch_url" => web::handle_fetch_url(input).await,
            // Basic tools
            "check_time" => basic::handle_check_time(input).await,
            "roll_dice" => basic::handle_roll_dice(input).await,
            // Other
            "activity_heatmap" => activity::handle_activity_heatmap(input, ctx).await,
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
                if let Some(deferred_path) =
                    crate::memory::deferred_edits::normalize_protected_path(&path)
                {
                    ctx.defer_edit(&path);
                    result["protected_file"] = serde_json::json!(true);
                    result["deferred_until_compaction"] = serde_json::json!(true);
                    result["deferred_path"] = serde_json::json!(deferred_path);
                    result["prompt_reload_required"] = serde_json::json!(true);
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
                if let Some(deferred_path) =
                    crate::memory::deferred_edits::normalize_protected_path(&path)
                {
                    ctx.defer_edit(&path);
                    result["protected_file"] = serde_json::json!(true);
                    result["deferred_until_compaction"] = serde_json::json!(true);
                    result["deferred_path"] = serde_json::json!(deferred_path);
                    result["prompt_reload_required"] = serde_json::json!(true);
                }
                Ok(result)
            }
            "list_files" => {
                ensure_workspace_memory_access(name, &input, ctx)?;
                workspace::handle_list_files(input, ctx.workspace_dir()).await
            }
            "search" => {
                ensure_workspace_memory_access(name, &input, ctx)?;
                workspace::handle_search(input, ctx.workspace_dir(), ctx.memory_read_allowed())
                    .await
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
        // {{user}} appears in check_time and must resolve, not ship
        // literal to the model.
        let toggles = ToolToggles::default();
        let defs = render_tool_defs(false, &toggles, "qifei", "ren");
        let check_time = defs
            .iter()
            .find(|d| d["name"] == "check_time")
            .expect("check_time present");
        let desc = check_time["description"].as_str().unwrap();
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
        // images(1) + web(2) + activity(1) + basic(3) + workspace(6) + history(1) = 14
        assert_eq!(tools.len(), 14);
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

        // Durable history search should be excluded.
        assert!(!private_names.contains(&"search_history"));
        assert!(!private_names.contains(&"exec"));

        // Web and other tools should remain.
        assert!(private_names.contains(&"web_search"));
        assert!(private_names.contains(&"fetch_url"));
        assert!(private_names.contains(&"activity_heatmap"));
        assert!(private_names.contains(&"generate_image"));
        assert!(private_names.contains(&"search"));
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
        assert!(names.contains(&"search"));
        assert!(names.contains(&"search_history"));
        assert!(names.contains(&"check_time"));
        assert_eq!(tools.len(), 12); // 14 - 2 disabled
    }

    #[test]
    fn memory_toggle_disables_all_memory_tools_and_exec() {
        let mut toggles = ToolToggles::default();
        toggles.set("memory", false);

        let tools = available_tools(false, &toggles);
        let names: Vec<&str> = tools.iter().map(|t| t.name).collect();

        assert!(!names.contains(&"search_history"));
        assert!(!names.contains(&"exec"));
        assert!(names.contains(&"read"));
        assert!(names.contains(&"write"));
        assert!(names.contains(&"edit"));
        assert!(names.contains(&"list_files"));
        assert!(names.contains(&"search"));
    }

    #[test]
    fn render_tool_defs_hides_memory_namespace_when_memory_disabled() {
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
    fn granular_memory_write_toggle_hides_memory_namespace_for_writes() {
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

        assert!(read["description"].as_str().unwrap().contains("memory"));
        assert!(!write["description"]
            .as_str()
            .unwrap()
            .contains("memory/..."));
        assert!(defs.iter().any(|d| d["name"] == "search_history"));
        assert!(defs.iter().all(|d| d["name"] != "exec"));
    }

    #[test]
    fn granular_memory_read_toggle_hides_read_surfaces() {
        let mut toggles = ToolToggles::default();
        toggles.set("memory_read", false);

        let defs = render_tool_defs(false, &toggles, "qifei", "ren");
        let names: Vec<&str> = defs.iter().filter_map(|d| d["name"].as_str()).collect();

        assert!(!names.contains(&"search_history"));
        assert!(!names.contains(&"exec"));
        for tool_name in ["read", "list_files", "search"] {
            let desc = defs
                .iter()
                .find(|d| d["name"] == tool_name)
                .and_then(|d| d["description"].as_str())
                .expect("workspace read surface present");
            assert!(!desc.contains("memory/..."));
        }

        let write_desc = defs
            .iter()
            .find(|d| d["name"] == "write")
            .and_then(|d| d["description"].as_str())
            .expect("write present");
        assert!(write_desc.contains("memory/..."));
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
    async fn test_dispatch_removed_memory_tool_is_not_implemented() {
        let ctx = TestToolContext::new();
        let result =
            dispatch_tool("memory_search", serde_json::json!({"query": "tea"}), &ctx).await;
        assert!(matches!(result, Err(ToolError::NotImplemented(_))));
    }

    #[tokio::test]
    async fn test_dispatch_rejects_history_search_when_memory_access_disabled() {
        let ctx = TestToolContext::new().with_memory_access_allowed(false);
        let result =
            dispatch_tool("search_history", serde_json::json!({"query": "tea"}), &ctx).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidArgs(_)),
            "memory disabled should return InvalidArgs, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_dispatch_rejects_memory_namespace_when_access_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        let ws_str = ws.to_string_lossy().to_string();
        let ctx = TestToolContext::new()
            .with_memory_access_allowed(false)
            .with_workspace_dir(&ws_str);

        let result = dispatch_tool(
            "read",
            serde_json::json!({"path": "memory/people/ren.md"}),
            &ctx,
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidArgs(_)),
            "memory namespace should be blocked, got: {err}"
        );

        let result = dispatch_tool(
            "read",
            serde_json::json!({"path": "workspace/memory/people/ren.md"}),
            &ctx,
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidArgs(_)),
            "workspace/memory namespace should be blocked, got: {err}"
        );

        let result = dispatch_tool(
            "read",
            serde_json::json!({"path": "./memory/people/ren.md"}),
            &ctx,
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidArgs(_)),
            "./memory namespace should be blocked, got: {err}"
        );

        let result = dispatch_tool(
            "read",
            serde_json::json!({"path": "workspace/./memory/people/ren.md"}),
            &ctx,
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidArgs(_)),
            "workspace/./memory namespace should be blocked, got: {err}"
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
            serde_json::json!({"path": "memory/people/ren.md", "content": "blocked"}),
            &ctx,
        )
        .await;
        assert!(result.is_err());

        let result = dispatch_tool(
            "write",
            serde_json::json!({"path": "workspace/memory/people/ren.md", "content": "blocked"}),
            &ctx,
        )
        .await;
        assert!(result.is_err());

        let result = dispatch_tool(
            "write",
            serde_json::json!({"path": "./memory/people/ren.md", "content": "blocked"}),
            &ctx,
        )
        .await;
        assert!(result.is_err());

        let result = dispatch_tool(
            "write",
            serde_json::json!({"path": "workspace/./memory/people/ren.md", "content": "blocked"}),
            &ctx,
        )
        .await;
        assert!(result.is_err());

        let result = dispatch_tool(
            "read",
            serde_json::json!({"path": "memory/people/ren.md"}),
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
}
