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
import { HeartbeatAction } from "../src/autonomy/heartbeat.ts";
import { setNextWakeHandler } from "../src/tools/basic.ts";
import { ToolError } from "../src/tools/registry.ts";
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
