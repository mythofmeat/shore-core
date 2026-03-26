use serde_json::json;
use shore_protocol::error::ErrorCode;

use crate::engine::ConversationEngine;

use super::{CommandContext, CommandResult};

/// Return system status: character, message count, model, token counts.
pub fn status(engine: &ConversationEngine, ctx: &CommandContext) -> CommandResult {
    Ok(json!({
        "character": engine.character_name(),
        "message_count": engine.message_count(),
        "active_model": ctx.active_model,
        "tokens": {
            "input": ctx.session_tokens.input,
            "output": ctx.session_tokens.output,
            "cache_read": ctx.session_tokens.cache_read,
            "cache_write": ctx.session_tokens.cache_write,
        },
    }))
}

/// List available model profiles from the model catalog.
pub fn list_models(ctx: &CommandContext) -> CommandResult {
    let mut models: Vec<_> = ctx
        .config
        .models
        .chat
        .values()
        .map(|m| {
            json!({
                "name": m.name,
                "qualified_name": m.qualified_name,
                "sdk": m.sdk.as_provider_str(),
                "provider": m.provider_key,
                "model_id": m.model_id,
            })
        })
        .collect();

    // Also include tool models.
    for m in ctx.config.models.tools.values() {
        models.push(json!({
            "name": m.name,
            "qualified_name": m.qualified_name,
            "sdk": m.sdk.as_provider_str(),
            "provider": m.provider_key,
            "model_id": m.model_id,
        }));
    }

    Ok(json!({
        "models": models,
        "active": ctx.active_model,
    }))
}

/// Show detailed info for a model. If no name given, uses the active model.
pub fn model_info(ctx: &CommandContext, args: &serde_json::Value) -> CommandResult {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or(ctx.active_model.as_deref());

    let name = name.ok_or_else(|| {
        (
            ErrorCode::InvalidRequest,
            "No model specified and no active model set".into(),
        )
    })?;

    let resolved = ctx
        .config
        .models
        .find_model(name)
        .map_err(|e| (ErrorCode::NotFound, e.to_string()))?;

    let data = serde_json::to_value(&resolved)
        .map_err(|e| (ErrorCode::InternalError, format!("Failed to serialize model: {e}")))?;

    Ok(data)
}

/// Switch model or show current. Validates against model catalog.
pub fn switch_model(ctx: &mut CommandContext, args: &serde_json::Value) -> CommandResult {
    let name = args.get("name").and_then(|v| v.as_str());

    match name {
        None => Ok(json!({ "active": ctx.active_model })),
        Some(name) => {
            if ctx.config.models.find_model(name).is_err() {
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
    use crate::config::models::ModelCatalog;
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
        make_ctx_with_models(tmp, ModelCatalog::default())
    }

    fn make_ctx_with_models(
        tmp: &TempDir,
        models: ModelCatalog,
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
            models,
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
            session_tokens: Default::default(),
        };
        (engine, ctx, push_rx)
    }

    fn sample_models() -> ModelCatalog {
        let toml_str = r#"
[anthropic.claude-sonnet]
model_id = "claude-sonnet-4-20250514"

[openrouter.gpt-4o]
model_id = "gpt-4o"
"#;
        let table: toml::Table = toml_str.parse().unwrap();
        ModelCatalog::from_sections(Some(&table), None, None, None).unwrap()
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
        assert_eq!(result["message_count"], 0);
        assert_eq!(result["active_model"], "claude-sonnet");
    }

    #[test]
    fn status_with_messages() {
        let tmp = TempDir::new().unwrap();
        let (mut engine, ctx, _rx) = make_ctx(&tmp);
        engine
            .append_message(make_msg("m1", Role::User, "Hi"))
            .unwrap();

        let result = status(&engine, &ctx).unwrap();
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
    fn model_info_by_name() {
        let tmp = TempDir::new().unwrap();
        let (_engine, ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

        let result = model_info(&ctx, &json!({"name": "claude-sonnet"})).unwrap();
        assert_eq!(result["name"], "claude-sonnet");
        assert_eq!(result["model_id"], "claude-sonnet-4-20250514");
        assert!(result["sdk"].is_string());
    }

    #[test]
    fn model_info_uses_active_model() {
        let tmp = TempDir::new().unwrap();
        let (_engine, mut ctx, _rx) = make_ctx_with_models(&tmp, sample_models());
        ctx.active_model = Some("gpt-4o".into());

        let result = model_info(&ctx, &json!({})).unwrap();
        assert_eq!(result["name"], "gpt-4o");
    }

    #[test]
    fn model_info_no_model() {
        let tmp = TempDir::new().unwrap();
        let (_engine, ctx, _rx) = make_ctx(&tmp);

        let result = model_info(&ctx, &json!({}));
        assert!(result.is_err());
        let (code, _msg) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn model_info_not_found() {
        let tmp = TempDir::new().unwrap();
        let (_engine, ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

        let result = model_info(&ctx, &json!({"name": "nonexistent"}));
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
    fn compact_stub() {
        let result = compact(&json!({})).unwrap();
        assert_eq!(result["status"], "not_implemented");
        assert_eq!(result["dry_run"], false);

        let result = compact(&json!({"dry_run": true})).unwrap();
        assert_eq!(result["dry_run"], true);
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
