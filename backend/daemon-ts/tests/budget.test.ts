/**
 * Mirror of `backend/ledger/src/budget.rs::tests` — UTC window math,
 * spike warnings, dedup behavior for crossed-warn thresholds, and the
 * compaction-bypass logic.
 *
 * The Rust impl runs in process-local TZ; the TS port keys off
 * `config.timezone`. These tests pin `timezone: "utc"` so the assertions
 * are reproducible regardless of host TZ.
 */

import { describe, expect, it } from "bun:test";

import {
  budgetStatuses,
  defaultSpikeWarningsConfig,
  defaultUsageBudgetConfig,
  enforceBudgetForCall,
  newlyCrossedBudgetWarnings,
  spikeWarnings,
  type CallType,
  type UsageBudgetConfig,
  type UsageConfig,
} from "../src/ledger/budget.ts";
import { Ledger } from "../src/ledger/ledger.ts";

function insertCall(ledger: Ledger, ts: string, cost: number, callType: string): void {
  ledger.insert({
    ts,
    character: "Alice",
    provider: "openrouter",
    api_key_name: "default",
    model: "model",
    call_type: callType,
    input_tokens: 0,
    output_tokens: 0,
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    total_ms: 0,
    ttft_ms: 0,
    finish_reason: "end_turn",
    thinking_enabled: false,
    cost_source: "provider_reported",
    total_cost: cost,
  });
}

function withBudgets(budgets: UsageBudgetConfig[], extra?: Partial<UsageConfig>): UsageConfig {
  return {
    timezone: "utc",
    allow_compaction_over_budget: false,
    budgets,
    spike_warnings: defaultSpikeWarningsConfig(),
    ...extra,
  };
}

function budget(overrides: Partial<UsageBudgetConfig> = {}): UsageBudgetConfig {
  return { ...defaultUsageBudgetConfig(), ...overrides };
}

describe("budget windows", () => {
  it("sums matching rows for a UTC day budget", () => {
    const ledger = Ledger.openInMemory();
    insertCall(ledger, "2026-05-18T03:00:00+00:00", 4, "message");
    insertCall(ledger, "2026-05-17T23:00:00+00:00", 8, "message");
    const config = withBudgets([budget({ name: "daily", cost_usd: 10 })]);
    const statuses = budgetStatuses(ledger, config, new Date("2026-05-18T12:00:00Z"));
    expect(statuses[0]!.current_cost).toBe(4);
    expect(statuses[0]!.status).toBe("ok");
    ledger.close();
  });

  it("respects reset_hour for day budgets", () => {
    const ledger = Ledger.openInMemory();
    const config = withBudgets([
      budget({ name: "daily", period: "day", cost_usd: 10, reset_hour: 6 }),
    ]);
    const before = budgetStatuses(ledger, config, new Date("2026-05-20T03:00:00Z"));
    expect(before[0]!.period_start).toBe("2026-05-19T06:00:00.000Z");
    expect(before[0]!.reset_at).toBe("2026-05-20T06:00:00.000Z");

    const after = budgetStatuses(ledger, config, new Date("2026-05-20T09:00:00Z"));
    expect(after[0]!.period_start).toBe("2026-05-20T06:00:00.000Z");
    expect(after[0]!.reset_at).toBe("2026-05-21T06:00:00.000Z");
    ledger.close();
  });

  it("respects reset_day_of_week and reset_hour for week budgets", () => {
    // 2026-05-20 is a Wednesday. Reset on Thursday 03:00 → most recent
    // Thursday is 2026-05-14T03:00.
    const ledger = Ledger.openInMemory();
    const config = withBudgets([
      budget({
        name: "weekly",
        period: "week",
        cost_usd: 50,
        reset_day_of_week: "thursday",
        reset_hour: 3,
      }),
    ]);
    const statuses = budgetStatuses(ledger, config, new Date("2026-05-20T14:00:00Z"));
    expect(statuses[0]!.period_start).toBe("2026-05-14T03:00:00.000Z");
    expect(statuses[0]!.reset_at).toBe("2026-05-21T03:00:00.000Z");
    ledger.close();
  });

  it("clamps reset_day_of_month past the end of the month", () => {
    const ledger = Ledger.openInMemory();
    const config = withBudgets([
      budget({
        name: "monthly",
        period: "month",
        cost_usd: 100,
        reset_day_of_month: 31,
      }),
    ]);
    // 2026 is not a leap year. Mid-Feb is before Feb 28 anchor.
    const midFeb = budgetStatuses(ledger, config, new Date("2026-02-15T12:00:00Z"));
    expect(midFeb[0]!.period_start).toBe("2026-01-31T00:00:00.000Z");
    expect(midFeb[0]!.reset_at).toBe("2026-02-28T00:00:00.000Z");

    const lateFeb = budgetStatuses(ledger, config, new Date("2026-02-28T12:00:00Z"));
    expect(lateFeb[0]!.period_start).toBe("2026-02-28T00:00:00.000Z");
    expect(lateFeb[0]!.reset_at).toBe("2026-03-31T00:00:00.000Z");
    ledger.close();
  });
});

