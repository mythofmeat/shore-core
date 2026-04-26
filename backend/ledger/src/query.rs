//! Aggregation and filter queries for the CLI.

use crate::ledger::{row_from_sqlite, CallRow, Ledger};
use rusqlite::params_from_iter;

// ── Filter ───────────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct QueryFilter {
    pub since: Option<String>,
    pub until: Option<String>,
    pub character: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub call_type: Option<String>,
}

/// Collects WHERE clause fragments and their bound values from a `QueryFilter`.
fn build_where(filter: &QueryFilter) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(ref v) = filter.since {
        clauses.push(format!("ts >= ?{}", values.len() + 1));
        values.push(Box::new(v.clone()));
    }
    if let Some(ref v) = filter.until {
        clauses.push(format!("ts <= ?{}", values.len() + 1));
        values.push(Box::new(v.clone()));
    }
    if let Some(ref v) = filter.character {
        clauses.push(format!("character = ?{}", values.len() + 1));
        values.push(Box::new(v.clone()));
    }
    if let Some(ref v) = filter.provider {
        clauses.push(format!("provider = ?{}", values.len() + 1));
        values.push(Box::new(v.clone()));
    }
    if let Some(ref v) = filter.model {
        clauses.push(format!("model = ?{}", values.len() + 1));
        values.push(Box::new(v.clone()));
    }
    if let Some(ref v) = filter.call_type {
        clauses.push(format!("call_type = ?{}", values.len() + 1));
        values.push(Box::new(v.clone()));
    }

    let sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    (sql, values)
}

// ── Summary ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct UsageSummary {
    pub provider: String,
    pub model: String,
    pub call_count: u32,
    pub total_input: u64,
    pub total_output: u64,
    pub total_cache_read: u64,
    pub total_cache_write: u64,
    pub total_cost: f64,
}

