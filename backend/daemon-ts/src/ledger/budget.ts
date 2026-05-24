/**
 * Usage budget evaluation over the append-only ledger.
 *
 * Mirrors `backend/ledger/src/budget.rs`. The Rust impl uses chrono's
 * `Local` timezone; the TS impl uses Bun's process timezone (the value
 * of `TZ` / system local), which is the same source.
 */

import type { Ledger, QueryFilter } from "./ledger.ts";
import { ledgerDatabase } from "./ledger.ts";

// ── Config shapes (mirror of shore-config app.rs) ───────────────────────

export type UsageBudgetPeriod = "hour" | "day" | "week" | "month";
export type UsageBudgetAction = "warn" | "block" | "pause_background";

/** 0 = Monday .. 6 = Sunday, matching chrono `num_days_from_monday`. */
export type BudgetWeekday =
  | "monday"
  | "tuesday"
  | "wednesday"
  | "thursday"
  | "friday"
  | "saturday"
  | "sunday";

const WEEKDAY_INDEX: Record<BudgetWeekday, number> = {
  monday: 0,
  tuesday: 1,
  wednesday: 2,
  thursday: 3,
  friday: 4,
  saturday: 5,
  sunday: 6,
};

export interface UsageBudgetConfig {
  name: string;
  period: UsageBudgetPeriod;
  cost_usd: number;
  warn_at: number[];
  limit: UsageBudgetAction;
  character?: string;
  provider?: string;
  api_key?: string;
  model?: string;
  call_type?: string;
  usage_kind: string[];
  allow_compaction_over_budget?: boolean;
  reset_hour?: number;
  reset_day_of_week?: BudgetWeekday;
  reset_day_of_month?: number;
}

export interface UsageSpikeWarningsConfig {
  enabled: boolean;
  period: UsageBudgetPeriod;
  multiplier: number;
  min_cost_usd: number;
}

export interface UsageConfig {
  timezone: string;
  allow_compaction_over_budget: boolean;
  budgets: UsageBudgetConfig[];
  spike_warnings: UsageSpikeWarningsConfig;
}

export function defaultSpikeWarningsConfig(): UsageSpikeWarningsConfig {
  return {
    enabled: false,
    period: "hour",
    multiplier: 3.0,
    min_cost_usd: 1.0,
  };
}

export function defaultUsageBudgetConfig(): UsageBudgetConfig {
  return {
    name: "",
    period: "day",
    cost_usd: 1.0,
    warn_at: [0.8, 1.0],
    limit: "warn",
    usage_kind: [],
  };
}

// ── Payloads ────────────────────────────────────────────────────────────

export interface BudgetStatusPayload {
  name: string;
  period: UsageBudgetPeriod;
  period_start: string;
  period_end: string;
  reset_at: string;
  timezone: string;
  current_cost: number;
  cost_limit: number;
  percent_used: number;
  status: "ok" | "warning" | "over_limit";
  action: UsageBudgetAction;
  warning_thresholds: number[];
  crossed_warn_at: number[];
  over_limit: boolean;
  compaction_allowed_over_budget: boolean;
  filters: Record<string, unknown>;
}

export interface SpikeWarningPayload {
  period: UsageBudgetPeriod;
  period_start: string;
  previous_period_start: string;
  timezone: string;
  current_cost: number;
  previous_cost: number;
  multiplier: number | null;
  threshold_multiplier: number;
  min_cost_usd: number;
  message: string;
}

export interface UsageBudgetWarningEvent {
  budget: string;
  message: string;
  current_cost: number;
  cost_limit: number;
  percent_used: number;
  crossed_warn_at: number[];
  period: UsageBudgetPeriod;
  period_start: string;
  reset_at: string;
}

export type CallType =
  | "message"
  | "tool_loop"
  | "heartbeat"
  | "heartbeat_tool_loop"
  | "keepalive"
  | "compaction"
  | "dreaming"
  | "memory_query";

export interface BudgetCallContext {
  provider: string;
  api_key_name?: string;
  model: string;
  call_type: CallType;
  character: string;
}

export interface BudgetBlock {
  budget_name: string;
  action: UsageBudgetAction;
  current_cost: number;
  cost_limit: number;
  period: UsageBudgetPeriod;
  reset_at: string;
  message: string;
}

// ── Public API ──────────────────────────────────────────────────────────