describe("budget enforcement", () => {
  it("blocks matching calls once over limit", () => {
    const ledger = Ledger.openInMemory();
    insertCall(ledger, "2026-05-18T03:00:00+00:00", 11, "message");
    const config = withBudgets([
      budget({ name: "daily", cost_usd: 10, limit: "block" }),
    ]);
    const block = enforceBudgetForCall(
      ledger,
      config,
      {
        provider: "openrouter",
        api_key_name: "default",
        model: "model",
        call_type: "message",
        character: "Alice",
      },
      new Date("2026-05-18T12:00:00Z"),
    );
    expect(block).toBeDefined();
    expect(block!.budget_name).toBe("daily");
    expect(block!.action).toBe("block");
    ledger.close();
  });

  it("lets compaction bypass a blocking budget when allowed", () => {
    const ledger = Ledger.openInMemory();
    insertCall(ledger, "2026-05-18T03:00:00+00:00", 11, "message");
    const config = withBudgets(
      [budget({ name: "daily", cost_usd: 10, limit: "block" })],
      { allow_compaction_over_budget: true },
    );
    const block = enforceBudgetForCall(
      ledger,
      config,
      {
        provider: "openrouter",
        api_key_name: "default",
        model: "model",
        call_type: "compaction" as CallType,
        character: "Alice",
      },
      new Date("2026-05-18T12:00:00Z"),
    );
    expect(block).toBeUndefined();
    ledger.close();
  });

  it("filters by usage_kind", () => {
    const ledger = Ledger.openInMemory();
    insertCall(ledger, "2026-05-18T03:00:00+00:00", 3, "heartbeat");
    insertCall(ledger, "2026-05-18T04:00:00+00:00", 9, "message");
    const config = withBudgets([
      budget({ name: "heartbeat", cost_usd: 10, usage_kind: ["heartbeat"] }),
    ]);
    const statuses = budgetStatuses(ledger, config, new Date("2026-05-18T12:00:00Z"));
    expect(statuses[0]!.current_cost).toBe(3);
    ledger.close();
  });
});

describe("spike warnings", () => {
  it("returns nothing when disabled", () => {
    const ledger = Ledger.openInMemory();
    const config = withBudgets([]);
    expect(spikeWarnings(ledger, config, new Date()).length).toBe(0);
    ledger.close();
  });

  it("fires when the current window is N× the previous and clears min_cost_usd", () => {
    const ledger = Ledger.openInMemory();
    insertCall(ledger, "2026-05-18T10:30:00+00:00", 1, "message");
    insertCall(ledger, "2026-05-18T11:30:00+00:00", 5, "message");
    const config = withBudgets([], {
      spike_warnings: {
        enabled: true,
        period: "hour",
        multiplier: 3,
        min_cost_usd: 1,
      },
    });
    const events = spikeWarnings(ledger, config, new Date("2026-05-18T11:45:00Z"));
    expect(events.length).toBe(1);
    expect(events[0]!.current_cost).toBe(5);
    expect(events[0]!.multiplier).toBeCloseTo(5, 5);
    ledger.close();
  });
});

describe("newlyCrossedBudgetWarnings", () => {
  it("dedupes intermediate thresholds across calls", () => {
    const ledger = Ledger.openInMemory();
    insertCall(ledger, "2026-05-18T03:00:00+00:00", 8.5, "message");
    const config = withBudgets([
      budget({ name: "daily", cost_usd: 10, warn_at: [0.5, 0.8] }),
    ]);
    const now = new Date("2026-05-18T12:00:00Z");
    const first = newlyCrossedBudgetWarnings(ledger, config, now);
    expect(first.length).toBe(1);
    expect(first[0]!.crossed_warn_at).toEqual([0.5, 0.8]);

    const second = newlyCrossedBudgetWarnings(ledger, config, now);
    expect(second.length).toBe(0);
    ledger.close();
  });

  it("over-limit warning re-fires on every check", () => {
    const ledger = Ledger.openInMemory();
    insertCall(ledger, "2026-05-18T03:00:00+00:00", 12, "message");
    const config = withBudgets([
      budget({ name: "daily", cost_usd: 10, warn_at: [0.5, 0.8] }),
    ]);
    const now = new Date("2026-05-18T12:00:00Z");
    const first = newlyCrossedBudgetWarnings(ledger, config, now);
    expect(first[0]!.crossed_warn_at).toEqual([0.5, 0.8]);

    const second = newlyCrossedBudgetWarnings(ledger, config, now);
    expect(second.length).toBe(1);
    expect(second[0]!.crossed_warn_at).toEqual([1.0]);
    expect(second[0]!.current_cost).toBeGreaterThanOrEqual(second[0]!.cost_limit);

    const third = newlyCrossedBudgetWarnings(ledger, config, now);
    expect(third.length).toBe(1);
    expect(third[0]!.crossed_warn_at).toEqual([1.0]);
    ledger.close();
  });
});
