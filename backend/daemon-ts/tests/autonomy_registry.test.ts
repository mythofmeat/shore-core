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
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { AutonomyRegistry } from "../src/autonomy/registry.ts";
import { setNextWakeHandler } from "../src/tools/basic.ts";
import { ToolError } from "../src/tools/registry.ts";
import type { ConversationEngine } from "../src/engine/engine.ts";
import type { ToolContext } from "../src/tools/registry.ts";

function fakeEngine(name: string): ConversationEngine {
  const dir = mkdtempSync(join(tmpdir(), `autonomy-reg-test-${name}-`));
  return {
    name: () => name,
    dataDir: () => dir,
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
});
