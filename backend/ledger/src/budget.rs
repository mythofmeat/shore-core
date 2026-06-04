//! Usage budget evaluation over the append-only ledger.

use chrono::{
    DateTime, Datelike, Duration, Local, LocalResult, NaiveDate, NaiveDateTime, NaiveTime,
    TimeZone, Timelike, Utc,
};
use rusqlite::params;
use serde::Serialize;
use serde_json::json;
use shore_config::app::{
    BudgetWeekday, UsageBudgetAction, UsageBudgetConfig, UsageBudgetPeriod, UsageConfig,
};
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

#[derive(Debug, Clone, Serialize)]
pub struct UsageBudgetWarningEvent {
    pub budget: String,
    pub message: String,
    pub current_cost: f64,
    pub cost_limit: f64,
    pub percent_used: f64,
    pub crossed_warn_at: Vec<f64>,
    pub period: UsageBudgetPeriod,
    pub period_start: String,
    pub reset_at: String,
    /// `reset_at` rendered in the daemon's local time as `YYYY-MM-DD HH:MM AM|PM`
    /// for clients that surface this string verbatim. The structured `reset_at`
    /// field stays RFC 3339 UTC for machine consumers.
    pub reset_at_display: String,
}

#[derive(Debug, Clone, Copy)]
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

    let current = period_window(now, spike.period, &config.timezone, None);
    let previous_anchor = current
        .start
        .checked_sub_signed(Duration::seconds(1))
        .unwrap_or(current.start);
    let previous = period_window(previous_anchor, spike.period, &config.timezone, None);

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
    let is_spike = multiplier.map_or(previous_cost == 0.0, |m| m >= spike.multiplier);
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

/// Return newly crossed budget warning thresholds, recording each
/// budget/window/threshold so future checks don't repeat the same warning.
///
/// Once a budget is over its limit, the warning re-fires on every check
/// regardless of dedup — intermediate thresholds (50%, 80%) staying one-shot
/// is the right call for noise, but "still over budget" is an active signal
/// the operator needs to keep seeing as spend continues to accrue.
pub fn newly_crossed_budget_warnings(
    ledger: &Ledger,
    config: &UsageConfig,
    now: DateTime<Utc>,
) -> Result<Vec<UsageBudgetWarningEvent>, rusqlite::Error> {
    let statuses = budget_statuses(ledger, config, now)?;
    let mut events = Vec::new();

    for status in statuses {
        let mut newly_crossed = Vec::new();
        for threshold in &status.crossed_warn_at {
            if record_budget_warning_threshold(
                ledger,
                &status.name,
                &status.period_start,
                *threshold,
                now,
            )? {
                newly_crossed.push(*threshold);
            }
        }
        if newly_crossed.is_empty() && status.over_limit {
            newly_crossed.push(1.0);
        }
        if newly_crossed.is_empty() {
            continue;
        }

        let highest = newly_crossed.iter().copied().fold(0.0_f64, f64::max);
        let reset_display = format_local_ampm(&status.reset_at);
        events.push(UsageBudgetWarningEvent {
            budget: status.name.clone(),
            message: format!(
                "Usage budget \"{}\" reached {:.0}% (${:.2}/${:.2}); resets at {}.",
                status.name,
                highest * 100.0,
                status.current_cost,
                status.cost_limit,
                reset_display,
            ),
            current_cost: status.current_cost,
            cost_limit: status.cost_limit,
            percent_used: status.percent_used,
            crossed_warn_at: newly_crossed,
            period: status.period,
            period_start: status.period_start,
            reset_at: status.reset_at,
            reset_at_display: reset_display,
        });
    }

    Ok(events)
}

/// Render an RFC 3339 timestamp as `YYYY-MM-DD HH:MM AM|PM` in the daemon's
/// local time. Falls back to the raw input if it doesn't parse — better to
/// show something than nothing in a warning message.
fn format_local_ampm(rfc3339: &str) -> String {
    DateTime::parse_from_rfc3339(rfc3339).map_or_else(
        |_| rfc3339.to_owned(),
        |dt| {
            dt.with_timezone(&Local)
                .format("%Y-%m-%d %I:%M %p")
                .to_string()
        },
    )
}

