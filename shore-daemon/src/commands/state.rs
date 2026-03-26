use serde_json::json;
use shore_protocol::error::ErrorCode;

use crate::engine::ConversationEngine;

use super::{engine_err, CommandContext, CommandResult};

/// Return system status: character, conversation, model, autonomy state, token counts.
pub fn status(engine: &ConversationEngine, ctx: &CommandContext) -> CommandResult {
    let message_count = engine.messages().map(|m| m.len()).unwrap_or(0);

    Ok(json!({
        "character": engine.character_name(),
        "active_conversation": engine.active_conversation_id(),
        "message_count": message_count,
        "active_model": ctx.active_model,
        "autonomy_paused": ctx.autonomy_paused,
        "tokens": {
            "input": ctx.session_tokens.input,
            "output": ctx.session_tokens.output,
            "cache_read": ctx.session_tokens.cache_read,
            "cache_write": ctx.session_tokens.cache_write,
        },
    }))
}

/// List available model profiles from models.toml.
pub fn list_models(ctx: &CommandContext) -> CommandResult {
    let models: Vec<_> = ctx
        .config
        .models
        .models
        .iter()
        .map(|m| {
            json!({
                "name": m.name,
                "provider": m.provider,
                "model_id": m.model_id,
            })
        })
        .collect();

    Ok(json!({
        "models": models,
        "active": ctx.active_model,
    }))
}

/// Switch model or show current. Validates against models.toml profiles.
pub fn switch_model(ctx: &mut CommandContext, args: &serde_json::Value) -> CommandResult {
    let name = args.get("name").and_then(|v| v.as_str());

    match name {
        None => Ok(json!({ "active": ctx.active_model })),
        Some(name) => {
            if ctx.config.models.find_model(name).is_none() {
                return Err((
                    ErrorCode::NotFound,
                    format!(
                        "Model not found: {name}. Use list_models to see available models."
                    ),
                ));
            }
            ctx.active_model = Some(name.to_string());
            Ok(json!({ "active": name, "changed": true }))
        }
    }
}

/// Memory command (stub).
pub fn memory(args: &serde_json::Value) -> CommandResult {
    let query = args.get("query").and_then(|v| v.as_str());
    Ok(json!({
        "status": "not_implemented",
        "query": query,
        "message": "Memory system is not yet implemented",
    }))
}

/// Toggle private mode on the active conversation.
pub fn toggle_private(engine: &mut ConversationEngine, _ctx: &mut CommandContext) -> CommandResult {
    let conv_id = engine
        .active_conversation_id()
        .ok_or_else(|| (ErrorCode::InvalidRequest, "No active conversation".into()))?
        .to_string();

    let current_private = engine
        .list_conversations()
        .iter()
        .find(|c| c.id == conv_id)
        .map(|c| c.private)
        .unwrap_or(false);

    let new_private = !current_private;
    engine
        .set_private(&conv_id, new_private)
        .map_err(engine_err)?;

    // Trigger history push so clients see the updated private state.
    engine.broadcast_history();

    Ok(json!({
        "conversation_id": conv_id,
        "private": new_private,
    }))
}

/// Compaction command (stub).
pub fn compact(args: &serde_json::Value) -> CommandResult {
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Ok(json!({
        "status": "not_implemented",
        "dry_run": dry_run,
        "message": "Compaction is not yet implemented",
    }))
}

/// Toggle autonomy pause/resume (stub).
pub fn toggle_autonomy(ctx: &mut CommandContext) -> CommandResult {
    ctx.autonomy_paused = !ctx.autonomy_paused;
    Ok(json!({
        "autonomy_paused": ctx.autonomy_paused,
    }))
}

