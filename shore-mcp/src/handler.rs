use std::sync::Arc;

use rmcp::handler::server::tool::ToolRouter;
use rmcp::model::{CallToolResult, Content};
use rmcp::{tool_handler, ErrorData, ServerHandler};
use serde_json::Value;
use shore_client::SWPConnection;
use shore_protocol::server_msg::ServerMessage;
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

impl ShoreMcpHandler {
    pub fn new(conn: SWPConnection, cli: &Cli, profile_is_test: bool) -> Self {
        let gate = GateContext {
            profile_is_test,
            allow_main_writes: cli.allow_main_writes,
        };
        let tool_router = ShoreMcpHandler::status_router()
            + ShoreMcpHandler::log_router()
            + ShoreMcpHandler::usage_router();
        Self {
            conn: Arc::new(Mutex::new(conn)),
            gate,
            tool_router,
        }
    }
}

impl ShoreMcpHandler {
    /// Check gates, send an SWP command, drain to CommandOutput, return JSON.
    pub(crate) async fn run_cmd(
        &self,
        tool_name: &str,
        swp_name: &str,
        args: Value,
    ) -> Result<Value, ErrorData> {
        match crate::gating::check(tool_name, &self.gate) {
            crate::gating::GateDecision::Allow => {}
            crate::gating::GateDecision::Refuse(msg) => {
                return Err(ErrorData::invalid_params(msg, None));
            }
        }

        let mut conn = self.conn.lock().await;
        conn.send_command(swp_name, args)
            .await
            .map_err(|e| ErrorData::internal_error(format!("send_command: {e}"), None))?;

        loop {
            let msg = conn
                .recv()
                .await
                .map_err(|e| ErrorData::internal_error(format!("recv: {e}"), None))?;
            match msg {
                ServerMessage::CommandOutput(co) => return Ok(co.data),
                ServerMessage::Error(err) => {
                    return Err(ErrorData::internal_error(err.message, None));
                }
                ServerMessage::Ping(_)
                | ServerMessage::History(_)
                | ServerMessage::NewMessage(_)
                | ServerMessage::SendImage(_)
                | ServerMessage::Phase(_) => {}
                other => {
                    tracing::debug!(?other, "run_cmd: ignoring unexpected frame");
                }
            }
        }
    }

    /// Wrap a JSON Value as a successful `CallToolResult`.
    pub(crate) fn json_result(data: Value) -> Result<CallToolResult, ErrorData> {
        let content = Content::text(
            serde_json::to_string_pretty(&data)
                .unwrap_or_else(|_| "<non-serializable>".to_string()),
        );
        Ok(CallToolResult::success(vec![content]))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ShoreMcpHandler {}
