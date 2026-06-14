use serde_json::{json, Value};
use shore_protocol::error::ErrorCode;

use crate::commands::{CommandContext, CommandResult};
use crate::engine::ConversationEngine;

/// `delay [on|off|reset|status]` — control the human-like reply delay.
///
/// - `on` / `off` set a transient runtime override (cleared on daemon restart),
/// - `reset` drops the override so the configured default applies again,
/// - `status` (the default) reports the effective state, bounds, and — for
///   debugging — the countdown to any reply currently being held.
pub fn delay(engine: &ConversationEngine, ctx: &CommandContext, args: &Value) -> CommandResult {
    let char_name = engine.character_name();
    let cfg = &ctx.config.app.behavior.response_delay;

    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("status");

    // `None` = report only; `Some(value)` = mutate the override to `value`.
    let mutation = match action {
        "status" => None,
        "on" => Some(Some(true)),
        "off" => Some(Some(false)),
        "reset" => Some(None),
        other => {
            return Err((
                ErrorCode::InvalidRequest,
                format!("unknown delay action '{other}'; expected on, off, reset, or status"),
            ));
        }
    };

    if let Some(value) = mutation {
        if !ctx.autonomy.set_response_delay_override(char_name, value) {
            return Err((
                ErrorCode::InvalidRequest,
                format!("No autonomy state for character '{char_name}'"),
            ));
        }
    }

    let status = ctx
        .autonomy
        .response_delay_status(char_name, cfg)
        .ok_or_else(|| {
            (
                ErrorCode::InvalidRequest,
                format!("No autonomy state for character '{char_name}'"),
            )
        })?;

    let mut value =
        serde_json::to_value(&status).map_err(|e| (ErrorCode::InternalError, e.to_string()))?;
    if let Value::Object(map) = &mut value {
        let _ignored = map.insert("character".into(), json!(char_name));
    }
    Ok(value)
}
