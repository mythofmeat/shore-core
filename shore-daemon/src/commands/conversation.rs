use serde_json::json;
use shore_protocol::error::ErrorCode;
use shore_protocol::types::Role;

use super::{engine_err, CommandContext, CommandResult};

/// Navigate response candidates (swipe).
///
/// `target`: `"prev"`, `"next"` (default), or a numeric index.
/// Operates on the last assistant message in the current conversation.
/// At end of stack, `next` adds a new candidate and signals regen.
pub fn swipe(ctx: &mut CommandContext, args: &serde_json::Value) -> CommandResult {
    let target = args
        .get("target")
        .and_then(|v| v.as_str())
        .unwrap_or("next");

    let messages = ctx.engine.messages().map_err(engine_err)?;

    let last_assistant = messages
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant)
        .ok_or_else(|| {
            (
                ErrorCode::InvalidRequest,
                "No assistant message to swipe".into(),
            )
        })?;

    let msg_id = last_assistant.msg_id.clone();
    let current_index = last_assistant.alt_index.unwrap_or(0);
    let current_count = last_assistant.alt_count.unwrap_or(1);

    match target {
        "prev" => {
            if current_index == 0 {
                return Err((
                    ErrorCode::InvalidRequest,
                    "Already at first candidate".into(),
                ));
            }
            let new_index = current_index - 1;
            ctx.engine
                .set_swipe(&msg_id, new_index, current_count)
                .map_err(engine_err)?;
            Ok(json!({
                "alt_index": new_index,
                "alt_count": current_count,
                "regen": false,
            }))
        }
        "next" => {
            if current_index + 1 < current_count {
                let new_index = current_index + 1;
                ctx.engine
                    .set_swipe(&msg_id, new_index, current_count)
                    .map_err(engine_err)?;
                Ok(json!({
                    "alt_index": new_index,
                    "alt_count": current_count,
                    "regen": false,
                }))
            } else {
                // At end of stack — add candidate and signal regen.
                let new_count = ctx
                    .engine
                    .add_swipe_candidate(&msg_id)
                    .map_err(engine_err)?;
                Ok(json!({
                    "alt_index": new_count - 1,
                    "alt_count": new_count,
                    "regen": true,
                }))
            }
        }
        n => {
            let index: u32 = n.parse().map_err(|_| {
                (
                    ErrorCode::InvalidRequest,
                    format!("Invalid swipe target: {n}"),
                )
            })?;
            if index >= current_count {
                return Err((
                    ErrorCode::InvalidRequest,
                    format!("Swipe index {index} out of range (0..{current_count})"),
                ));
            }
            ctx.engine
                .set_swipe(&msg_id, index, current_count)
                .map_err(engine_err)?;
            Ok(json!({
                "alt_index": index,
                "alt_count": current_count,
                "regen": false,
            }))
        }
    }
}

/// Show conversation history, optionally limited to the last N messages.
pub fn log(ctx: &CommandContext, args: &serde_json::Value) -> CommandResult {
    let messages = ctx.engine.messages().map_err(engine_err)?;

    let count = args.get("count").and_then(|v| v.as_u64()).map(|c| c as usize);

    let result: Vec<_> = match count {
        Some(n) => messages.iter().rev().take(n).rev().collect(),
        None => messages.iter().collect(),
    };

    Ok(json!({ "messages": result }))
}

/// Edit a message by ID.
pub fn edit(ctx: &mut CommandContext, args: &serde_json::Value) -> CommandResult {
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

    ctx.engine.edit_message(msg_ref, content).map_err(engine_err)?;

    Ok(json!({ "ref": msg_ref, "edited": true }))
}

