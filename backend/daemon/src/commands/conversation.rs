use serde_json::json;
use shore_protocol::error::ErrorCode;
use shore_protocol::types::{ContentBlock, Message, Role};
use tracing::debug;

use super::{engine_err, CommandContext, CommandResult};
use crate::engine::ConversationEngine;

/// Resolve a message reference to a concrete msg_id.
///
/// Supports: `"last"`, `"latest"`, negative indices (`"-1"` = last),
/// positive 1-based indices (`"3"` = third message), or literal msg_ids (passthrough).
fn resolve_ref(messages: &[Message], reference: &str) -> Result<String, (ErrorCode, String)> {
    if reference == "last" || reference == "latest" {
        return messages
            .last()
            .map(|m| m.msg_id.clone())
            .ok_or_else(|| (ErrorCode::NotFound, "No messages in conversation".into()));
    }

    if let Ok(n) = reference.parse::<i64>() {
        if n == 0 {
            return Err((
                ErrorCode::InvalidRequest,
                "Message index must be non-zero (use 1 for first, -1 for last)".into(),
            ));
        }

        let idx = if n < 0 {
            // -1 = last, -2 = second-to-last, etc.
            (messages.len() as i64) + n
        } else {
            // 1-based positive index
            n - 1
        };

        if idx < 0 || idx as usize >= messages.len() {
            return Err((
                ErrorCode::NotFound,
                format!(
                    "Message index {} out of range (conversation has {} messages)",
                    reference,
                    messages.len()
                ),
            ));
        }

        return Ok(messages[idx as usize].msg_id.clone());
    }

    // Literal msg_id passthrough
    Ok(reference.to_string())
}

fn resolve_assistant_ref(
    messages: &[Message],
    reference: Option<&str>,
) -> Result<String, (ErrorCode, String)> {
    match reference {
        None | Some("last") | Some("latest") => messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            .map(|m| m.msg_id.clone())
            .ok_or_else(|| {
                (
                    ErrorCode::NotFound,
                    "No assistant messages in conversation".into(),
                )
            }),
        Some(reference) => {
            let msg_id = resolve_ref(messages, reference)?;
            let msg = messages
                .iter()
                .find(|m| m.msg_id == msg_id)
                .ok_or_else(|| (ErrorCode::NotFound, format!("Message not found: {msg_id}")))?;
            if msg.role != Role::Assistant {
                return Err((
                    ErrorCode::InvalidRequest,
                    "Alternate response selection only applies to assistant messages".into(),
                ));
            }
            Ok(msg_id)
        }
    }
}

/// Get a single message by index or reference.
///
/// Refs resolve against the merged (client-visible) message list.
pub fn get(
    engine: &ConversationEngine,
    _ctx: &CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let raw_ref = args.get("ref").and_then(|v| v.as_str()).ok_or_else(|| {
        (
            ErrorCode::InvalidRequest,
            "Missing required argument: ref".into(),
        )
    })?;

    let merged = shore_protocol::merge::merge_tool_loop_messages(engine.messages());
    let msg_id = resolve_ref(&merged, raw_ref)?;
    let msg = merged
        .iter()
        .find(|m| m.msg_id == msg_id)
        .ok_or_else(|| (ErrorCode::NotFound, format!("Message not found: {msg_id}")))?;

    debug!(msg_id = %msg_id, role = ?msg.role, "Message retrieved");
    serde_json::to_value(msg).map_err(|e| (ErrorCode::InternalError, e.to_string()))
}

/// Show conversation history, optionally limited to the last N messages.
///
/// Tool-loop messages are merged into single assistant turns. The `count`
/// parameter applies to the merged (logical) message list.
pub fn log(
    engine: &ConversationEngine,
    _ctx: &CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let merged = shore_protocol::merge::merge_tool_loop_messages(engine.messages());

    let count = args
        .get("count")
        .and_then(|v| v.as_u64())
        .map(|c| c as usize);

    // Embed image data so clients can display without filesystem access.
    let mut with_data: Vec<Message> = match count {
        Some(n) => merged.into_iter().rev().take(n).rev().collect(),
        None => merged,
    };
    for msg in &mut with_data {
        crate::handler::embed_image_data(&mut msg.images);
    }

    Ok(json!({ "messages": with_data }))
}

