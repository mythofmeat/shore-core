/**
 * `shore usage` command payloads backed by the TS ledger.
 *
 * This mirrors `backend/daemon/src/commands/usage.rs` for the modes the
 * CLI renders directly. Pricing refresh/recalculation are acknowledged
 * but left as no-op payloads until the pricing catalog port lands.
 */

import type { Ledger, QueryFilter } from "./ledger.ts";

export interface UsageConfigSlice {
  timezone: string;
  allow_compaction_over_budget: boolean;
}

export function usagePayload(
  ledger: Ledger,
  args: Record<string, unknown>,
  config: UsageConfigSlice,
): Record<string, unknown> {
  const timezone = config.timezone;
  const { filter, last } = buildFilter(args, timezone);

  if (args["budget"] === true) {
    return {
      mode: "budget",
      timezone,
      allow_compaction_over_budget: config.allow_compaction_over_budget,
      budgets: [],
      spike_warnings: [],
    };
  }

  if (args["export_tsv"] === true) {
    return { mode: "tsv", data: ledger.exportTsv(filter) };
  }

  if (args["export_csv"] === true) {
    return { mode: "csv", data: tsvToCsv(ledger.exportTsv(filter)) };
  }

  if (args["by_kind"] === true) {
    return {
      mode: "summary_by_usage_kind",
      period: last,
      summary: ledger.usageSummaryByUsageKind(filter).map((s) => ({
        usage_kind: s.usage_kind,
        call_count: s.call_count,
        total_input: s.total_input,
        total_output: s.total_output,
        total_cache_read: s.total_cache_read,
        total_cache_write: s.total_cache_write,
        total_cost: s.total_cost,
      })),
    };
  }

  if (args["by_api_key"] === true) {
    return {
      mode: "summary_by_api_key",
      period: last,
      summary: ledger.usageSummaryByApiKey(filter).map((s) => ({
        provider: s.provider,
        api_key_name: s.api_key_name,
        call_count: s.call_count,
        total_input: s.total_input,
        total_output: s.total_output,
        total_cache_read: s.total_cache_read,
        total_cache_write: s.total_cache_write,
        total_cost: s.total_cost,
      })),
    };
  }

  if (args["by_call_type"] === true) {
    return {
      mode: "summary_by_call_type",
      period: last,
      summary: ledger.usageSummaryByCallType(filter).map((s) => ({
        call_type: s.call_type,
        call_count: s.call_count,
        total_input: s.total_input,
        total_output: s.total_output,
        total_cache_read: s.total_cache_read,
        total_cache_write: s.total_cache_write,
        total_cost: s.total_cost,
      })),
    };
  }

  if (args["anomalies"] === true) {
    const anomalyFilter = last === "today"
      ? withSince(filter, parseLastPeriod("7d", timezone))
      : filter;
    const anomalies = ledger.queryAnomalies(compactFilter(anomalyFilter)).map((r) => ({
      ts: r.ts,
      character: r.character,
      model: r.model,
      call_type: r.call_type,
      anomaly: r.cache_anomaly,
      cache_read_tokens: r.cache_read_tokens,
      cache_write_tokens: r.cache_write_tokens,
    }));
    return { mode: "anomalies", anomalies };
  }

  if (args["refresh_pricing"] === true) {
    return { mode: "refresh_pricing" };
  }

  if (args["recalculate"] === true) {
    return { mode: "recalculate", updated: 0, total: 0, failures: [] };
  }

  const summary = ledger.usageSummary(filter).map((s) => ({
    provider: s.provider,
    model: s.model,
    call_count: s.call_count,
    total_input: s.total_input,
    total_output: s.total_output,
    total_cache_read: s.total_cache_read,
    total_cache_write: s.total_cache_write,
    total_cost: s.total_cost,
  }));

  const cacheHealth = ledger.activeAnthropicCharacters(filter).map(([character, row]) => {
    const tracker = importReconstruct(row);
    return {
      character,
      state: tracker,
      streak: ledger.warmStreak(character),
    };
  });

  const anomalyFilter = withSince({}, parseLastPeriod("7d", timezone));
  const anomalyCount = ledger.queryAnomalies(compactFilter(anomalyFilter)).length;

  return {
    mode: "summary",
    period: last,
    timezone,
    summary,
    cache_health: cacheHealth,
    anomaly_count_7d: anomalyCount,
    budgets: [],
    spike_warnings: [],
  };
}

export function parseLastPeriodAt(
  period: string,
  now: Date,
  timezone: string,
): string | undefined {
  switch (period) {
    case "today":
      return formatUtcRfc3339(calendarStart(now, "day", timezone));
    case "week":
    case "this_week":
      return formatUtcRfc3339(calendarStart(now, "week", timezone));
    case "month":
    case "this_month":
      return formatUtcRfc3339(calendarStart(now, "month", timezone));
    case "all":
      return undefined;
    default:
      return parseRelativePeriod(period, now);
  }
}

