use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::handler::ShoreMcpHandler;

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct CharacterListParams {}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct CharacterInfoParams {
    /// Character name to query. Omit to return info for the current character.
    pub name: Option<String>,
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct CharacterSwitchParams {
    pub name: String,
}

#[tool_router(router = character_router, vis = "pub")]
impl ShoreMcpHandler {
    #[tool(
        name = "character_list",
        description = "List all available characters."
    )]
    pub async fn tool_character_list(
        &self,
        Parameters(_p): Parameters<CharacterListParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("character_list", "list_characters", json!({}))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "character_info",
        description = "Show details for a character. Omit `name` to query the current character."
    )]
    pub async fn tool_character_info(
        &self,
        Parameters(p): Parameters<CharacterInfoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let args = match p.name {
            Some(n) => json!({ "name": n }),
            None => json!({}),
        };
        let data = self.run_cmd("character_info", "character_info", args).await?;
        Self::json_result(data)
    }

    #[tool(
        name = "character_switch",
        description = "Switch the active character. Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_character_switch(
        &self,
        Parameters(p): Parameters<CharacterSwitchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "character_switch",
                "switch_character",
                json!({ "name": p.name }),
            )
            .await?;
        Self::json_result(data)
    }
}
