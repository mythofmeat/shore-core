use serde_json::json;
use shore_protocol::error::ErrorCode;
use shore_protocol::types::CharacterInfo;
use tracing::info;

use super::{engine_err, CommandContext, CommandResult};
use crate::engine::ConversationEngine;

/// List available characters by scanning the config characters directory.
pub fn list_characters(ctx: &CommandContext) -> CommandResult {
    let characters_dir = ctx.config.dirs.config.join("characters");
    let mut characters = vec![CharacterInfo {
        name: ctx.engine.character_name().to_string(),
    }];

    if let Ok(entries) = std::fs::read_dir(&characters_dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                let name = entry.file_name().to_string_lossy().to_string();
                if name != ctx.engine.character_name() {
                    characters.push(CharacterInfo { name });
                }
            }
        }
    }

    Ok(json!({ "characters": characters }))
}

/// Switch to a different character. Creates a new engine for the target character.
pub fn switch_character(ctx: &mut CommandContext, args: &serde_json::Value) -> CommandResult {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            (
                ErrorCode::InvalidRequest,
                "Missing required argument: name".into(),
            )
        })?;

    if name == ctx.engine.character_name() {
        return Ok(json!({ "character": name, "changed": false }));
    }

    // Verify character directory exists.
    let char_dir = ctx.config.dirs.config.join("characters").join(name);
    if !char_dir.exists() {
        return Err((
            ErrorCode::NotFound,
            format!("Character not found: {name}"),
        ));
    }

    info!(character = name, "Switching character");

    let engine = ConversationEngine::new(name.to_string(), ctx.data_dir.clone(), ctx.push_tx.clone())
        .map_err(engine_err)?;

    ctx.engine = engine;
    // Push history so clients see the new character's state.
    ctx.engine.broadcast_history();

    Ok(json!({ "character": name, "changed": true }))
}

/// List conversations for the active character.
pub fn list_chats(ctx: &CommandContext) -> CommandResult {
    let conversations = ctx.engine.list_conversations();
    let active_id = ctx.engine.active_conversation_id();
    Ok(json!({
        "conversations": conversations,
        "active_id": active_id,
    }))
}

/// Switch to an existing conversation by ID.
pub fn switch_chat(ctx: &mut CommandContext, args: &serde_json::Value) -> CommandResult {
    let id = args.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
        (
            ErrorCode::InvalidRequest,
            "Missing required argument: id".into(),
        )
    })?;

    ctx.engine.switch_conversation(id).map_err(engine_err)?;

    Ok(json!({ "id": id }))
}