function parseLastPeriod(period: string, timezone: string): string | undefined {
  return parseLastPeriodAt(period, new Date(), timezone);
}

function buildFilter(
  args: Record<string, unknown>,
  timezone: string,
): { filter: QueryFilter; last: string } {
  const last = typeof args["last"] === "string" ? args["last"] : "today";
  const filter: QueryFilter = {};
  const since = parseLastPeriod(last, timezone);
  if (since !== undefined) filter.since = since;

  setStringFilter(filter, "character", args["character"]);
  setStringFilter(filter, "provider", args["provider"]);
  setStringFilter(filter, "api_key_name", args["api_key"]);
  setStringFilter(filter, "model", args["model"]);
  setStringFilter(filter, "call_type", args["call_type"]);

  return { filter, last };
}

function setStringFilter(
  filter: QueryFilter,
  key: "character" | "provider" | "api_key_name" | "model" | "call_type",
  value: unknown,
): void {
  if (typeof value === "string") filter[key] = value;
}

function parseRelativePeriod(period: string, now: Date): string | undefined {
  const match = /^(\d+)([hdw])$/.exec(period);
  if (!match) return undefined;
  const amount = Number(match[1]);
  const unit = match[2];
  if (!Number.isFinite(amount)) return undefined;
  const hours =
    unit === "h" ? amount : unit === "d" ? amount * 24 : amount * 24 * 7;
  return formatUtcRfc3339(new Date(now.getTime() - hours * 60 * 60 * 1000));
}

function calendarStart(
  now: Date,
  window: "day" | "week" | "month",
  timezone: string,
): Date {
  if (timezone === "utc") {
    const year = now.getUTCFullYear();
    const month = now.getUTCMonth();
    const day = now.getUTCDate();
    if (window === "month") return new Date(Date.UTC(year, month, 1, 0, 0, 0));
    if (window === "week") {
      const dow = (now.getUTCDay() + 6) % 7;
      return new Date(Date.UTC(year, month, day - dow, 0, 0, 0));
    }
    return new Date(Date.UTC(year, month, day, 0, 0, 0));
  }

  const year = now.getFullYear();
  const month = now.getMonth();
  const day = now.getDate();
  if (window === "month") return new Date(year, month, 1, 0, 0, 0, 0);
  if (window === "week") {
    const dow = (now.getDay() + 6) % 7;
    return new Date(year, month, day - dow, 0, 0, 0, 0);
  }
  return new Date(year, month, day, 0, 0, 0, 0);
}

function formatUtcRfc3339(date: Date): string {
  const pad = (n: number): string => String(n).padStart(2, "0");
  return (
    `${date.getUTCFullYear()}-${pad(date.getUTCMonth() + 1)}-${pad(date.getUTCDate())}` +
    `T${pad(date.getUTCHours())}:${pad(date.getUTCMinutes())}:${pad(date.getUTCSeconds())}+00:00`
  );
}

function tsvToCsv(tsv: string): string {
  return tsv
    .split("\n")
    .map((line) =>
      line
        .split("\t")
        .map((field) =>
          field.includes(",") || field.includes("\"") || field.includes("\n")
            ? `"${field.replaceAll("\"", "\"\"")}"`
            : field,
        )
        .join(","),
    )
    .join("\n");
}

function importReconstruct(row: {
  ts: string;
  model: string;
  thinking_enabled: boolean;
  cache_read_tokens: number;
}): "warm" | "cold" {
  // Local import would be cleaner, but keeping this helper narrow avoids
  // exposing the tracker object through the command payload surface.
  const ageSecs = (Date.now() - new Date(row.ts).getTime()) / 1000;
  if (Number.isFinite(ageSecs) && ageSecs < 3600 && row.cache_read_tokens > 0) {
    return "warm";
  }
  return "cold";
}

function compactFilter(filter: QueryFilter): QueryFilter {
  const out: QueryFilter = {};
  if (filter.since !== undefined) out.since = filter.since;
  if (filter.until !== undefined) out.until = filter.until;
  if (filter.character !== undefined) out.character = filter.character;
  if (filter.provider !== undefined) out.provider = filter.provider;
  if (filter.api_key_name !== undefined) out.api_key_name = filter.api_key_name;
  if (filter.model !== undefined) out.model = filter.model;
  if (filter.call_type !== undefined) out.call_type = filter.call_type;
  if (filter.usage_kinds !== undefined) out.usage_kinds = filter.usage_kinds;
  return out;
}

function withSince(filter: QueryFilter, since: string | undefined): QueryFilter {
  const out = compactFilter(filter);
  if (since !== undefined) out.since = since;
  return out;
}
