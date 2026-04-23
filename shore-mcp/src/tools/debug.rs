use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::handler::ShoreMcpHandler;

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct DebugEmptyParams {}

#[tool_router(router = debug_router, vis = "pub")]
impl ShoreMcpHandler {
    #[tool(
        name = "debug_tick_now",
        description = "Force a heartbeat tick right now. Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_debug_tick_now(
        &self,
        Parameters(_p): Parameters<DebugEmptyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("debug_tick_now", "heartbeat_tick_now", json!({}))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "debug_status_dormant",
        description = "Set heartbeat status to dormant. Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_debug_status_dormant(
        &self,
        Parameters(_p): Parameters<DebugEmptyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("debug_status_dormant", "heartbeat_set_dormant", json!({}))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "debug_status_active",
        description = "Set heartbeat status to active. Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_debug_status_active(
        &self,
        Parameters(_p): Parameters<DebugEmptyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("debug_status_active", "heartbeat_set_active", json!({}))
            .await?;
        Self::json_result(data)
    }
}
