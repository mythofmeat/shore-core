use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use shore_client::collect_stream;
use shore_protocol::client_msg::MessageOverrides;

use crate::handler::ShoreMcpHandler;

#[derive(Deserialize, JsonSchema, Debug)]
pub struct SendParams {
    /// Message text.
    pub text: String,
    /// Optional sampling temperature override for this message.
    pub temperature: Option<f64>,
    /// Optional top-p override.
    pub top_p: Option<f64>,
    /// Optional extended-thinking budget in tokens. Pass 0 to disable.
    pub thinking: Option<u32>,
    /// If true, inject as a system instruction instead of a user message.
    #[serde(default)]
    pub system: bool,
}

#[derive(Deserialize, JsonSchema, Debug, Default)]
pub struct RegenParams {
    /// Optional guidance for the regeneration.
    pub guidance: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct SendOutput {
    pub text: String,
    pub finish_reason: String,
    pub model: String,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub total_ms: u32,
    pub tool_calls: usize,
    pub tool_results: usize,
}

#[tool_router(router = send_router, vis = "pub")]
impl ShoreMcpHandler {
    #[tool(
        name = "send",
        description = "Send a message to the active character and return the full assembled response. Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_send(
        &self,
        Parameters(p): Parameters<SendParams>,
    ) -> Result<CallToolResult, ErrorData> {
        if p.system {
            // `run_cmd` handles gating + locking + exhaustive-match draining
            // (including Shutdown → terminal error from Task 12).
            let data = self
                .run_cmd("send", "inject_system", json!({ "text": p.text }))
                .await?;
            return Self::json_result(data);
        }

        // Non-system path: gate first, then drive the stream ourselves
        // (run_cmd can't help here — we need send_message_full, not send_command).
        match crate::gating::check("send", &self.gate) {
            crate::gating::GateDecision::Allow => {}
            crate::gating::GateDecision::Refuse(msg) => {
                return Err(ErrorData::internal_error(msg, None));
            }
        }

        let overrides = if p.temperature.is_some() || p.top_p.is_some() || p.thinking.is_some() {
            Some(MessageOverrides {
                temperature: p.temperature,
                top_p: p.top_p,
                thinking_budget: p.thinking,
            })
        } else {
            None
        };

        let mut conn = self.conn.lock().await;
        conn.send_message_full(&p.text, true, vec![], overrides)
            .await
            .map_err(|e| ErrorData::internal_error(format!("send_message_full: {e}"), None))?;

        let resp = collect_stream(&mut conn)
            .await
            .map_err(|e| ErrorData::internal_error(format!("collect_stream: {e}"), None))?;

        let output = SendOutput {
            text: resp.text,
            finish_reason: resp.finish_reason,
            model: resp.metadata.model,
            tokens_in: resp.metadata.tokens.input,
            tokens_out: resp.metadata.tokens.output,
            total_ms: resp.metadata.timing.total_ms,
            tool_calls: resp.tool_calls.len(),
            tool_results: resp.tool_results.len(),
        };
        Self::json_result(serde_json::to_value(output).unwrap())
    }

    #[tool(
        name = "regen",
        description = "Regenerate the last assistant response, optionally with guidance. Mutating — refused on main without --allow-main-writes."
    )]
    pub async fn tool_regen(
        &self,
        Parameters(p): Parameters<RegenParams>,
    ) -> Result<CallToolResult, ErrorData> {
        match crate::gating::check("regen", &self.gate) {
            crate::gating::GateDecision::Allow => {}
            crate::gating::GateDecision::Refuse(msg) => {
                return Err(ErrorData::internal_error(msg, None));
            }
        }

        let mut conn = self.conn.lock().await;
        conn.send_regen(true, p.guidance.clone())
            .await
            .map_err(|e| ErrorData::internal_error(format!("send_regen: {e}"), None))?;

        let resp = collect_stream(&mut conn)
            .await
            .map_err(|e| ErrorData::internal_error(format!("collect_stream: {e}"), None))?;

        let output = SendOutput {
            text: resp.text,
            finish_reason: resp.finish_reason,
            model: resp.metadata.model,
            tokens_in: resp.metadata.tokens.input,
            tokens_out: resp.metadata.tokens.output,
            total_ms: resp.metadata.timing.total_ms,
            tool_calls: resp.tool_calls.len(),
            tool_results: resp.tool_results.len(),
        };
        Self::json_result(serde_json::to_value(output).unwrap())
    }
}
