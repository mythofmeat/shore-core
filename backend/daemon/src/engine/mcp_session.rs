//! Per-request MCP session for the `claude_code` provider.
//!
//! When the engine dispatches an LLM request whose `sdk = ClaudeCode`,
//! it allocates a session here, threads `mcp_endpoint` /
//! `allowed_tools` / `session_id` into `provider_options`, and the
//! `claude` subprocess calls back into the daemon's HTTP listener
//! via MCP. Each `tools/call` is dispatched against `dispatch_tool`
//! and recorded to the session's ledger; the engine drains the
//! ledger after the LLM dispatch returns and splices synthetic
//! `ToolUse` + `ToolResult` ContentBlocks into the persisted
//! assistant turn so future history rounds remain faithful.
//!
//! The session is allocated per-request in M3 / M4. The long-lived
//! subprocess cache (M6) will keep the session alive across the
//! full subprocess lifetime; for now, allocation lifetime equals
//! one chat request.

use std::collections::HashSet;
use std::sync::Arc;

use dashmap::DashMap;
use serde_json::Value;
use shore_protocol::types::ContentBlock;
use tokio::sync::Mutex;
use tracing::warn;

use crate::tools::ToolContext;

/// A single tool invocation observed during an MCP session, in the
/// order the CLI made it. Drained by the engine and spliced into the
/// persisted assistant turn after the LLM dispatch returns.
#[derive(Debug, Clone)]
pub struct LedgerEntry {
    /// The MCP `tool_use_id` the CLI assigned. Used to pair the
    /// resulting `ToolResult` block.
    pub tool_use_id: String,
    /// Bare tool name (without the `mcp__shore__` prefix the CLI
    /// presents to the model).
    pub name: String,
    /// JSON arguments passed to the tool.
    pub input: Value,
    /// Result blocks. Currently always one `ContentBlock::Text` with
    /// a JSON-serialized representation of the tool's return value.
    pub content: Vec<ContentBlock>,
    /// Whether the dispatcher returned a `ToolError`.
    pub is_error: bool,
}

/// One active MCP session.
///
/// The session is shared between the HTTP handler (which records
/// tool calls) and the engine (which drains the ledger after the
/// LLM dispatch returns).
pub struct McpSession {
    pub id: String,
    /// Bare tool names that the model is permitted to call. Tools
    /// outside this set are filtered out of `tools/list` and rejected
    /// at `tools/call` with a JSON-RPC error.
    pub allowed_tools: HashSet<String>,
    /// Tool definitions (Anthropic format: `{name, description,
    /// input_schema}`) — what the engine already produces via
    /// `tools::render_tool_defs`. Filtered to `allowed_tools` before
    /// being returned over MCP.
    pub tool_defs: Vec<Value>,
    /// Reference to the active tool context. Held as
    /// `dyn ToolContext + Send + Sync` so the HTTP handler can
    /// dispatch from any tokio task.
    pub tool_ctx: Arc<dyn ToolContext + Send + Sync>,
    /// Recorded tool calls, in CLI-emit order.
    pub ledger: Mutex<Vec<LedgerEntry>>,
}

impl std::fmt::Debug for McpSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpSession")
            .field("id", &self.id)
            .field("allowed_tools", &self.allowed_tools)
            .field("tool_defs_count", &self.tool_defs.len())
            .finish()
    }
}

impl McpSession {
    /// Whether `name` (the bare tool name) is permitted in this session.
    pub fn allows(&self, name: &str) -> bool {
        self.allowed_tools.contains(name)
    }

    /// Tool definitions to advertise via MCP `tools/list`, filtered
    /// down to `allowed_tools`.
    pub fn list_tools(&self) -> Vec<Value> {
        self.tool_defs
            .iter()
            .filter(|d| {
                d.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| self.allowed_tools.contains(n))
            })
            .cloned()
            .collect()
    }

    /// Append an entry to the ledger.
    pub async fn record(&self, entry: LedgerEntry) {
        self.ledger.lock().await.push(entry);
    }
}

/// Process-wide registry of active MCP sessions, keyed by session id
/// (a UUID string passed in the `/mcp/<session_id>` URL path).
#[derive(Debug, Default, Clone)]
pub struct McpSessionRegistry {
    sessions: Arc<DashMap<String, Arc<McpSession>>>,
}

impl McpSessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate a new session and register it. Returns a `Guard` that
    /// removes the session from the registry when dropped — callers
    /// should hold it for the duration of the LLM dispatch so the
    /// HTTP handler can find the session, then drop it (which also
    /// returns the drained ledger via [`Guard::drain`]).
    pub fn allocate(
        &self,
        id: String,
        allowed_tools: HashSet<String>,
        tool_defs: Vec<Value>,
        tool_ctx: Arc<dyn ToolContext + Send + Sync>,
    ) -> McpSessionGuard {
        let session = Arc::new(McpSession {
            id: id.clone(),
            allowed_tools,
            tool_defs,
            tool_ctx,
            ledger: Mutex::new(Vec::new()),
        });
        self.sessions.insert(id.clone(), session.clone());
        McpSessionGuard {
            id,
            registry: self.clone(),
            session,
        }
    }

    /// Look up a session by id, returning `None` if unknown or
    /// already deallocated.
    pub fn get(&self, id: &str) -> Option<Arc<McpSession>> {
        self.sessions.get(id).map(|r| r.value().clone())
    }
}

