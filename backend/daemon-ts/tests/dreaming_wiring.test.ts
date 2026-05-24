/**
 * Dreaming wiring tests.
 *
 * Covers:
 *  - `isDueNow` cron-gate evaluation (mirror of Rust
 *    `memory/dreaming.rs::is_due` + the test block at
 *    `due_and_schedule_validation_still_work` /
 *    `weekly_cron_due_checks_catch_up_once_per_occurrence`).
 *  - AutonomyRegistry's dreaming backoff state machine:
 *    `notifyDreamingSuccess` / `notifyDreamingFailed` plus the
 *    `tickCharacter` dreaming arm gating on enable flags and the
 *    `nextAttemptAtMs` backoff window.
 *
 * End-to-end runLibrarianSweep coverage already lives in
 * `dreaming.test.ts`; this file pins the wiring around it (in particular
 * the autonomy-side gating that previously didn't exist at all).
 */

import { describe, expect, test } from "bun:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { AutonomyRegistry } from "../src/autonomy/registry.ts";
import { DEFAULT_HEARTBEAT_CONFIG } from "../src/autonomy/heartbeat.ts";
import { isDueNow } from "../src/memory/dreaming_schedule.ts";
import { defaultDreamingConfig } from "../src/memory/dreaming.ts";
import type { ConversationEngine } from "../src/engine/engine.ts";
import type { AutonomyConfig } from "../src/config/loader.ts";
import type { DreamingConfig } from "../src/memory/dreaming.ts";

function fakeEngine(name: string, messageCount = 0): ConversationEngine {
  const dir = mkdtempSync(join(tmpdir(), `dreaming-wiring-${name}-`));
  return {
    name: () => name,
    dataDir: () => dir,
    messageCount: () => messageCount,
    historySnapshot: () => ({
      messages: [],
      active_start: 0,
      selected_character: name,
      revision: 0,
    }),
  } as unknown as ConversationEngine;
}

function autonomyOn(): AutonomyConfig {
  return {
    enabled: true,
    heartbeat: {
      ...DEFAULT_HEARTBEAT_CONFIG,
      enabled: false,
      maxToolRounds: 12,
      wrapUpGraceRounds: 3,
    },
  };
}

function dreamingEnabled(overrides: Partial<DreamingConfig> = {}): DreamingConfig {
  return { ...defaultDreamingConfig(), enabled: true, ...overrides };
}

// ─── isDueNow ──────────────────────────────────────────────────────────

describe("isDueNow", () => {
  test("never-ran is due for a sane recurring schedule", () => {
    expect(isDueNow("0 3 * * *", undefined)).toBe(true);
    expect(isDueNow("*/5 * * * *", undefined)).toBe(true);
  });

  test("invalid cron string returns false (no throw)", () => {
    expect(isDueNow("not a real cron", undefined)).toBe(false);
    expect(isDueNow("totally bogus", "2026-05-24T00:00:00Z")).toBe(false);
  });

  test("malformed lastRunAt falls back to never-ran behavior", () => {
    expect(isDueNow("0 3 * * *", "this is not a date")).toBe(true);
  });

  test("daily cron: due if last run was before today's scheduled window", () => {
    // Daily at 3am.
    const now = new Date("2026-05-24T10:00:00Z");
    // Last run yesterday at 3am — today's 3am has passed, due.
    expect(
      isDueNow("0 3 * * *", "2026-05-23T03:00:00Z", now),
    ).toBe(true);
    // Last run today at 3am — next would be tomorrow, not due.
    expect(
      isDueNow("0 3 * * *", "2026-05-24T03:00:00Z", now),
    ).toBe(false);
  });

  test("weekly cron: only catches up once per occurrence", () => {
    // Mondays at 6am (Mon = day-of-week 1).
    const freq = "0 6 * * 1";
    const beforeMonday = new Date("2026-05-23T05:00:00Z"); // Sat
    const afterMonday = new Date("2026-05-25T07:00:00Z"); // Mon, post-trigger
    const tuesday = new Date("2026-05-26T12:00:00Z");
    const alreadyRanMonday = "2026-05-25T06:00:00Z";
    const ranLastWeek = "2026-05-18T06:00:00Z";

    expect(isDueNow(freq, undefined, beforeMonday)).toBe(true); // never ran, schedule has past
    expect(isDueNow(freq, undefined, afterMonday)).toBe(true);
    expect(isDueNow(freq, alreadyRanMonday, tuesday)).toBe(false); // ran this week's monday
    expect(isDueNow(freq, ranLastWeek, tuesday)).toBe(true); // last ran last week
  });
});

// ─── AutonomyRegistry dreaming wiring ─────────────────────────────────

