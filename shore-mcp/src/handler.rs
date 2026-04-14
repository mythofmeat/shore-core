use std::sync::Arc;

use rmcp::handler::server::tool::ToolRouter;
use rmcp::tool_router;
use shore_client::SWPConnection;
use tokio::sync::Mutex;

use crate::cli::Cli;
use crate::gating::GateContext;

/// Handler struct passed to rmcp as the server state.
///
/// Holds the single `SWPConnection` to shore-daemon (wrapped in a Mutex
/// because MCP tool calls may be concurrent and we need serial SWP access),
/// plus the gate context for mutation-tool refusal.
pub struct ShoreMcpHandler {
    pub conn: Arc<Mutex<SWPConnection>>,
    pub gate: GateContext,
    pub(crate) tool_router: ToolRouter<Self>,
}

#[tool_router(server_handler)]
impl ShoreMcpHandler {
    pub fn new(conn: SWPConnection, cli: &Cli, profile_is_test: bool) -> Self {
        let gate = GateContext {
            profile_is_test,
            allow_main_writes: cli.allow_main_writes,
        };
        Self {
            conn: Arc::new(Mutex::new(conn)),
            gate,
            tool_router: Self::tool_router(),
        }
    }
}
