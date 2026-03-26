use serde_json::json;
use shore_protocol::error::ErrorCode;
use shore_protocol::types::CharacterInfo;
use tracing::info;

use super::{engine_err, CommandContext, CommandResult};
use crate::engine::ConversationEngine;

/// List available characters by scanning the config characters directory.
pub fn list_characters(engine: &ConversationEngine, ctx: &CommandContext) -> CommandResult {
    let characters_dir = ctx.config.dirs.config.join("characters");
    let mut characters = vec![CharacterInfo {
        name: engine.character_name().to_string(),
    }];

    if let Ok(entries) = std::fs::read_dir(&characters_dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                let name = entry.file_name().to_string_lossy().to_string();
                if name != engine.character_name() {
                    characters.push(CharacterInfo { name });
                }
            }
        }
    }

    Ok(json!({ "characters": characters }))
}

/// Switch to a different character. In multi-character mode, this is handled
/// by the registry — this command validates the character exists.
pub fn switch_character(
    engine: &ConversationEngine,
    ctx: &mut CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            (
                ErrorCode::InvalidRequest,
                "Missing required argument: name".into(),
            )
        })?;

    if name == engine.character_name() {
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

    info!(character = name, "Character switch requested");

    // In multi-character mode, the client should reconnect with the new character.
    // For now, acknowledge the request.
    Ok(json!({ "character": name, "changed": true, "reconnect_required": true }))
}

/// Clear the active conversation and start fresh.
pub fn reset(engine: &mut ConversationEngine) -> CommandResult {
    engine.reset().map_err(engine_err)?;
    Ok(json!({ "reset": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::SessionTokens;
    use shore_protocol::server_msg::ServerMessage;
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
            autonomy_paused: false,
            session_tokens: SessionTokens::default(),
        };
        (engine, ctx, push_rx)
    }

    #[test]
    fn list_characters_includes_active() {
        let tmp = TempDir::new().unwrap();
        let (engine, ctx, _rx) = make_ctx(&tmp);

        let result = list_characters(&engine, &ctx).unwrap();
        let chars = result["characters"].as_array().unwrap();
        assert_eq!(chars.len(), 1);
        assert_eq!(chars[0]["name"], "TestChar");
    }

    #[test]
    fn list_characters_scans_config_dir() {
        let tmp = TempDir::new().unwrap();
        let (engine, ctx, _rx) = make_ctx(&tmp);

        // Create character directories.
        let chars_dir = tmp.path().join("config").join("characters");
        std::fs::create_dir_all(chars_dir.join("Alice")).unwrap();
        std::fs::create_dir_all(chars_dir.join("Bob")).unwrap();
        // Active character directory (should be deduped).
        std::fs::create_dir_all(chars_dir.join("TestChar")).unwrap();

        let result = list_characters(&engine, &ctx).unwrap();
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
        let (engine, mut ctx, _rx) = make_ctx(&tmp);

        let result = switch_character(&engine, &mut ctx, &json!({"name": "TestChar"})).unwrap();
        assert_eq!(result["changed"], false);
    }

    #[test]
    fn switch_character_not_found() {
        let tmp = TempDir::new().unwrap();
        let (engine, mut ctx, _rx) = make_ctx(&tmp);

        let result = switch_character(&engine, &mut ctx, &json!({"name": "Nonexistent"}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
    }

    #[test]
    fn switch_character_missing_name() {
        let tmp = TempDir::new().unwrap();
        let (engine, mut ctx, _rx) = make_ctx(&tmp);

        let result = switch_character(&engine, &mut ctx, &json!({}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn reset_clears_conversation() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, _ctx, _rx) = make_ctx(&tmp);

        use shore_protocol::types::{Message, Role};
        engine
            .append_message(Message {
                msg_id: "m1".into(),
                role: Role::User,
                content: "Hello".into(),
                images: vec![],
                alt_index: None,
                alt_count: None,
                timestamp: "2026-01-01T00:00:00Z".into(),
            })
            .unwrap();
        assert_eq!(engine.message_count(), 1);

        let result = reset(&mut engine).unwrap();
        assert_eq!(result["reset"], true);
        assert_eq!(engine.message_count(), 0);
    }

    #[test]
    fn reset_broadcasts_empty_history() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, _ctx, mut rx) = make_ctx(&tmp);
        while rx.try_recv().is_ok() {}

        reset(&mut engine).unwrap();

        let msg = rx.try_recv().unwrap();
        match msg {
            ServerMessage::History(h) => assert!(h.messages.is_empty()),
            other => panic!("Expected History, got {:?}", other),
        }
    }
}
