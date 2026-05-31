use serde_json::{Value, json};
use shore_protocol::error::ErrorCode;

use crate::autonomy::activity::HourClassification;
use crate::convert::u64_to_usize;
use crate::engine::ConversationEngine;
use crate::sync::lock_or_recover;

use crate::commands::{CommandContext, CommandResult};

/// Return system status: character, turn count, model, token counts.
pub fn status(engine: &ConversationEngine, ctx: &CommandContext) -> CommandResult {
    let turn_count = engine.turn_count();
    let activity =
        ctx.autonomy
            .activity_stats(engine.character_name())
            .map(|(stats, activity_turn_count)| {
                let classifications: Vec<&str> = stats
                    .hour_classifications
                    .iter()
                    .map(|c| match c {
                        HourClassification::Peak => "peak",
                        HourClassification::Trough => "trough",
                        HourClassification::Normal => "normal",
                    })
                    .collect();
                json!({
                    "hour_histogram": stats.hour_histogram.to_vec(),
                    "hour_classifications": classifications,
                    "has_sufficient_heatmap": stats.has_sufficient_heatmap,
                    "engagement_score": stats.engagement_score,
                    "sessions_per_day": stats.sessions_per_day,
                    "message_count": activity_turn_count,
                    "turn_count": activity_turn_count,
                })
            });

    // Show the effective model: runtime override -> per-character/global default.
    let effective_model = ctx
        .active_model
        .as_deref()
        .or(ctx.config.app.defaults.model.as_deref());

    let tokens = lock_or_recover("command session tokens", &ctx.session_tokens);
    let character_data_dir = ctx.config.dirs.data.join(engine.character_name());
    let pending_deferred_edits =
        crate::memory::deferred_edits::pending_deferred_edit_paths(&character_data_dir)
            .unwrap_or_default();
    Ok(json!({
        "character": engine.character_name(),
        "message_count": turn_count,
        "turn_count": turn_count,
        "active_model": effective_model,
        "config_dir": ctx.config.dirs.config.display().to_string(),
        "data_dir": ctx.config.dirs.data.display().to_string(),
        "cache_dir": ctx.config.dirs.cache.display().to_string(),
        "memory_mode": "markdown",
        "pending_deferred_edit_count": pending_deferred_edits.len(),
        "pending_deferred_edits": pending_deferred_edits,
        "tokens": {
            "input": tokens.input,
            "output": tokens.output,
            "cache_read": tokens.cache_read,
            "cache_write": tokens.cache_write,
        },
        "autonomy": ctx.autonomy.status(engine.character_name()),
        "activity": activity,
    }))
}

/// Return recent diagnostics from in-memory ring buffers.
pub fn diagnostics(ctx: &CommandContext, args: &Value) -> CommandResult {
    let count = count_arg(args, 10);
    let diag = lock_or_recover("command diagnostics buffer", &ctx.diagnostics);
    Ok(diag.to_json(count))
}

/// Return heartbeat event log for the active character.
pub fn heartbeat_log(
    engine: &ConversationEngine,
    ctx: &CommandContext,
    args: &Value,
) -> CommandResult {
    let limit = count_arg(args, 20);
    let events = ctx.autonomy.heartbeat_log(engine.character_name(), limit);
    let events_json: Vec<serde_json::Value> = events
        .iter()
        .map(|e| {
            json!({
                "timestamp": e.timestamp,
                "kind": e.kind,
                "detail": e.detail,
            })
        })
        .collect();
    Ok(json!({ "events": events_json }))
}

pub fn heartbeat_tick_now(engine: &ConversationEngine, ctx: &CommandContext) -> CommandResult {
    let char_name = engine.character_name();
    match ctx.autonomy.heartbeat_tick_now(char_name) {
        Some(dormant) => {
            let mut result = json!({
                "status": "scheduled",
                "character": char_name,
            });
            if dormant {
                result["warning"] = json!(
                    "Heartbeat is dormant. The scheduled tick will be suppressed \
                     by the abandonment guard. Run `shore debug heartbeat_status_active` \
                     first to wake the clock."
                );
            }
            Ok(result)
        }
        None => Err((
            ErrorCode::InvalidRequest,
            format!("No autonomy state for character '{char_name}'"),
        )),
    }
}

fn count_arg(args: &Value, default: usize) -> usize {
    args.get("count")
        .and_then(Value::as_u64)
        .map_or(default, u64_to_usize)
}

pub fn heartbeat_set_dormant(engine: &ConversationEngine, ctx: &CommandContext) -> CommandResult {
    let char_name = engine.character_name();
    if ctx.autonomy.heartbeat_set_dormant(char_name) {
        Ok(json!({ "status": "dormant", "character": char_name }))
    } else {
        Err((
            ErrorCode::InvalidRequest,
            format!("No autonomy state for character '{char_name}'"),
        ))
    }
}

pub fn heartbeat_set_active(engine: &ConversationEngine, ctx: &CommandContext) -> CommandResult {
    let char_name = engine.character_name();
    if ctx.autonomy.heartbeat_set_active(char_name) {
        Ok(json!({ "status": "active", "character": char_name }))
    } else {
        Err((
            ErrorCode::InvalidRequest,
            format!("No autonomy state for character '{char_name}'"),
        ))
    }
}
