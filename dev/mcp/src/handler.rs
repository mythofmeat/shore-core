use std::sync::Arc;

use rmcp::handler::server::tool::ToolRouter;
use rmcp::model::{CallToolResult, Content};
use rmcp::{tool_handler, ErrorData, ServerHandler};
use serde_json::Value;
use shore_protocol::server_msg::ServerMessage;
use shore_swp_client::SWPConnection;
use tokio::sync::Mutex;

use crate::cli::Cli;
use crate::gating::GateContext;

/// Handler struct passed to rmcp as the server state.
///
/// The `SWPConnection` is wrapped in a Mutex because SWP has no
/// request-id multiplexing on a single stream: `run_cmd` must hold the
/// lock across send AND the full recv loop so no other tool call
/// interleaves and steals our `CommandOutput` frame. Shrinking that
/// critical section would desync the stream, not just parallelise it.
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
        let tool_router = Self::all_tools_router();
        Self {
            conn: Arc::new(Mutex::new(conn)),
            gate,
            tool_router,
        }
    }
}

impl ShoreMcpHandler {
    /// Composition of every per-category tool router. Each tool task
    /// (12-17) extends this chain by one summand.
    pub(crate) fn all_tools_router() -> ToolRouter<Self> {
        Self::status_router()
            + Self::log_router()
            + Self::usage_router()
            + Self::character_router()
            + Self::model_router()
            + Self::memory_router()
            + Self::config_router()
            + Self::debug_router()
            + Self::send_router()
    }

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
                return Err(ErrorData::internal_error(msg, None));
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
                ServerMessage::Shutdown(_) => {
                    return Err(ErrorData::internal_error(
                        "daemon shut down before returning CommandOutput",
                        None,
                    ));
                }
                // Benign async-push frames: the daemon emits these outside
                // of command/response scope. Keep waiting for our reply.
                ServerMessage::Hello(_)
                | ServerMessage::History(_)
                | ServerMessage::Ping(_)
                | ServerMessage::NewMessage(_)
                | ServerMessage::SendImage(_)
                | ServerMessage::Phase(_)
                | ServerMessage::CacheWarning(_)
                | ServerMessage::ProviderFallbackWarning(_)
                | ServerMessage::AudioStart(_)
                | ServerMessage::AudioChunk(_)
                | ServerMessage::AudioEnd(_)
                | ServerMessage::AudioError(_) => {}
                // Streaming frames are only legitimate for send/regen
                // (Task 16), which owns its own helper. If we see them
                // here the daemon sent a stream for a request we didn't
                // make — warn but keep draining rather than hang.
                ServerMessage::StreamStart(_)
                | ServerMessage::StreamChunk(_)
                | ServerMessage::StreamEnd(_)
                | ServerMessage::ToolCall(_)
                | ServerMessage::ToolResult(_) => {
                    tracing::warn!(
                        tool = tool_name,
                        "run_cmd: unexpected stream frame for read-only command, ignoring"
                    );
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