export function budgetStatuses(
  ledger: Ledger,
  config: UsageConfig,
  now: Date,
): BudgetStatusPayload[] {
  return config.budgets.map((budget, idx) => budgetStatus(ledger, config, budget, idx, now));
}

export function enforceBudgetForCall(
  ledger: Ledger,
  config: UsageConfig,
  call: BudgetCallContext,
  now: Date,
): BudgetBlock | undefined {
  if (config.budgets.length === 0) return undefined;
  for (let idx = 0; idx < config.budgets.length; idx++) {
    const budget = config.budgets[idx]!;
    if (!budgetMatchesCall(budget, call)) continue;
    const status = budgetStatus(ledger, config, budget, idx, now);
    if (status.over_limit && shouldBlock(config, budget, call.call_type)) {
      return {
        budget_name: status.name,
        action: status.action,
        current_cost: status.current_cost,
        cost_limit: status.cost_limit,
        period: status.period,
        reset_at: status.reset_at,
        message: formatBlockMessage(status),
      };
    }
  }
  return undefined;
}

export function spikeWarnings(
  ledger: Ledger,
  config: UsageConfig,
  now: Date,
): SpikeWarningPayload[] {
  const spike = config.spike_warnings;
  if (!spike.enabled) return [];

  const current = periodWindow(now, spike.period, config.timezone, undefined);
  const previous = periodWindow(
    new Date(current.start.getTime() - 1000),
    spike.period,
    config.timezone,
    undefined,
  );

  const currentCost = totalCostBetween(ledger, { since: current.start.toISOString() });
  const previousCost = totalCostBetween(ledger, {
    since: previous.start.toISOString(),
    until: current.start.toISOString(),
  });

  if (currentCost < spike.min_cost_usd) return [];

  const multiplier = previousCost > 0 ? currentCost / previousCost : null;
  const isSpike =
    multiplier !== null ? multiplier >= spike.multiplier : previousCost === 0;
  if (!isSpike) return [];

  const message =
    multiplier !== null
      ? `Current ${periodLabel(spike.period)} spend is ${multiplier.toFixed(1)}x the previous ${periodLabel(spike.period)} ($${currentCost.toFixed(2)} vs $${previousCost.toFixed(2)}).`
      : `Current ${periodLabel(spike.period)} spend is $${currentCost.toFixed(2)}; the previous ${periodLabel(spike.period)} had no recorded cost.`;

  return [
    {
      period: spike.period,
      period_start: current.start.toISOString(),
      previous_period_start: previous.start.toISOString(),
      timezone: current.timezone,
      current_cost: currentCost,
      previous_cost: previousCost,
      multiplier,
      threshold_multiplier: spike.multiplier,
      min_cost_usd: spike.min_cost_usd,
      message,
    },
  ];
}

/**
 * Return newly crossed budget warning thresholds and record each so future
 * checks don't repeat. The over-limit threshold (1.0) re-fires on every
 * call once over budget; intermediate thresholds (0.5, 0.8) are one-shot.
 */
export function newlyCrossedBudgetWarnings(
  ledger: Ledger,
  config: UsageConfig,
  now: Date,
): UsageBudgetWarningEvent[] {
  const statuses = budgetStatuses(ledger, config, now);
  const events: UsageBudgetWarningEvent[] = [];

  for (const status of statuses) {
    const newlyCrossed: number[] = [];
    for (const threshold of status.crossed_warn_at) {
      if (recordBudgetWarningThreshold(ledger, status.name, status.period_start, threshold, now)) {
        newlyCrossed.push(threshold);
      }
    }
    if (newlyCrossed.length === 0 && status.over_limit) {
      newlyCrossed.push(1.0);
    }
    if (newlyCrossed.length === 0) continue;

    const highest = newlyCrossed.reduce((a, b) => Math.max(a, b), 0);
    events.push({
      budget: status.name,
      message: `Usage budget "${status.name}" reached ${(highest * 100).toFixed(0)}% ($${status.current_cost.toFixed(2)}/$${status.cost_limit.toFixed(2)}); resets at ${status.reset_at}.`,
      current_cost: status.current_cost,
      cost_limit: status.cost_limit,
      percent_used: status.percent_used,
      crossed_warn_at: newlyCrossed,
      period: status.period,
      period_start: status.period_start,
      reset_at: status.reset_at,
    });
  }

  return events;
}

