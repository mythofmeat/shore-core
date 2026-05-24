/**
 * Heartbeat clock tests — mirrors the `#[cfg(test)] mod tests` block in
 * `backend/daemon/src/autonomy/heartbeat.rs` line-for-line.
 *
 * Times are monotonic milliseconds. We pick an arbitrary base (1_000_000)
 * to avoid `performance.now()` non-determinism and to make negative-delta
 * arithmetic safe.
 */

import { describe, expect, test } from "bun:test";
import {
  HeartbeatAction,
  HeartbeatClock,
  MAX_WAKE_INTERVAL_MS,
  MIN_WAKE_INTERVAL_MS,
  type HeartbeatConfig,
} from "../src/autonomy/heartbeat.ts";

const BASE_MS = 1_000_000;
const HOUR_MS = 3600 * 1000;

const secs = (s: number) => s * 1000;

function clock(
  intervalSecs: number,
  maxIdle: number,
  silentSecs = 172800, // 48h, matches Rust helper default
): HeartbeatClock {
  const config: HeartbeatConfig = {
    fallbackHeartbeatIntervalSecs: intervalSecs,
    dormantAfterHeartbeatTurns: maxIdle,
    dormantAfterIdleTimeSecs: silentSecs,
    minimumHeartbeatLatencySecs: 3600,
  };
  return HeartbeatClock.withConfig(config, BASE_MS);
}

// ── basic lifecycle ────────────────────────────────────────────────────

describe("basic lifecycle", () => {
  test("first_tick_bootstraps_deadline", () => {
    const c = clock(60, 3);
    const now = BASE_MS;
    expect(c.tick(now)).toBe(HeartbeatAction.None);
    expect(c.nextWake()).not.toBeUndefined();
  });

  test("tick_fires_after_default_interval", () => {
    const c = clock(60, 3);
    const now = BASE_MS;
    c.tick(now); // bootstrap
    expect(c.tick(now + secs(61))).toBe(HeartbeatAction.RunTick);
    expect(c.ticksWithoutUser()).toBe(1);
  });

  test("tick_does_not_fire_before_deadline", () => {
    const c = clock(60, 3);
    const now = BASE_MS;
    c.tick(now); // bootstrap
    expect(c.tick(now + secs(30))).toBe(HeartbeatAction.None);
  });

  test("after_tick_fires_next_bootstrap_applies", () => {
    // After RunTick, nextWake is undefined. Next poll re-bootstraps with
    // defaultInterval from the new anchor.
    const c = clock(60, 3);
    const now = BASE_MS;
    c.tick(now); // bootstrap
    const t1 = now + secs(61);
    expect(c.tick(t1)).toBe(HeartbeatAction.RunTick);
    // nextWake is now undefined; next tick re-bootstraps.
    expect(c.tick(t1 + secs(1))).toBe(HeartbeatAction.None);
    // Fires again after another full interval from anchor.
    expect(c.tick(t1 + secs(61))).toBe(HeartbeatAction.RunTick);
  });
});

// ── abandonment guard: tick count ──────────────────────────────────────

describe("abandonment guard (tick count)", () => {
  test("guard_trips_after_max_idle_ticks", () => {
    const c = clock(60, 2);
    let now = BASE_MS;

    c.tick(now); // bootstrap

    // Tick 1.
    now += secs(61);
    expect(c.tick(now)).toBe(HeartbeatAction.RunTick);

    // Tick 2.
    now += secs(61);
    c.tick(now); // bootstrap
    now += secs(61);
    expect(c.tick(now)).toBe(HeartbeatAction.RunTick);

    // ticksWithoutUser is now 2 == max_idle. Next deadline: guard trips.
    now += secs(61);
    c.tick(now); // bootstrap
    now += secs(61);
    expect(c.tick(now)).toBe(HeartbeatAction.None);
    expect(c.nextWake()).toBeUndefined();
  });

  test("guard_does_not_trip_if_user_active", () => {
    const c = clock(60, 2);
    let now = BASE_MS;

    c.tick(now); // bootstrap

    now += secs(61);
    expect(c.tick(now)).toBe(HeartbeatAction.RunTick); // tick 1

    // User resets the counter.
    now += secs(10);
    c.onUserMessage(now);
    expect(c.ticksWithoutUser()).toBe(0);

    // Next tick fires normally (deadline pushed to now + 1h by onUserMessage).
    now += secs(3601);
    expect(c.tick(now)).toBe(HeartbeatAction.RunTick);
    expect(c.ticksWithoutUser()).toBe(1);
  });
});

// ── abandonment guard: silent duration ─────────────────────────────────

describe("abandonment guard (silent duration)", () => {
  test("guard_trips_on_silent_duration", () => {
    // High tick count so the count guard doesn't trip first; 2h silent.
    const c = clock(3600, 100, 7200);
    const now = BASE_MS;

    // Simulate: user sent a message, then silence.
    c.onUserMessage(now);

    // Fast-forward past the first tick (1h).
    const t1 = now + secs(3601);
    expect(c.tick(t1)).toBe(HeartbeatAction.RunTick);

    // Bootstrap next deadline.
    const t2 = t1 + secs(1);
    c.tick(t2);

    // At 2h+1s past user message → silent guard trips.
    const t3 = now + secs(7201);
    expect(c.tick(t3)).toBe(HeartbeatAction.None);
    expect(c.nextWake()).toBeUndefined();
  });
});

// ── schedule() ─────────────────────────────────────────────────────────