fn record_budget_warning_threshold(
    ledger: &Ledger,
    budget_name: &str,
    period_start: &str,
    threshold: f64,
    now: DateTime<Utc>,
) -> Result<bool, rusqlite::Error> {
    let threshold_key = format!("{threshold:.6}");
    ledger.with_conn(|conn| {
        let changed = conn.execute(
            r"INSERT OR IGNORE INTO usage_budget_warnings
               (budget_name, period_start, threshold, created_at)
               VALUES (?1, ?2, ?3, ?4)",
            params![budget_name, period_start, threshold_key, now.to_rfc3339()],
        )?;
        Ok(changed > 0)
    })
}

fn budget_status(
    ledger: &Ledger,
    config: &UsageConfig,
    budget: &UsageBudgetConfig,
    idx: usize,
    now: DateTime<Utc>,
) -> Result<BudgetStatus, rusqlite::Error> {
    let anchors = BudgetAnchors::from_budget(budget);
    let window = period_window(now, budget.period, &config.timezone, Some(&anchors));
    let totals = usage_totals(ledger, &filter_for_budget(budget, &window))?;
    let current_cost = totals.total_cost;
    let percent_used = current_cost / budget.cost_usd;
    let mut warning_thresholds = budget.warn_at.clone();
    warning_thresholds.sort_by(f64::total_cmp);
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
        status: status.to_owned(),
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
        let display_index = idx.saturating_add(1);
        format!("budget {display_index}")
    } else {
        name.to_owned()
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

#[derive(Debug, Clone, Copy)]
struct BudgetAnchors {
    /// Hour-of-day (0-23) at which day/week/month windows reset.
    hour: u32,
    /// 0 = Monday .. 6 = Sunday. Used only for week windows.
    day_of_week: u32,
    /// Day-of-month (1-31). Clamped to the last day on short months.
    day_of_month: u32,
}

impl Default for BudgetAnchors {
    fn default() -> Self {
        Self {
            hour: 0,
            day_of_week: 0,
            day_of_month: 1,
        }
    }
}

impl BudgetAnchors {
    fn from_budget(budget: &UsageBudgetConfig) -> Self {
        Self {
            hour: budget.reset_hour.unwrap_or(0),
            day_of_week: budget
                .reset_day_of_week
                .map_or(0, BudgetWeekday::num_days_from_monday),
            day_of_month: budget.reset_day_of_month.unwrap_or(1),
        }
    }
}

fn period_window(
    now: DateTime<Utc>,
    period: UsageBudgetPeriod,
    timezone: &str,
    anchors: Option<&BudgetAnchors>,
) -> PeriodWindow {
    match timezone {
        "utc" => period_window_utc(now, period, anchors),
        _ => period_window_local(now, period, anchors),
    }
}

fn period_window_utc(
    now: DateTime<Utc>,
    period: UsageBudgetPeriod,
    anchors: Option<&BudgetAnchors>,
) -> PeriodWindow {
    let now_naive = now.naive_utc();
    let start_naive = period_start_naive(now_naive, period, anchors);
    let end_naive = period_end_naive(start_naive, period, anchors);
    PeriodWindow {
        start: Utc.from_utc_datetime(&start_naive),
        end: Utc.from_utc_datetime(&end_naive),
        timezone: "utc".into(),
    }
}

fn period_window_local(
    now: DateTime<Utc>,
    period: UsageBudgetPeriod,
    anchors: Option<&BudgetAnchors>,
) -> PeriodWindow {
    let local_now = now.with_timezone(&Local);
    let now_naive = local_now.naive_local();
    let start_naive = period_start_naive(now_naive, period, anchors);
    let end_naive = period_end_naive(start_naive, period, anchors);
    PeriodWindow {
        start: resolve_local(start_naive).with_timezone(&Utc),
        end: resolve_local(end_naive).with_timezone(&Utc),
        timezone: "local".into(),
    }
}

fn period_start_naive(
    now: NaiveDateTime,
    period: UsageBudgetPeriod,
    anchors: Option<&BudgetAnchors>,
) -> NaiveDateTime {
    let anchors = anchors.copied().unwrap_or_default();
    let date = now.date();
    match period {
        UsageBudgetPeriod::Hour => at_hour(date, now.hour()),
        UsageBudgetPeriod::Day => {
            let today_reset = at_hour(date, anchors.hour);
            if now >= today_reset {
                today_reset
            } else {
                let yesterday = date.pred_opt().unwrap_or(date);
                at_hour(yesterday, anchors.hour)
            }
        }
        UsageBudgetPeriod::Week => {
            let today_dow = date.weekday().num_days_from_monday();
            let days_back = today_dow
                .saturating_add(7)
                .saturating_sub(anchors.day_of_week)
                .checked_rem(7)
                .unwrap_or(0);
            let candidate_date = date
                .checked_sub_signed(Duration::days(i64::from(days_back)))
                .unwrap_or(date);
            let candidate = at_hour(candidate_date, anchors.hour);
            if now >= candidate {
                candidate
            } else {
                let previous_week = candidate_date
                    .checked_sub_signed(Duration::days(7))
                    .unwrap_or(candidate_date);
                at_hour(previous_week, anchors.hour)
            }
        }
        UsageBudgetPeriod::Month => {
            let this_month = month_anchor_naive(date.year(), date.month(), &anchors);
            if now >= this_month {
                this_month
            } else {
                let (prev_year, prev_month) = if date.month() == 1 {
                    (date.year().saturating_sub(1), 12)
                } else {
                    (date.year(), date.month().saturating_sub(1))
                };
                month_anchor_naive(prev_year, prev_month, &anchors)
            }
        }
    }
}

fn period_end_naive(
    start: NaiveDateTime,
    period: UsageBudgetPeriod,
    anchors: Option<&BudgetAnchors>,
) -> NaiveDateTime {
    let anchors = anchors.copied().unwrap_or_default();
    match period {
        UsageBudgetPeriod::Hour => start
            .checked_add_signed(Duration::hours(1))
            .unwrap_or(start),
        UsageBudgetPeriod::Day => start.checked_add_signed(Duration::days(1)).unwrap_or(start),
        UsageBudgetPeriod::Week => start.checked_add_signed(Duration::days(7)).unwrap_or(start),
        UsageBudgetPeriod::Month => {
            let (next_year, next_month) = if start.month() == 12 {
                (start.year().saturating_add(1), 1)
            } else {
                (start.year(), start.month().saturating_add(1))
            };
            month_anchor_naive(next_year, next_month, &anchors)
        }
    }
}

fn month_anchor_naive(year: i32, month: u32, anchors: &BudgetAnchors) -> NaiveDateTime {
    let max_day = days_in_month(year, month);
    let day = anchors.day_of_month.clamp(1, max_day);
    // `month` originates from a valid `NaiveDate` (1..=12) and `day` is clamped
    // into range, so construction succeeds; fall back to the first of the month
    // (always valid) rather than panic if a caller ever passes a bad month.
    let date = NaiveDate::from_ymd_opt(year, month, day)
        .or_else(|| NaiveDate::from_ymd_opt(year, month, 1))
        .unwrap_or_default();
    at_hour(date, anchors.hour)
}

/// Datetime at `hour:00:00` on `date`. `hour` is clamped into 0..=23 and
/// [`NaiveDate::and_time`] is total, so this never panics.
fn at_hour(date: NaiveDate, hour: u32) -> NaiveDateTime {
    let time = NaiveTime::from_hms_opt(hour.min(23), 0, 0).unwrap_or_default();
    date.and_time(time)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year.saturating_add(1), 1)
    } else {
        (year, month.saturating_add(1))
    };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .and_then(|d| d.pred_opt())
        .map_or(28, |d| d.day())
}

