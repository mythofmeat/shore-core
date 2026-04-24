use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::handler::ShoreMcpHandler;

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct StatusParams {}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct DiagnosticsParams {
    #[serde(default = "default_count")]
    pub count: u32,
}

fn default_count() -> u32 {
    10
}

#[tool_router(router = status_router, vis = "pub")]
impl ShoreMcpHandler {
    #[tool(
        name = "status",
        description = "Show daemon and session status. Returns the full status JSON."
    )]
    pub async fn tool_status(
        &self,
        Parameters(_p): Parameters<StatusParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self.run_cmd("status", "status", json!({})).await?;
        Self::json_result(data)
    }

    #[tool(
        name = "status_diagnostics",
        description = "Show recent API calls, tool invocations, and errors from the daemon."
    )]
    pub async fn tool_status_diagnostics(
        &self,
        Parameters(p): Parameters<DiagnosticsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "status_diagnostics",
                "diagnostics",
                json!({ "count": p.count }),
            )
            .await?;
        Self::json_result(data)
    }
}
