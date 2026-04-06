use serde_json::json;
use shore_ledger::query::QueryFilter;
use shore_protocol::error::ErrorCode;

use super::{CommandContext, CommandResult};

fn parse_last_period(period: &str) -> Option<String> {
    let now = chrono::Utc::now();
    match period {
        "today" => {
            let start = now.date_naive().and_hms_opt(0, 0, 0)?;
            Some(start.and_utc().to_rfc3339())
        }
        "all" => None,
        s if s.ends_with('d') => {
            let days: i64 = s.trim_end_matches('d').parse().ok()?;
            Some((now - chrono::Duration::days(days)).to_rfc3339())
        }
        _ => None,
    }
}

fn build_filter(args: &serde_json::Value) -> (QueryFilter, String) {
    let last = args
        .get("last")
        .and_then(|v| v.as_str())
        .unwrap_or("today")
        .to_string();
    let since = parse_last_period(&last);
    let filter = QueryFilter {
        since,
        character: args
            .get("character")
            .and_then(|v| v.as_str())
            .map(String::from),
        provider: args
            .get("provider")
            .and_then(|v| v.as_str())
            .map(String::from),
        model: args.get("model").and_then(|v| v.as_str()).map(String::from),
        call_type: args
            .get("call_type")
            .and_then(|v| v.as_str())
            .map(String::from),
        ..Default::default()
    };
    (filter, last)
}

pub async fn usage(ctx: &CommandContext, args: &serde_json::Value) -> CommandResult {
    let ledger = ctx.llm_client.ledger();

    let (filter, last) = build_filter(args);

    if args.get("export_tsv").and_then(|v| v.as_bool()) == Some(true) {
        let output = shore_ledger::query::export_tsv(ledger, &filter)
            .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;
        return Ok(json!({ "mode": "tsv", "data": output }));
    }

    if args.get("export_csv").and_then(|v| v.as_bool()) == Some(true) {
        let tsv = shore_ledger::query::export_tsv(ledger, &filter)
            .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;
        let mut csv_lines = Vec::new();
        for line in tsv.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            let csv_fields: Vec<String> = fields
                .iter()
                .map(|f| {
                    if f.contains(',') || f.contains('"') || f.contains('\n') {
                        format!("\"{}\"", f.replace('"', "\"\""))
                    } else {
                        f.to_string()
                    }
                })
                .collect();
            csv_lines.push(csv_fields.join(","));
        }
        return Ok(json!({ "mode": "csv", "data": csv_lines.join("\n") }));
    }

    if args.get("anomalies").and_then(|v| v.as_bool()) == Some(true) {
        let rows = shore_ledger::query::query_anomalies(ledger, &filter)
            .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;
        let anomalies: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                json!({
                    "ts": r.ts,
                    "character": r.character,
                    "model": r.model,
                    "call_type": r.call_type,
                    "anomaly": r.cache_anomaly,
                    "cache_read_tokens": r.cache_read_tokens,
                    "cache_write_tokens": r.cache_write_tokens,
                })
            })
            .collect();
        return Ok(json!({ "mode": "anomalies", "anomalies": anomalies }));
    }

    if args.get("refresh_pricing").and_then(|v| v.as_bool()) == Some(true) {
        let pricing = ctx.llm_client.pricing();
        pricing
            .clear_cache()
            .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;
        return Ok(json!({ "mode": "refresh_pricing" }));
    }

    if args.get("recalculate").and_then(|v| v.as_bool()) == Some(true) {
        let null_rows = shore_ledger::query::null_cost_rows(ledger)
            .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;
        if null_rows.is_empty() {
            return Ok(json!({ "mode": "recalculate", "updated": 0, "total": 0 }));
        }

        let pricing = ctx.llm_client.pricing();
        let mut models_fetched = std::collections::HashSet::new();
        for row in &null_rows {
            let key = format!("{}/{}", row.provider, row.model);
            if models_fetched.insert(key) {
                let _ = pricing.get_or_fetch(&row.provider, &row.model).await;
            }
        }

        let mut updated = 0u32;
        for row in &null_rows {
            if let Ok(Some(cost)) = pricing.calculate_cost(
                &row.provider,
                &row.model,
                row.input_tokens,
                row.output_tokens,
                row.cache_read_tokens,
                row.cache_write_tokens,
            ) {
                if shore_ledger::query::update_costs(ledger, row.id, &cost).is_ok() {
                    updated += 1;
                }
            }
        }

        return Ok(json!({
            "mode": "recalculate",
            "updated": updated,
            "total": null_rows.len(),
        }));
    }

    let summary = shore_ledger::query::usage_summary(ledger, &filter)
        .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;

    let summary_rows: Vec<serde_json::Value> = summary
        .iter()
        .map(|s| {
            json!({
                "provider": s.provider,
                "model": s.model,
                "call_count": s.call_count,
                "total_input": s.total_input,
                "total_output": s.total_output,
                "total_cache_read": s.total_cache_read,
                "total_cache_write": s.total_cache_write,
                "total_cost": s.total_cost,
            })
        })
        .collect();

    let characters = shore_ledger::query::active_anthropic_characters(ledger, &filter)
        .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;

    let cache_health: Vec<serde_json::Value> = characters
        .iter()
        .map(|(char_name, last_row)| {
            let tracker = shore_ledger::cache_tracker::CacheTracker::reconstruct(
                &last_row.ts,
                &last_row.model,
                last_row.thinking_enabled,
                last_row.cache_read_tokens,
                3600,
            );
            let streak = shore_ledger::query::warm_streak(ledger, char_name).unwrap_or(0);
            let state = match tracker.state() {
                shore_ledger::cache_tracker::CacheState::Warm => "warm",
                shore_ledger::cache_tracker::CacheState::Cold => "cold",
            };
            json!({
                "character": char_name,
                "state": state,
                "streak": streak,
            })
        })
        .collect();

    let anomaly_filter = QueryFilter {
        since: parse_last_period("7d"),
        ..Default::default()
    };
    let anomaly_count = shore_ledger::query::query_anomalies(ledger, &anomaly_filter)
        .map(|r| r.len())
        .unwrap_or(0);

    Ok(json!({
        "mode": "summary",
        "period": last,
        "summary": summary_rows,
        "cache_health": cache_health,
        "anomaly_count_7d": anomaly_count,
    }))
}
