use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::handler::ShoreMcpHandler;

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct UsageParams {
    /// Time period: "today", "4h", "7d", "30d", "all". Default: "today".
    #[serde(default = "default_last")]
    pub last: String,
    pub character: Option<String>,
    pub provider: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub call_type: Option<String>,
    /// Group results by call type instead of filtering.
    #[serde(default)]
    pub by_call_type: bool,
    /// Group results by higher-level usage kind.
    #[serde(default)]
    pub by_kind: bool,
    /// Group results by provider and configured API key name.
    #[serde(default)]
    pub by_api_key: bool,
    #[serde(default)]
    pub anomalies: bool,
}

fn default_last() -> String {
    "today".to_string()
}

#[tool_router(router = usage_router, vis = "pub")]
impl ShoreMcpHandler {
    #[tool(
        name = "usage",
        description = "Token usage statistics and costs. Read-only — excludes refresh_pricing / recalculate / export_csv which are CLI-only."
    )]
    pub async fn tool_usage(
        &self,
        Parameters(p): Parameters<UsageParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "usage",
                "usage",
                json!({
                    "last": p.last,
                    "character": p.character,
                    "provider": p.provider,
                    "api_key": p.api_key,
                    "model": p.model,
                    "call_type": p.call_type,
                    "by_call_type": p.by_call_type,
                    "by_kind": p.by_kind,
                    "by_api_key": p.by_api_key,
                    "anomalies": p.anomalies,
                    "export_csv": false,
                    "export_tsv": false,
                    "refresh_pricing": false,
                    "recalculate": false,
                    "force": false,
                }),
            )
            .await?;
        Self::json_result(data)
    }
}
