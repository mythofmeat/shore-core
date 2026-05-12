//! Persistence and notification for completed generations.
//!
//! Writes assistant messages to the conversation engine, records diagnostics,
//! tracks token usage, and sends push notifications.

use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};
use shore_config::models::Sdk;
use shore_protocol::server_msg::{MessageOrigin, NewMessage, ServerMessage};
use shore_protocol::types::{derive_content_from_blocks, ContentBlock, Message, Role};
use tokio::sync::{broadcast, Mutex};
use tracing::{info, instrument};

use crate::engine::messages::{MessageStore, PendingAlt};

use super::GenContext;

#[derive(Debug, Clone, PartialEq)]
struct CompletedResponseMessage {
    role: Role,
    content_blocks: Vec<ContentBlock>,
}

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
    regen_alt: Option<PendingAlt>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let notify_content = {
        let mut engine = engine_arc.lock().await;

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

        let response_messages = completed_response_messages(result, &request.sdk);

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
            append_response_messages_to_request(
                &mut full_request,
                &response_messages,
                &request.sdk,
            );
            crate::content_util::maybe_strip_prior_thinking(
                &mut full_request.messages,
                preserve_prior_turn_thinking,
                &resolved.provider_key,
            );
            ctx.autonomy.notify_last_request(char_name, full_request);
        }
        let notify_content = notify_content_from_response_messages(&response_messages);
        let mut generated_messages = tool_intermediate_messages;
        let response_messages: Vec<Message> = response_messages
            .into_iter()
            .map(message_from_response)
            .collect();
        let response_event_ids: Vec<String> = response_messages
            .iter()
            .filter(|msg| msg.role == Role::Assistant)
            .map(|msg| msg.msg_id.clone())
            .collect();
        generated_messages.extend(response_messages);
        if let Some(pending) = regen_alt {
            MessageStore::attach_generated_alt(&mut generated_messages, pending.alternatives);
            let event_messages: Vec<Message> = generated_messages
                .iter()
                .filter(|msg| {
                    response_event_ids
                        .iter()
                        .any(|msg_id| msg_id == &msg.msg_id)
                })
                .cloned()
                .collect();
            engine.replace_after_last_user_turn(generated_messages)?;
            let revision = engine.current_revision();
            for msg in &event_messages {
                emit_new_message_event(
                    &ctx.event_tx,
                    char_name,
                    MessageOrigin::AssistantReply,
                    revision,
                    msg,
                );
            }
        } else {
            for msg in generated_messages {
                let event_msg = response_event_ids
                    .iter()
                    .any(|msg_id| msg_id == &msg.msg_id)
                    .then(|| msg.clone());
                engine.append_message(msg)?;
                if let Some(msg) = event_msg {
                    emit_new_message_event(
                        &ctx.event_tx,
                        char_name,
                        MessageOrigin::AssistantReply,
                        engine.current_revision(),
                        &msg,
                    );
                }
            }
        }
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

fn emit_new_message_event(
    event_tx: &broadcast::Sender<ServerMessage>,
    character: &str,
    origin: MessageOrigin,
    revision: u64,
    msg: &Message,
) {
    let mut wire_msg = msg.clone();
    crate::handler::embed_image_data(&mut wire_msg.images);
    let _ = event_tx.send(ServerMessage::NewMessage(NewMessage {
        revision,
        character: Some(character.to_string()),
        origin: Some(origin),
        message: wire_msg,
    }));
}

fn message_from_response(response_msg: CompletedResponseMessage) -> Message {
    let content = derive_content_from_blocks(&response_msg.content_blocks);
    Message {
        msg_id: format!("m_{}", uuid::Uuid::new_v4()),
        role: response_msg.role,
        content,
        images: vec![],
        content_blocks: response_msg.content_blocks,
        alt_index: None,
        alt_count: None,
        alternatives: vec![],
        timestamp: chrono::Local::now().to_rfc3339(),
    }
}

fn completed_response_messages(
    result: &shore_llm::types::StreamResult,
    sdk: &Sdk,
) -> Vec<CompletedResponseMessage> {
    let content_blocks = content_blocks_for_result(result);
    if matches!(sdk, Sdk::ClaudeCode) {
        split_claude_code_response_blocks(content_blocks)
    } else {
        vec![CompletedResponseMessage {
            role: Role::Assistant,
            content_blocks,
        }]
    }
}

fn content_blocks_for_result(result: &shore_llm::types::StreamResult) -> Vec<ContentBlock> {
    if result.content_blocks.is_empty() && !result.content.is_empty() {
        vec![ContentBlock::Text {
            text: result.content.clone(),
        }]
    } else {
        result.content_blocks.clone()
    }
}