/// Groups calls by provider + model, sums token counts and cost.
/// Orders by total_cost DESC (nulls last).
pub fn usage_summary(
    ledger: &Ledger,
    filter: &QueryFilter,
) -> Result<Vec<UsageSummary>, rusqlite::Error> {
    let (where_clause, values) = build_where(filter);
    let sql = format!(
        r#"SELECT provider, model,
                  COUNT(*) as call_count,
                  SUM(input_tokens) as total_input,
                  SUM(output_tokens) as total_output,
                  SUM(cache_read_tokens) as total_cache_read,
                  SUM(cache_write_tokens) as total_cache_write,
                  TOTAL(total_cost) as total_cost
             FROM calls
             {where_clause}
            GROUP BY provider, model
            ORDER BY total_cost DESC"#,
    );

    ledger.with_conn(|conn| {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values.iter()), |row| {
            Ok(UsageSummary {
                provider: row.get(0)?,
                model: row.get(1)?,
                call_count: row.get::<_, i64>(2)? as u32,
                total_input: row.get::<_, i64>(3)? as u64,
                total_output: row.get::<_, i64>(4)? as u64,
                total_cache_read: row.get::<_, i64>(5)? as u64,
                total_cache_write: row.get::<_, i64>(6)? as u64,
                total_cost: row.get::<_, f64>(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
    })
}

// ── Summary by call type ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CallTypeSummary {
    pub call_type: String,
    pub call_count: u32,
    pub total_input: u64,
    pub total_output: u64,
    pub total_cache_read: u64,
    pub total_cache_write: u64,
    pub total_cost: f64,
}

/// Groups calls by `call_type`. Ordered by total_cost DESC, then call_count DESC.
pub fn usage_summary_by_call_type(
    ledger: &Ledger,
    filter: &QueryFilter,
) -> Result<Vec<CallTypeSummary>, rusqlite::Error> {
    let (where_clause, values) = build_where(filter);
    let sql = format!(
        r#"SELECT call_type,
                  COUNT(*) as call_count,
                  SUM(input_tokens) as total_input,
                  SUM(output_tokens) as total_output,
                  SUM(cache_read_tokens) as total_cache_read,
                  SUM(cache_write_tokens) as total_cache_write,
                  TOTAL(total_cost) as total_cost
             FROM calls
             {where_clause}
            GROUP BY call_type
            ORDER BY total_cost DESC, call_count DESC"#,
    );

    ledger.with_conn(|conn| {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values.iter()), |row| {
            Ok(CallTypeSummary {
                call_type: row.get(0)?,
                call_count: row.get::<_, i64>(1)? as u32,
                total_input: row.get::<_, i64>(2)? as u64,
                total_output: row.get::<_, i64>(3)? as u64,
                total_cache_read: row.get::<_, i64>(4)? as u64,
                total_cache_write: row.get::<_, i64>(5)? as u64,
                total_cost: row.get::<_, f64>(6)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
    })
}

// ── Anomalies ────────────────────────────────────────────────────────────────

/// Returns rows where `cache_anomaly IS NOT NULL`, ordered by id DESC.
pub fn query_anomalies(
    ledger: &Ledger,
    filter: &QueryFilter,
) -> Result<Vec<CallRow>, rusqlite::Error> {
    let (where_clause, values) = build_where(filter);

    // If filter already produced a WHERE, append with AND; otherwise start one.
    let anomaly_clause = if where_clause.is_empty() {
        " WHERE cache_anomaly IS NOT NULL".to_string()
    } else {
        format!("{where_clause} AND cache_anomaly IS NOT NULL")
    };

    let sql = format!("SELECT * FROM calls{anomaly_clause} ORDER BY id DESC");

    ledger.with_conn(|conn| {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values.iter()), row_from_sqlite)?;
        rows.collect::<Result<Vec<_>, _>>()
    })
}

// ── TSV export ───────────────────────────────────────────────────────────────

const TSV_HEADER: &str = "ts\tcharacter\tprovider\tmodel\tcall_type\t\
    input_tokens\toutput_tokens\tcache_read_tokens\tcache_write_tokens\tcache_ttl\t\
    total_ms\tttft_ms\tfinish_reason\tthinking_enabled\t\
    cache_state\tcache_anomaly\t\
    input_cost\toutput_cost\tcache_read_cost\tcache_write_cost\ttotal_cost";

fn opt_str(v: &Option<String>) -> &str {
    v.as_deref().unwrap_or("")
}

fn opt_f64(v: Option<f64>) -> String {
    match v {
        Some(f) => f.to_string(),
        None => String::new(),
    }
}

fn row_to_tsv(r: &CallRow) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        r.ts,
        r.character,
        r.provider,
        r.model,
        r.call_type,
        r.input_tokens,
        r.output_tokens,
        r.cache_read_tokens,
        r.cache_write_tokens,
        opt_str(&r.cache_ttl),
        r.total_ms,
        r.ttft_ms,
        r.finish_reason,
        r.thinking_enabled,
        opt_str(&r.cache_state),
        opt_str(&r.cache_anomaly),
        opt_f64(r.input_cost),
        opt_f64(r.output_cost),
        opt_f64(r.cache_read_cost),
        opt_f64(r.cache_write_cost),
        opt_f64(r.total_cost),
    )
}

/// Tab-separated header + all matching rows (all 21 CallRow columns).
pub fn export_tsv(ledger: &Ledger, filter: &QueryFilter) -> Result<String, rusqlite::Error> {
    let (where_clause, values) = build_where(filter);
    let sql = format!("SELECT * FROM calls{where_clause} ORDER BY id ASC");

    ledger.with_conn(|conn| {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(values.iter()), row_from_sqlite)?
            .collect::<Result<Vec<_>, _>>()?;

        let mut out = String::from(TSV_HEADER);
        for r in &rows {
            out.push('\n');
            out.push_str(&row_to_tsv(r));
        }
        Ok(out)
    })
}

// ── Active Anthropic characters ──────────────────────────────────────────────

/// Returns distinct characters with recent Anthropic calls and their last call row.
/// Used for cache health display.
pub fn active_anthropic_characters(
    ledger: &Ledger,
    filter: &QueryFilter,
) -> Result<Vec<(String, CallRow)>, rusqlite::Error> {
    let (where_clause, values) = build_where(filter);

    // Add anthropic provider condition.
    let provider_cond = if where_clause.is_empty() {
        " WHERE provider = 'anthropic'".to_string()
    } else {
        format!("{where_clause} AND provider = 'anthropic'")
    };

    // Subquery: for each character, find the max id among matching Anthropic rows.
    let sql = format!(
        r#"SELECT c.* FROM calls c
           INNER JOIN (
               SELECT character, MAX(id) as max_id
               FROM calls
               {provider_cond}
               GROUP BY character
           ) latest ON c.id = latest.max_id
           ORDER BY c.id DESC"#,
    );

    ledger.with_conn(|conn| {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values.iter()), |row| {
            let call = row_from_sqlite(row)?;
            let character = call.character.clone();
            Ok((character, call))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
    })
}

