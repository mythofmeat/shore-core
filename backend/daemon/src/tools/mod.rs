pub mod activity;
pub mod basic;
pub(crate) mod context;
pub mod history;
pub mod images;
pub mod web;
pub mod workspace;

use crate::autonomy::manager::AutonomyManager;
use crate::memory::compaction_impls::ImageGenConfig;
use serde_json::Value;
use shore_config::app::{RetrievalConfig, RetrievalMode};
use shore_llm::LlmClient;
use shore_llm::embed::Embedder;
use std::future::Future;
use std::pin::Pin;

// ---------------------------------------------------------------------------
// Tool category — coarse capability grouping
// ---------------------------------------------------------------------------

/// Tool categories for coarse routing and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    /// Web/HTTP tools — always available.
    Web,
    /// Other tools (filesystem, history, dice, time, activity).
    Other,
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
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "real ToolContext implementations return character names borrowed from self"
    )]
    fn character_name(&self) -> &str {
        ""
    }
    fn schedule_next_wake(&self, _input: &Value) -> Option<Result<Value, ToolError>> {
        None
    }

    // Workspace directory for general filesystem tools
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "real ToolContext implementations return workspace paths borrowed from self"
    )]
    fn workspace_dir(&self) -> &str {
        ""
    }

    // Character data directory for conversation history search.
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "real ToolContext implementations return data paths borrowed from self"
    )]
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
    fn embedder(&self) -> Option<&dyn Embedder> {
        None
    }
    fn memory_index_path(&self) -> Option<&std::path::Path> {
        None
    }

    // Config directory for deferred character self-edits
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "real ToolContext implementations return config paths borrowed from self"
    )]
    fn config_dir(&self) -> &str {
        ""
    }

    /// Queue a deferred edit for a prompt-visible workspace file. Called by
    /// the tool dispatch layer after a successful write or edit to a path
    /// whose content should only become prompt-active at the next compaction
    /// boundary.
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
            if is_private && matches!(t.name, "search_history" | "exec") {
                return false;
            }
            toggles.is_enabled(t.name)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

fn default_search_mode(ctx: &dyn ToolContext, index_path_available: bool) -> &'static str {
    match ctx.memory_retrieval_config().mode {
        RetrievalMode::Lexical => "lexical",
        RetrievalMode::Hybrid => "hybrid",
        RetrievalMode::Auto => {
            if ctx.embedder().is_some() && index_path_available {
                "hybrid"
            } else {
                "lexical"
            }
        }
    }
}

fn apply_default_search_mode(input: &mut Value, ctx: &dyn ToolContext, index_path_available: bool) {
    if input.get("mode").is_some() {
        return;
    }
    if let Some(obj) = input.as_object_mut() {
        obj.insert(
            "mode".into(),
            serde_json::json!(default_search_mode(ctx, index_path_available)),
        );
    }
}

