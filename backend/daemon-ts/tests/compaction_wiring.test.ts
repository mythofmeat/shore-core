/**
 * Compaction wiring tests.
 *
 * Covers the AutonomyRegistry surface that bridges the message handler
 * and the autonomy tick to `runCompaction`:
 *
 *  - `shouldCompactNow` decision matrix (mirror of Rust
 *    `AutonomyManager::should_compact_now`, manager.rs:619).
 *  - `notifyCompactionComplete` / `notifyCompactionFailed` state mutations.
 *  - `tickCharacter` idle/max-turns trigger detection (mirror of the
 *    Rust `tick_character` compaction arm, manager.rs:999-1063):
 *    + with `onIdleCompaction` wired → `runIdleCompaction=true`
 *    + without callback → `pending` flag set, consumed by `shouldCompactNow`
 *
 * The end-to-end runCompaction call path itself is already covered by
 * `compaction_background.test.ts`; this file pins the wiring around it.
 */

import { describe, expect, test } from "bun:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { AutonomyRegistry } from "../src/autonomy/registry.ts";
import {
  DEFAULT_HEARTBEAT_CONFIG,
} from "../src/autonomy/heartbeat.ts";
import { DEFAULT_COMPACTION_CONFIG } from "../src/memory/compaction/types.ts";
import type { CompactionConfig } from "../src/memory/compaction/types.ts";
import type { ConversationEngine } from "../src/engine/engine.ts";
import type { ChatRequest } from "../src/llm/types.ts";
import type { AutonomyConfig } from "../src/config/loader.ts";

