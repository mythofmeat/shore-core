use serde_json::json;
use shore_protocol::error::ErrorCode;
use shore_protocol::types::Message;

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

/// Show conversation history, optionally limited to the last N messages.
pub fn log(
    engine: &ConversationEngine,
    _ctx: &CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let messages = engine.messages();

    let count = args.get("count").and_then(|v| v.as_u64()).map(|c| c as usize);

    let result: Vec<_> = match count {
        Some(n) => messages.iter().rev().take(n).rev().collect(),
        None => messages.iter().collect(),
    };

    Ok(json!({ "messages": result }))
}

/// Edit a message by ID.
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

    let msg_id = resolve_ref(engine.messages(), raw_ref)?;
    engine.edit_message(&msg_id, content).map_err(engine_err)?;

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

    // Resolve all refs up front before mutating, so indices stay stable.
    let resolved: Vec<String> = raw_refs
        .iter()
        .map(|r| resolve_ref(engine.messages(), r))
        .collect::<Result<Vec<_>, _>>()?;

    let mut deleted = Vec::new();
    for msg_id in &resolved {
        engine.delete_message(msg_id).map_err(engine_err)?;
        deleted.push(msg_id.clone());
    }

    Ok(json!({ "deleted": deleted }))
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

        let config = crate::config::LoadedConfig::new_for_test(
            crate::config::app::AppConfig::default(),
            crate::config::models::ModelCatalog::default(),
            crate::config::ShoreDirs {
                config: tmp.path().join("config"),
                data: data_dir.clone(),
                runtime: tmp.path().join("runtime"),
            },
        );

        let (_tx, rx) = tokio::sync::watch::channel(());
        let (autonomy, _compaction_rx) = crate::autonomy::manager::AutonomyManager::new(Default::default(), data_dir.clone(), rx);

        let ctx = CommandContext {
            config,
            push_tx,
            data_dir: data_dir.clone(),
            active_model: None,
            session_tokens: SessionTokens::default(),
            autonomy,
            llm_client: crate::llm_client::LlmClient::new(data_dir.join("dummy.sock")),
        };
        (engine, ctx, push_rx)
    }

    fn make_msg(id: &str, role: Role, content: &str) -> Message {
        Message {
            msg_id: id.to_string(),
            role,
            content: content.to_string(),
            images: vec![],
            alt_index: None,
            alt_count: None,
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

        let result =
            edit(&mut engine, &mut ctx, &json!({"ref": "m1", "content": "Edited"})).unwrap();
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

        let result = edit(&mut engine, &mut ctx, &json!({"ref": "nope", "content": "x"}));
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

        edit(&mut engine, &mut ctx, &json!({"ref": "m1", "content": "Edited"})).unwrap();

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

        let result =
            edit(&mut engine, &mut ctx, &json!({"ref": "last", "content": "Edited"})).unwrap();
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

        let result =
            edit(&mut engine, &mut ctx, &json!({"ref": "-1", "content": "Edited"})).unwrap();
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

        let result =
            edit(&mut engine, &mut ctx, &json!({"ref": "1", "content": "Edited"})).unwrap();
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
}