/// Show effective configuration. Optionally filtered by section name.
pub fn config(ctx: &CommandContext, args: &serde_json::Value) -> CommandResult {
    let section = args.get("section").and_then(|v| v.as_str());

    let app_json = serde_json::to_value(&ctx.config.app)
        .map_err(|e| (ErrorCode::InternalError, format!("Failed to serialize config: {e}")))?;

    match section {
        None => Ok(json!({ "config": app_json })),
        Some(name) => match app_json.get(name) {
            Some(data) => Ok(json!({ "section": name, "config": data })),
            None => Err((
                ErrorCode::NotFound,
                format!("Config section not found: {name}"),
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CommandContext;
    use crate::config::models::{ModelProfile, ModelsConfig};
    use crate::engine::ConversationEngine;
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
        make_ctx_with_models(tmp, ModelsConfig::default())
    }

    fn make_ctx_with_models(
        tmp: &TempDir,
        models: ModelsConfig,
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

        let config = crate::config::LoadedConfig {
            app: crate::config::app::AppConfig::default(),
            models,
            dirs: crate::config::ShoreDirs {
                config: tmp.path().join("config"),
                data: data_dir.clone(),
                runtime: tmp.path().join("runtime"),
            },
        };

        let ctx = CommandContext {
            config,
            push_tx,
            data_dir,
            active_model: None,
            autonomy_paused: false,
            session_tokens: Default::default(),
        };
        (engine, ctx, push_rx)
    }

    fn sample_models() -> ModelsConfig {
        ModelsConfig {
            provider_defaults: Default::default(),
            models: vec![
                ModelProfile {
                    name: "claude-sonnet".into(),
                    provider: "anthropic".into(),
                    model_id: "claude-sonnet-4-20250514".into(),
                    max_context_tokens: None,
                    max_tokens: None,
                    temperature: None,
                    top_p: None,
                    base_url: None,
                    api_key_env: None,
                },
                ModelProfile {
                    name: "gpt-4o".into(),
                    provider: "openai".into(),
                    model_id: "gpt-4o".into(),
                    max_context_tokens: None,
                    max_tokens: None,
                    temperature: None,
                    top_p: None,
                    base_url: None,
                    api_key_env: None,
                },
            ],
        }
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
    fn status_returns_state() {
        let tmp = TempDir::new().unwrap();
        let (engine, mut ctx, _rx) = make_ctx(&tmp);
        ctx.active_model = Some("claude-sonnet".into());

        let result = status(&engine, &ctx).unwrap();
        assert_eq!(result["character"], "TestChar");
        assert!(result["active_conversation"].is_null());
        assert_eq!(result["message_count"], 0);
        assert_eq!(result["active_model"], "claude-sonnet");
        assert_eq!(result["autonomy_paused"], false);
    }

    #[test]
    fn status_with_active_conversation() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, _ctx, _rx) = make_ctx(&tmp);
        engine.new_conversation("Test").unwrap();
        engine
            .append_message(make_msg("m1", Role::User, "Hi"))
            .unwrap();

        let result = status(&engine, &_ctx).unwrap();
        assert!(result["active_conversation"].is_string());
        assert_eq!(result["message_count"], 1);
    }

    #[test]
    fn list_models_empty() {
        let tmp = TempDir::new().unwrap();
        let (_engine, ctx, _rx) = make_ctx(&tmp);

        let result = list_models(&ctx).unwrap();
        assert!(result["models"].as_array().unwrap().is_empty());
        assert!(result["active"].is_null());
    }

    #[test]
    fn list_models_with_profiles() {
        let tmp = TempDir::new().unwrap();
        let (_engine, ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

        let result = list_models(&ctx).unwrap();
        let models = result["models"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0]["name"], "claude-sonnet");
        assert_eq!(models[1]["name"], "gpt-4o");
    }

    #[test]
    fn switch_model_show_current() {
        let tmp = TempDir::new().unwrap();
        let (_engine, mut ctx, _rx) = make_ctx(&tmp);
        ctx.active_model = Some("claude-sonnet".into());

        let result = switch_model(&mut ctx, &json!({})).unwrap();
        assert_eq!(result["active"], "claude-sonnet");
    }

    #[test]
    fn switch_model_valid() {
        let tmp = TempDir::new().unwrap();
        let (_engine, mut ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

        let result = switch_model(&mut ctx, &json!({"name": "gpt-4o"})).unwrap();
        assert_eq!(result["active"], "gpt-4o");
        assert_eq!(result["changed"], true);
        assert_eq!(ctx.active_model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn switch_model_not_found() {
        let tmp = TempDir::new().unwrap();
        let (_engine, mut ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

        let result = switch_model(&mut ctx, &json!({"name": "nonexistent"}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
    }

    #[test]
    fn memory_stub() {
        let result = memory(&json!({})).unwrap();
        assert_eq!(result["status"], "not_implemented");

        let result = memory(&json!({"query": "test"})).unwrap();
        assert_eq!(result["query"], "test");
    }

    #[test]
    fn toggle_private_on_off() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);
        engine.new_conversation("Test").unwrap();

        // Toggle on.
        let result = toggle_private(&mut engine, &mut ctx).unwrap();
        assert_eq!(result["private"], true);

        // Toggle off.
        let result = toggle_private(&mut engine, &mut ctx).unwrap();
        assert_eq!(result["private"], false);
    }

    #[test]
    fn toggle_private_no_conversation() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, _rx) = make_ctx(&tmp);

        let result = toggle_private(&mut engine, &mut ctx);
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn toggle_private_triggers_history_push() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, mut ctx, mut rx) = make_ctx(&tmp);
        engine.new_conversation("Test").unwrap();
        while rx.try_recv().is_ok() {}

        toggle_private(&mut engine, &mut ctx).unwrap();

        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMessage::History(_)));
    }

    #[test]
    fn compact_stub() {
        let result = compact(&json!({})).unwrap();
        assert_eq!(result["status"], "not_implemented");
        assert_eq!(result["dry_run"], false);

        let result = compact(&json!({"dry_run": true})).unwrap();
        assert_eq!(result["dry_run"], true);
    }

    #[test]
    fn toggle_autonomy_toggles() {
        let tmp = TempDir::new().unwrap();
        let (_engine, mut ctx, _rx) = make_ctx(&tmp);
        assert!(!ctx.autonomy_paused);

        let result = toggle_autonomy(&mut ctx).unwrap();
        assert_eq!(result["autonomy_paused"], true);
        assert!(ctx.autonomy_paused);

        let result = toggle_autonomy(&mut ctx).unwrap();
        assert_eq!(result["autonomy_paused"], false);
        assert!(!ctx.autonomy_paused);
    }

    #[test]
    fn config_full() {
        let tmp = TempDir::new().unwrap();
        let (_engine, ctx, _rx) = make_ctx(&tmp);

        let result = config(&ctx, &json!({})).unwrap();
        assert!(result["config"].is_object());
    }

    #[test]
    fn config_section() {
        let tmp = TempDir::new().unwrap();
        let (_engine, ctx, _rx) = make_ctx(&tmp);

        let result = config(&ctx, &json!({"section": "defaults"})).unwrap();
        assert_eq!(result["section"], "defaults");
    }

    #[test]
    fn config_section_not_found() {
        let tmp = TempDir::new().unwrap();
        let (_engine, ctx, _rx) = make_ctx(&tmp);

        let result = config(&ctx, &json!({"section": "nonexistent"}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
    }
}
