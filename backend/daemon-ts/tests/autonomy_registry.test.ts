/**
 * AutonomyRegistry × set_next_wake integration tests.
 *
 * Phase 8b: the registry now owns a HeartbeatClock per character and
 * exposes `scheduleNextWake()` as the implementation behind the
 * `ctx.scheduleNextWake` hook that `set_next_wake` consumes. There is no
 * production ticker yet — the only driver is this test, which simulates
 * a heartbeat-context tool dispatch.
 */

import { describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { AutonomyRegistry } from "../src/autonomy/registry.ts";
import { CacheKeepaliveAction } from "../src/autonomy/cache_keepalive.ts";
import {
  DEFAULT_HEARTBEAT_CONFIG,
  HeartbeatAction,
} from "../src/autonomy/heartbeat.ts";
import { DEFAULT_COMPACTION_CONFIG } from "../src/memory/compaction/types.ts";
import { setNextWakeHandler } from "../src/tools/basic.ts";
import { ToolError } from "../src/tools/registry.ts";
import type { CompactionConfig } from "../src/memory/compaction/types.ts";
import type { AutonomyConfig, LoadedConfig } from "../src/config/loader.ts";
import type { ConversationEngine } from "../src/engine/engine.ts";
import type { ToolContext } from "../src/tools/registry.ts";

function fakeEngine(name: string): ConversationEngine {
  const dir = mkdtempSync(join(tmpdir(), `autonomy-reg-test-${name}-`));
  return {
    name: () => name,
    dataDir: () => dir,
    messageCount: () => 0,
    historySnapshot: () => ({
      messages: [],
      active_start: 0,
      selected_character: name,
      revision: 0,
    }),
  } as unknown as ConversationEngine;
}

function fakeEngineAt(name: string, dir: string): ConversationEngine {
  return {
    name: () => name,
    dataDir: () => dir,
    messageCount: () => 0,
    historySnapshot: () => ({
      messages: [],
      active_start: 0,
      selected_character: name,
      revision: 0,
    }),
  } as unknown as ConversationEngine;
}

function fakeContext(
  charName: string,
  scheduleNextWake: ToolContext["scheduleNextWake"],
): ToolContext {
  return {
    characterName: charName,
    characterConfigDir: "/tmp/cfg",
    characterDataDir: "/tmp/data",
    workspaceDir: "/tmp/cfg/workspace",
    configDir: "/tmp/cfg",
    imageDir: "/tmp/images",
    engine: {} as ToolContext["engine"],
    searchConfig: { providerOrder: [] } as unknown as ToolContext["searchConfig"],
    retrievalConfig: {} as unknown as ToolContext["retrievalConfig"],
    scheduleNextWake,
  };
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
      enabled: false,
      maxToolRounds: 12,
      wrapUpGraceRounds: 3,
    },
  };
}

function autonomyHeartbeatOn(
  heartbeat: Partial<AutonomyConfig["heartbeat"]> = {},
): AutonomyConfig {
  return {
    enabled: true,
    heartbeat: {
      ...DEFAULT_HEARTBEAT_CONFIG,
      enabled: true,
      fallbackHeartbeatIntervalSecs: 60,
      dormantAfterHeartbeatTurns: 3,
      dormantAfterIdleTimeSecs: 7200,
      minimumHeartbeatLatencySecs: 3600,
      maxToolRounds: 12,
      wrapUpGraceRounds: 3,
      ...heartbeat,
    },
  };
}

function compactionConfig(overrides: Partial<CompactionConfig>): CompactionConfig {
  return { ...DEFAULT_COMPACTION_CONFIG, ...overrides };
}

function loadedConfig(
  autonomy: AutonomyConfig,
  compaction: CompactionConfig,
): LoadedConfig {
  return {
    app: {
      defaults: {
        model: undefined,
        embedding: undefined,
        display_name: undefined,
      },
      behavior: { autonomy },
      advanced: { cache_forensics: false },
      usage: {},
    },
    embedding: {},
    memory: {
      compaction,
      dreaming: {
        enabled: false,
        frequency: "0 3 * * *",
        max_tool_rounds: 12,
      },
      retrieval: {},
    },
  } as unknown as LoadedConfig;
}

