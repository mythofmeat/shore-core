use serde_json::json;
use shore_protocol::error::ErrorCode;
use shore_protocol::types::CharacterInfo;
use tracing::{debug, info};

use super::{CommandContext, CommandResult};
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

    debug!(count = characters.len(), "Listed characters");
    Ok(json!({ "characters": characters }))
}

/// List characters without requiring an active engine (for use before
/// character resolution, e.g. when multiple characters are available).
pub fn list_characters_standalone(ctx: &CommandContext) -> CommandResult {
    let characters_dir = ctx.config.dirs.config.join("characters");
    let mut characters = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&characters_dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                let name = entry.file_name().to_string_lossy().to_string();
                characters.push(CharacterInfo { name });
            }
        }
    }

    // Also check data dir for characters that have data but no config dir.
    if let Ok(entries) = std::fs::read_dir(&ctx.data_dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                let name = entry.file_name().to_string_lossy().to_string();
                if !characters.iter().any(|c| c.name == name) {
                    characters.push(CharacterInfo { name });
                }
            }
        }
    }

    debug!(count = characters.len(), "Listed characters (standalone)");
    Ok(json!({ "characters": characters }))
}

/// Show detailed info about a character: definition path, user.md presence,
/// prompt override files, and a preview of the character definition.
pub fn character_info(
    engine: &ConversationEngine,
    ctx: &CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| engine.character_name());

    let char_dir = ctx.config.dirs.config.join("characters").join(name);

    if !char_dir.exists() && name != engine.character_name() {
        return Err((ErrorCode::NotFound, format!("Character not found: {name}")));
    }

    let definition_path = char_dir.join("character.md");
    let has_definition = definition_path.exists();
    let definition_preview = if has_definition {
        std::fs::read_to_string(&definition_path)
            .ok()
            .map(|s| s.chars().take(500).collect::<String>())
    } else {
        None
    };

    let user_path = char_dir.join("user.md");
    let has_user_definition = user_path.exists();

    // Scan for prompt override files.
    let prompts_dir = char_dir.join("prompts");
    let prompt_overrides: Vec<String> = if prompts_dir.exists() {
        std::fs::read_dir(&prompts_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect()
    } else {
        vec![]
    };

    let config_override_path = char_dir.join("config.toml");
    let has_config_override = config_override_path.exists();

    let data_dir = ctx.data_dir.join(name);
    let has_data = data_dir.exists();

    debug!(
        character = name,
        has_definition,
        has_user_definition,
        has_config_override,
        prompt_override_count = prompt_overrides.len(),
        "Character info queried"
    );
    Ok(json!({
        "name": name,
        "active": name == engine.character_name(),
        "config_dir": char_dir.display().to_string(),
        "has_definition": has_definition,
        "definition_preview": definition_preview,
        "has_user_definition": has_user_definition,
        "has_config_override": has_config_override,
        "prompt_overrides": prompt_overrides,
        "data_dir": data_dir.display().to_string(),
        "has_data": has_data,
    }))
}

/// Switch to a different character. In multi-character mode, this is handled
/// by the registry — this command validates the character exists.
pub fn switch_character(
    engine: &ConversationEngine,
    ctx: &mut CommandContext,
    args: &serde_json::Value,
) -> CommandResult {
    let name = args.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
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
        return Err((ErrorCode::NotFound, format!("Character not found: {name}")));
    }

    info!(character = name, "Character switch requested");

    // In multi-character mode, the client should reconnect with the new character.
    // For now, acknowledge the request.
    Ok(json!({ "character": name, "changed": true, "reconnect_required": true }))
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
        let (autonomy, _compaction_rx) = crate::autonomy::manager::AutonomyManager::new(
            Default::default(),
            Default::default(),
            data_dir.clone(),
            rx,
        );

        let ctx = CommandContext {
            config,
            push_tx,
            data_dir: data_dir.clone(),
            active_model: None,
            session_tokens: std::sync::Arc::new(std::sync::Mutex::new(SessionTokens::default())),
            autonomy,
            llm_client: shore_ledger::LedgerClient::new(shore_llm_client::LlmClient::new(), &data_dir.join("ledger.db")).unwrap(),
            diagnostics: std::sync::Arc::new(std::sync::Mutex::new(
                shore_diagnostics::Diagnostics::default(),
            )),
            memory_shell_sessions: std::collections::HashMap::new(),
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
    fn character_info_active() {
        let tmp = TempDir::new().unwrap();
        let (engine, ctx, _rx) = make_ctx(&tmp);

        let result = character_info(&engine, &ctx, &json!({})).unwrap();
        assert_eq!(result["name"], "TestChar");
        assert_eq!(result["active"], true);
        assert!(!result["has_definition"].as_bool().unwrap());
    }

    #[test]
    fn character_info_with_definition() {
        let tmp = TempDir::new().unwrap();
        let (engine, ctx, _rx) = make_ctx(&tmp);

        // Create character definition.
        let char_dir = tmp
            .path()
            .join("config")
            .join("characters")
            .join("TestChar");
        std::fs::create_dir_all(&char_dir).unwrap();
        std::fs::write(char_dir.join("character.md"), "You are a test character.").unwrap();
        std::fs::write(char_dir.join("user.md"), "The user likes tests.").unwrap();

        let result = character_info(&engine, &ctx, &json!({})).unwrap();
        assert!(result["has_definition"].as_bool().unwrap());
        assert!(result["has_user_definition"].as_bool().unwrap());
        assert!(result["definition_preview"]
            .as_str()
            .unwrap()
            .contains("test character"));
    }

    #[test]
    fn character_info_not_found() {
        let tmp = TempDir::new().unwrap();
        let (engine, ctx, _rx) = make_ctx(&tmp);

        let result = character_info(&engine, &ctx, &json!({"name": "Nonexistent"}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
    }

    #[test]
    fn character_info_with_prompt_overrides() {
        let tmp = TempDir::new().unwrap();
        let (engine, ctx, _rx) = make_ctx(&tmp);

        let prompts_dir = tmp
            .path()
            .join("config")
            .join("characters")
            .join("TestChar")
            .join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();
        std::fs::write(prompts_dir.join("system.md"), "override").unwrap();

        let result = character_info(&engine, &ctx, &json!({})).unwrap();
        let overrides = result["prompt_overrides"].as_array().unwrap();
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0], "system.md");
    }
}
