import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { CacheForensics } from "../src/ledger/cache_forensics.ts";
import { CacheTracker } from "../src/ledger/cache_tracker.ts";
import { type CallRow, Ledger } from "../src/ledger/ledger.ts";
import { parseLastPeriodAt, usagePayload } from "../src/ledger/usage.ts";

function sampleRow(overrides: Partial<CallRow> = {}): CallRow {
  return {
    ts: "2026-04-05T12:00:00Z",
    character: "aria",
    provider: "anthropic",
    api_key_name: "default",
    model: "claude-opus-4-6",
    call_type: "message",
    input_tokens: 100,
    output_tokens: 50,
    cache_read_tokens: 80,
    cache_write_tokens: 20,
    cache_ttl: "1h",
    total_ms: 1500,
    ttft_ms: 200,
    finish_reason: "end_turn",
    thinking_enabled: true,
    cache_state: "warm",
    cost_source: "pricing_catalog",
    ...overrides,
  };
}

describe("Ledger", () => {
  it("inserts and reads rows with Rust-compatible columns", () => {
    const ledger = Ledger.openInMemory();
    const id = ledger.insert(sampleRow());
    expect(id).toBe(1);

    const rows = ledger.recent(1);
    expect(rows).toHaveLength(1);
    expect(rows[0]?.character).toBe("aria");
    expect(rows[0]?.api_key_name).toBe("default");
    expect(rows[0]?.cache_ttl).toBe("1h");
    expect(rows[0]?.cost_source).toBe("pricing_catalog");
    ledger.close();
  });

  it("records cache anomalies and response-side forensics", () => {
    const ledger = Ledger.openInMemory();
    const cacheDir = mkdtempSync(path.join(tmpdir(), "shore-forensics-test-"));
    const forensics = CacheForensics.open(cacheDir);

    ledger.recordCall(
      {
        provider: "anthropic",
        model: "claude-opus-4-6",
        callType: "message",
        character: "aria",
        inputTokens: 100,
        outputTokens: 10,
        cacheReadTokens: 500,
        cacheWriteTokens: 0,
        totalMs: 40,
        ttftMs: 4,
        finishReason: "end_turn",
        thinkingEnabled: true,
        cacheTtl: "1h",
        ts: "2026-04-05T12:00:00Z",
      },
      forensics,
    );

    ledger.recordCall(
      {
        provider: "anthropic",
        model: "claude-opus-4-6",
        callType: "message",
        character: "aria",
        inputTokens: 120,
        outputTokens: 12,
        cacheReadTokens: 100,
        cacheWriteTokens: 400,
        totalMs: 50,
        ttftMs: 5,
        finishReason: "end_turn",
        thinkingEnabled: true,
        cacheTtl: "1h",
        ts: "2026-04-05T12:01:00Z",
      },
      forensics,
    );

    const rows = ledger.recent(2);
    expect(rows[0]?.cache_anomaly).toBe("unexpected_write");
    expect(rows[0]?.cache_state).toBe("cold");

    const forensicLines = fs
      .readFileSync(path.join(cacheDir, "cache_forensics.jsonl"), "utf8")
      .trim()
      .split("\n")
      .map((line) => JSON.parse(line) as Record<string, unknown>);
    expect(forensicLines).toHaveLength(2);
    expect(forensicLines[1]?.type).toBe("response");
    expect(forensicLines[1]?.cache_creation_tokens).toBe(400);
    ledger.close();
  });

  it("summarizes by usage kind, call type, and api key", () => {
    const ledger = Ledger.openInMemory();
    ledger.insert(sampleRow({ finish_reason: "tool_use", total_cost: 0.01 }));
    ledger.insert(sampleRow({
      ts: "2026-04-05T12:01:00Z",
      call_type: "tool_loop",
      api_key_name: "overflow",
      input_tokens: 200,
      total_cost: 0.02,
    }));
    ledger.insert(sampleRow({
      ts: "2026-04-05T12:02:00Z",
      provider: "openai",
      model: "gpt-4o",
      finish_reason: "end_turn",
      cache_read_tokens: 0,
      cache_write_tokens: 0,
      total_cost: 0.03,
    }));

    const kindPayload = usagePayload(ledger, { last: "all", by_kind: true }, {
      timezone: "utc",
      allow_compaction_over_budget: true,
    });
    expect(kindPayload["mode"]).toBe("summary_by_usage_kind");
    const byKind = new Map(
      (kindPayload["summary"] as Array<Record<string, unknown>>).map((row) => [
        row["usage_kind"],
        row["call_count"],
      ]),
    );
    expect(byKind.get("message_with_tools")).toBe(2);
    expect(byKind.get("message_no_tools")).toBe(1);

    const byCallType = ledger.usageSummaryByCallType();
    expect(byCallType.find((row) => row.call_type === "message")?.call_count).toBe(2);
    expect(ledger.usageSummaryByApiKey().find((row) => row.api_key_name === "overflow")?.call_count).toBe(1);
    expect(ledger.exportTsv().split("\n")[0]).toContain("cache_ttl");
    ledger.close();
  });
});

describe("CacheTracker", () => {
  it("detects keepalive misses after TTL expiry", () => {
    const tracker = CacheTracker.withTtlSecs(60);
    tracker.observe({
      ts: "2026-04-05T12:00:00Z",
      model: "claude-opus-4-6",
      thinkingEnabled: true,
      cacheReadTokens: 0,
      cacheWriteTokens: 500,
      callType: "message",
    });
    const result = tracker.observe({
      ts: "2026-04-05T12:02:00Z",
      model: "claude-opus-4-6",
      thinkingEnabled: true,
      cacheReadTokens: 0,
      cacheWriteTokens: 500,
      callType: "message",
    });
    expect(result.anomaly).toBe("keepalive_miss");
    expect(tracker.state()).toBe("warm");
  });
});

describe("usage period parsing", () => {
  const fixed = new Date("2026-05-13T12:30:00Z");

  it("accepts relative hour ranges", () => {
    expect(parseLastPeriodAt("4h", fixed, "utc")).toBe("2026-05-13T08:30:00+00:00");
  });

  it("uses UTC calendar starts for named windows", () => {
    expect(parseLastPeriodAt("today", fixed, "utc")).toBe("2026-05-13T00:00:00+00:00");
    expect(parseLastPeriodAt("week", fixed, "utc")).toBe("2026-05-11T00:00:00+00:00");
    expect(parseLastPeriodAt("month", fixed, "utc")).toBe("2026-05-01T00:00:00+00:00");
  });
});
