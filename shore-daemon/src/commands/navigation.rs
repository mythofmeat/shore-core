use serde_json::json;
use shore_protocol::error::ErrorCode;
use shore_protocol::types::CharacterInfo;
use shore_config::{character_workspace_dir, AGENTS_FILE, HEARTBEAT_FILE, SOUL_FILE, TOOLS_FILE, USER_FILE};
use tracing::{debug, info};

use super::{CommandContext, CommandResult};
use crate::engine::ConversationEngine;

/// List available characters by scanning the config characters directory.
pub fn list_characters(engine: &ConversationEngine, ctx: &CommandContext) -> CommandResult {
    let mut characters = vec![CharacterInfo {
        name: engine.character_name().to_string(),
    }];

    for name in shore_config::discover_characters(&ctx.config.dirs.config) {
        if name != engine.character_name() {
            characters.push(CharacterInfo { name });
        }
    }

    debug!(count = characters.len(), "Listed characters");
    Ok(json!({ "characters": characters }))
}

/// List characters without requiring an active engine (for use before
/// character resolution, e.g. when multiple characters are available).
pub fn list_characters_standalone(ctx: &CommandContext) -> CommandResult {
    let mut characters: Vec<CharacterInfo> = shore_config::discover_characters(&ctx.config.dirs.config)
        .into_iter()
        .map(|name| CharacterInfo { name })
        .collect();

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

/// Show detailed info about a character's workspace bootstrap files.
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

    let workspace_dir = character_workspace_dir(&ctx.config.dirs.config, name);
    let definition_path = workspace_dir.join(SOUL_FILE);
    let has_definition = definition_path.exists();
    let definition_preview = if has_definition {
        std::fs::read_to_string(&definition_path)
            .ok()
            .map(|s| s.chars().take(500).collect::<String>())
    } else {
        None
    };

    let bootstrap_files = [SOUL_FILE, USER_FILE, AGENTS_FILE, TOOLS_FILE, HEARTBEAT_FILE]
        .into_iter()
        .filter(|name| workspace_dir.join(name).exists())
        .map(str::to_string)
        .collect::<Vec<_>>();

    let config_override_path = char_dir.join("config.toml");
    let has_config_override = config_override_path.exists();

    let data_dir = ctx.data_dir.join(name);
    let has_data = data_dir.exists();
    let pending_deferred_edits =
        crate::memory::deferred_edits::pending_deferred_edit_paths(&data_dir).unwrap_or_default();

    debug!(
        character = name,
        has_definition,
        has_config_override,
        bootstrap_file_count = bootstrap_files.len(),
        "Character info queried"
    );
    Ok(json!({
        "name": name,
        "active": name == engine.character_name(),
        "config_dir": char_dir.display().to_string(),
        "workspace_dir": workspace_dir.display().to_string(),
        "has_definition": has_definition,
        "definition_preview": definition_preview,
        "bootstrap_files": bootstrap_files,
        "has_config_override": has_config_override,
        "pending_deferred_edits": pending_deferred_edits,
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

    Ok(json!({ "character": name, "changed": true }))
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
        let autonomy = crate::autonomy::manager::AutonomyManager::new(
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
            reasoning_effort_override: None,
            session_tokens: std::sync::Arc::new(std::sync::Mutex::new(SessionTokens::default())),
            autonomy,
            llm_client: shore_ledger::LedgerClient::new(
                shore_llm_client::LlmClient::new(),
                &data_dir.join("ledger.db"),
            )
            .unwrap(),
            diagnostics: std::sync::Arc::new(std::sync::Mutex::new(
                shore_diagnostics::Diagnostics::default(),
            )),
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
        std::fs::create_dir_all(chars_dir.join("Alice").join("workspace")).unwrap();
        std::fs::create_dir_all(chars_dir.join("Bob").join("workspace")).unwrap();
        // Active character directory (should be deduped).
        std::fs::create_dir_all(chars_dir.join("TestChar").join("workspace")).unwrap();
        std::fs::write(chars_dir.join("Alice").join("workspace").join("SOUL.md"), "Alice").unwrap();
        std::fs::write(chars_dir.join("Bob").join("workspace").join("SOUL.md"), "Bob").unwrap();
        std::fs::write(chars_dir.join("TestChar").join("workspace").join("SOUL.md"), "Test").unwrap();

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
        let workspace_dir = char_dir.join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();
        std::fs::write(workspace_dir.join("SOUL.md"), "You are a test character.").unwrap();
        std::fs::write(workspace_dir.join("USER.md"), "The user likes tests.").unwrap();

        let result = character_info(&engine, &ctx, &json!({})).unwrap();
        assert!(result["has_definition"].as_bool().unwrap());
        assert!(result["bootstrap_files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "USER.md"));
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

        let workspace_dir = tmp
            .path()
            .join("config")
            .join("characters")
            .join("TestChar")
            .join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();
        std::fs::write(workspace_dir.join("AGENTS.md"), "override").unwrap();

        let result = character_info(&engine, &ctx, &json!({})).unwrap();
        let bootstrap_files = result["bootstrap_files"].as_array().unwrap();
        assert!(bootstrap_files.iter().any(|v| v == "AGENTS.md"));
    }
}