/// Create a new conversation, optionally with a title.
pub fn new_chat(ctx: &mut CommandContext, args: &serde_json::Value) -> CommandResult {
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("New conversation");

    let id = ctx.engine.new_conversation(title).map_err(engine_err)?;

    Ok(json!({ "id": id, "title": title }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use shore_protocol::server_msg::ServerMessage;
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
            session_tokens: Default::default(),
        };
        (ctx, push_rx)
    }

    #[test]
    fn list_characters_includes_active() {
        let tmp = TempDir::new().unwrap();
        let (ctx, _rx) = make_ctx(&tmp);

        let result = list_characters(&ctx).unwrap();
        let chars = result["characters"].as_array().unwrap();
        assert_eq!(chars.len(), 1);
        assert_eq!(chars[0]["name"], "TestChar");
    }

    #[test]
    fn list_characters_scans_config_dir() {
        let tmp = TempDir::new().unwrap();
        let (ctx, _rx) = make_ctx(&tmp);

        // Create character directories.
        let chars_dir = tmp.path().join("config").join("characters");
        std::fs::create_dir_all(chars_dir.join("Alice")).unwrap();
        std::fs::create_dir_all(chars_dir.join("Bob")).unwrap();
        // Active character directory (should be deduped).
        std::fs::create_dir_all(chars_dir.join("TestChar")).unwrap();

        let result = list_characters(&ctx).unwrap();
        let chars = result["characters"].as_array().unwrap();
        // TestChar + Alice + Bob (TestChar not duplicated).
        assert_eq!(chars.len(), 3);
        let names: Vec<&str> = chars.iter().map(|c| c["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"TestChar"));
        assert!(names.contains(&"Alice"));
        assert!(names.contains(&"Bob"));
    }

    #[test]
    fn switch_character_same_is_noop() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);

        let result = switch_character(&mut ctx, &json!({"name": "TestChar"})).unwrap();
        assert_eq!(result["changed"], false);
    }

    #[test]
    fn switch_character_not_found() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);

        let result = switch_character(&mut ctx, &json!({"name": "Nonexistent"}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
    }

    #[test]
    fn switch_character_missing_name() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);

        let result = switch_character(&mut ctx, &json!({}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn switch_character_creates_new_engine() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, mut rx) = make_ctx(&tmp);

        // Create character directory.
        let chars_dir = tmp.path().join("config").join("characters");
        std::fs::create_dir_all(chars_dir.join("Alice")).unwrap();

        let result = switch_character(&mut ctx, &json!({"name": "Alice"})).unwrap();
        assert_eq!(result["changed"], true);
        assert_eq!(result["character"], "Alice");
        assert_eq!(ctx.engine.character_name(), "Alice");

        // Should have broadcast history.
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMessage::History(_)));
    }

    #[test]
    fn list_chats_empty() {
        let tmp = TempDir::new().unwrap();
        let (ctx, _rx) = make_ctx(&tmp);

        let result = list_chats(&ctx).unwrap();
        assert!(result["conversations"].as_array().unwrap().is_empty());
        assert!(result["active_id"].is_null());
    }

    #[test]
    fn list_chats_with_conversations() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);

        ctx.engine.new_conversation("Chat 1").unwrap();
        ctx.engine.new_conversation("Chat 2").unwrap();

        let result = list_chats(&ctx).unwrap();
        let convs = result["conversations"].as_array().unwrap();
        assert_eq!(convs.len(), 2);
        assert!(result["active_id"].is_string());
    }

    #[test]
    fn switch_chat_valid() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);

        let id1 = ctx.engine.new_conversation("Chat 1").unwrap();
        let _id2 = ctx.engine.new_conversation("Chat 2").unwrap();

        let result = switch_chat(&mut ctx, &json!({"id": id1})).unwrap();
        assert_eq!(result["id"], id1);
        assert_eq!(ctx.engine.active_conversation_id(), Some(id1.as_str()));
    }

    #[test]
    fn switch_chat_not_found() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);

        let result = switch_chat(&mut ctx, &json!({"id": "nonexistent"}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
    }

    #[test]
    fn switch_chat_missing_id() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);

        let result = switch_chat(&mut ctx, &json!({}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn new_chat_with_title() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);

        let result = new_chat(&mut ctx, &json!({"title": "My Chat"})).unwrap();
        assert_eq!(result["title"], "My Chat");
        assert!(result["id"].is_string());
        assert!(ctx.engine.active_conversation_id().is_some());
    }

    #[test]
    fn new_chat_default_title() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, _rx) = make_ctx(&tmp);

        let result = new_chat(&mut ctx, &json!({})).unwrap();
        assert_eq!(result["title"], "New conversation");
    }

    #[test]
    fn new_chat_triggers_history_push() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, mut rx) = make_ctx(&tmp);

        new_chat(&mut ctx, &json!({"title": "Test"})).unwrap();

        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMessage::History(_)));
    }

    #[test]
    fn switch_chat_triggers_history_push() {
        let tmp = TempDir::new().unwrap();
        let (mut ctx, mut rx) = make_ctx(&tmp);

        let id1 = ctx.engine.new_conversation("Chat 1").unwrap();
        let _id2 = ctx.engine.new_conversation("Chat 2").unwrap();
        // Drain broadcast events from conversation creation.
        while rx.try_recv().is_ok() {}

        switch_chat(&mut ctx, &json!({"id": id1})).unwrap();

        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMessage::History(_)));
    }
}
