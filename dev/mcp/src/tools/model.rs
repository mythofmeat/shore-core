use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::handler::ShoreMcpHandler;

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct ModelListParams {}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct ModelInfoParams {
    pub name: Option<String>,
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct ModelSwitchParams {
    pub name: String,
}

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct ModelResetParams {}

#[tool_router(router = model_router, vis = "pub")]
impl ShoreMcpHandler {
    #[tool(name = "model_list", description = "List all available models.")]
    pub async fn tool_model_list(
        &self,
        Parameters(_p): Parameters<ModelListParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self.run_cmd("model_list", "list_models", json!({})).await?;
        Self::json_result(data)
    }

    #[tool(
        name = "model_info",
        description = "Show details for a model. Omit name to query the active model."
    )]
    pub async fn tool_model_info(
        &self,
        Parameters(p): Parameters<ModelInfoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let args = match p.name {
            Some(n) => json!({ "name": n }),
            None => json!({}),
        };
        let data = self.run_cmd("model_info", "model_info", args).await?;
        Self::json_result(data)
    }

    #[tool(
        name = "model_switch",
        description = "Switch the active chat model. Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_model_switch(
        &self,
        Parameters(p): Parameters<ModelSwitchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("model_switch", "switch_model", json!({ "name": p.name }))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "model_reset",
        description = "Reset the active model to the config default. Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_model_reset(
        &self,
        Parameters(_p): Parameters<ModelResetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("model_reset", "reset_model", json!({}))
            .await?;
        Self::json_result(data)
    }
}