describe("AutonomyRegistry × set_next_wake", () => {
  test("set_next_wake throws when no autonomy state exists for the character", async () => {
    const registry = new AutonomyRegistry();
    const ctx = fakeContext("ghost-char", (hours, reason) =>
      registry.scheduleNextWake("ghost-char", hours, reason),
    );
    await expect(
      setNextWakeHandler.execute(
        { hours_from_now: 6, reason: "just because" },
        ctx,
      ),
    ).rejects.toThrow(/no autonomy state/);
  });

  test("set_next_wake via registry mutates the clock with the clamped wake-time", async () => {
    const registry = new AutonomyRegistry();
    const engine = fakeEngine("alice");
    registry.ensureState(engine);

    const ctx = fakeContext("alice", (hours, reason) =>
      registry.scheduleNextWake("alice", hours, reason),
    );

    const before = performance.now();
    const result = await setNextWakeHandler.execute(
      { hours_from_now: 6, reason: "feel like sleeping in" },
      ctx,
    );
    const after = performance.now();

    // Wire-shape parity with Rust: tool output is the JSON-stringified
    // format string (note the surrounding quotes from JSON.stringify).
    expect(result).toBe('"Scheduled next moment in 6.0 hours."');

    const clock = registry.heartbeatClock("alice");
    expect(clock).not.toBeUndefined();
    const wake = clock!.nextWake();
    expect(wake).not.toBeUndefined();
    // wake = scheduledNow + 6h, where scheduledNow ∈ [before, after].
    const sixHoursMs = 6 * 3600 * 1000;
    expect(wake!).toBeGreaterThanOrEqual(before + sixHoursMs);
    expect(wake!).toBeLessThanOrEqual(after + sixHoursMs);
  });

  test("set_next_wake clamps below-minimum requests to 1 hour", async () => {
    const registry = new AutonomyRegistry();
    const engine = fakeEngine("bob");
    registry.ensureState(engine);

    const ctx = fakeContext("bob", (hours, reason) =>
      registry.scheduleNextWake("bob", hours, reason),
    );

    // Tool-layer clamps to 1.0; clock then sees 1h.
    const before = performance.now();
    const result = await setNextWakeHandler.execute(
      { hours_from_now: 0.1, reason: "too soon" },
      ctx,
    );
    const after = performance.now();

    expect(result).toBe('"Scheduled next moment in 1.0 hours."');

    const wake = registry.heartbeatClock("bob")!.nextWake()!;
    const oneHourMs = 3600 * 1000;
    expect(wake).toBeGreaterThanOrEqual(before + oneHourMs);
    expect(wake).toBeLessThanOrEqual(after + oneHourMs);
  });

  test("notifyUserMessage advances the heartbeat clock's last-user timestamp", () => {
    const registry = new AutonomyRegistry();
    const engine = fakeEngine("carol");
    registry.ensureState(engine);

    const clock = registry.heartbeatClock("carol")!;
    expect(clock.lastUserAt()).toBeUndefined();

    registry.notifyUserMessage("carol");
    expect(clock.lastUserAt()).not.toBeUndefined();
    expect(clock.ticksWithoutUser()).toBe(0);
    expect(registry.cacheKeepalive("carol")!.nextWakeAt()).toBe(clock.nextWake());
  });

  test("set_next_wake without a heartbeat context hook still rejects at the tool layer", async () => {
    // Sanity: the 4c.2 refusal path should remain intact even with the
    // registry in place. Build a ctx with scheduleNextWake explicitly
    // undefined.
    const ctx = fakeContext("nobody", undefined);
    await expect(
      setNextWakeHandler.execute(
        { hours_from_now: 2, reason: "..." },
        ctx,
      ),
    ).rejects.toBeInstanceOf(ToolError);
  });

  test("tickCharacter drives the heartbeat clock and persists state", () => {
    let now = 10_000;
    const wall = new Date("2026-05-24T00:00:00.000Z");
    const registry = new AutonomyRegistry({
      autonomyConfig: {
        enabled: true,
        heartbeat: {
          enabled: true,
          fallbackHeartbeatIntervalSecs: 60,
          dormantAfterHeartbeatTurns: 3,
          dormantAfterIdleTimeSecs: 7200,
          minimumHeartbeatLatencySecs: 3600,
          maxToolRounds: 12,
          wrapUpGraceRounds: 3,
        },
      },
      nowMs: () => now,
      wallNow: () => wall,
    });
    const dir = mkdtempSync(join(tmpdir(), "autonomy-state-test-"));
    registry.ensureState(fakeEngineAt("dana", dir));

    expect(registry.tickCharacter("dana")).toEqual({
      heartbeat: HeartbeatAction.None,
      keepalive: CacheKeepaliveAction.None,
      guardTripped: false,
      runIdleCompaction: false,
      runScheduledDream: false,
    });
    now += 61_000;
    expect(registry.tickCharacter("dana").heartbeat).toBe(HeartbeatAction.RunTick);
    registry.saveState("dana");

    const file = join(dir, "autonomy_state.json");
    expect(existsSync(file)).toBe(true);
    const persisted = JSON.parse(readFileSync(file, "utf8")) as Record<string, unknown>;
    expect(persisted.version).toBe(4);
    expect(persisted.ticks_without_user).toBe(1);
  });

  test("ensureState restores persisted heartbeat deadlines and primes keepalive", () => {
    const dir = mkdtempSync(join(tmpdir(), "autonomy-restore-test-"));
    writeFileSync(
      join(dir, "autonomy_state.json"),
      JSON.stringify({
        version: 4,
        ticks_without_user: 2,
        next_wake_at: "2026-05-24T02:00:00.000Z",
        last_user_at: "2026-05-24T00:30:00.000Z",
      }),
    );
    const registry = new AutonomyRegistry({
      nowMs: () => 1_000_000,
      wallNow: () => new Date("2026-05-24T00:00:00.000Z"),
    });
    registry.ensureState(fakeEngineAt("erin", dir));

    const clock = registry.heartbeatClock("erin")!;
    expect(clock.ticksWithoutUser()).toBe(2);
    expect(clock.nextWake()).toBe(1_000_000 + 2 * 3600 * 1000);
    expect(clock.lastUserAt()).toBe(1_000_000 + 30 * 60 * 1000);
    expect(registry.cacheKeepalive("erin")!.nextWakeAt()).toBe(clock.nextWake());
    expect(registry.cacheKeepalive("erin")!.nextPingAt()).not.toBeUndefined();
  });
});