/// Dispatch a tool call by name to its handler.
pub fn dispatch_tool<'a>(
    name: &'a str,
    input: Value,
    ctx: &'a dyn ToolContext,
) -> Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send + 'a>> {
    Box::pin(async move {
        match name {
            "search_history" => history::handle_search_history(&input, ctx),
            "generate_image" => images::handle_generate_image(input, ctx).await,
            // Web tools
            "web_search" => web::handle_web_search(input, ctx).await,
            "fetch_url" => web::handle_fetch_url(input).await,
            // Basic tools
            "check_time" => basic::handle_check_time(input),
            "roll_dice" => basic::handle_roll_dice(&input),
            // Other
            "activity_heatmap" => activity::handle_activity_heatmap(&input, ctx),
            // Workspace tools
            "read" => workspace::handle_read(input, ctx.workspace_dir()).await,
            "write" => {
                let path = input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut result = workspace::handle_write(input, ctx.workspace_dir()).await?;
                if let Some(deferred_path) =
                    crate::memory::deferred_edits::normalize_prompt_visible_path(&path)
                {
                    ctx.defer_edit(&path);
                    result["prompt_visible_file"] = serde_json::json!(true);
                    if crate::memory::deferred_edits::normalize_protected_path(&path).is_some() {
                        result["protected_file"] = serde_json::json!(true);
                    }
                    result["deferred_until_compaction"] = serde_json::json!(true);
                    result["deferred_path"] = serde_json::json!(deferred_path);
                    result["prompt_reload_required"] = serde_json::json!(true);
                }
                Ok(result)
            }
            "edit" => {
                let path = input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut result = workspace::handle_edit(input, ctx.workspace_dir()).await?;
                if let Some(deferred_path) =
                    crate::memory::deferred_edits::normalize_prompt_visible_path(&path)
                {
                    ctx.defer_edit(&path);
                    result["prompt_visible_file"] = serde_json::json!(true);
                    if crate::memory::deferred_edits::normalize_protected_path(&path).is_some() {
                        result["protected_file"] = serde_json::json!(true);
                    }
                    result["deferred_until_compaction"] = serde_json::json!(true);
                    result["deferred_path"] = serde_json::json!(deferred_path);
                    result["prompt_reload_required"] = serde_json::json!(true);
                }
                Ok(result)
            }
            "list_files" => workspace::handle_list_files(input, ctx.workspace_dir()).await,
            "search" => {
                let index_path = ctx.memory_index_path();
                let mut input = input;
                apply_default_search_mode(&mut input, ctx, index_path.is_some());
                workspace::handle_search(
                    input,
                    ctx.workspace_dir(),
                    Some(ctx.memory_retrieval_config()),
                    ctx.embedder(),
                    index_path,
                )
                .await
            }
            "delete" => {
                workspace::handle_delete(input, ctx.workspace_dir(), ctx.character_data_dir()).await
            }
            "exec" => workspace::handle_exec(input, ctx.workspace_dir()).await,
            // set_next_wake is in the base tool set for cache stability but
            // only heartbeat-capable contexts are allowed to handle it.
            "set_next_wake" => ctx.schedule_next_wake(&input).unwrap_or_else(|| {
                Err(ToolError::InvalidArgs(
                    "set_next_wake is only available during heartbeat ticks".into(),
                ))
            }),
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
        // images(1) + web(2) + activity(1) + basic(3) + workspace(7) + history(1) = 15
        assert_eq!(tools.len(), 15);
    }

    #[test]
    fn test_available_tools_filters_private() {
        let toggles = ToolToggles::default();
        let all = all_tools();
        let private = available_tools(true, &toggles);
        let public = available_tools(false, &toggles);

        assert_eq!(public.len(), all.len());
        assert!(private.len() < public.len());
        assert!(private.iter().all(|tool| tool.name != "search_history"));
        assert!(private.iter().all(|tool| tool.name != "exec"));
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
        assert_eq!(tools.len(), 13); // 15 - 2 disabled
    }

    #[test]
    fn legacy_memory_toggles_do_not_gate_tools() {
        let mut toggles = ToolToggles::default();
        toggles.set("memory", false);
        toggles.set("memory_read", false);
        toggles.set("memory_write", false);

        let tools = available_tools(false, &toggles);
        let names: Vec<&str> = tools.iter().map(|t| t.name).collect();

        assert!(names.contains(&"search_history"));
        assert!(names.contains(&"exec"));
        assert!(names.contains(&"read"));
        assert!(names.contains(&"write"));
        assert!(names.contains(&"edit"));
        assert!(names.contains(&"list_files"));
        assert!(names.contains(&"search"));
    }

    #[test]
    fn render_tool_defs_ignores_legacy_memory_toggles() {
        let mut toggles = ToolToggles::default();
        toggles.set("memory", false);
        toggles.set("memory_read", false);
        toggles.set("memory_write", false);

        let defs = render_tool_defs(false, &toggles, "qifei", "ren");
        let names: Vec<&str> = defs.iter().filter_map(|d| d["name"].as_str()).collect();
        assert!(names.contains(&"search_history"));
        assert!(names.contains(&"exec"));
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
    async fn test_dispatch_search_auto_without_embedder_uses_lexical() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        tokio::fs::write(ws.join("notes.md"), "tea time")
            .await
            .unwrap();
        let ws_str = ws.to_string_lossy().to_string();
        let ctx = TestToolContext::new().with_workspace_dir(&ws_str);

        let result = dispatch_tool("search", serde_json::json!({"query": "tea"}), &ctx)
            .await
            .unwrap();

        assert_eq!(result["mode"], "lexical");
        assert!(result.get("semantic_unavailable").is_none());
    }

    #[tokio::test]
    async fn test_dispatch_search_respects_lexical_retrieval_config() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        tokio::fs::write(ws.join("notes.md"), "tea time")
            .await
            .unwrap();
        let ws_str = ws.to_string_lossy().to_string();
        let ctx = TestToolContext::new()
            .with_workspace_dir(&ws_str)
            .with_retrieval_config(shore_config::app::RetrievalConfig {
                mode: shore_config::app::RetrievalMode::Lexical,
                ..Default::default()
            });

        let result = dispatch_tool("search", serde_json::json!({"query": "tea"}), &ctx)
            .await
            .unwrap();

        assert_eq!(result["mode"], "lexical");
        assert!(result.get("semantic_unavailable").is_none());
    }

    #[tokio::test]
    async fn test_dispatch_removed_memory_tool_is_not_implemented() {
        let ctx = TestToolContext::new();
        let result =
            dispatch_tool("memory_search", serde_json::json!({"query": "tea"}), &ctx).await;
        assert!(matches!(result, Err(ToolError::NotImplemented(_))));
    }

    #[tokio::test]
    async fn test_dispatch_history_search_routes_without_memory_gate() {
        let ctx = TestToolContext::new();
        let result =
            dispatch_tool("search_history", serde_json::json!({"query": "tea"}), &ctx).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidArgs(_)),
            "missing history config should return InvalidArgs, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_dispatch_allows_memory_namespace_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        let ws_str = ws.to_string_lossy().to_string();
        let ctx = TestToolContext::new().with_workspace_dir(&ws_str);

        let result = dispatch_tool(
            "write",
            serde_json::json!({"path": "memory/people/ren.md", "content": "Ren likes tea."}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(result["bytes_written"], 14);

        let result = dispatch_tool(
            "read",
            serde_json::json!({"path": "memory/people/ren.md"}),
            &ctx,
        )
        .await;
        assert!(result.is_ok());
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
