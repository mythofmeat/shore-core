use serde_json::json;
use shore_protocol::error::ErrorCode;

use super::{engine_err, CommandContext, CommandResult};
use crate::engine::ConversationEngine;

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
    let msg_ref = args.get("ref").and_then(|v| v.as_str()).ok_or_else(|| {
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

    engine.edit_message(msg_ref, content).map_err(engine_err)?;

    Ok(json!({ "ref": msg_ref, "edited": true }))
}

/// Delete one or more messages by ID.
///
/// `refs` can be a JSON array of strings or a single string.
pub fn delete(
    engine: &mut ConversationEngine,
    _ctx: &mut CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let refs: Vec<&str> = if let Some(arr) = args.get("refs").and_then(|v| v.as_array()) {
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

    let mut deleted = Vec::new();
    for msg_id in refs {
        engine.delete_message(msg_id).map_err(engine_err)?;
        deleted.push(msg_id.to_string());
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

        let ctx = CommandContext {
            config,
            push_tx,
            data_dir,
            active_model: None,
            session_tokens: SessionTokens::default(),
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
}