describe("AutonomyRegistry Phase 9b orchestration surface", () => {
  test("setResources accepts runtime dependencies and reloadRuntimeConfig swaps autonomy + compaction config", () => {
    let now = 0;
    const registry = new AutonomyRegistry({
      autonomyConfig: autonomyOff(),
      compactionConfig: compactionConfig({ enabled: false }),
      nowMs: () => now,
    });
    registry.ensureState(fakeEngine("runtime"));

    registry.heartbeatClock("runtime")!.forceWake(now);
    expect(registry.tickCharacter("runtime").heartbeat).toBe(HeartbeatAction.None);
    expect(registry.shouldCompactNow("runtime", 12, 0)).toBe(false);

    const nextConfig = loadedConfig(
      autonomyHeartbeatOn(),
      compactionConfig({
        enabled: true,
        minTurns: 4,
        maxTurns: 10,
        maxContextTokens: 0,
        keepRecentTurns: 2,
      }),
    );
    registry.setResources(
      { stream: async function* () {} },
      { broadcast: () => undefined },
      nextConfig,
      { notify: () => undefined },
    );
    registry.reloadRuntimeConfig(nextConfig);

    expect(registry.heartbeatTickNow("runtime")).toBe(false);
    expect(registry.tickCharacter("runtime", now).heartbeat).toBe(HeartbeatAction.RunTick);
    expect(registry.shouldCompactNow("runtime", 12, 0)).toBe(true);
  });

  test("notifyAssistantMessage re-anchors idle compaction activity", () => {
    let now = 0;
    const registry = new AutonomyRegistry({
      autonomyConfig: autonomyOn(),
      compactionConfig: compactionConfig({
        enabled: true,
        minTurns: 4,
        maxTurns: 100,
        maxContextTokens: 0,
        idleTriggerSecs: 30,
        keepRecentTurns: 2,
      }),
      nowMs: () => now,
      onIdleCompaction: () => undefined,
    });
    registry.ensureState(fakeEngine("assistant"));

    now = 25_000;
    registry.notifyAssistantMessage("assistant", 8);

    now = 54_000;
    expect(registry.tickCharacter("assistant").runIdleCompaction).toBe(false);

    now = 56_000;
    expect(registry.tickCharacter("assistant").runIdleCompaction).toBe(true);
  });

  test("heartbeat debug methods drive clock state and respect paused guard", () => {
    let now = 1_000;
    const registry = new AutonomyRegistry({
      autonomyConfig: autonomyHeartbeatOn(),
      nowMs: () => now,
      wallNow: () => new Date("2026-05-24T00:00:00.000Z"),
    });
    registry.ensureState(fakeEngine("debug"));

    expect(registry.heartbeatTickNow("ghost")).toBeUndefined();
    expect(registry.heartbeatSetDormant("ghost")).toBe(false);
    expect(registry.heartbeatSetActive("ghost")).toBe(false);
    expect(registry.setPaused("ghost", true)).toBeUndefined();

    expect(registry.heartbeatTickNow("debug")).toBe(false);
    expect(registry.tickCharacter("debug", now).heartbeat).toBe(HeartbeatAction.RunTick);

    expect(registry.heartbeatSetDormant("debug")).toBe(true);
    expect(registry.status("debug")!.heartbeat_state).toBe("Dormant");
    expect(registry.heartbeatTickNow("debug")).toBe(true);
    const dormantTick = registry.tickCharacter("debug", now);
    expect(dormantTick.heartbeat).toBe(HeartbeatAction.None);
    expect(dormantTick.guardTripped).toBe(true);

    expect(registry.heartbeatSetActive("debug")).toBe(true);
    expect(registry.status("debug")!.heartbeat_state).toBe("Active");
    expect(registry.setPaused("debug", true)).toBe(true);
    expect(registry.heartbeatTickNow("debug")).toBe(false);
    expect(registry.tickCharacter("debug", now).heartbeat).toBe(HeartbeatAction.None);
    expect(registry.status("debug")!.paused).toBe(true);

    expect(registry.setPaused("debug", false)).toBe(false);
    now += 1;
    expect(registry.tickCharacter("debug", now).heartbeat).toBe(HeartbeatAction.RunTick);
  });

  test("status mirrors Rust AutonomyStatus wire fields and recent heartbeat events", () => {
    const registry = new AutonomyRegistry({
      autonomyConfig: autonomyHeartbeatOn(),
      nowMs: () => 1_000,
      wallNow: () => new Date("2026-05-24T00:00:00.000Z"),
    });
    registry.ensureState(fakeEngine("status"));
    registry.scheduleNextWake("status", 1, "soon");
    for (let i = 1; i <= 6; i++) {
      registry.pushHeartbeatEvent("status", "tool_use", `event ${i}`);
    }

    const status = registry.status("status")!;
    expect(status).toMatchObject({
      paused: false,
      heartbeat_state: "Active",
      ticks_without_user: 0,
      dormant_after_heartbeat_turns: 3,
      effective_interval_secs: 60,
      next_wake_at: "2026-05-24T01:00:00.000Z",
      seconds_until_wake: 3600,
      minimum_heartbeat_latency_secs: 3600,
      dormant_after_idle_time_secs: 7200,
    });
    expect(status.last_user_at).toBeUndefined();
    expect(status.seconds_since_user).toBeUndefined();
    expect(status.recent_events.map((event) => event.detail)).toEqual([
      "event 2",
      "event 3",
      "event 4",
      "event 5",
      "event 6",
    ]);

    expect(registry.heartbeatLog("status", 3).map((event) => event.detail)).toEqual([
      "event 4",
      "event 5",
      "event 6",
    ]);
    expect(registry.status("ghost")).toBeUndefined();
    expect(registry.heartbeatLog("ghost", 10)).toEqual([]);
  });

  test("shutdown clears intervals, persists state, and awaits in-flight ticks", async () => {
    const originalSetInterval = globalThis.setInterval;
    const originalClearInterval = globalThis.clearInterval;
    let intervalCallback: (() => void) | undefined;
    const fakeTimer = { id: "timer" };
    const cleared: unknown[] = [];
    let resolveTick: (() => void) | undefined;
    let tickStarted = false;
    let tickFinished = false;

    const globals = globalThis as unknown as {
      setInterval: (handler: () => void, timeout?: number) => unknown;
      clearInterval: (timer: unknown) => void;
    };
    globals.setInterval = (handler: () => void) => {
      intervalCallback = handler;
      return fakeTimer;
    };
    globals.clearInterval = (timer: unknown) => {
      cleared.push(timer);
    };

    try {
      const dir = mkdtempSync(join(tmpdir(), "autonomy-shutdown-test-"));
      const registry = new AutonomyRegistry({
        autonomyConfig: autonomyHeartbeatOn(),
        autoStartTicker: true,
        nowMs: () => 0,
        wallNow: () => new Date("2026-05-24T00:00:00.000Z"),
        onTickActions: async () => {
          tickStarted = true;
          await new Promise<void>((resolve) => {
            resolveTick = resolve;
          });
          tickFinished = true;
        },
      });
      registry.ensureState(fakeEngineAt("ticker", dir));
      expect(intervalCallback).not.toBeUndefined();

      expect(registry.heartbeatTickNow("ticker")).toBe(false);
      intervalCallback!();
      expect(tickStarted).toBe(true);
      expect(tickFinished).toBe(false);

      const shutdown = registry.shutdown();
      await Promise.resolve();
      expect(cleared).toEqual([fakeTimer]);
      expect(tickFinished).toBe(false);

      resolveTick!();
      await shutdown;
      expect(tickFinished).toBe(true);
      expect(existsSync(join(dir, "autonomy_state.json"))).toBe(true);
    } finally {
      globalThis.setInterval = originalSetInterval;
      globalThis.clearInterval = originalClearInterval;
    }
  });
});