/// Edit a message by ID.
///
/// Refs resolve against the merged (client-visible) message list, then the
/// edit is applied to the raw store by msg_id.
pub fn edit(
    engine: &mut ConversationEngine,
    _ctx: &mut CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let raw_ref = args.get("ref").and_then(|v| v.as_str()).ok_or_else(|| {
        (
            ErrorCode::InvalidRequest,
            "Missing required argument: ref".into(),
        )
    })?;

    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            (
                ErrorCode::InvalidRequest,
                "Missing required argument: content".into(),
            )
        })?;

    let merged = shore_protocol::merge::merge_tool_loop_messages(engine.messages());
    let msg_id = resolve_ref(&merged, raw_ref)?;
    engine.edit_message(&msg_id, content).map_err(engine_err)?;

    debug!(msg_id = %msg_id, content_len = content.len(), "Message edited");
    Ok(json!({ "ref": msg_id, "edited": true }))
}

/// Delete one or more messages by ID.
///
/// `refs` can be a JSON array of strings or a single string.
pub fn delete(
    engine: &mut ConversationEngine,
    _ctx: &mut CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let raw_refs: Vec<&str> = if let Some(arr) = args.get("refs").and_then(|v| v.as_array()) {
        arr.iter()
            .map(|v| {
                v.as_str().ok_or_else(|| {
                    (
                        ErrorCode::InvalidRequest,
                        "refs must be an array of strings".into(),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?
    } else if let Some(s) = args.get("refs").and_then(|v| v.as_str()) {
        vec![s]
    } else {
        return Err((
            ErrorCode::InvalidRequest,
            "Missing required argument: refs".into(),
        ));
    };

    // Resolve all refs against merged list, so indices match what users see.
    let merged = shore_protocol::merge::merge_tool_loop_messages(engine.messages());
    let resolved: Vec<String> = raw_refs
        .iter()
        .map(|r| resolve_ref(&merged, r))
        .collect::<Result<Vec<_>, _>>()?;

    let mut deleted = Vec::new();
    for msg_id in &resolved {
        engine.delete_message(msg_id).map_err(engine_err)?;
        deleted.push(msg_id.clone());
    }

    debug!(count = deleted.len(), "Messages deleted");
    Ok(json!({ "deleted": deleted }))
}

/// List stored alternate responses for an assistant message.
///
/// Args:
/// - `ref` (optional): message ref, defaulting to the latest assistant message.
pub fn list_alternatives(
    engine: &ConversationEngine,
    _ctx: &CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let merged = shore_protocol::merge::merge_tool_loop_messages(engine.messages());
    let raw_ref = args.get("ref").and_then(|v| v.as_str());
    let msg_id = resolve_assistant_ref(&merged, raw_ref)?;
    let msg = merged
        .iter()
        .find(|m| m.msg_id == msg_id)
        .ok_or_else(|| (ErrorCode::NotFound, format!("Message not found: {msg_id}")))?;

    let alt_count = msg.alternatives.len() as u32;
    let current = msg.alt_index.unwrap_or(0).min(alt_count.saturating_sub(1));
    let alternatives: Vec<serde_json::Value> = msg
        .alternatives
        .iter()
        .enumerate()
        .map(|(index, alt)| {
            let mut images = alt.images.clone();
            crate::handler::embed_image_data(&mut images);
            json!({
                "index": index as u32,
                "position": index as u32 + 1,
                "active": index as u32 == current,
                "content": alt.content.clone(),
                "images": images,
                "timestamp": alt.timestamp.clone(),
            })
        })
        .collect();

    Ok(json!({
        "ref": msg_id,
        "alt_index": msg.alt_index,
        "position": msg.alt_index.map(|i| i + 1),
        "alt_count": alt_count,
        "alternatives": alternatives,
    }))
}

/// Select a stored alternate response on an assistant message.
///
/// Args:
/// - `ref` (optional): message ref, defaulting to the latest assistant message.
/// - `direction`: `prev`, `next`, `first`, or `last`.
/// - `position`: 1-based alternative position.
/// - `index`: 0-based alternative index for programmatic callers.
pub fn alt(
    engine: &mut ConversationEngine,
    _ctx: &mut CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let merged = shore_protocol::merge::merge_tool_loop_messages(engine.messages());
    let raw_ref = args.get("ref").and_then(|v| v.as_str());
    let msg_id = resolve_assistant_ref(&merged, raw_ref)?;
    let msg = merged
        .iter()
        .find(|m| m.msg_id == msg_id)
        .ok_or_else(|| (ErrorCode::NotFound, format!("Message not found: {msg_id}")))?;

    let alt_count = msg.alternatives.len() as u32;
    if alt_count == 0 {
        return Err((
            ErrorCode::InvalidRequest,
            format!("message {msg_id} has no alternate responses"),
        ));
    }
    let current = msg.alt_index.unwrap_or(0).min(alt_count.saturating_sub(1));
    let target = resolve_alt_target(args, current, alt_count)?;

    let selection = engine.select_alt(&msg_id, target).map_err(engine_err)?;
    debug!(
        msg_id = %selection.msg_id,
        alt_index = selection.alt_index,
        alt_count = selection.alt_count,
        "Alternate response selected"
    );
    Ok(json!({
        "ref": selection.msg_id,
        "alt_index": selection.alt_index,
        "position": selection.alt_index + 1,
        "alt_count": selection.alt_count,
        "content": selection.content,
    }))
}

fn resolve_alt_target(
    args: &serde_json::Value,
    current: u32,
    count: u32,
) -> Result<u32, (ErrorCode, String)> {
    if let Some(index) = args.get("index").and_then(|v| v.as_u64()) {
        let index = index as u32;
        if index >= count {
            return Err((
                ErrorCode::InvalidRequest,
                format!(
                    "alternate index {} out of range (message has {} alternate response(s))",
                    index + 1,
                    count
                ),
            ));
        }
        return Ok(index);
    }

    if let Some(position) = args.get("position").and_then(|v| v.as_u64()) {
        if position == 0 || position > count as u64 {
            return Err((
                ErrorCode::InvalidRequest,
                format!(
                    "alternate position {position} out of range (message has {count} alternate response(s))"
                ),
            ));
        }
        return Ok(position as u32 - 1);
    }

    match args
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("next")
    {
        "prev" | "previous" => Ok(current.saturating_sub(1)),
        "next" => Ok((current + 1).min(count.saturating_sub(1))),
        "first" => Ok(0),
        "last" => Ok(count.saturating_sub(1)),
        other => Err((
            ErrorCode::InvalidRequest,
            format!("unknown alt direction: {other}"),
        )),
    }
}

/// Inject a system-role instruction into the conversation.
///
/// This allows mid-conversation behavioral correction (e.g. "stop using
/// roleplay actions") without modifying the system prompt or polluting the
/// conversation with user-role meta-instructions.
pub fn inject_system(
    engine: &mut ConversationEngine,
    _ctx: &mut CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let text = args.get("text").and_then(|v| v.as_str()).ok_or_else(|| {
        (
            ErrorCode::InvalidRequest,
            "Missing required argument: text".into(),
        )
    })?;

    let msg = Message {
        msg_id: format!("m_{}", uuid::Uuid::new_v4()),
        role: Role::System,
        content: text.to_string(),
        images: vec![],
        content_blocks: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        alt_index: None,
        alt_count: None,
        alternatives: vec![],
        timestamp: chrono::Local::now().to_rfc3339(),
    };

    engine.append_message(msg).map_err(engine_err)?;
    debug!(text_len = text.len(), "System message injected");
    Ok(json!({ "injected": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{CommandContext, SessionTokens};
    use shore_protocol::server_msg::ServerMessage;
    use shore_protocol::types::{Message, Role};
    use tempfile::TempDir;
    use tokio::sync::broadcast;

    fn make_ctx(
        tmp: &TempDir,
    ) -> (
        ConversationEngine,
        CommandContext,
        broadcast::Receiver<ServerMessage>,
    ) {
        let (push_tx, push_rx) = broadcast::channel(16);
        let data_dir = tmp.path().to_path_buf();
        let engine =
            ConversationEngine::new("TestChar".to_string(), data_dir.clone(), push_tx.clone())
                .unwrap();

        let config = shore_config::LoadedConfig::new_for_test(
            shore_config::app::AppConfig::default(),
            shore_config::models::ModelCatalog::default(),
            shore_config::ShoreDirs {
                config: tmp.path().join("config"),
                data: data_dir.clone(),
                runtime: tmp.path().join("runtime"),
                cache: tmp.path().join("cache"),
            },
        );

        let (_tx, rx) = tokio::sync::watch::channel(());
        let autonomy = crate::autonomy::manager::AutonomyManager::new(
            Default::default(),
            Default::default(),
            data_dir.clone(),
            rx,
        );

        let ctx = CommandContext {
            config_path: config.dirs.config.join("config.toml"),
            config,
            push_tx,
            data_dir: data_dir.clone(),
            character_name: Some("TestChar".into()),
            active_model: None,
            session_tokens: std::sync::Arc::new(std::sync::Mutex::new(SessionTokens::default())),
            autonomy,
            llm_client: shore_ledger::LedgerClient::new(
                shore_llm::LlmClient::new(),
                &data_dir.join("ledger.db"),
            )
            .unwrap(),
            diagnostics: std::sync::Arc::new(std::sync::Mutex::new(
                shore_diagnostics::Diagnostics::default(),
            )),
            http: None,
        };
        (engine, ctx, push_rx)
    }

    fn make_msg(id: &str, role: Role, content: &str) -> Message {
        Message {
            msg_id: id.to_string(),
            role,
            content: content.to_string(),
            images: vec![],
            content_blocks: vec![],
            alt_index: None,
            alt_count: None,
            alternatives: vec![],
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn log_all_messages() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, ctx, _rx) = make_ctx(&tmp);
        engine
            .append_message(make_msg("m1", Role::User, "Hello"))
            .unwrap();
        engine
            .append_message(make_msg("m2", Role::Assistant, "Hi"))
            .unwrap();
        engine
            .append_message(make_msg("m3", Role::User, "How?"))
            .unwrap();

        let result = log(&engine, &ctx, &json!({})).unwrap();
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
    }

    #[test]
    fn log_with_count() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, ctx, _rx) = make_ctx(&tmp);
        engine
            .append_message(make_msg("m1", Role::User, "A"))
            .unwrap();
        engine
            .append_message(make_msg("m2", Role::Assistant, "B"))
            .unwrap();
        engine
            .append_message(make_msg("m3", Role::User, "C"))
            .unwrap();

        let result = log(&engine, &ctx, &json!({"count": 2})).unwrap();
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        // Should be the last 2 messages.
        assert_eq!(msgs[0]["msg_id"], "m2");
        assert_eq!(msgs[1]["msg_id"], "m3");
    }

    #[test]
    fn log_empty_conversation() {
        let tmp = TempDir::new().unwrap();
        let (engine, ctx, _rx) = make_ctx(&tmp);

        let result = log(&engine, &ctx, &json!({})).unwrap();
        let msgs = result["messages"].as_array().unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn edit_message() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);
        engine
            .append_message(make_msg("m1", Role::User, "Original"))
            .unwrap();

        let result = edit(
            &mut engine,
            &mut ctx,
            &json!({"ref": "m1", "content": "Edited"}),
        )
        .unwrap();
        assert_eq!(result["ref"], "m1");
        assert_eq!(result["edited"], true);
        assert_eq!(engine.messages()[0].content, "Edited");
    }

    #[test]
    fn edit_missing_ref() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);

        let result = edit(&mut engine, &mut ctx, &json!({"content": "x"}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn edit_missing_content() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);

        let result = edit(&mut engine, &mut ctx, &json!({"ref": "m1"}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn edit_nonexistent_message() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);

        let result = edit(
            &mut engine,
            &mut ctx,
            &json!({"ref": "nope", "content": "x"}),
        );
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
    }

    #[test]
    fn edit_triggers_history_push() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, mut rx) = make_ctx(&tmp);
        engine
            .append_message(make_msg("m1", Role::User, "Original"))
            .unwrap();
        while rx.try_recv().is_ok() {}

        edit(
            &mut engine,
            &mut ctx,
            &json!({"ref": "m1", "content": "Edited"}),
        )
        .unwrap();

        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMessage::History(_)));
    }

    #[test]
    fn delete_messages() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);
        engine
            .append_message(make_msg("m1", Role::User, "A"))
            .unwrap();
        engine
            .append_message(make_msg("m2", Role::Assistant, "B"))
            .unwrap();

        let result = delete(&mut engine, &mut ctx, &json!({"refs": ["m1"]})).unwrap();
        assert_eq!(result["deleted"].as_array().unwrap().len(), 1);
        assert_eq!(engine.messages().len(), 1);
        assert_eq!(engine.messages()[0].msg_id, "m2");
    }

    #[test]
    fn delete_single_string_ref() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);
        engine
            .append_message(make_msg("m1", Role::User, "A"))
            .unwrap();

        let result = delete(&mut engine, &mut ctx, &json!({"refs": "m1"})).unwrap();
        assert_eq!(result["deleted"].as_array().unwrap().len(), 1);
        assert!(engine.messages().is_empty());
    }

    #[test]
    fn delete_missing_refs() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);

        let result = delete(&mut engine, &mut ctx, &json!({}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn delete_nonexistent_message() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);

        let result = delete(&mut engine, &mut ctx, &json!({"refs": ["nope"]}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
    }

    #[test]
    fn delete_triggers_history_push() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, mut rx) = make_ctx(&tmp);
        engine
            .append_message(make_msg("m1", Role::User, "A"))
            .unwrap();
        while rx.try_recv().is_ok() {}

        delete(&mut engine, &mut ctx, &json!({"refs": ["m1"]})).unwrap();

        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMessage::History(_)));
    }

    #[test]
    fn alt_selects_previous_alternative() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);

        engine
            .append_message(make_msg("u1", Role::User, "Prompt"))
            .unwrap();
        let mut msg = make_msg("a2", Role::Assistant, "Second answer");
        msg.content_blocks = vec![ContentBlock::Text {
            text: "Second answer".into(),
        }];
        msg.alt_index = Some(1);
        msg.alt_count = Some(2);
        msg.alternatives = vec![
            shore_protocol::types::MessageAlternative {
                content: "First answer".into(),
                images: vec![],
                content_blocks: vec![ContentBlock::Text {
                    text: "First answer".into(),
                }],
                timestamp: "2026-01-01T00:00:00Z".into(),
            },
            shore_protocol::types::MessageAlternative {
                content: "Second answer".into(),
                images: vec![],
                content_blocks: vec![ContentBlock::Text {
                    text: "Second answer".into(),
                }],
                timestamp: "2026-01-01T00:00:01Z".into(),
            },
        ];
        engine.append_message(msg).unwrap();

        let result = alt(&mut engine, &mut ctx, &json!({"direction": "prev"})).unwrap();
        assert_eq!(result["position"], 1);
        assert_eq!(result["alt_count"], 2);
        assert_eq!(engine.messages()[1].content, "First answer");
        assert_eq!(engine.messages()[1].alt_index, Some(0));
    }

    #[test]
    fn list_alternatives_returns_active_marker() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, ctx, _rx) = make_ctx(&tmp);

        engine
            .append_message(make_msg("u1", Role::User, "Prompt"))
            .unwrap();
        let mut msg = make_msg("a2", Role::Assistant, "Second answer");
        msg.alt_index = Some(1);
        msg.alt_count = Some(2);
        msg.alternatives = vec![
            shore_protocol::types::MessageAlternative {
                content: "First answer".into(),
                images: vec![],
                content_blocks: vec![ContentBlock::Text {
                    text: "First answer".into(),
                }],
                timestamp: "2026-01-01T00:00:00Z".into(),
            },
            shore_protocol::types::MessageAlternative {
                content: "Second answer".into(),
                images: vec![],
                content_blocks: vec![ContentBlock::Text {
                    text: "Second answer".into(),
                }],
                timestamp: "2026-01-01T00:00:01Z".into(),
            },
        ];
        engine.append_message(msg).unwrap();

        let result = list_alternatives(&engine, &ctx, &json!({})).unwrap();
        assert_eq!(result["ref"], "a2");
        assert_eq!(result["alt_count"], 2);
        assert_eq!(result["alternatives"][0]["position"], 1);
        assert_eq!(result["alternatives"][0]["active"], false);
        assert_eq!(result["alternatives"][1]["content"], "Second answer");
        assert_eq!(result["alternatives"][1]["active"], true);
    }

    // ── resolve_ref tests ───────────────────────────────────────────

    #[test]
    fn resolve_ref_last() {
        let messages = vec![
            make_msg("m1", Role::User, "A"),
            make_msg("m2", Role::Assistant, "B"),
        ];
        assert_eq!(resolve_ref(&messages, "last").unwrap(), "m2");
        assert_eq!(resolve_ref(&messages, "latest").unwrap(), "m2");
    }

    #[test]
    fn resolve_ref_negative_index() {
        let messages = vec![
            make_msg("m1", Role::User, "A"),
            make_msg("m2", Role::Assistant, "B"),
            make_msg("m3", Role::User, "C"),
        ];
        assert_eq!(resolve_ref(&messages, "-1").unwrap(), "m3");
        assert_eq!(resolve_ref(&messages, "-2").unwrap(), "m2");
        assert_eq!(resolve_ref(&messages, "-3").unwrap(), "m1");
    }

    #[test]
    fn resolve_ref_positive_index() {
        let messages = vec![
            make_msg("m1", Role::User, "A"),
            make_msg("m2", Role::Assistant, "B"),
            make_msg("m3", Role::User, "C"),
        ];
        assert_eq!(resolve_ref(&messages, "1").unwrap(), "m1");
        assert_eq!(resolve_ref(&messages, "2").unwrap(), "m2");
        assert_eq!(resolve_ref(&messages, "3").unwrap(), "m3");
    }

    #[test]
    fn resolve_ref_literal_passthrough() {
        let messages = vec![make_msg("m_abc123", Role::User, "A")];
        assert_eq!(resolve_ref(&messages, "m_abc123").unwrap(), "m_abc123");
    }

    #[test]
    fn resolve_ref_zero_is_error() {
        let messages = vec![make_msg("m1", Role::User, "A")];
        let (code, _msg) = resolve_ref(&messages, "0").unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn resolve_ref_out_of_range() {
        let messages = vec![make_msg("m1", Role::User, "A")];
        let (code, _msg) = resolve_ref(&messages, "99").unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
        let (code, _msg) = resolve_ref(&messages, "-99").unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
    }

    #[test]
    fn resolve_ref_empty_conversation() {
        let messages: Vec<Message> = vec![];
        let (code, _msg) = resolve_ref(&messages, "last").unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
        let (code, _msg) = resolve_ref(&messages, "-1").unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
    }

    #[test]
    fn edit_by_relative_ref_last() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);
        engine
            .append_message(make_msg("m1", Role::User, "First"))
            .unwrap();
        engine
            .append_message(make_msg("m2", Role::Assistant, "Second"))
            .unwrap();

        let result = edit(
            &mut engine,
            &mut ctx,
            &json!({"ref": "last", "content": "Edited"}),
        )
        .unwrap();
        assert_eq!(result["ref"], "m2");
        assert_eq!(engine.messages()[1].content, "Edited");
    }

    #[test]
    fn edit_by_negative_index() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);
        engine
            .append_message(make_msg("m1", Role::User, "First"))
            .unwrap();
        engine
            .append_message(make_msg("m2", Role::Assistant, "Second"))
            .unwrap();

        let result = edit(
            &mut engine,
            &mut ctx,
            &json!({"ref": "-1", "content": "Edited"}),
        )
        .unwrap();
        assert_eq!(result["ref"], "m2");
        assert_eq!(engine.messages()[1].content, "Edited");
    }

    #[test]
    fn edit_by_positive_index() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);
        engine
            .append_message(make_msg("m1", Role::User, "First"))
            .unwrap();
        engine
            .append_message(make_msg("m2", Role::Assistant, "Second"))
            .unwrap();

        let result = edit(
            &mut engine,
            &mut ctx,
            &json!({"ref": "1", "content": "Edited"}),
        )
        .unwrap();
        assert_eq!(result["ref"], "m1");
        assert_eq!(engine.messages()[0].content, "Edited");
    }

    #[test]
    fn delete_by_relative_ref() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);
        engine
            .append_message(make_msg("m1", Role::User, "A"))
            .unwrap();
        engine
            .append_message(make_msg("m2", Role::Assistant, "B"))
            .unwrap();

        let result = delete(&mut engine, &mut ctx, &json!({"refs": "last"})).unwrap();
        assert_eq!(result["deleted"].as_array().unwrap()[0], "m2");
        assert_eq!(engine.messages().len(), 1);
        assert_eq!(engine.messages()[0].msg_id, "m1");
    }

    #[test]
    fn test_inject_system_appends_message() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);

        let result = inject_system(
            &mut engine,
            &mut ctx,
            &json!({"text": "Stop using actions"}),
        )
        .unwrap();
        assert_eq!(result["injected"], true);

        assert_eq!(engine.messages().len(), 1);
        let msg = &engine.messages()[0];
        assert_eq!(msg.role, Role::System);
        assert_eq!(msg.content, "Stop using actions");
        assert!(msg.msg_id.starts_with("m_"));
    }

    #[test]
    fn test_inject_system_missing_text() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);

        let result = inject_system(&mut engine, &mut ctx, &json!({}));
        assert!(result.is_err());
        let (code, msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
        assert!(msg.contains("text"));
    }
}