// ── internals ───────────────────────────────────────────────────────────

function budgetStatus(
  ledger: Ledger,
  config: UsageConfig,
  budget: UsageBudgetConfig,
  idx: number,
  now: Date,
): BudgetStatusPayload {
  const anchors = anchorsFromBudget(budget);
  const window = periodWindow(now, budget.period, config.timezone, anchors);
  const totals = ledger.usageTotals(filterForBudget(budget, window.start));
  const currentCost = totals.total_cost;
  const percentUsed = budget.cost_usd === 0 ? 0 : currentCost / budget.cost_usd;
  const warningThresholds = [...budget.warn_at]
    .sort((a, b) => a - b)
    .filter((v, i, arr) => i === 0 || Math.abs(v - arr[i - 1]!) >= Number.EPSILON);
  const crossedWarnAt = warningThresholds.filter((threshold) => percentUsed >= threshold);
  const overLimit = currentCost >= budget.cost_usd;
  const status = overLimit
    ? ("over_limit" as const)
    : crossedWarnAt.length === 0
      ? ("ok" as const)
      : ("warning" as const);

  return {
    name: budgetName(budget, idx),
    period: budget.period,
    period_start: window.start.toISOString(),
    period_end: window.end.toISOString(),
    reset_at: window.end.toISOString(),
    timezone: window.timezone,
    current_cost: currentCost,
    cost_limit: budget.cost_usd,
    percent_used: percentUsed,
    status,
    action: budget.limit,
    warning_thresholds: warningThresholds,
    crossed_warn_at: crossedWarnAt,
    over_limit: overLimit,
    compaction_allowed_over_budget: compactionAllowed(config, budget),
    filters: budgetFiltersJson(budget),
  };
}

function filterForBudget(budget: UsageBudgetConfig, since: Date): QueryFilter {
  const out: QueryFilter = { since: since.toISOString() };
  if (budget.character !== undefined) out.character = budget.character;
  if (budget.provider !== undefined) out.provider = budget.provider;
  if (budget.api_key !== undefined) out.api_key_name = budget.api_key;
  if (budget.model !== undefined) out.model = budget.model;
  if (budget.call_type !== undefined) out.call_type = budget.call_type;
  if (budget.usage_kind.length > 0) out.usage_kinds = budget.usage_kind;
  return out;
}

function budgetName(budget: UsageBudgetConfig, idx: number): string {
  const name = budget.name.trim();
  return name === "" ? `budget ${idx + 1}` : name;
}

function budgetFiltersJson(budget: UsageBudgetConfig): Record<string, unknown> {
  return {
    character: budget.character ?? null,
    provider: budget.provider ?? null,
    api_key: budget.api_key ?? null,
    model: budget.model ?? null,
    call_type: budget.call_type ?? null,
    usage_kind: budget.usage_kind,
  };
}

function budgetMatchesCall(budget: UsageBudgetConfig, call: BudgetCallContext): boolean {
  if (budget.character !== undefined && budget.character !== call.character) return false;
  if (budget.provider !== undefined && budget.provider !== call.provider) return false;
  if (budget.model !== undefined && budget.model !== call.model) return false;
  if (budget.call_type !== undefined && budget.call_type !== call.call_type) return false;
  if (budget.api_key !== undefined) {
    const actual = call.api_key_name ?? "unknown";
    if (budget.api_key !== actual) return false;
  }
  if (budget.usage_kind.length > 0) {
    const matches = budget.usage_kind.some((kind) => callTypeMatchesUsageKind(call.call_type, kind));
    if (!matches) return false;
  }
  return true;
}

function callTypeMatchesUsageKind(callType: CallType, usageKind: string): boolean {
  switch (callType) {
    case "message":
      return usageKind === "message"
        || usageKind === "message_no_tools"
        || usageKind === "message_with_tools";
    case "tool_loop":
      return usageKind === "message_with_tools" || usageKind === "tool_loop";
    case "heartbeat":
    case "heartbeat_tool_loop":
      return usageKind === "heartbeat";
    case "keepalive":
      return usageKind === "keepalive";
    case "compaction":
      return usageKind === "compaction";
    case "dreaming":
      return usageKind === "dreaming";
    case "memory_query":
      return usageKind === "memory_query";
  }
}

