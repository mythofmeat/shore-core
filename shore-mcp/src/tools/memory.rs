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
    /// Return raw markdown search matches instead of an LLM-synthesized answer.
    #[serde(default)]
    pub direct: bool,
}

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct MemoryCompactParams {
    /// Optional override for retained user turns at the tail of active.jsonl.
    /// 0 = retain none (full pipeline runs, compaction digest becomes the carry-forward).
    /// Omitted = use the configured default.
    pub keep_turns: Option<u32>,
}

#[derive(Deserialize, JsonSchema, Debug)]
pub struct MemoryChangelogParams {
    #[serde(default = "default_changelog_limit")]
    pub limit: u32,
}

fn default_changelog_limit() -> u32 {
    20
}

#[tool_router(router = memory_router, vis = "pub")]
impl ShoreMcpHandler {
    #[tool(
        name = "memory_query",
        description = "Query markdown memory. Set direct=true for raw text matches. Read-only."
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
        description = "Trigger a memory compaction pass. Optional keep_turns overrides retained user turns (0 = retain none, leaving only the system prompt and compaction digest). Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_memory_compact(
        &self,
        Parameters(p): Parameters<MemoryCompactParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut args = json!({});
        if let Some(n) = p.keep_turns {
            args["keep_turns"] = json!(n);
        }
        let data = self.run_cmd("memory_compact", "compact", args).await?;
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
}
