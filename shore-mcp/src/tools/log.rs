use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

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
            .run_cmd("log_heartbeat", "heartbeat_log", json!({ "count": p.count }))
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
}