function shouldBlock(config: UsageConfig, budget: UsageBudgetConfig, callType: CallType): boolean {
  if (callType === "compaction" && compactionAllowed(config, budget)) return false;
  switch (budget.limit) {
    case "warn":
      return false;
    case "block":
      return true;
    case "pause_background":
      return isBackgroundCall(callType);
  }
}

function compactionAllowed(config: UsageConfig, budget: UsageBudgetConfig): boolean {
  return budget.allow_compaction_over_budget ?? config.allow_compaction_over_budget;
}

function isBackgroundCall(callType: CallType): boolean {
  return (
    callType === "heartbeat"
    || callType === "heartbeat_tool_loop"
    || callType === "keepalive"
    || callType === "compaction"
    || callType === "dreaming"
    || callType === "memory_query"
  );
}

function formatBlockMessage(status: BudgetStatusPayload): string {
  return (
    `Shore usage budget "${status.name}" is over limit ` +
    `($${status.current_cost.toFixed(2)}/$${status.cost_limit.toFixed(2)} for ${status.period}); ` +
    `action ${status.action}; resets at ${status.reset_at}`
  );
}

function periodLabel(period: UsageBudgetPeriod): string {
  // Match Rust's debug-format ("Hour", "Day", "Week", "Month") so message
  // strings line up byte-for-byte where they're compared.
  return period.charAt(0).toUpperCase() + period.slice(1);
}

interface BudgetAnchors {
  reset_hour: number;
  reset_day_of_week: number;
  reset_day_of_month: number;
}

function anchorsFromBudget(budget: UsageBudgetConfig): BudgetAnchors {
  return {
    reset_hour: budget.reset_hour ?? 0,
    reset_day_of_week:
      budget.reset_day_of_week !== undefined ? WEEKDAY_INDEX[budget.reset_day_of_week] : 0,
    reset_day_of_month: budget.reset_day_of_month ?? 1,
  };
}

interface PeriodWindow {
  start: Date;
  end: Date;
  timezone: string;
}

function periodWindow(
  now: Date,
  period: UsageBudgetPeriod,
  timezone: string,
  anchors: BudgetAnchors | undefined,
): PeriodWindow {
  const a = anchors ?? { reset_hour: 0, reset_day_of_week: 0, reset_day_of_month: 1 };
  if (timezone === "utc") {
    const parts = utcParts(now);
    const start = periodStart(parts, period, a, "utc");
    const end = periodEnd(start, period, a, "utc");
    return { start, end, timezone: "utc" };
  }
  const parts = localParts(now);
  const start = periodStart(parts, period, a, "local");
  const end = periodEnd(start, period, a, "local");
  return { start, end, timezone: "local" };
}

interface DateParts {
  year: number;
  month: number; // 1..12
  day: number;
  hour: number;
  minute: number;
  second: number;
  /** 0 = Monday, 6 = Sunday (chrono `num_days_from_monday`). */
  weekdayMon0: number;
}

function utcParts(d: Date): DateParts {
  const dow = d.getUTCDay(); // 0=Sun..6=Sat
  return {
    year: d.getUTCFullYear(),
    month: d.getUTCMonth() + 1,
    day: d.getUTCDate(),
    hour: d.getUTCHours(),
    minute: d.getUTCMinutes(),
    second: d.getUTCSeconds(),
    weekdayMon0: (dow + 6) % 7,
  };
}

function localParts(d: Date): DateParts {
  const dow = d.getDay();
  return {
    year: d.getFullYear(),
    month: d.getMonth() + 1,
    day: d.getDate(),
    hour: d.getHours(),
    minute: d.getMinutes(),
    second: d.getSeconds(),
    weekdayMon0: (dow + 6) % 7,
  };
}

type Zone = "utc" | "local";

