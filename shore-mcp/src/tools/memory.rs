use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::handler::ShoreMcpHandler;

#[derive(Deserialize, JsonSchema, Debug)]
pub struct MemoryQueryParams {
    pub query: String,
    /// Skip the researcher and query the memory agent directly.
    #[serde(default)]
    pub direct: bool,
}

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct MemoryCompactParams {}

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct MemoryCollateParams {
    #[serde(default)]
    pub full: bool,
    pub limit: Option<u32>,
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct MemoryPurgeParams {
    pub older_than: String,
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct MemoryChangelogParams {
    #[serde(default = "default_changelog_limit")]
    pub limit: u32,
}

fn default_changelog_limit() -> u32 {
    20
}

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct MemoryReindexParams {}

#[tool_router(router = memory_router, vis = "pub")]
impl ShoreMcpHandler {
    #[tool(
        name = "memory_query",
        description = "Query the memory system via the researcher (or directly with direct=true). Read-only."
    )]
    pub async fn tool_memory_query(
        &self,
        Parameters(p): Parameters<MemoryQueryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "memory_query",
                "memory",
                json!({ "query": p.query, "direct": p.direct }),
            )
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "memory_compact",
        description = "Trigger a memory compaction pass. Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_memory_compact(
        &self,
        Parameters(_p): Parameters<MemoryCompactParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("memory_compact", "compact", json!({ "collate": true }))
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "memory_collate",
        description = "Run a memory collation pass. Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_memory_collate(
        &self,
        Parameters(p): Parameters<MemoryCollateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut args = json!({ "full": p.full });
        if let Some(l) = p.limit {
            args["limit"] = json!(l);
        }
        let data = self.run_cmd("memory_collate", "collate", args).await?;
        Self::json_result(data)
    }

    #[tool(
        name = "memory_purge",
        description = "Purge memory entries older than the given cutoff. Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_memory_purge(
        &self,
        Parameters(p): Parameters<MemoryPurgeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "memory_purge",
                "memory_purge",
                json!({ "older_than": p.older_than }),
            )
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "memory_changelog",
        description = "Recent memory changes log. Read-only."
    )]
    pub async fn tool_memory_changelog(
        &self,
        Parameters(p): Parameters<MemoryChangelogParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd(
                "memory_changelog",
                "memory_changelog",
                json!({ "limit": p.limit }),
            )
            .await?;
        Self::json_result(data)
    }

    #[tool(
        name = "memory_reindex",
        description = "Rebuild memory indices. Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_memory_reindex(
        &self,
        Parameters(_p): Parameters<MemoryReindexParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let data = self
            .run_cmd("memory_reindex", "memory_reindex", json!({}))
            .await?;
        Self::json_result(data)
    }
}
