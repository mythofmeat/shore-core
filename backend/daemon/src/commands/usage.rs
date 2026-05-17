use serde_json::json;
use shore_ledger::query::QueryFilter;
use shore_protocol::error::ErrorCode;
use tracing::debug;

use super::{CommandContext, CommandResult};

fn parse_last_period_at(period: &str, now: chrono::DateTime<chrono::Utc>) -> Option<String> {
    match period {
        "today" => {
            let start = now.date_naive().and_hms_opt(0, 0, 0)?;
            Some(start.and_utc().to_rfc3339())
        }
        "all" => None,
        s if s.ends_with('h') => {
            let hours: i64 = s.trim_end_matches('h').parse().ok()?;
            Some((now - chrono::Duration::hours(hours)).to_rfc3339())
        }
        s if s.ends_with('d') => {
            let days: i64 = s.trim_end_matches('d').parse().ok()?;
            Some((now - chrono::Duration::days(days)).to_rfc3339())
        }
        _ => None,
    }
}

fn parse_last_period(period: &str) -> Option<String> {
    parse_last_period_at(period, chrono::Utc::now())
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
        api_key_name: args
            .get("api_key")
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
    debug!(period = %last, "Usage query started");

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

    if args.get("by_kind").and_then(|v| v.as_bool()) == Some(true) {
        let summary = shore_ledger::query::usage_summary_by_usage_kind(ledger, &filter)
            .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;
        let rows: Vec<serde_json::Value> = summary
            .iter()
            .map(|s| {
                json!({
                    "usage_kind": s.usage_kind,
                    "call_count": s.call_count,
                    "total_input": s.total_input,
                    "total_output": s.total_output,
                    "total_cache_read": s.total_cache_read,
                    "total_cache_write": s.total_cache_write,
                    "total_cost": s.total_cost,
                })
            })
            .collect();
        return Ok(json!({
            "mode": "summary_by_usage_kind",
            "period": last,
            "summary": rows,
        }));
    }

    if args.get("by_api_key").and_then(|v| v.as_bool()) == Some(true) {
        let summary = shore_ledger::query::usage_summary_by_api_key(ledger, &filter)
            .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;
        let rows: Vec<serde_json::Value> = summary
            .iter()
            .map(|s| {
                json!({
                    "provider": s.provider,
                    "api_key_name": s.api_key_name,
                    "call_count": s.call_count,
                    "total_input": s.total_input,
                    "total_output": s.total_output,
                    "total_cache_read": s.total_cache_read,
                    "total_cache_write": s.total_cache_write,
                    "total_cost": s.total_cost,
                })
            })
            .collect();
        return Ok(json!({
            "mode": "summary_by_api_key",
            "period": last,
            "summary": rows,
        }));
    }

    if args.get("by_call_type").and_then(|v| v.as_bool()) == Some(true) {
        let summary = shore_ledger::query::usage_summary_by_call_type(ledger, &filter)
            .map_err(|e| (ErrorCode::InternalError, e.to_string()))?;
        let rows: Vec<serde_json::Value> = summary
            .iter()
            .map(|s| {
                json!({
                    "call_type": s.call_type,
                    "call_count": s.call_count,
                    "total_input": s.total_input,
                    "total_output": s.total_output,
                    "total_cache_read": s.total_cache_read,
                    "total_cache_write": s.total_cache_write,
                    "total_cost": s.total_cost,
                })
            })
            .collect();
        return Ok(json!({
            "mode": "summary_by_call_type",
            "period": last,
            "summary": rows,
        }));
    }

    if args.get("anomalies").and_then(|v| v.as_bool()) == Some(true) {
        let anomaly_filter = if last == "today" {
            QueryFilter {
                since: parse_last_period("7d"),
                ..filter.clone()
            }
        } else {
            filter
        };
        let rows = shore_ledger::query::query_anomalies(ledger, &anomaly_filter)
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
        let force = args.get("force").and_then(|v| v.as_bool()) == Some(true);
        let rows = if force {
            shore_ledger::query::all_cost_rows(ledger)
                .map_err(|e| (ErrorCode::InternalError, e.to_string()))?
        } else {
            shore_ledger::query::null_cost_rows(ledger)
                .map_err(|e| (ErrorCode::InternalError, e.to_string()))?
        };
        if rows.is_empty() {
            return Ok(json!({ "mode": "recalculate", "updated": 0, "total": 0, "failures": [] }));
        }

        let pricing = ctx.llm_client.pricing();
        let mut models_fetched = std::collections::HashSet::new();
        let mut fetch_results: std::collections::HashMap<String, Option<String>> =
            std::collections::HashMap::new();

        for row in &rows {
            let key = format!("{}/{}", row.provider, row.model);
            if models_fetched.insert(key.clone()) {
                let model_id = shore_ledger::pricing::to_openrouter_id(&row.provider, &row.model);
                match pricing.get_or_fetch(&row.provider, &row.model).await {
                    Some(_) => {
                        fetch_results.insert(key, None);
                    }
                    None => {
                        tracing::warn!(model_id, "Pricing fetch returned no data for model");
                        fetch_results.insert(key, Some(format!("no pricing data for {model_id}")));
                    }
                }
            }
        }

        let mut updated = 0u32;
        for row in &rows {
            if let Ok(Some(cost)) = pricing.calculate_cost(shore_ledger::pricing::CostRequest {
                provider: &row.provider,
                model: &row.model,
                input_tokens: row.input_tokens,
                output_tokens: row.output_tokens,
                cache_read_tokens: row.cache_read_tokens,
                cache_write_tokens: row.cache_write_tokens,
                cache_ttl: row.cache_ttl.as_deref(),
            }) {
                if shore_ledger::query::update_costs(ledger, row.id, &cost).is_ok() {
                    updated += 1;
                }
            }
        }

        let failures: Vec<serde_json::Value> = fetch_results
            .iter()
            .filter_map(|(key, reason)| {
                reason
                    .as_ref()
                    .map(|r| json!({ "model": key, "reason": r }))
            })
            .collect();

        debug!(
            updated,
            total = rows.len(),
            failures = failures.len(),
            "Recalculation complete"
        );
        return Ok(json!({
            "mode": "recalculate",
            "updated": updated,
            "total": rows.len(),
            "failures": failures,
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

    let claude_code_rows: Vec<_> = summary
        .iter()
        .filter(|s| s.provider == "claude_code")
        .collect();
    let max_subscription = if claude_code_rows.is_empty() {
        serde_json::Value::Null
    } else {
        let call_count: u32 = claude_code_rows.iter().map(|s| s.call_count).sum();
        let would_be_api_cost: f64 = claude_code_rows.iter().map(|s| s.total_cost).sum();
        let models: Vec<serde_json::Value> = claude_code_rows
            .iter()
            .map(|s| {
                json!({
                    "model": s.model,
                    "call_count": s.call_count,
                    "would_be_api_cost": s.total_cost,
                })
            })
            .collect();
        json!({
            "provider": "claude_code",
            "badge": "Max subscription",
            "call_count": call_count,
            "would_be_api_cost": would_be_api_cost,
            "models": models,
        })
    };

    let anomaly_filter = QueryFilter {
        since: parse_last_period("7d"),
        ..Default::default()
    };
    let anomaly_count = shore_ledger::query::query_anomalies(ledger, &anomaly_filter)
        .map(|r| r.len())
        .unwrap_or(0);
    let rate_limit_events: Vec<serde_json::Value> = {
        let diag = ctx.diagnostics.lock().unwrap_or_else(|e| e.into_inner());
        diag.api_calls
            .iter()
            .filter_map(|entry| {
                entry.rate_limit_info.as_ref().map(|info| {
                    json!({
                        "timestamp": entry.timestamp,
                        "provider": entry.provider,
                        "model": entry.model,
                        "rate_limit_info": info,
                    })
                })
            })
            .collect()
    };

    debug!(
        period = %last,
        models = summary_rows.len(),
        characters = cache_health.len(),
        anomaly_count_7d = anomaly_count,
        "Usage summary complete"
    );
    Ok(json!({
        "mode": "summary",
        "period": last,
        "summary": summary_rows,
        "cache_health": cache_health,
        "max_subscription": max_subscription,
        "rate_limit_events": rate_limit_events,
        "anomaly_count_7d": anomaly_count,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_now() -> chrono::DateTime<chrono::Utc> {
        chrono::NaiveDate::from_ymd_opt(2026, 5, 13)
            .unwrap()
            .and_hms_opt(12, 30, 0)
            .unwrap()
            .and_utc()
    }

    #[test]
    fn parse_last_period_accepts_hour_ranges() {
        assert_eq!(
            parse_last_period_at("4h", fixed_now()).as_deref(),
            Some("2026-05-13T08:30:00+00:00"),
        );
    }

    #[test]
    fn parse_last_period_keeps_day_ranges() {
        assert_eq!(
            parse_last_period_at("7d", fixed_now()).as_deref(),
            Some("2026-05-06T12:30:00+00:00"),
        );
    }

    #[test]
    fn parse_last_period_today_uses_utc_midnight() {
        assert_eq!(
            parse_last_period_at("today", fixed_now()).as_deref(),
            Some("2026-05-13T00:00:00+00:00"),
        );
    }
}