function periodStart(
  now: DateParts,
  period: UsageBudgetPeriod,
  anchors: BudgetAnchors,
  zone: Zone,
): Date {
  switch (period) {
    case "hour":
      return makeDate(now.year, now.month, now.day, now.hour, 0, 0, zone);
    case "day": {
      const todayReset = makeDate(now.year, now.month, now.day, anchors.reset_hour, 0, 0, zone);
      if (compareParts(now, partsOf(todayReset, zone)) >= 0) return todayReset;
      const yesterday = addDaysAt(now.year, now.month, now.day, -1);
      return makeDate(yesterday.y, yesterday.m, yesterday.d, anchors.reset_hour, 0, 0, zone);
    }
    case "week": {
      const todayDow = now.weekdayMon0;
      const daysBack = (todayDow + 7 - anchors.reset_day_of_week) % 7;
      const candidate = addDaysAt(now.year, now.month, now.day, -daysBack);
      const candidateDate = makeDate(
        candidate.y,
        candidate.m,
        candidate.d,
        anchors.reset_hour,
        0,
        0,
        zone,
      );
      if (compareParts(now, partsOf(candidateDate, zone)) >= 0) return candidateDate;
      const prev = addDaysAt(candidate.y, candidate.m, candidate.d, -7);
      return makeDate(prev.y, prev.m, prev.d, anchors.reset_hour, 0, 0, zone);
    }
    case "month": {
      const thisMonth = monthAnchor(now.year, now.month, anchors, zone);
      if (compareParts(now, partsOf(thisMonth, zone)) >= 0) return thisMonth;
      const prev = now.month === 1
        ? { y: now.year - 1, m: 12 }
        : { y: now.year, m: now.month - 1 };
      return monthAnchor(prev.y, prev.m, anchors, zone);
    }
  }
}

function periodEnd(
  start: Date,
  period: UsageBudgetPeriod,
  anchors: BudgetAnchors,
  zone: Zone,
): Date {
  switch (period) {
    case "hour":
      return new Date(start.getTime() + 3600_000);
    case "day":
      return new Date(start.getTime() + 86400_000);
    case "week":
      return new Date(start.getTime() + 7 * 86400_000);
    case "month": {
      const p = partsOf(start, zone);
      const next = p.month === 12 ? { y: p.year + 1, m: 1 } : { y: p.year, m: p.month + 1 };
      return monthAnchor(next.y, next.m, anchors, zone);
    }
  }
}

function monthAnchor(year: number, month: number, anchors: BudgetAnchors, zone: Zone): Date {
  const maxDay = daysInMonth(year, month);
  const day = Math.min(anchors.reset_day_of_month, maxDay);
  return makeDate(year, month, day, anchors.reset_hour, 0, 0, zone);
}

function daysInMonth(year: number, month: number): number {
  return new Date(Date.UTC(year, month, 0)).getUTCDate();
}

function addDaysAt(
  year: number,
  month: number,
  day: number,
  delta: number,
): { y: number; m: number; d: number } {
  const t = Date.UTC(year, month - 1, day) + delta * 86400_000;
  const d = new Date(t);
  return { y: d.getUTCFullYear(), m: d.getUTCMonth() + 1, d: d.getUTCDate() };
}

function makeDate(
  year: number,
  month: number,
  day: number,
  hour: number,
  minute: number,
  second: number,
  zone: Zone,
): Date {
  if (zone === "utc") {
    return new Date(Date.UTC(year, month - 1, day, hour, minute, second));
  }
  return new Date(year, month - 1, day, hour, minute, second);
}

function partsOf(d: Date, zone: Zone): DateParts {
  return zone === "utc" ? utcParts(d) : localParts(d);
}

function compareParts(a: DateParts, b: DateParts): number {
  if (a.year !== b.year) return a.year - b.year;
  if (a.month !== b.month) return a.month - b.month;
  if (a.day !== b.day) return a.day - b.day;
  if (a.hour !== b.hour) return a.hour - b.hour;
  if (a.minute !== b.minute) return a.minute - b.minute;
  return a.second - b.second;
}

function totalCostBetween(
  ledger: Ledger,
  filter: { since?: string; until?: string },
): number {
  return ledger.usageTotals({
    ...(filter.since !== undefined ? { since: filter.since } : {}),
    ...(filter.until !== undefined ? { until: filter.until } : {}),
  }).total_cost;
}

function recordBudgetWarningThreshold(
  ledger: Ledger,
  budgetName: string,
  periodStartStr: string,
  threshold: number,
  now: Date,
): boolean {
  const db = ledgerDatabase(ledger);
  const result = db
    .query(
      `INSERT OR IGNORE INTO usage_budget_warnings
         (budget_name, period_start, threshold, created_at)
       VALUES ($budget, $period, $threshold, $now)`,
    )
    .run({
      $budget: budgetName,
      $period: periodStartStr,
      $threshold: threshold.toFixed(6),
      $now: now.toISOString(),
    });
  return result.changes > 0;
}
