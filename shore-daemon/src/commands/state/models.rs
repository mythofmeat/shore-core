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

/// Set or clear the per-session `reasoning_effort` override.
///
/// Args:
/// - omitted → read-only, return current state
/// - `{ "value": null }` or `{ "clear": true }` → clear override (use config)
/// - `{ "value": "off" | "none" | "disable" }` → force off (null in request)
/// - `{ "value": "<string>" }` → force to that string (typically "low"|"medium"|"high")
pub fn set_reasoning_effort(ctx: &mut CommandContext, args: &serde_json::Value) -> CommandResult {
    let has_value_key = args.get("value").is_some();
    let clear_flag = args.get("clear").and_then(|v| v.as_bool()).unwrap_or(false);

    let resolved_default = resolve_config_default(ctx);

    if !has_value_key && !clear_flag {
        return Ok(json!({
            "override": override_to_json(&ctx.reasoning_effort_override),
            "effective": effective_value(&ctx.reasoning_effort_override, &resolved_default),
            "config_default": resolved_default,
        }));
    }

    if clear_flag {
        ctx.reasoning_effort_override = None;
        info!("reasoning_effort override cleared");
        return Ok(json!({
            "changed": true,
            "override": serde_json::Value::Null,
            "effective": resolved_default.clone(),
            "config_default": resolved_default,
        }));
    }

    let raw = args.get("value").unwrap();
    let new_override: Option<String> = if raw.is_null() {
        None
    } else if let Some(s) = raw.as_str() {
        match s.trim().to_ascii_lowercase().as_str() {
            "" => {
                ctx.reasoning_effort_override = None;
                info!("reasoning_effort override cleared");
                return Ok(json!({
                    "changed": true,
                    "override": serde_json::Value::Null,
                    "effective": resolved_default.clone(),
                    "config_default": resolved_default,
                }));
            }
            "off" | "none" | "disable" | "disabled" | "unset" => None,
            _ => Some(s.trim().to_string()),
        }
    } else {
        return Err((
            shore_protocol::error::ErrorCode::InvalidRequest,
            "value must be a string or null".into(),
        ));
    };

    ctx.reasoning_effort_override = Some(new_override.clone());
    info!(
        value = new_override.as_deref().unwrap_or("<off>"),
        "reasoning_effort override set"
    );

    Ok(json!({
        "changed": true,
        "override": override_to_json(&ctx.reasoning_effort_override),
        "effective": effective_value(&ctx.reasoning_effort_override, &resolved_default),
        "config_default": resolved_default,
    }))
}

fn resolve_config_default(ctx: &CommandContext) -> Option<String> {
    let name = ctx
        .active_model
        .as_deref()
        .or(ctx.config.app.defaults.model.as_deref())?;
    ctx.config
        .models
        .find_model(name)
        .ok()
        .and_then(|m| m.reasoning_effort.clone())
}

fn override_to_json(o: &Option<Option<String>>) -> serde_json::Value {
    match o {
        None => serde_json::Value::Null,
        Some(None) => json!({ "set": true, "value": serde_json::Value::Null }),
        Some(Some(v)) => json!({ "set": true, "value": v }),
    }
}

fn effective_value(
    o: &Option<Option<String>>,
    default: &Option<String>,
) -> serde_json::Value {
    match o {
        None => default
            .clone()
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null),
        Some(None) => serde_json::Value::Null,
        Some(Some(v)) => serde_json::Value::String(v.clone()),
    }
}
