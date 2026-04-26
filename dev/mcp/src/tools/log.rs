use std::time::{Duration, Instant};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use shore_protocol::server_msg::ServerMessage;

use crate::handler::ShoreMcpHandler;

#[derive(Deserialize, JsonSchema, Debug)]
pub struct LogTailParams {
    /// Number of recent messages to return.
    #[serde(default = "default_tail_count")]
    pub count: u32,
}

fn default_tail_count() -> u32 {
    20
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct LogFollowParams {
    #[serde(default = "default_follow_seconds")]
    pub seconds: u64,
    #[serde(default = "default_follow_cap")]
    pub cap: u32,
}

fn default_follow_seconds() -> u64 {
    5
}

fn default_follow_cap() -> u32 {
    50
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct LogShowParams {
    /// Message reference (e.g. "last", "-1", "3").
    pub msg_ref: String,
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct LogDeleteParams {
    /// Message refs to delete.
    pub msg_refs: Vec<String>,
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct LogEditParams {
    pub msg_ref: String,
    pub content: String,
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct LogHeartbeatParams {
    #[serde(default = "default_tail_count")]
    pub count: u32,
}

#[tool_router(router = log_router, vis = "pub")]
impl ShoreMcpHandler {
    #[tool(
        name = "log_tail",
        description = "Return the last N messages from the conversation log."
    )]
    pub async fn tool_log_tail(
        &self,
        Parameters(p): Parameters<LogTailParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("log_tail", "log", json!({ "count": p.count }))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "log_show",
        description = "Fetch a single message by reference (last, -1, or a numeric index)."
    )]
    pub async fn tool_log_show(
        &self,
        Parameters(p): Parameters<LogShowParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("log_show", "get", json!({ "ref": p.msg_ref }))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "log_heartbeat",
        description = "Show heartbeat probe decisions and timing history for the last N messages."
    )]
    pub async fn tool_log_heartbeat(
        &self,
        Parameters(p): Parameters<LogHeartbeatParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "log_heartbeat",
                "heartbeat_log",
                json!({ "count": p.count }),
            )
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "log_delete",
        description = "Delete one or more messages from the conversation log. Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_log_delete(
        &self,
        Parameters(p): Parameters<LogDeleteParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("log_delete", "delete", json!({ "refs": p.msg_refs }))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "log_edit",
        description = "Edit the content of a single message in the conversation log. Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_log_edit(
        &self,
        Parameters(p): Parameters<LogEditParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "log_edit",
                "edit",
                json!({ "ref": p.msg_ref, "content": p.content }),
            )
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "log_follow",
        description = "Tail the log for new messages for a bounded duration (max 60 seconds). Returns whatever arrives before the timeout or message cap. Read-only. While running, other MCP tool calls against this daemon will wait."
    )]
    pub async fn tool_log_follow(
        &self,
        Parameters(p): Parameters<LogFollowParams>,
    ) -> Result<CallToolResult, ErrorData> {
        // Gate (read-only, but gate to keep consistency).
        match crate::gating::check("log_follow", &self.gate) {
            crate::gating::GateDecision::Allow => {}
            crate::gating::GateDecision::Refuse(msg) => {
                return Err(ErrorData::internal_error(msg, None));
            }
        }

        let seconds = p.seconds.min(60);
        let mut conn = self.conn.lock().await;
        let deadline = Instant::now() + Duration::from_secs(seconds);
        let mut collected: Vec<serde_json::Value> = Vec::new();

        while Instant::now() < deadline && collected.len() < p.cap as usize {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let recv_fut = conn.recv();
            let msg = match tokio::time::timeout(remaining, recv_fut).await {
                Ok(Ok(m)) => m,
                Ok(Err(e)) => {
                    return Err(ErrorData::internal_error(format!("recv: {e}"), None));
                }
                Err(_elapsed) => break,
            };
            match msg {
                ServerMessage::NewMessage(nm) => {
                    collected.push(
                        serde_json::to_value(nm)
                            .expect("NewMessage: Serialize derive is infallible"),
                    );
                }
                ServerMessage::Ping(_) | ServerMessage::History(_) | ServerMessage::Phase(_) => {}
                ServerMessage::Shutdown(_) => break,
                _ => {}
            }
        }

        Self::json_result(json!({ "messages": collected }))
    }
}