// ── Warm streak ──────────────────────────────────────────────────────────────

/// Counts consecutive warm calls from most recent backwards for `character`.
/// Bounded to prevent loading unbounded rows for high-volume characters.
pub fn warm_streak(ledger: &Ledger, character: &str) -> Result<u32, rusqlite::Error> {
    ledger.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT cache_state FROM calls WHERE character = ?1 ORDER BY id DESC LIMIT 10000",
        )?;
        let mut rows = stmt.query_map([character], |row| row.get::<_, Option<String>>(0))?;

        let mut count = 0u32;
        while let Some(Ok(state)) = rows.next() {
            if state.as_deref() == Some("warm") {
                count += 1;
            } else {
                break;
            }
        }
        Ok(count)
    })
}

// ── Recalculate ─────────────────────────────────────────────────────────────

/// Row with NULL costs that needs recalculation.
pub struct CostRow {
    pub id: i64,
    pub provider: String,
    pub model: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub cache_ttl: Option<String>,
}

/// Find all rows with NULL total_cost.
pub fn null_cost_rows(ledger: &Ledger) -> Result<Vec<CostRow>, rusqlite::Error> {
    ledger.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, provider, model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cache_ttl FROM calls WHERE total_cost IS NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(CostRow {
                id: row.get(0)?,
                provider: row.get(1)?,
                model: row.get(2)?,
                input_tokens: row.get(3)?,
                output_tokens: row.get(4)?,
                cache_read_tokens: row.get(5)?,
                cache_write_tokens: row.get(6)?,
                cache_ttl: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
    })
}

/// Find all rows (for force recalculation).
pub fn all_cost_rows(ledger: &Ledger) -> Result<Vec<CostRow>, rusqlite::Error> {
    ledger.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, provider, model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cache_ttl FROM calls",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(CostRow {
                id: row.get(0)?,
                provider: row.get(1)?,
                model: row.get(2)?,
                input_tokens: row.get(3)?,
                output_tokens: row.get(4)?,
                cache_read_tokens: row.get(5)?,
                cache_write_tokens: row.get(6)?,
                cache_ttl: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
    })
}

