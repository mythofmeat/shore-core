//! Usage budget evaluation over the append-only ledger.

use chrono::{
    DateTime, Datelike, Duration, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Timelike,
    Utc,
};
use serde::Serialize;
use serde_json::json;
use shore_config::app::{UsageBudgetAction, UsageBudgetConfig, UsageBudgetPeriod, UsageConfig};
use tracing::warn;

use crate::client::CallType;
use crate::ledger::Ledger;
use crate::query::{usage_totals, QueryFilter};

#[derive(Debug, Clone)]
struct PeriodWindow {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    timezone: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BudgetStatus {
    pub name: String,
    pub period: UsageBudgetPeriod,
    pub period_start: String,
    pub period_end: String,
    pub reset_at: String,
    pub timezone: String,
    pub current_cost: f64,
    pub cost_limit: f64,
    pub percent_used: f64,
    pub status: String,
    pub action: UsageBudgetAction,
    pub warning_thresholds: Vec<f64>,
    pub crossed_warn_at: Vec<f64>,
    pub over_limit: bool,
    pub compaction_allowed_over_budget: bool,
    pub filters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpikeWarning {
    pub period: UsageBudgetPeriod,
    pub period_start: String,
    pub previous_period_start: String,
    pub timezone: String,
    pub current_cost: f64,
    pub previous_cost: f64,
    pub multiplier: Option<f64>,
    pub threshold_multiplier: f64,
    pub min_cost_usd: f64,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct BudgetCallContext<'a> {
    pub provider: &'a str,
    pub api_key_name: Option<&'a str>,
    pub model: &'a str,
    pub call_type: CallType,
    pub character: &'a str,
}

#[derive(Debug, Clone)]
pub struct BudgetBlock {
    pub budget_name: String,
    pub action: UsageBudgetAction,
    pub current_cost: f64,
    pub cost_limit: f64,
    pub period: UsageBudgetPeriod,
    pub reset_at: String,
}

impl std::fmt::Display for BudgetBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Shore usage budget \"{}\" is over limit (${:.2}/${:.2} for {:?}); action {:?}; resets at {}",
            self.budget_name,
            self.current_cost,
            self.cost_limit,
            self.period,
            self.action,
            self.reset_at
        )
    }
}

impl std::error::Error for BudgetBlock {}

pub fn budget_statuses(
    ledger: &Ledger,
    config: &UsageConfig,
    now: DateTime<Utc>,
) -> Result<Vec<BudgetStatus>, rusqlite::Error> {
    config
        .budgets
        .iter()
        .enumerate()
        .map(|(idx, budget)| budget_status(ledger, config, budget, idx, now))
        .collect()
}

pub fn enforce_budget_for_call(
    ledger: &Ledger,
    config: &UsageConfig,
    call: BudgetCallContext<'_>,
    now: DateTime<Utc>,
) -> Result<(), BudgetBlock> {
    if config.budgets.is_empty() {
        return Ok(());
    }

    for (idx, budget) in config.budgets.iter().enumerate() {
        if !budget_matches_call(budget, &call) {
            continue;
        }
        let status = match budget_status(ledger, config, budget, idx, now) {
            Ok(status) => status,
            Err(e) => {
                warn!(
                    budget = %budget_name(budget, idx),
                    error = %e,
                    "Usage budget query failed; allowing call"
                );
                continue;
            }
        };
        if status.over_limit && should_block(config, budget, call.call_type) {
            return Err(BudgetBlock {
                budget_name: status.name,
                action: status.action,
                current_cost: status.current_cost,
                cost_limit: status.cost_limit,
                period: status.period,
                reset_at: status.reset_at,
            });
        }
    }

    Ok(())
}

