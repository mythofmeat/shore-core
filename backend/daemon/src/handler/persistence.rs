//! Persistence and notification for completed generations.
//!
//! Writes assistant messages to the conversation engine, records diagnostics,
//! tracks token usage, and sends push notifications.

use std::sync::Arc;
use std::time::Instant;

use shore_protocol::types::{derive_content_from_blocks, ContentBlock, Message, Role};
use tokio::sync::Mutex;
use tracing::{info, instrument};

use super::GenContext;

/// Phase 12: Persist messages, record diagnostics, and send notifications.
#[instrument(skip(ctx, engine_arc, result, request, tool_intermediate_messages), fields(char = char_name, model = %resolved.qualified_name))]
#[allow(clippy::too_many_arguments)]
pub(super) async fn persist_and_notify(
    ctx: &GenContext,
    engine_arc: &Arc<Mutex<crate::engine::ConversationEngine>>,
    char_name: &str,
    resolved: &shore_config::models::ResolvedModel,
    result: &shore_llm::types::StreamResult,
    request: &shore_llm::types::LlmRequest,
    tool_intermediate_messages: Vec<Message>,
    wall_clock_start: Instant,
    preserve_prior_turn_thinking: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let notify_content = {
        let mut engine = engine_arc.lock().await;

        for msg in tool_intermediate_messages {
            engine.append_message(msg)?;
        }

        // Track cumulative token usage.
        {
            let mut tokens = ctx.session_tokens.lock().unwrap_or_else(|e| e.into_inner());
            tokens.input += result.usage.input_tokens;
            tokens.output += result.usage.output_tokens;
            tokens.cache_read += result.usage.cache_read_tokens;
            tokens.cache_write += result.usage.cache_creation_tokens;
        }

        // Record API call in diagnostics ring buffer.
        {
            let entry = shore_diagnostics::ApiCallEntry {
                timestamp: chrono::Local::now().to_rfc3339(),
                model: result.model.clone(),
                provider: request
                    .provider_key
                    .clone()
                    .unwrap_or_else(|| resolved.provider_key.clone()),
                input_tokens: result.usage.input_tokens,
                output_tokens: result.usage.output_tokens,
                cache_read_tokens: result.usage.cache_read_tokens,
                cache_write_tokens: result.usage.cache_creation_tokens,
                ttft_ms: result.timing.time_to_first_token_ms,
                total_ms: result.timing.total_ms,
                finish_reason: result.finish_reason.clone(),
                total_cost_usd: result.usage.total_cost_usd,
                rate_limit_info: result.usage.rate_limit_info.clone(),
                error: None,
            };
            ctx.diagnostics
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .api_calls
                .push(entry);
        }

        info!(
            input_tokens = result.usage.input_tokens,
            output_tokens = result.usage.output_tokens,
            cache_read = result.usage.cache_read_tokens,
            cache_creation = result.usage.cache_creation_tokens,
            model = %result.model,
            "Response complete"
        );

        let content_blocks = if result.content_blocks.is_empty() && !result.content.is_empty() {
            vec![ContentBlock::Text {
                text: result.content.clone(),
            }]
        } else {
            result.content_blocks.clone()
        };
        let content = derive_content_from_blocks(&content_blocks);

        // Include the assistant response in last_request so the
        // heartbeat system sees a complete conversation ending on an
        // assistant turn — not the user turn that triggered this call.
        // The turn has just ended (finish_reason != tool_use), so from the
        // perspective of any future request this entire message list is
        // history: strip thinking blocks across all assistant messages
        // here to keep the next-turn cache prefix consistent with what
        // `build_llm_messages` will emit. Skip the strip only when the
        // user has explicitly opted to preserve prior-turn thinking.
        {
            let mut full_request = request.clone();
            let assistant_api_content: Vec<serde_json::Value> = content_blocks
                .iter()
                .filter_map(|block| {
                    crate::content_util::content_block_to_request_json_for_sdk(block, &request.sdk)
                })
                .collect();
            full_request.messages.push(serde_json::json!({
                "role": "assistant",
                "content": assistant_api_content,
            }));
            crate::content_util::maybe_strip_prior_thinking(
                &mut full_request.messages,
                preserve_prior_turn_thinking,
                &resolved.provider_key,
            );
            ctx.autonomy.notify_last_request(char_name, full_request);
        }
        let notify_content = content.clone();
        let assistant_msg = Message {
            msg_id: format!("m_{}", uuid::Uuid::new_v4()),
            role: Role::Assistant,
            content,
            images: vec![],
            content_blocks,
            alt_index: None,
            alt_count: None,
            timestamp: chrono::Local::now().to_rfc3339(),
        };
        engine.append_message(assistant_msg)?;
        ctx.autonomy
            .notify_assistant_message(char_name, engine.turn_count());
        notify_content
    }; // engine lock released

    let wall_clock_ms = wall_clock_start.elapsed().as_millis() as u32;
    ctx.notifier.notify_message_complete(
        &format!("Shore — {char_name}"),
        &notify_content,
        wall_clock_ms,
    );

    Ok(())
}
