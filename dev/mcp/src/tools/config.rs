use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::handler::ShoreMcpHandler;

#[derive(Deserialize, JsonSchema, Debug)]
pub struct ConfigGetParams {
    /// Config key to get. Omit `key` to return the full config.
    pub key: Option<String>,
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct ConfigSetParams {
    pub key: String,
    pub value: String,
}

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct ConfigCheckParams {}

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct ConfigResetParams {}

#[tool_router(router = config_router, vis = "pub")]
impl ShoreMcpHandler {
    #[tool(
        name = "config_get",
        description = "Get a config value by key. Omit `key` to return the full config. Read-only."
    )]
    pub async fn tool_config_get(
        &self,
        Parameters(p): Parameters<ConfigGetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let key = p.key.unwrap_or_default();
        let data = self
            .run_cmd("config_get", "config", json!({ "key": key, "value": null }))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "config_set",
        description = "Set a config value. Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_config_set(
        &self,
        Parameters(p): Parameters<ConfigSetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "config_set",
                "config",
                json!({ "key": p.key, "value": p.value }),
            )
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "config_check",
        description = "Validate configuration and return any warnings. Read-only."
    )]
    pub async fn tool_config_check(
        &self,
        Parameters(_p): Parameters<ConfigCheckParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("config_check", "config_check", json!({}))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "config_reset",
        description = "Reset runtime overrides and reload config from disk. Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_config_reset(
        &self,
        Parameters(_p): Parameters<ConfigResetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("config_reset", "config_reset", json!({}))
            .await?;
        Self::json_result(data)
    }
}