pub fn spike_warnings(
    ledger: &Ledger,
    config: &UsageConfig,
    now: DateTime<Utc>,
) -> Result<Vec<SpikeWarning>, rusqlite::Error> {
    let spike = &config.spike_warnings;
    if !spike.enabled {
        return Ok(Vec::new());
    }

    let current = period_window(now, spike.period, &config.timezone);
    let previous = period_window(
        current.start - Duration::seconds(1),
        spike.period,
        &config.timezone,
    );

    let current_cost = usage_totals(
        ledger,
        &QueryFilter {
            since: Some(current.start.to_rfc3339()),
            ..Default::default()
        },
    )?
    .total_cost;
    let previous_cost = usage_totals(
        ledger,
        &QueryFilter {
            since: Some(previous.start.to_rfc3339()),
            until: Some(current.start.to_rfc3339()),
            ..Default::default()
        },
    )?
    .total_cost;

    if current_cost < spike.min_cost_usd {
        return Ok(Vec::new());
    }

    let multiplier = if previous_cost > 0.0 {
        Some(current_cost / previous_cost)
    } else {
        None
    };
    let is_spike = multiplier
        .map(|m| m >= spike.multiplier)
        .unwrap_or(previous_cost == 0.0);
    if !is_spike {
        return Ok(Vec::new());
    }

    let message = match multiplier {
        Some(m) => format!(
            "Current {:?} spend is {:.1}x the previous {:?} (${:.2} vs ${:.2}).",
            spike.period, m, spike.period, current_cost, previous_cost
        ),
        None => format!(
            "Current {:?} spend is ${:.2}; the previous {:?} had no recorded cost.",
            spike.period, current_cost, spike.period
        ),
    };

    Ok(vec![SpikeWarning {
        period: spike.period,
        period_start: current.start.to_rfc3339(),
        previous_period_start: previous.start.to_rfc3339(),
        timezone: current.timezone,
        current_cost,
        previous_cost,
        multiplier,
        threshold_multiplier: spike.multiplier,
        min_cost_usd: spike.min_cost_usd,
        message,
    }])
}

fn budget_status(
    ledger: &Ledger,
    config: &UsageConfig,
    budget: &UsageBudgetConfig,
    idx: usize,
    now: DateTime<Utc>,
) -> Result<BudgetStatus, rusqlite::Error> {
    let window = period_window(now, budget.period, &config.timezone);
    let totals = usage_totals(ledger, &filter_for_budget(budget, &window))?;
    let current_cost = totals.total_cost;
    let percent_used = current_cost / budget.cost_usd;
    let mut warning_thresholds = budget.warn_at.clone();
    warning_thresholds.sort_by(|a, b| a.total_cmp(b));
    warning_thresholds.dedup_by(|a, b| (*a - *b).abs() < f64::EPSILON);
    let crossed_warn_at: Vec<f64> = warning_thresholds
        .iter()
        .copied()
        .filter(|threshold| percent_used >= *threshold)
        .collect();
    let over_limit = current_cost >= budget.cost_usd;
    let status = if over_limit {
        "over_limit"
    } else if crossed_warn_at.is_empty() {
        "ok"
    } else {
        "warning"
    };

    Ok(BudgetStatus {
        name: budget_name(budget, idx),
        period: budget.period,
        period_start: window.start.to_rfc3339(),
        period_end: window.end.to_rfc3339(),
        reset_at: window.end.to_rfc3339(),
        timezone: window.timezone,
        current_cost,
        cost_limit: budget.cost_usd,
        percent_used,
        status: status.to_string(),
        action: budget.limit,
        warning_thresholds,
        crossed_warn_at,
        over_limit,
        compaction_allowed_over_budget: compaction_allowed(config, budget),
        filters: budget_filters_json(budget),
    })
}

fn filter_for_budget(budget: &UsageBudgetConfig, window: &PeriodWindow) -> QueryFilter {
    QueryFilter {
        since: Some(window.start.to_rfc3339()),
        character: budget.character.clone(),
        provider: budget.provider.clone(),
        api_key_name: budget.api_key.clone(),
        model: budget.model.clone(),
        call_type: budget.call_type.clone(),
        usage_kinds: budget.usage_kind.clone(),
        ..Default::default()
    }
}