fn split_claude_code_response_blocks(blocks: Vec<ContentBlock>) -> Vec<CompletedResponseMessage> {
    let mut messages = Vec::new();
    let mut assistant_blocks = Vec::new();

    for block in blocks {
        match block {
            ContentBlock::ToolResult { .. } => {
                if !assistant_blocks.is_empty() {
                    messages.push(CompletedResponseMessage {
                        role: Role::Assistant,
                        content_blocks: std::mem::take(&mut assistant_blocks),
                    });
                }
                messages.push(CompletedResponseMessage {
                    role: Role::User,
                    content_blocks: vec![block],
                });
            }
            other => assistant_blocks.push(other),
        }
    }

    if !assistant_blocks.is_empty() || messages.is_empty() {
        messages.push(CompletedResponseMessage {
            role: Role::Assistant,
            content_blocks: assistant_blocks,
        });
    }

    messages
}

fn append_response_messages_to_request(
    request: &mut shore_llm::types::LlmRequest,
    response_messages: &[CompletedResponseMessage],
    sdk: &Sdk,
) {
    for message in response_messages {
        let api_content: Vec<Value> = message
            .content_blocks
            .iter()
            .filter_map(|block| {
                crate::content_util::content_block_to_request_json_for_sdk(block, sdk)
            })
            .collect();
        request.messages.push(json!({
            "role": request_role(&message.role),
            "content": api_content,
        }));
    }
}

fn request_role(role: &Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
    }
}

fn notify_content_from_response_messages(messages: &[CompletedResponseMessage]) -> String {
    let text = messages
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .map(|message| derive_content_from_blocks(&message.content_blocks))
        .filter(|content| !content.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    if text.is_empty() {
        messages
            .iter()
            .map(|message| derive_content_from_blocks(&message.content_blocks))
            .filter(|content| !content.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use shore_llm::types::{LlmRequest, StreamResult, Timing, Usage};

    fn stream_result(content_blocks: Vec<ContentBlock>) -> StreamResult {
        StreamResult {
            content: derive_content_from_blocks(&content_blocks),
            model: "claude-sonnet-4-5".into(),
            finish_reason: "end_turn".into(),
            usage: Usage::default(),
            timing: Timing::default(),
            tool_uses: vec![],
            content_blocks,
        }
    }

    fn request() -> LlmRequest {
        LlmRequest {
            sdk: Sdk::ClaudeCode,
            model: "claude-sonnet-4-5".into(),
            api_key: String::new(),
            base_url: None,
            messages: vec![json!({"role": "user", "content": "please use a tool"})],
            system: None,
            tools: None,
            max_tokens: 1024,
            temperature: None,
            top_p: None,
            provider_options: None,
            provider_key: Some("claude_code".into()),
            rid: None,
            forensic_character: None,
        }
    }

    #[test]
    fn claude_code_tool_results_are_persisted_as_user_messages() {
        let result = stream_result(vec![
            ContentBlock::Text {
                text: "Checking. ".into(),
            },
            ContentBlock::ToolUse {
                id: "toolu_1".into(),
                name: "read".into(),
                input: json!({"path": "notes.txt"}),
            },
            ContentBlock::ToolResult {
                tool_use_id: "toolu_1".into(),
                content: "file contents".into(),
                is_error: false,
            },
            ContentBlock::Text {
                text: "Done.".into(),
            },
        ]);

        let messages = completed_response_messages(&result, &Sdk::ClaudeCode);

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, Role::Assistant);
        assert_eq!(messages[1].role, Role::User);
        assert_eq!(messages[2].role, Role::Assistant);
        assert!(matches!(
            messages[0].content_blocks.last(),
            Some(ContentBlock::ToolUse { id, .. }) if id == "toolu_1"
        ));
        assert!(matches!(
            messages[1].content_blocks.as_slice(),
            [ContentBlock::ToolResult { tool_use_id, .. }] if tool_use_id == "toolu_1"
        ));
    }

    #[test]
    fn claude_code_last_request_tool_pairs_survive_sanitizer() {
        let result = stream_result(vec![
            ContentBlock::ToolUse {
                id: "toolu_1".into(),
                name: "read".into(),
                input: json!({"path": "notes.txt"}),
            },
            ContentBlock::ToolResult {
                tool_use_id: "toolu_1".into(),
                content: "file contents".into(),
                is_error: false,
            },
            ContentBlock::Text {
                text: "Done.".into(),
            },
        ]);
        let messages = completed_response_messages(&result, &Sdk::ClaudeCode);
        let mut req = request();

        append_response_messages_to_request(&mut req, &messages, &Sdk::ClaudeCode);

        assert!(shore_llm::sanitize::sanitize_tool_pairs(&req.messages).is_none());
        assert_eq!(req.messages[1]["role"], "assistant");
        assert_eq!(req.messages[2]["role"], "user");
        assert_eq!(req.messages[2]["content"][0]["type"], "tool_result");
    }
}