function fakeEngine(name: string, messageCount = 0): ConversationEngine {
  const dir = mkdtempSync(join(tmpdir(), `compaction-wiring-${name}-`));
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

function autonomyOff(): AutonomyConfig {
  return {
    enabled: false,
    heartbeat: {
      ...DEFAULT_HEARTBEAT_CONFIG,
      enabled: false,
      maxToolRounds: 12,
      wrapUpGraceRounds: 3,
    },
  };
}

function autonomyOn(): AutonomyConfig {
  return {
    enabled: true,
    heartbeat: {
      ...DEFAULT_HEARTBEAT_CONFIG,
      enabled: false, // disable so we isolate the compaction arm
      maxToolRounds: 12,
      wrapUpGraceRounds: 3,
    },
  };
}

function compactionConfig(overrides: Partial<CompactionConfig>): CompactionConfig {
  return { ...DEFAULT_COMPACTION_CONFIG, ...overrides };
}

const sampleRequest: ChatRequest = {
  system: "sys",
  messages: [{ role: "user", content: [{ type: "text", text: "hi" }] }],
  tools: [],
  thinking: { enabled: false },
  cacheTtl: "1h",
  modelId: "m",
  apiKey: "k",
  maxTokens: 1024,
};

describe("shouldCompactNow", () => {
  test("returns false when compaction is disabled in config", () => {
    const reg = new AutonomyRegistry({
      compactionConfig: compactionConfig({ enabled: false }),
    });
    reg.ensureState(fakeEngine("a"));
    expect(reg.shouldCompactNow("a", 100, 0)).toBe(false);
  });

  test("returns false when character has no state", () => {
    const reg = new AutonomyRegistry({
      compactionConfig: compactionConfig({ enabled: true }),
    });
    expect(reg.shouldCompactNow("ghost", 100, 0)).toBe(false);
  });

  test("fires on max_turns crossing once above min_turns", () => {
    const reg = new AutonomyRegistry({
      compactionConfig: compactionConfig({
        enabled: true,
        minTurns: 4,
        maxTurns: 10,
        maxContextTokens: 0,
        keepRecentTurns: 2,
      }),
    });
    reg.ensureState(fakeEngine("a"));
    expect(reg.shouldCompactNow("a", 9, 0)).toBe(false);
    expect(reg.shouldCompactNow("a", 10, 0)).toBe(true);
  });

  test("does not fire on max_turns when below min_turns", () => {
    const reg = new AutonomyRegistry({
      compactionConfig: compactionConfig({
        enabled: true,
        minTurns: 20,
        maxTurns: 10, // misconfigured (max < min) → sanitize disables
        maxContextTokens: 0,
        keepRecentTurns: 2,
      }),
    });
    reg.ensureState(fakeEngine("a"));
    // Sanitizer disabled compaction; threshold can't fire.
    expect(reg.shouldCompactNow("a", 100, 0)).toBe(false);
  });

  test("fires on max_context_tokens when above min_turns", () => {
    const reg = new AutonomyRegistry({
      compactionConfig: compactionConfig({
        enabled: true,
        minTurns: 4,
        maxTurns: 1_000, // far away
        maxContextTokens: 10_000,
        keepRecentTurns: 2,
      }),
    });
    reg.ensureState(fakeEngine("a"));
    expect(reg.shouldCompactNow("a", 5, 9_999)).toBe(false);
    expect(reg.shouldCompactNow("a", 5, 10_000)).toBe(true);
  });

  test("ignores max_context_tokens when configured to 0", () => {
    const reg = new AutonomyRegistry({
      compactionConfig: compactionConfig({
        enabled: true,
        minTurns: 4,
        maxTurns: 1_000,
        maxContextTokens: 0,
        keepRecentTurns: 2,
      }),
    });
    reg.ensureState(fakeEngine("a"));
    expect(reg.shouldCompactNow("a", 5, 999_999)).toBe(false);
  });

  test("sets triggered flag, so the next tick won't double-fire", () => {
    const reg = new AutonomyRegistry({
      autonomyConfig: autonomyOn(),
      compactionConfig: compactionConfig({
        enabled: true,
        minTurns: 4,
        maxTurns: 10,
        idleTriggerSecs: 0,
        keepRecentTurns: 2,
      }),
    });
    reg.ensureState(fakeEngine("a", 12));
    expect(reg.shouldCompactNow("a", 12, 0)).toBe(true);

    // Tick after trigger: arm should NOT re-fire because `triggered` is set.
    const actions = reg.tickCharacter("a");
    expect(actions.runIdleCompaction).toBe(false);
  });
});

describe("notifyCompactionComplete / notifyCompactionFailed", () => {
  test("notifyCompactionComplete clears triggered + pending and invalidates cached request", () => {
    const reg = new AutonomyRegistry({
      compactionConfig: compactionConfig({
        enabled: true,
        minTurns: 4,
        maxTurns: 10,
        keepRecentTurns: 2,
      }),
    });
    reg.ensureState(fakeEngine("a"));
    reg.notifyLastRequest("a", sampleRequest);
    expect(reg.cachedLastRequest("a")).not.toBeUndefined();

    expect(reg.shouldCompactNow("a", 10, 0)).toBe(true);
    reg.notifyCompactionComplete("a", 3);

    // Cached request gone after compaction (message tail no longer matches).
    expect(reg.cachedLastRequest("a")).toBeUndefined();
    // Trigger flag cleared — subsequent should_compact below max_turns is false.
    expect(reg.shouldCompactNow("a", 5, 0)).toBe(false);
  });

  test("notifyCompactionFailed clears triggered for retry but keeps cached request", () => {
    const reg = new AutonomyRegistry({
      compactionConfig: compactionConfig({
        enabled: true,
        minTurns: 4,
        maxTurns: 10,
        keepRecentTurns: 2,
      }),
    });
    reg.ensureState(fakeEngine("a"));
    reg.notifyLastRequest("a", sampleRequest);

    expect(reg.shouldCompactNow("a", 10, 0)).toBe(true);
    reg.notifyCompactionFailed("a");

    // Failed attempt does NOT invalidate cached request — the chat tail
    // is unchanged.
    expect(reg.cachedLastRequest("a")).not.toBeUndefined();
    // Trigger flag cleared; same condition fires again on retry.
    expect(reg.shouldCompactNow("a", 10, 0)).toBe(true);
  });

  test("notifyCompactionComplete is a no-op for unknown characters", () => {
    const reg = new AutonomyRegistry({
      compactionConfig: compactionConfig({ enabled: true }),
    });
    // Doesn't throw.
    reg.notifyCompactionComplete("ghost", 5);
    reg.notifyCompactionFailed("ghost");
  });
});

describe("tickCharacter compaction trigger", () => {
  test("fires when active_turn_count crosses max_turns and onIdleCompaction is wired", async () => {
    let now = 0;
    const called: string[] = [];
    const reg = new AutonomyRegistry({
      autonomyConfig: autonomyOn(),
      compactionConfig: compactionConfig({
        enabled: true,
        minTurns: 4,
        maxTurns: 10,
        idleTriggerSecs: 0,
        keepRecentTurns: 2,
      }),
      nowMs: () => now,
      onIdleCompaction: (name) => {
        called.push(name);
      },
    });
    reg.ensureState(fakeEngine("a", 12));
    const actions = reg.tickCharacter("a");
    expect(actions.runIdleCompaction).toBe(true);
  });

  test("idle trigger fires after idle_trigger_secs elapses with min_turns met", () => {
    let now = 0;
    const reg = new AutonomyRegistry({
      autonomyConfig: autonomyOn(),
      compactionConfig: compactionConfig({
        enabled: true,
        minTurns: 4,
        maxTurns: 1_000,
        maxContextTokens: 0,
        idleTriggerSecs: 30,
        keepRecentTurns: 2,
      }),
      nowMs: () => now,
      onIdleCompaction: () => undefined,
    });
    reg.ensureState(fakeEngine("a", 8));

    // Just below threshold: no fire.
    now = 29_000;
    expect(reg.tickCharacter("a").runIdleCompaction).toBe(false);

    // At threshold: fires.
    now = 30_500;
    expect(reg.tickCharacter("a").runIdleCompaction).toBe(true);
  });

  test("idle trigger does NOT fire when active_turn_count is below min_turns", () => {
    let now = 0;
    const reg = new AutonomyRegistry({
      autonomyConfig: autonomyOn(),
      compactionConfig: compactionConfig({
        enabled: true,
        minTurns: 10,
        maxTurns: 100,
        maxContextTokens: 0,
        idleTriggerSecs: 30,
        keepRecentTurns: 2,
      }),
      nowMs: () => now,
      onIdleCompaction: () => undefined,
    });
    reg.ensureState(fakeEngine("a", 5));

    now = 60_000;
    expect(reg.tickCharacter("a").runIdleCompaction).toBe(false);
  });

  test("without onIdleCompaction wired, trigger sets pending instead", () => {
    let now = 0;
    const reg = new AutonomyRegistry({
      autonomyConfig: autonomyOn(),
      compactionConfig: compactionConfig({
        enabled: true,
        minTurns: 4,
        maxTurns: 10,
        idleTriggerSecs: 0,
        keepRecentTurns: 2,
      }),
      nowMs: () => now,
      // no onIdleCompaction
    });
    reg.ensureState(fakeEngine("a", 12));

    const actions = reg.tickCharacter("a");
    expect(actions.runIdleCompaction).toBe(false);

    // shouldCompactNow with a turn count below max_turns falls through to
    // the pending check, consumes the flag set by the tick, returns true.
    // A second call with the same shape returns false (pending is one-shot).
    expect(reg.shouldCompactNow("a", 5, 0)).toBe(true);
    expect(reg.shouldCompactNow("a", 5, 0)).toBe(false);
  });

  test("compaction arm stays inert when autonomy.enabled is false", () => {
    const reg = new AutonomyRegistry({
      autonomyConfig: autonomyOff(),
      compactionConfig: compactionConfig({
        enabled: true,
        minTurns: 4,
        maxTurns: 10,
        keepRecentTurns: 2,
      }),
      onIdleCompaction: () => undefined,
    });
    reg.ensureState(fakeEngine("a", 12));
    expect(reg.tickCharacter("a").runIdleCompaction).toBe(false);
  });

  test("user message updates active_turn_count + idle anchor so trigger uses fresh state", () => {
    let now = 0;
    const reg = new AutonomyRegistry({
      autonomyConfig: autonomyOn(),
      compactionConfig: compactionConfig({
        enabled: true,
        minTurns: 4,
        maxTurns: 1_000,
        maxContextTokens: 0,
        idleTriggerSecs: 30,
        keepRecentTurns: 2,
      }),
      nowMs: () => now,
      onIdleCompaction: () => undefined,
    });
    reg.ensureState(fakeEngine("a", 5));

    // Past threshold from ensureState anchor.
    now = 31_000;
    // But a user message at t=30 resets the anchor.
    now = 30_000;
    reg.notifyUserMessage("a", 6);

    // Just 5s later, not idle yet.
    now = 35_000;
    expect(reg.tickCharacter("a").runIdleCompaction).toBe(false);

    // Now 31s after the user message — fires.
    now = 61_000;
    expect(reg.tickCharacter("a").runIdleCompaction).toBe(true);
  });
});