/// RAII handle returned by `allocate`. Drops the session from the
/// registry when dropped. The engine uses [`drain`] before drop to
/// pull the recorded tool calls out for splicing.
pub struct McpSessionGuard {
    id: String,
    registry: McpSessionRegistry,
    session: Arc<McpSession>,
}

impl McpSessionGuard {
    /// Stable session id, matching the path segment in
    /// `/mcp/<session_id>`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Endpoint URL given the daemon's HTTP bind address.
    pub fn endpoint(&self, base: &str) -> String {
        format!("{base}/mcp/{}", self.id)
    }

    /// Take the recorded tool calls in order, leaving the ledger
    /// empty. Safe to call multiple times.
    pub async fn drain(&self) -> Vec<LedgerEntry> {
        std::mem::take(&mut *self.session.ledger.lock().await)
    }

    /// Borrow the underlying session — used by tests.
    #[cfg(test)]
    pub fn session(&self) -> &Arc<McpSession> {
        &self.session
    }
}

impl Drop for McpSessionGuard {
    fn drop(&mut self) {
        if self.registry.sessions.remove(&self.id).is_none() {
            warn!(
                session_id = %self.id,
                "MCP session guard dropped, but session was already removed from registry"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Minimal test impl of ToolContext. We don't actually dispatch
    /// here — that's covered by the http::mcp tests.
    struct FakeCtx;
    impl crate::tools::ToolContext for FakeCtx {
        fn image_dir(&self) -> &str {
            ""
        }
        fn llm_client(&self) -> Option<&shore_llm::LlmClient> {
            None
        }
        fn image_gen_config(&self) -> Option<&crate::memory::compaction_impls::ImageGenConfig> {
            None
        }
        fn search_config(&self) -> &shore_config::app::SearchConfig {
            static C: std::sync::OnceLock<shore_config::app::SearchConfig> =
                std::sync::OnceLock::new();
            C.get_or_init(shore_config::app::SearchConfig::default)
        }
    }

    fn make_tool_defs() -> Vec<Value> {
        vec![
            json!({"name": "memory", "description": "Memory ops", "input_schema": {"type": "object"}}),
            json!({"name": "web_search", "description": "Search the web", "input_schema": {"type": "object"}}),
            json!({"name": "roll_dice", "description": "Roll dice", "input_schema": {"type": "object"}}),
        ]
    }

    #[test]
    fn allocate_registers_session_findable_by_id() {
        let reg = McpSessionRegistry::new();
        let allowed: HashSet<String> = ["memory".into(), "web_search".into()].into_iter().collect();
        let guard = reg.allocate("abc".into(), allowed, make_tool_defs(), Arc::new(FakeCtx));
        assert!(reg.get("abc").is_some());
        assert_eq!(guard.id(), "abc");
    }

    #[test]
    fn dropping_guard_removes_session_from_registry() {
        let reg = McpSessionRegistry::new();
        {
            let _g = reg.allocate(
                "drop-me".into(),
                HashSet::new(),
                Vec::new(),
                Arc::new(FakeCtx),
            );
            assert!(reg.get("drop-me").is_some());
        }
        assert!(reg.get("drop-me").is_none());
    }

    #[test]
    fn list_tools_filters_to_allowed() {
        let reg = McpSessionRegistry::new();
        let allowed: HashSet<String> = ["memory".into(), "web_search".into()].into_iter().collect();
        let guard = reg.allocate("s1".into(), allowed, make_tool_defs(), Arc::new(FakeCtx));
        let names: Vec<String> = guard
            .session()
            .list_tools()
            .iter()
            .filter_map(|d| d.get("name").and_then(Value::as_str).map(String::from))
            .collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"memory".into()));
        assert!(names.contains(&"web_search".into()));
        assert!(!names.contains(&"roll_dice".into()));
    }

    #[tokio::test]
    async fn ledger_records_in_order_and_drains() {
        let reg = McpSessionRegistry::new();
        let guard = reg.allocate("s2".into(), HashSet::new(), Vec::new(), Arc::new(FakeCtx));
        for i in 0..3 {
            guard
                .session()
                .record(LedgerEntry {
                    tool_use_id: format!("t-{i}"),
                    name: "memory".into(),
                    input: json!({"i": i}),
                    content: vec![ContentBlock::Text {
                        text: format!("ok {i}"),
                    }],
                    is_error: false,
                })
                .await;
        }
        let drained = guard.drain().await;
        assert_eq!(drained.len(), 3);
        for (i, e) in drained.iter().enumerate() {
            assert_eq!(e.tool_use_id, format!("t-{i}"));
        }
        // Second drain returns empty.
        let again = guard.drain().await;
        assert!(again.is_empty());
    }

    #[test]
    fn endpoint_concatenates_base_and_id() {
        let reg = McpSessionRegistry::new();
        let guard = reg.allocate(
            "11111111-2222-3333-4444-555555555555".into(),
            HashSet::new(),
            Vec::new(),
            Arc::new(FakeCtx),
        );
        assert_eq!(
            guard.endpoint("http://127.0.0.1:7321"),
            "http://127.0.0.1:7321/mcp/11111111-2222-3333-4444-555555555555"
        );
    }

    #[test]
    fn allows_checks_membership() {
        let reg = McpSessionRegistry::new();
        let allowed: HashSet<String> = ["memory".into()].into_iter().collect();
        let guard = reg.allocate("s3".into(), allowed, Vec::new(), Arc::new(FakeCtx));
        assert!(guard.session().allows("memory"));
        assert!(!guard.session().allows("web_search"));
    }
}