describe("schedule()", () => {
  test("schedule_sets_deadline", () => {
    const c = clock(3600, 3);
    const now = BASE_MS;
    c.onUserMessage(now);

    // Character schedules 4h out.
    c.schedule(now + 4 * HOUR_MS, now);
    expect(c.nextWake()).not.toBeUndefined();

    // Should not fire at 3h.
    expect(c.tick(now + secs(3 * 3600))).toBe(HeartbeatAction.None);
    // Should fire at 4h+1s.
    expect(c.tick(now + secs(4 * 3600 + 1))).toBe(HeartbeatAction.RunTick);
  });

  test("schedule_clamps_below_minimum", () => {
    const c = clock(3600, 3);
    const now = BASE_MS;
    c.onUserMessage(now);

    // Try to schedule 10 minutes out — clamped to 1h.
    c.schedule(now + secs(600), now);
    const wake = c.nextWake()!;
    expect(wake - now).toBe(MIN_WAKE_INTERVAL_MS);
  });

  test("schedule_clamps_above_maximum", () => {
    const c = clock(3600, 3);
    const now = BASE_MS;
    c.onUserMessage(now);

    // Try to schedule 72h out — clamped to 48h.
    c.schedule(now + secs(72 * 3600), now);
    const wake = c.nextWake()!;
    expect(wake - now).toBe(MAX_WAKE_INTERVAL_MS);
  });
});

// ── onUserMessage() ────────────────────────────────────────────────────

describe("onUserMessage()", () => {
  test("user_message_resets_counter", () => {
    const c = clock(60, 3);
    let now = BASE_MS;
    c.tick(now);
    now += secs(61);
    c.tick(now); // ticksWithoutUser = 1
    expect(c.ticksWithoutUser()).toBe(1);

    c.onUserMessage(now);
    expect(c.ticksWithoutUser()).toBe(0);
  });

  test("user_message_preserves_further_schedule", () => {
    const c = clock(3600, 3);
    const now = BASE_MS;

    // Character scheduled 6h out.
    c.schedule(now + secs(6 * 3600), now);
    const original = c.nextWake()!;

    // User message at t+30min. The 6h schedule is further out than
    // now + MIN_WAKE (1h), so it should be preserved.
    c.onUserMessage(now + secs(1800));
    expect(c.nextWake()).toBe(original);
  });

  test("user_message_pushes_imminent_deadline", () => {
    const c = clock(60, 3);
    const now = BASE_MS;
    c.tick(now); // bootstrap: deadline at now + 60s

    // User message at t+50s. Existing deadline (now+60) is only 10s away,
    // less than MIN_WAKE (1h), so onUserMessage pushes it to now+50 + 1h.
    const msgTime = now + secs(50);
    c.onUserMessage(msgTime);
    expect(c.nextWake()).toBe(msgTime + MIN_WAKE_INTERVAL_MS);
  });

  test("user_message_bootstraps_from_none", () => {
    const c = clock(3600, 3);
    const now = BASE_MS;
    // nextWake is undefined (fresh clock, no tick yet).
    expect(c.nextWake()).toBeUndefined();

    c.onUserMessage(now);
    expect(c.nextWake()).toBe(now + MIN_WAKE_INTERVAL_MS);
  });

  test("user_message_wakes_from_abandoned", () => {
    const c = clock(60, 1);
    let now = BASE_MS;
    c.tick(now); // bootstrap
    now += secs(61);
    c.tick(now); // tick 1

    // Bootstrap and trip the guard.
    now += secs(61);
    c.tick(now); // bootstrap
    now += secs(61);
    expect(c.tick(now)).toBe(HeartbeatAction.None); // guard trips
    expect(c.nextWake()).toBeUndefined();

    // User returns.
    now += secs(100);
    c.onUserMessage(now);
    expect(c.ticksWithoutUser()).toBe(0);
    expect(c.nextWake()).not.toBeUndefined();
  });
});

// ── restore() ──────────────────────────────────────────────────────────

describe("restore()", () => {
  test("restore_with_future_wake", () => {
    const c = clock(3600, 3);
    const now = BASE_MS;
    const future = now + secs(7200);

    c.restore(2, future, now);
    expect(c.ticksWithoutUser()).toBe(2);
    expect(c.nextWake()).toBe(future);
    expect(c.lastUserAt()).toBe(now);
  });

  test("restore_with_past_wake_fires_immediately", () => {
    const c = clock(3600, 3);
    const now = BASE_MS;
    const past = now - secs(100);

    c.restore(1, past, now);
    // Deadline is in the past → tick() fires immediately.
    expect(c.tick(now)).toBe(HeartbeatAction.RunTick);
  });
});

// ── stateAt() label ────────────────────────────────────────────────────

describe("stateAt() label", () => {
  test("state_label_active_when_healthy", () => {
    const c = clock(3600, 3);
    expect(c.stateAt(BASE_MS)).toBe("Active");
  });

  test("state_label_dormant_when_tick_guard_tripped", () => {
    const c = clock(60, 1);
    let now = BASE_MS;
    c.tick(now); // bootstrap
    now += secs(61);
    c.tick(now); // tick 1
    now += secs(61);
    c.tick(now); // bootstrap
    now += secs(61);
    c.tick(now); // guard trips
    expect(c.stateAt(now)).toBe("Dormant");
  });

  test("state_label_dormant_when_silent_duration_tripped", () => {
    const c = clock(3600, 100, 7200);
    const now = BASE_MS;

    c.onUserMessage(now);

    const t1 = now + secs(3601);
    expect(c.tick(t1)).toBe(HeartbeatAction.RunTick);

    const t2 = t1 + secs(1);
    c.tick(t2);

    const t3 = now + secs(7201);
    expect(c.tick(t3)).toBe(HeartbeatAction.None);
    expect(c.stateAt(t3)).toBe("Dormant");
  });

  test("state_label_dormant_when_forced_dormant", () => {
    const c = clock(3600, 3);
    c.forceDormant();
    expect(c.stateAt(BASE_MS)).toBe("Dormant");
  });
});