fn budget_name(budget: &UsageBudgetConfig, idx: usize) -> String {
    let name = budget.name.trim();
    if name.is_empty() {
        format!("budget {}", idx + 1)
    } else {
        name.to_string()
    }
}

fn budget_filters_json(budget: &UsageBudgetConfig) -> serde_json::Value {
    json!({
        "character": budget.character,
        "provider": budget.provider,
        "api_key": budget.api_key,
        "model": budget.model,
        "call_type": budget.call_type,
        "usage_kind": budget.usage_kind,
    })
}

fn budget_matches_call(budget: &UsageBudgetConfig, call: &BudgetCallContext<'_>) -> bool {
    if budget
        .character
        .as_deref()
        .is_some_and(|v| v != call.character)
    {
        return false;
    }
    if budget
        .provider
        .as_deref()
        .is_some_and(|v| v != call.provider)
    {
        return false;
    }
    if budget.model.as_deref().is_some_and(|v| v != call.model) {
        return false;
    }
    if budget
        .call_type
        .as_deref()
        .is_some_and(|v| v != call.call_type.as_str())
    {
        return false;
    }
    if let Some(api_key) = budget.api_key.as_deref() {
        let actual = call.api_key_name.unwrap_or("unknown");
        if api_key != actual {
            return false;
        }
    }
    if !budget.usage_kind.is_empty()
        && !budget
            .usage_kind
            .iter()
            .any(|kind| call_type_matches_usage_kind(call.call_type, kind))
    {
        return false;
    }

    true
}

fn call_type_matches_usage_kind(call_type: CallType, usage_kind: &str) -> bool {
    match call_type {
        CallType::Message => {
            usage_kind == "message"
                || usage_kind == "message_no_tools"
                || usage_kind == "message_with_tools"
        }
        CallType::ToolLoop => usage_kind == "message_with_tools" || usage_kind == "tool_loop",
        CallType::HeartbeatToolLoop | CallType::Heartbeat => usage_kind == "heartbeat",
        CallType::Keepalive => usage_kind == "keepalive",
        CallType::Compaction => usage_kind == "compaction",
        CallType::Dreaming => usage_kind == "dreaming",
        CallType::MemoryQuery => usage_kind == "memory_query",
    }
}

fn should_block(config: &UsageConfig, budget: &UsageBudgetConfig, call_type: CallType) -> bool {
    if matches!(call_type, CallType::Compaction) && compaction_allowed(config, budget) {
        return false;
    }

    match budget.limit {
        UsageBudgetAction::Warn => false,
        UsageBudgetAction::Block => true,
        UsageBudgetAction::PauseBackground => is_background_call(call_type),
    }
}

fn compaction_allowed(config: &UsageConfig, budget: &UsageBudgetConfig) -> bool {
    budget
        .allow_compaction_over_budget
        .unwrap_or(config.allow_compaction_over_budget)
}

fn is_background_call(call_type: CallType) -> bool {
    matches!(
        call_type,
        CallType::Heartbeat
            | CallType::HeartbeatToolLoop
            | CallType::Keepalive
            | CallType::Compaction
            | CallType::Dreaming
            | CallType::MemoryQuery
    )
}

fn period_window(now: DateTime<Utc>, period: UsageBudgetPeriod, timezone: &str) -> PeriodWindow {
    match timezone {
        "utc" => period_window_utc(now, period),
        _ => period_window_local(now, period),
    }
}

