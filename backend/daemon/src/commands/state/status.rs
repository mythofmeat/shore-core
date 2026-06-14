use serde_json::{json, Value};
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
    let events_json: Vec<Value> = events
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

/// Query the raw call-payload store. With `id`, return that one call's
/// decompressed request/response; otherwise return an index of recent calls
/// (newest first) filtered by `call_type` and `character` (defaulting to the
/// active character).
pub fn call_log(engine: &ConversationEngine, ctx: &CommandContext, args: &Value) -> CommandResult {
    use shore_call_store::CallFilter;

    let Some(store) = ctx.llm_client.inner().call_store() else {
        return Ok(json!({ "enabled": false, "entries": [] }));
    };

    if let Some(id) = args.get("id").and_then(Value::as_i64) {
        return match store.get_call(id) {
            Ok(Some(payload)) => Ok(json!({
                "enabled": true,
                "call": serde_json::to_value(&payload).unwrap_or(Value::Null),
            })),
            Ok(None) => Err((ErrorCode::InvalidRequest, format!("no call with id {id}"))),
            Err(e) => Err((
                ErrorCode::InternalError,
                format!("call store query failed: {e}"),
            )),
        };
    }

    let character = args
        .get("character")
        .and_then(Value::as_str)
        .map_or_else(|| engine.character_name().to_owned(), str::to_owned);
    let filter = CallFilter {
        call_type: args
            .get("call_type")
            .and_then(Value::as_str)
            .map(str::to_owned),
        character: Some(character),
        limit: count_arg(args, 20),
    };
    match store.query_calls(&filter) {
        Ok(rows) => {
            let entries: Vec<Value> = rows
                .iter()
                .map(|row| serde_json::to_value(row).unwrap_or(Value::Null))
                .collect();
            Ok(json!({ "enabled": true, "entries": entries }))
        }
        Err(e) => Err((
            ErrorCode::InternalError,
            format!("call store query failed: {e}"),
        )),
    }
}

/// Query curated background transcripts (`source` = `heartbeat` or `dreaming`)
/// for the active character, newest first.
pub fn transcript(
    engine: &ConversationEngine,
    ctx: &CommandContext,
    args: &Value,
) -> CommandResult {
    let source = args
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("heartbeat");
    if source != "heartbeat" && source != "dreaming" {
        return Err((
            ErrorCode::InvalidRequest,
            format!("unknown transcript source '{source}' (expected 'heartbeat' or 'dreaming')"),
        ));
    }
    let char_name = engine.character_name();
    let Some(store) = ctx.llm_client.inner().call_store() else {
        return Ok(json!({ "enabled": false, "source": source, "entries": [] }));
    };
    match store.query_transcripts(source, Some(char_name), count_arg(args, 20)) {
        Ok(rows) => {
            let entries: Vec<Value> = rows
                .iter()
                .map(|row| serde_json::to_value(row).unwrap_or(Value::Null))
                .collect();
            Ok(json!({
                "enabled": true,
                "source": source,
                "character": char_name,
                "entries": entries,
            }))
        }
        Err(e) => Err((
            ErrorCode::InternalError,
            format!("transcript query failed: {e}"),
        )),
    }
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
                if let Some(obj) = result.as_object_mut() {
                    let _ignored = obj.insert(
                        "warning".into(),
                        json!(
                            "Heartbeat is dormant. The scheduled tick will be suppressed \
                             by the abandonment guard. Run `shore debug heartbeat_status_active` \
                             first to wake the clock."
                        ),
                    );
                }
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