describe("AutonomyRegistry dreaming state", () => {
  test("notifyDreamingSuccess clears failure count + next attempt", () => {
    const reg = new AutonomyRegistry({
      autonomyConfig: autonomyOn(),
      dreamingConfig: dreamingEnabled(),
    });
    reg.ensureState(fakeEngine("a"));

    reg.notifyDreamingFailed("a");
    reg.notifyDreamingFailed("a");
    let s = reg.dreamingState("a")!;
    expect(s.failureCount).toBe(2);
    expect(s.nextAttemptAtMs).toBeGreaterThan(0);

    reg.notifyDreamingSuccess("a");
    s = reg.dreamingState("a")!;
    expect(s.failureCount).toBe(0);
    expect(s.nextAttemptAtMs).toBeUndefined();
    expect(s.running).toBe(false);
  });

  test("notifyDreamingFailed backs off exponentially", () => {
    let now = 1_000_000;
    const reg = new AutonomyRegistry({
      autonomyConfig: autonomyOn(),
      dreamingConfig: dreamingEnabled(),
      nowMs: () => now,
    });
    reg.ensureState(fakeEngine("a"));

    reg.notifyDreamingFailed("a");
    expect(reg.dreamingState("a")!.nextAttemptAtMs).toBe(now + 60_000);

    reg.notifyDreamingFailed("a");
    expect(reg.dreamingState("a")!.nextAttemptAtMs).toBe(now + 120_000);

    reg.notifyDreamingFailed("a");
    expect(reg.dreamingState("a")!.nextAttemptAtMs).toBe(now + 240_000);

    // Caps at 1 hour after enough failures (60 * 2^6 = 3840s > 3600 cap).
    for (let i = 0; i < 10; i++) reg.notifyDreamingFailed("a");
    expect(reg.dreamingState("a")!.nextAttemptAtMs).toBe(now + 3_600_000);
  });
});

describe("tickCharacter dreaming arm", () => {
  test("fires runScheduledDream when autonomy+dreaming enabled and callback wired", () => {
    let now = 1_000_000;
    const reg = new AutonomyRegistry({
      autonomyConfig: autonomyOn(),
      dreamingConfig: dreamingEnabled(),
      nowMs: () => now,
      onScheduledDream: () => undefined,
    });
    reg.ensureState(fakeEngine("a"));

    const actions = reg.tickCharacter("a");
    expect(actions.runScheduledDream).toBe(true);
    // running is set so the next tick won't re-fire.
    expect(reg.dreamingState("a")!.running).toBe(true);

    const second = reg.tickCharacter("a");
    expect(second.runScheduledDream).toBe(false);
  });

  test("does NOT fire when autonomy is disabled", () => {
    const reg = new AutonomyRegistry({
      autonomyConfig: { enabled: false, heartbeat: autonomyOn().heartbeat },
      dreamingConfig: dreamingEnabled(),
      onScheduledDream: () => undefined,
    });
    reg.ensureState(fakeEngine("a"));
    expect(reg.tickCharacter("a").runScheduledDream).toBe(false);
  });

  test("does NOT fire when dreaming is disabled", () => {
    const reg = new AutonomyRegistry({
      autonomyConfig: autonomyOn(),
      dreamingConfig: { ...defaultDreamingConfig(), enabled: false },
      onScheduledDream: () => undefined,
    });
    reg.ensureState(fakeEngine("a"));
    expect(reg.tickCharacter("a").runScheduledDream).toBe(false);
  });

  test("does NOT fire when no onScheduledDream callback is wired", () => {
    const reg = new AutonomyRegistry({
      autonomyConfig: autonomyOn(),
      dreamingConfig: dreamingEnabled(),
      // no callback
    });
    reg.ensureState(fakeEngine("a"));
    expect(reg.tickCharacter("a").runScheduledDream).toBe(false);
  });

  test("does NOT fire during the backoff window after a failure", () => {
    let now = 1_000_000;
    const reg = new AutonomyRegistry({
      autonomyConfig: autonomyOn(),
      dreamingConfig: dreamingEnabled(),
      nowMs: () => now,
      onScheduledDream: () => undefined,
    });
    reg.ensureState(fakeEngine("a"));
    reg.notifyDreamingFailed("a"); // sets nextAttempt = now + 60s

    expect(reg.tickCharacter("a").runScheduledDream).toBe(false);

    now += 30_000; // 30s later, still inside the window
    expect(reg.tickCharacter("a").runScheduledDream).toBe(false);

    now += 31_000; // past the 60s backoff
    expect(reg.tickCharacter("a").runScheduledDream).toBe(true);
  });
});
