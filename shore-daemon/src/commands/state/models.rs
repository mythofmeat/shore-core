use serde_json::json;
use shore_protocol::error::ErrorCode;
use tracing::info;

use crate::commands::{CommandContext, CommandResult};

/// List available chat model profiles. Tool-only profiles, embedding
/// profiles, and image-generation profiles are intentionally excluded:
/// they are not user-selectable chat targets.
pub fn list_models(ctx: &CommandContext) -> CommandResult {
    let models: Vec<_> = ctx
        .config
        .models
        .chat
        .values()
        .map(|m| {
            json!({
                "name": m.name,
                "qualified_name": m.qualified_name,
                "sdk": m.sdk.as_str(),
                "provider": m.provider_key,
                "model_id": m.model_id,
            })
        })
        .collect();

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

    let data = serde_json::to_value(resolved).map_err(|e| {
        (
            ErrorCode::InternalError,
            format!("Failed to serialize model: {e}"),
        )
    })?;

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
                    format!("Model not found: {name}. Use list_models to see available models."),
                ));
            }
            ctx.active_model = Some(name.to_string());
            info!(model = name, "Model switched");
            Ok(json!({ "active": name, "changed": true }))
        }
    }
}

/// Reset model to config default.
pub fn reset_model(ctx: &mut CommandContext) -> CommandResult {
    let previous = ctx.active_model.take();
    ctx.active_model = ctx.config.app.defaults.model.clone();
    Ok(json!({
        "previous": previous,
        "active": ctx.active_model,
        "reset_to": "config default",
    }))
}