fn period_window_utc(now: DateTime<Utc>, period: UsageBudgetPeriod) -> PeriodWindow {
    let date = now.date_naive();
    let start_naive = match period {
        UsageBudgetPeriod::Hour => date
            .and_hms_opt(now.hour(), 0, 0)
            .expect("valid UTC hour boundary"),
        UsageBudgetPeriod::Day => date.and_hms_opt(0, 0, 0).expect("valid UTC day boundary"),
        UsageBudgetPeriod::Week => {
            let monday = date - Duration::days(date.weekday().num_days_from_monday() as i64);
            monday
                .and_hms_opt(0, 0, 0)
                .expect("valid UTC week boundary")
        }
        UsageBudgetPeriod::Month => NaiveDate::from_ymd_opt(date.year(), date.month(), 1)
            .expect("valid UTC month boundary")
            .and_hms_opt(0, 0, 0)
            .expect("valid UTC month boundary"),
    };
    let end_naive = period_end_naive(start_naive, period);
    PeriodWindow {
        start: Utc.from_utc_datetime(&start_naive),
        end: Utc.from_utc_datetime(&end_naive),
        timezone: "utc".into(),
    }
}

fn period_window_local(now: DateTime<Utc>, period: UsageBudgetPeriod) -> PeriodWindow {
    let local_now = now.with_timezone(&Local);
    let date = local_now.date_naive();
    let start_naive = match period {
        UsageBudgetPeriod::Hour => date
            .and_hms_opt(local_now.hour(), 0, 0)
            .expect("valid local hour boundary"),
        UsageBudgetPeriod::Day => date.and_hms_opt(0, 0, 0).expect("valid local day boundary"),
        UsageBudgetPeriod::Week => {
            let monday = date - Duration::days(date.weekday().num_days_from_monday() as i64);
            monday
                .and_hms_opt(0, 0, 0)
                .expect("valid local week boundary")
        }
        UsageBudgetPeriod::Month => NaiveDate::from_ymd_opt(date.year(), date.month(), 1)
            .expect("valid local month boundary")
            .and_hms_opt(0, 0, 0)
            .expect("valid local month boundary"),
    };
    let end_naive = period_end_naive(start_naive, period);
    PeriodWindow {
        start: resolve_local(start_naive).with_timezone(&Utc),
        end: resolve_local(end_naive).with_timezone(&Utc),
        timezone: "local".into(),
    }
}

fn period_end_naive(start: NaiveDateTime, period: UsageBudgetPeriod) -> NaiveDateTime {
    match period {
        UsageBudgetPeriod::Hour => start + Duration::hours(1),
        UsageBudgetPeriod::Day => start + Duration::days(1),
        UsageBudgetPeriod::Week => start + Duration::days(7),
        UsageBudgetPeriod::Month => {
            let date = start.date();
            let (year, month) = if date.month() == 12 {
                (date.year() + 1, 1)
            } else {
                (date.year(), date.month() + 1)
            };
            NaiveDate::from_ymd_opt(year, month, 1)
                .expect("valid next month boundary")
                .and_hms_opt(0, 0, 0)
                .expect("valid next month boundary")
        }
    }
}