/// Delete one or more messages by ID.
///
/// `refs` can be a JSON array of strings or a single string.
pub fn delete(ctx: &mut CommandContext, args: &serde_json::Value) -> CommandResult {
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
        ctx.engine.delete_message(msg_id).map_err(engine_err)?;
        deleted.push(msg_id.to_string());
    }

    Ok(json!({ "deleted": deleted }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CommandContext;
    use crate::engine::ConversationEngine;
    use shore_protocol::server_msg::ServerMessage;
    use shore_protocol::types::Message;
    use tempfile::TempDir;
    use tokio::sync::broadcast;

    fn make_ctx(tmp: &TempDir) -> (CommandContext, broadcast::Receiver<ServerMessage>) {
        let (push_tx, push_rx) = broadcast::channel(16);
        let data_dir = tmp.path().to_path_buf();
        let engine =
            ConversationEngine::new("TestChar".to_string(), data_dir.clone(), push_tx.clone())
                .unwrap();

        let config = crate::config::LoadedConfig {
            app: crate::config::app::AppConfig::default(),
            models: crate::config::models::ModelsConfig::default(),
            dirs: crate::config::ShoreDirs {
                config: tmp.path().join("config"),
                data: data_dir.clone(),
                runtime: tmp.path().join("runtime"),
            },
            character_definition: None,
            user_definition: None,
        };

        let ctx = CommandContext {
            engine,
            config,
            push_tx,
            data_dir,
            active_model: None,
            autonomy_paused: false,
        };
        (ctx, push_rx)
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

    /// Set up a context with a conversation and messages for swipe tests.
    fn setup_swipe_ctx(
        tmp: &TempDir,
    ) -> (CommandContext, broadcast::Receiver<ServerMessage>) {
        let (mut ctx, rx) = make_ctx(tmp);
        ctx.engine.new_conversation("Test").unwrap();
        ctx.engine
            .append_message(make_msg("u1", Role::User, "Hello"))
            .unwrap();
        ctx.engine
            .append_message(make_msg("a1", Role::Assistant, "Hi there"))
            .unwrap();
        // Initialize swipe state: 1 candidate at index 0.
        ctx.engine.set_swipe("a1", 0, 2).unwrap();
        (ctx, rx)
    }

    #[test]
    fn swipe_next_within_stack() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = setup_swipe_ctx(&tmp);

        let result = swipe(&mut ctx, &json!({"target": "next"})).unwrap();
        assert_eq!(result["alt_index"], 1);
        assert_eq!(result["alt_count"], 2);
        assert_eq!(result["regen"], false);
    }

    #[test]
    fn swipe_next_at_end_triggers_regen() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = setup_swipe_ctx(&tmp);

        // Move to index 1 first.
        ctx.engine.set_swipe("a1", 1, 2).unwrap();

        let result = swipe(&mut ctx, &json!({"target": "next"})).unwrap();
        assert_eq!(result["alt_index"], 2);
        assert_eq!(result["alt_count"], 3);
        assert_eq!(result["regen"], true);
    }

    #[test]
    fn swipe_prev() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = setup_swipe_ctx(&tmp);

        // Move to index 1 first.
        ctx.engine.set_swipe("a1", 1, 2).unwrap();

        let result = swipe(&mut ctx, &json!({"target": "prev"})).unwrap();
        assert_eq!(result["alt_index"], 0);
        assert_eq!(result["regen"], false);
    }

    #[test]
    fn swipe_prev_at_start_errors() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = setup_swipe_ctx(&tmp);

        let result = swipe(&mut ctx, &json!({"target": "prev"}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn swipe_numeric_index() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = setup_swipe_ctx(&tmp);

        let result = swipe(&mut ctx, &json!({"target": "1"})).unwrap();
        assert_eq!(result["alt_index"], 1);
        assert_eq!(result["regen"], false);
    }

    #[test]
    fn swipe_numeric_out_of_range() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = setup_swipe_ctx(&tmp);

        let result = swipe(&mut ctx, &json!({"target": "5"}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn swipe_invalid_target() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = setup_swipe_ctx(&tmp);

        let result = swipe(&mut ctx, &json!({"target": "sideways"}));
        assert!(result.is_err());
    }

    #[test]
    fn swipe_no_assistant_message() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);
        ctx.engine.new_conversation("Test").unwrap();
        ctx.engine
            .append_message(make_msg("u1", Role::User, "Hello"))
            .unwrap();

        let result = swipe(&mut ctx, &json!({}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn swipe_default_is_next() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = setup_swipe_ctx(&tmp);

        // Default target is "next", should move from 0 to 1.
        let result = swipe(&mut ctx, &json!({})).unwrap();
        assert_eq!(result["alt_index"], 1);
    }

    #[test]
    fn swipe_triggers_history_push() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, mut rx) = setup_swipe_ctx(&tmp);
        while rx.try_recv().is_ok() {}

        swipe(&mut ctx, &json!({"target": "next"})).unwrap();

        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMessage::History(_)));
    }

    #[test]
    fn log_all_messages() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);
        ctx.engine.new_conversation("Test").unwrap();
        ctx.engine
            .append_message(make_msg("m1", Role::User, "Hello"))
            .unwrap();
        ctx.engine
            .append_message(make_msg("m2", Role::Assistant, "Hi"))
            .unwrap();
        ctx.engine
            .append_message(make_msg("m3", Role::User, "How?"))
            .unwrap();

        let result = log(&ctx, &json!({})).unwrap();
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
    }

    #[test]
    fn log_with_count() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);
        ctx.engine.new_conversation("Test").unwrap();
        ctx.engine
            .append_message(make_msg("m1", Role::User, "A"))
            .unwrap();
        ctx.engine
            .append_message(make_msg("m2", Role::Assistant, "B"))
            .unwrap();
        ctx.engine
            .append_message(make_msg("m3", Role::User, "C"))
            .unwrap();

        let result = log(&ctx, &json!({"count": 2})).unwrap();
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        // Should be the last 2 messages.
        assert_eq!(msgs[0]["msg_id"], "m2");
        assert_eq!(msgs[1]["msg_id"], "m3");
    }

    #[test]
    fn log_no_active_conversation() {
        let tmp = TempDir::new().unwrap();
        let (ctx, _rx) = make_ctx(&tmp);

        let result = log(&ctx, &json!({}));
        assert!(result.is_err());
    }

    #[test]
    fn edit_message() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);
        ctx.engine.new_conversation("Test").unwrap();
        ctx.engine
            .append_message(make_msg("m1", Role::User, "Original"))
            .unwrap();

        let result = edit(&mut ctx, &json!({"ref": "m1", "content": "Edited"})).unwrap();
        assert_eq!(result["ref"], "m1");
        assert_eq!(result["edited"], true);
        assert_eq!(ctx.engine.messages().unwrap()[0].content, "Edited");
    }

    #[test]
    fn edit_missing_ref() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);

        let result = edit(&mut ctx, &json!({"content": "x"}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn edit_missing_content() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);

        let result = edit(&mut ctx, &json!({"ref": "m1"}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn edit_nonexistent_message() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);
        ctx.engine.new_conversation("Test").unwrap();

        let result = edit(&mut ctx, &json!({"ref": "nope", "content": "x"}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
    }

    #[test]
    fn edit_triggers_history_push() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, mut rx) = make_ctx(&tmp);
        ctx.engine.new_conversation("Test").unwrap();
        ctx.engine
            .append_message(make_msg("m1", Role::User, "Original"))
            .unwrap();
        while rx.try_recv().is_ok() {}

        edit(&mut ctx, &json!({"ref": "m1", "content": "Edited"})).unwrap();

        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMessage::History(_)));
    }

    #[test]
    fn delete_messages() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);
        ctx.engine.new_conversation("Test").unwrap();
        ctx.engine
            .append_message(make_msg("m1", Role::User, "A"))
            .unwrap();
        ctx.engine
            .append_message(make_msg("m2", Role::Assistant, "B"))
            .unwrap();

        let result = delete(&mut ctx, &json!({"refs": ["m1"]})).unwrap();
        assert_eq!(result["deleted"].as_array().unwrap().len(), 1);
        assert_eq!(ctx.engine.messages().unwrap().len(), 1);
        assert_eq!(ctx.engine.messages().unwrap()[0].msg_id, "m2");
    }

    #[test]
    fn delete_single_string_ref() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);
        ctx.engine.new_conversation("Test").unwrap();
        ctx.engine
            .append_message(make_msg("m1", Role::User, "A"))
            .unwrap();

        let result = delete(&mut ctx, &json!({"refs": "m1"})).unwrap();
        assert_eq!(result["deleted"].as_array().unwrap().len(), 1);
        assert!(ctx.engine.messages().unwrap().is_empty());
    }

    #[test]
    fn delete_missing_refs() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);

        let result = delete(&mut ctx, &json!({}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn delete_nonexistent_message() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);
        ctx.engine.new_conversation("Test").unwrap();

        let result = delete(&mut ctx, &json!({"refs": ["nope"]}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
    }

    #[test]
    fn delete_triggers_history_push() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, mut rx) = make_ctx(&tmp);
        ctx.engine.new_conversation("Test").unwrap();
        ctx.engine
            .append_message(make_msg("m1", Role::User, "A"))
            .unwrap();
        while rx.try_recv().is_ok() {}

        delete(&mut ctx, &json!({"refs": ["m1"]})).unwrap();

        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMessage::History(_)));
    }
}