fn resolve_local(naive: NaiveDateTime) -> DateTime<Local> {
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(dt) => dt,
        LocalResult::Ambiguous(early, _) => early,
        LocalResult::None => Local
            .from_local_datetime(
                &naive
                    .checked_add_signed(Duration::hours(1))
                    .unwrap_or(naive),
            )
            .earliest()
            .unwrap_or_else(|| Utc.from_utc_datetime(&naive).with_timezone(&Local)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{CallRow, Ledger};
    use shore_llm::types::Timing;

    fn first_item<T>(items: &[T]) -> &T {
        items.first().expect("expected at least one item")
    }

    fn insert_call(ledger: &Ledger, ts: &str, cost: f64, call_type: &str) {
        let _ignored = ledger
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
        let status = first_item(&statuses);
        #[expect(clippy::float_cmp, reason = "sum of four $1.00 rows is exact in f64")]
        {
            assert_eq!(status.current_cost, 4.0);
        }
        assert_eq!(status.status, "ok");
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
        let status = first_item(&statuses);
        #[expect(clippy::float_cmp, reason = "sum of three $1.00 rows is exact in f64")]
        {
            assert_eq!(status.current_cost, 3.0);
        }
    }

    #[test]
    fn day_budget_respects_reset_hour() {
        // Before the reset hour, the window starts yesterday at reset_hour.
        let ledger = Ledger::open_in_memory().unwrap();
        let config = UsageConfig {
            timezone: "utc".into(),
            budgets: vec![UsageBudgetConfig {
                name: "daily".into(),
                period: UsageBudgetPeriod::Day,
                cost_usd: 10.0,
                reset_hour: Some(6),
                ..usage_budget()
            }],
            ..UsageConfig::default()
        };

        let before = budget_statuses(
            &ledger,
            &config,
            "2026-05-20T03:00:00+00:00".parse().unwrap(),
        )
        .unwrap();
        let before_status = first_item(&before);
        assert_eq!(before_status.period_start, "2026-05-19T06:00:00+00:00");
        assert_eq!(before_status.reset_at, "2026-05-20T06:00:00+00:00");

        let after = budget_statuses(
            &ledger,
            &config,
            "2026-05-20T09:00:00+00:00".parse().unwrap(),
        )
        .unwrap();
        let after_status = first_item(&after);
        assert_eq!(after_status.period_start, "2026-05-20T06:00:00+00:00");
        assert_eq!(after_status.reset_at, "2026-05-21T06:00:00+00:00");
    }

    #[test]
    fn week_budget_respects_reset_day_and_hour() {
        // 2026-05-20 is a Wednesday. With reset_day_of_week=thursday and
        // reset_hour=3, the most recent Thursday 03:00 is 2026-05-14T03:00.
        let ledger = Ledger::open_in_memory().unwrap();
        let config = UsageConfig {
            timezone: "utc".into(),
            budgets: vec![UsageBudgetConfig {
                name: "weekly".into(),
                period: UsageBudgetPeriod::Week,
                cost_usd: 50.0,
                reset_day_of_week: Some(BudgetWeekday::Thursday),
                reset_hour: Some(3),
                ..usage_budget()
            }],
            ..UsageConfig::default()
        };

        let statuses = budget_statuses(
            &ledger,
            &config,
            "2026-05-20T14:00:00+00:00".parse().unwrap(),
        )
        .unwrap();
        let status = first_item(&statuses);
        assert_eq!(status.period_start, "2026-05-14T03:00:00+00:00");
        assert_eq!(status.reset_at, "2026-05-21T03:00:00+00:00");
    }

    #[test]
    fn week_budget_resets_today_when_past_anchor_hour() {
        // 2026-05-21 is a Thursday. Past 03:00, the window starts today at 03:00.
        let ledger = Ledger::open_in_memory().unwrap();
        let config = UsageConfig {
            timezone: "utc".into(),
            budgets: vec![UsageBudgetConfig {
                name: "weekly".into(),
                period: UsageBudgetPeriod::Week,
                cost_usd: 50.0,
                reset_day_of_week: Some(BudgetWeekday::Thursday),
                reset_hour: Some(3),
                ..usage_budget()
            }],
            ..UsageConfig::default()
        };

        let statuses = budget_statuses(
            &ledger,
            &config,
            "2026-05-21T10:00:00+00:00".parse().unwrap(),
        )
        .unwrap();
        let status = first_item(&statuses);
        assert_eq!(status.period_start, "2026-05-21T03:00:00+00:00");
        assert_eq!(status.reset_at, "2026-05-28T03:00:00+00:00");
    }

    #[test]
    fn month_budget_respects_reset_day_of_month() {
        let ledger = Ledger::open_in_memory().unwrap();
        let config = UsageConfig {
            timezone: "utc".into(),
            budgets: vec![UsageBudgetConfig {
                name: "billing".into(),
                period: UsageBudgetPeriod::Month,
                cost_usd: 200.0,
                reset_day_of_month: Some(15),
                ..usage_budget()
            }],
            ..UsageConfig::default()
        };

        // Before the 15th: window started on the prior 15th.
        let before = budget_statuses(
            &ledger,
            &config,
            "2026-05-10T12:00:00+00:00".parse().unwrap(),
        )
        .unwrap();
        let before_status = first_item(&before);
        assert_eq!(before_status.period_start, "2026-04-15T00:00:00+00:00");
        assert_eq!(before_status.reset_at, "2026-05-15T00:00:00+00:00");

        // After the 15th: window started this month.
        let after = budget_statuses(
            &ledger,
            &config,
            "2026-05-20T12:00:00+00:00".parse().unwrap(),
        )
        .unwrap();
        let after_status = first_item(&after);
        assert_eq!(after_status.period_start, "2026-05-15T00:00:00+00:00");
        assert_eq!(after_status.reset_at, "2026-06-15T00:00:00+00:00");
    }

    #[test]
    fn month_anchor_clamps_to_short_months() {
        // reset_day_of_month=31, now is mid-February: clamp to Feb 28 (non-leap).
        let ledger = Ledger::open_in_memory().unwrap();
        let config = UsageConfig {
            timezone: "utc".into(),
            budgets: vec![UsageBudgetConfig {
                name: "monthly".into(),
                period: UsageBudgetPeriod::Month,
                cost_usd: 100.0,
                reset_day_of_month: Some(31),
                ..usage_budget()
            }],
            ..UsageConfig::default()
        };

        // 2026 is not a leap year. Mid-Feb is before Feb 28 anchor.
        let mid_feb = budget_statuses(
            &ledger,
            &config,
            "2026-02-15T12:00:00+00:00".parse().unwrap(),
        )
        .unwrap();
        let mid_feb_status = first_item(&mid_feb);
        assert_eq!(mid_feb_status.period_start, "2026-01-31T00:00:00+00:00");
        assert_eq!(mid_feb_status.reset_at, "2026-02-28T00:00:00+00:00");

        // Past the clamped Feb anchor: window starts on Feb 28.
        let late_feb = budget_statuses(
            &ledger,
            &config,
            "2026-02-28T12:00:00+00:00".parse().unwrap(),
        )
        .unwrap();
        let late_feb_status = first_item(&late_feb);
        assert_eq!(late_feb_status.period_start, "2026-02-28T00:00:00+00:00");
        assert_eq!(late_feb_status.reset_at, "2026-03-31T00:00:00+00:00");
    }

    #[test]
    fn month_anchor_clamps_across_year_boundary() {
        // reset_day_of_month=31, now is early January: previous anchor is Dec 31.
        let ledger = Ledger::open_in_memory().unwrap();
        let config = UsageConfig {
            timezone: "utc".into(),
            budgets: vec![UsageBudgetConfig {
                name: "monthly".into(),
                period: UsageBudgetPeriod::Month,
                cost_usd: 100.0,
                reset_day_of_month: Some(31),
                ..usage_budget()
            }],
            ..UsageConfig::default()
        };

        let statuses = budget_statuses(
            &ledger,
            &config,
            "2026-01-10T12:00:00+00:00".parse().unwrap(),
        )
        .unwrap();
        let status = first_item(&statuses);
        assert_eq!(status.period_start, "2025-12-31T00:00:00+00:00");
        assert_eq!(status.reset_at, "2026-01-31T00:00:00+00:00");
    }

    #[test]
    fn unanchored_day_budget_matches_legacy_behavior() {
        // No anchor fields: the window starts at local/UTC midnight, matching
        // the previous fixed-boundary semantics.
        let ledger = Ledger::open_in_memory().unwrap();
        let config = UsageConfig {
            timezone: "utc".into(),
            budgets: vec![UsageBudgetConfig {
                name: "daily".into(),
                period: UsageBudgetPeriod::Day,
                cost_usd: 10.0,
                ..usage_budget()
            }],
            ..UsageConfig::default()
        };

        let statuses = budget_statuses(
            &ledger,
            &config,
            "2026-05-20T15:30:00+00:00".parse().unwrap(),
        )
        .unwrap();
        let status = first_item(&statuses);
        assert_eq!(status.period_start, "2026-05-20T00:00:00+00:00");
        assert_eq!(status.reset_at, "2026-05-21T00:00:00+00:00");
    }

    #[test]
    fn newly_crossed_budget_warnings_are_deduped() {
        let ledger = Ledger::open_in_memory().unwrap();
        insert_call(&ledger, "2026-05-18T03:00:00+00:00", 8.5, "message");
        let config = UsageConfig {
            timezone: "utc".into(),
            budgets: vec![UsageBudgetConfig {
                name: "daily".into(),
                cost_usd: 10.0,
                warn_at: vec![0.5, 0.8],
                ..usage_budget()
            }],
            ..UsageConfig::default()
        };
        let now = "2026-05-18T12:00:00+00:00".parse().unwrap();

        let first = newly_crossed_budget_warnings(&ledger, &config, now).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first_item(&first).crossed_warn_at, vec![0.5, 0.8]);

        let second = newly_crossed_budget_warnings(&ledger, &config, now).unwrap();
        assert!(second.is_empty());
    }

    #[test]
    fn over_limit_warning_refires_each_call() {
        let ledger = Ledger::open_in_memory().unwrap();
        insert_call(&ledger, "2026-05-18T03:00:00+00:00", 12.0, "message");
        let config = UsageConfig {
            timezone: "utc".into(),
            budgets: vec![UsageBudgetConfig {
                name: "daily".into(),
                cost_usd: 10.0,
                warn_at: vec![0.5, 0.8],
                ..usage_budget()
            }],
            ..UsageConfig::default()
        };
        let now = "2026-05-18T12:00:00+00:00".parse().unwrap();

        let first = newly_crossed_budget_warnings(&ledger, &config, now).unwrap();
        assert_eq!(first.len(), 1);
        // First call records 0.5 and 0.8 as newly crossed; over_limit doesn't
        // need to synthesize anything yet.
        assert_eq!(first_item(&first).crossed_warn_at, vec![0.5, 0.8]);

        let second = newly_crossed_budget_warnings(&ledger, &config, now).unwrap();
        assert_eq!(second.len(), 1, "over-limit warning should re-fire");
        let second_warning = first_item(&second);
        assert_eq!(second_warning.crossed_warn_at, vec![1.0]);
        assert!(second_warning.current_cost >= second_warning.cost_limit);

        // And again — every subsequent call while over budget.
        let third = newly_crossed_budget_warnings(&ledger, &config, now).unwrap();
        assert_eq!(third.len(), 1);
        assert_eq!(first_item(&third).crossed_warn_at, vec![1.0]);
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
            reset_hour: None,
            reset_day_of_week: None,
            reset_day_of_month: None,
        }
    }
}