fn resolve_local(naive: NaiveDateTime) -> DateTime<Local> {
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(dt) => dt,
        LocalResult::Ambiguous(early, _) => early,
        LocalResult::None => Local
            .from_local_datetime(&(naive + Duration::hours(1)))
            .earliest()
            .unwrap_or_else(|| Utc.from_utc_datetime(&naive).with_timezone(&Local)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{CallRow, Ledger};
    use shore_llm::types::Timing;

    fn insert_call(ledger: &Ledger, ts: &str, cost: f64, call_type: &str) {
        ledger
            .insert(&CallRow {
                ts: ts.into(),
                character: "Alice".into(),
                provider: "openrouter".into(),
                api_key_name: Some("default".into()),
                model: "model".into(),
                call_type: call_type.into(),
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cache_ttl: None,
                total_ms: Timing::default().total_ms,
                ttft_ms: Timing::default().time_to_first_token_ms,
                finish_reason: "end_turn".into(),
                thinking_enabled: false,
                cache_state: None,
                cache_anomaly: None,
                input_cost: None,
                output_cost: None,
                cache_read_cost: None,
                cache_write_cost: None,
                cost_source: Some("provider_reported".into()),
                total_cost: Some(cost),
            })
            .unwrap();
    }

    #[test]
    fn utc_day_budget_sums_matching_rows() {
        let ledger = Ledger::open_in_memory().unwrap();
        insert_call(&ledger, "2026-05-18T03:00:00+00:00", 4.0, "message");
        insert_call(&ledger, "2026-05-17T23:00:00+00:00", 8.0, "message");
        let config = UsageConfig {
            timezone: "utc".into(),
            budgets: vec![UsageBudgetConfig {
                name: "daily".into(),
                cost_usd: 10.0,
                ..usage_budget()
            }],
            ..UsageConfig::default()
        };

        let statuses = budget_statuses(
            &ledger,
            &config,
            "2026-05-18T12:00:00+00:00".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(statuses[0].current_cost, 4.0);
        assert_eq!(statuses[0].status, "ok");
    }

    #[test]
    fn block_budget_blocks_matching_call() {
        let ledger = Ledger::open_in_memory().unwrap();
        insert_call(&ledger, "2026-05-18T03:00:00+00:00", 11.0, "message");
        let config = UsageConfig {
            timezone: "utc".into(),
            budgets: vec![UsageBudgetConfig {
                name: "daily".into(),
                cost_usd: 10.0,
                limit: UsageBudgetAction::Block,
                ..usage_budget()
            }],
            ..UsageConfig::default()
        };

        let result = enforce_budget_for_call(
            &ledger,
            &config,
            BudgetCallContext {
                provider: "openrouter",
                api_key_name: Some("default"),
                model: "model",
                call_type: CallType::Message,
                character: "Alice",
            },
            "2026-05-18T12:00:00+00:00".parse().unwrap(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn compaction_can_bypass_block_budget() {
        let ledger = Ledger::open_in_memory().unwrap();
        insert_call(&ledger, "2026-05-18T03:00:00+00:00", 11.0, "message");
        let config = UsageConfig {
            timezone: "utc".into(),
            allow_compaction_over_budget: true,
            budgets: vec![UsageBudgetConfig {
                name: "daily".into(),
                cost_usd: 10.0,
                limit: UsageBudgetAction::Block,
                ..usage_budget()
            }],
            ..UsageConfig::default()
        };

        let result = enforce_budget_for_call(
            &ledger,
            &config,
            BudgetCallContext {
                provider: "openrouter",
                api_key_name: Some("default"),
                model: "model",
                call_type: CallType::Compaction,
                character: "Alice",
            },
            "2026-05-18T12:00:00+00:00".parse().unwrap(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn budget_can_filter_by_usage_kind() {
        let ledger = Ledger::open_in_memory().unwrap();
        insert_call(&ledger, "2026-05-18T03:00:00+00:00", 3.0, "heartbeat");
        insert_call(&ledger, "2026-05-18T04:00:00+00:00", 9.0, "message");
        let config = UsageConfig {
            timezone: "utc".into(),
            budgets: vec![UsageBudgetConfig {
                name: "heartbeat".into(),
                cost_usd: 10.0,
                usage_kind: vec!["heartbeat".into()],
                ..usage_budget()
            }],
            ..UsageConfig::default()
        };

        let statuses = budget_statuses(
            &ledger,
            &config,
            "2026-05-18T12:00:00+00:00".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(statuses[0].current_cost, 3.0);
    }

    fn usage_budget() -> UsageBudgetConfig {
        UsageBudgetConfig {
            name: String::new(),
            period: UsageBudgetPeriod::Day,
            cost_usd: 1.0,
            warn_at: vec![0.8, 1.0],
            limit: UsageBudgetAction::Warn,
            character: None,
            provider: None,
            api_key: None,
            model: None,
            call_type: None,
            usage_kind: Vec::new(),
            allow_compaction_over_budget: None,
        }
    }
}