/// Update costs for a single row by id.
pub fn update_costs(
    ledger: &Ledger,
    id: i64,
    cost: &crate::pricing::CostBreakdown,
) -> Result<(), rusqlite::Error> {
    ledger.with_conn(|conn| {
        conn.execute(
            "UPDATE calls SET input_cost=?1, output_cost=?2, cache_read_cost=?3, cache_write_cost=?4, total_cost=?5 WHERE id=?6",
            rusqlite::params![cost.input, cost.output, cost.cache_read, cost.cache_write, cost.total, id],
        )?;
        Ok(())
    })
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{CallRow, Ledger};

    fn populated_ledger() -> Ledger {
        let ledger = Ledger::open_in_memory().unwrap();
        let base = CallRow {
            ts: "2026-04-05T10:00:00Z".into(),
            character: "aria".into(),
            provider: "anthropic".into(),
            model: "claude-opus-4-6".into(),
            call_type: "message".into(),
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 80,
            cache_write_tokens: 20,
            cache_ttl: None,
            total_ms: 1500,
            ttft_ms: 200,
            finish_reason: "end_turn".into(),
            thinking_enabled: true,
            cache_state: Some("warm".into()),
            cache_anomaly: None,
            input_cost: Some(0.0015),
            output_cost: Some(0.00375),
            cache_read_cost: Some(0.00012),
            cache_write_cost: Some(0.000375),
            total_cost: Some(0.005745),
        };
        ledger.insert(&base).unwrap();

        let mut row2 = base.clone();
        row2.ts = "2026-04-05T10:01:00Z".into();
        row2.call_type = "tool_loop".into();
        row2.input_tokens = 200;
        row2.total_cost = Some(0.01);
        ledger.insert(&row2).unwrap();

        let mut row3 = base.clone();
        row3.ts = "2026-04-05T10:02:00Z".into();
        row3.provider = "openai".into();
        row3.model = "gpt-4o".into();
        row3.cache_read_tokens = 0;
        row3.cache_write_tokens = 0;
        row3.cache_state = None;
        row3.total_cost = Some(0.002);
        ledger.insert(&row3).unwrap();

        ledger
    }

    #[test]
    fn summary_groups_by_call_type() {
        let ledger = populated_ledger();
        let summary = usage_summary_by_call_type(&ledger, &QueryFilter::default()).unwrap();
        // Populated ledger has two call types: "message" (2 rows) and
        // "tool_loop" (1 row).
        assert_eq!(summary.len(), 2);
        let by_type: std::collections::HashMap<_, _> = summary
            .iter()
            .map(|s| (s.call_type.clone(), s.call_count))
            .collect();
        assert_eq!(by_type.get("message"), Some(&2));
        assert_eq!(by_type.get("tool_loop"), Some(&1));
    }

    #[test]
    fn summary_groups_by_provider_model() {
        let ledger = populated_ledger();
        let summary = usage_summary(&ledger, &QueryFilter::default()).unwrap();
        assert_eq!(summary.len(), 2); // anthropic/opus + openai/gpt-4o
        let anthropic = summary.iter().find(|s| s.provider == "anthropic").unwrap();
        assert_eq!(anthropic.call_count, 2);
    }

    #[test]
    fn filter_by_provider() {
        let ledger = populated_ledger();
        let filter = QueryFilter {
            provider: Some("anthropic".into()),
            ..Default::default()
        };
        let summary = usage_summary(&ledger, &filter).unwrap();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].call_count, 2);
    }

    #[test]
    fn anomalies_query() {
        let ledger = Ledger::open_in_memory().unwrap();
        let mut row = CallRow {
            ts: "2026-04-05T10:00:00Z".into(),
            character: "aria".into(),
            provider: "anthropic".into(),
            model: "claude-opus-4-6".into(),
            call_type: "message".into(),
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            cache_ttl: None,
            total_ms: 1500,
            ttft_ms: 200,
            finish_reason: "end_turn".into(),
            thinking_enabled: true,
            cache_state: Some("cold".into()),
            cache_anomaly: Some("unexpected_read".into()),
            input_cost: None,
            output_cost: None,
            cache_read_cost: None,
            cache_write_cost: None,
            total_cost: None,
        };
        ledger.insert(&row).unwrap();
        row.cache_anomaly = None;
        ledger.insert(&row).unwrap();
        let anomalies = query_anomalies(&ledger, &QueryFilter::default()).unwrap();
        assert_eq!(anomalies.len(), 1);
    }

    #[test]
    fn export_tsv_format() {
        let ledger = populated_ledger();
        let tsv = export_tsv(&ledger, &QueryFilter::default()).unwrap();
        let lines: Vec<&str> = tsv.lines().collect();
        assert!(lines[0].contains("ts\t")); // header
        assert_eq!(lines.len(), 4); // header + 3 rows
    }

    #[test]
    fn warm_streak_counts_consecutive() {
        let ledger = Ledger::open_in_memory().unwrap();
        let base = CallRow {
            ts: "2026-04-05T10:00:00Z".into(),
            character: "aria".into(),
            provider: "anthropic".into(),
            model: "claude-opus-4-6".into(),
            call_type: "message".into(),
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 80,
            cache_write_tokens: 20,
            cache_ttl: None,
            total_ms: 1000,
            ttft_ms: 100,
            finish_reason: "end_turn".into(),
            thinking_enabled: true,
            cache_state: Some("cold".into()),
            cache_anomaly: None,
            input_cost: None,
            output_cost: None,
            cache_read_cost: None,
            cache_write_cost: None,
            total_cost: None,
        };
        ledger.insert(&base).unwrap();
        let mut warm = base.clone();
        warm.cache_state = Some("warm".into());
        warm.ts = "2026-04-05T10:01:00Z".into();
        ledger.insert(&warm).unwrap();
        warm.ts = "2026-04-05T10:02:00Z".into();
        ledger.insert(&warm).unwrap();
        warm.ts = "2026-04-05T10:03:00Z".into();
        ledger.insert(&warm).unwrap();
        assert_eq!(warm_streak(&ledger, "aria").unwrap(), 3);
    }
}
