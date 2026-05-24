/**
 * HeartbeatClock — deadline holder with abandonment guard.
 *
 * Mirrors `backend/daemon/src/autonomy/heartbeat.rs` line-for-line. The
 * Rust "Instant" is `tokio::time::Instant` (monotonic); the TS port uses
 * milliseconds-since-an-arbitrary-base (the same convention activity.ts
 * uses). All times in this file are monotonic `number` (ms) unless an
 * accessor name says otherwise.
 *
 * The character schedules its own next wake via `set_next_wake`. The
 * clock holds that deadline and fires `RunTick` when it passes. An
 * abandonment guard stops ticking when the user has been absent too long.
 *
 * "PORT with care": this is pure logic, but the abandonment-guard ordering
 * and the `on_user_message` deadline-push semantics are subtle. The Rust
 * tests in `heartbeat.rs::tests` are reproduced 1:1 in
 * `tests/heartbeat.test.ts` to lock the behaviour down.
 */

// ── Constants ───────────────────────────────────────────────────────────

/** Minimum interval a character can schedule (1 hour) — in ms. */
export const MIN_WAKE_INTERVAL_MS = 60 * 60 * 1000;

/** Maximum interval a character can schedule (48 hours) — in ms. */
export const MAX_WAKE_INTERVAL_MS = 48 * 60 * 60 * 1000;

// ── Types ───────────────────────────────────────────────────────────────

export enum HeartbeatAction {
  /** Nothing to do this tick. */
  None = "None",
  /** Fire a full heartbeat tick (private LLM call with tools). */
  RunTick = "RunTick",
}

/**
 * Subset of Rust's `HeartbeatConfig` consumed by the clock. The
 * production loader hookup (Phase 8c) will populate this from
 * `shore-config`. For Phase 8b the registry hardcodes a default.
 */
export interface HeartbeatConfig {
  /** Fallback interval when the character doesn't call set_next_wake (seconds). */
  fallbackHeartbeatIntervalSecs: number;
  /** Max consecutive ticks without user before the guard stops ticking. */
  dormantAfterHeartbeatTurns: number;
  /** Max wall-clock duration without user before the guard stops ticking (seconds). */
  dormantAfterIdleTimeSecs: number;
  /** Minimum interval between a user message and the next tick (seconds). */
  minimumHeartbeatLatencySecs: number;
}

// ── HeartbeatClock ──────────────────────────────────────────────────────

/**
 * Deadline holder with abandonment guard. The character drives its own
 * cadence via `schedule()`. The clock's job is to hold that deadline,
 * apply bounds, and stop ticking when the user has been gone too long.
 */
export class HeartbeatClock {
  // -- deadline state ------------------------------------------------------
  private nextWakeAt: number | undefined = undefined;
  private lastAnchor: number;

  // -- abandonment guard ---------------------------------------------------
  private ticksWithoutUserCount = 0;
  private lastUserAtMs: number | undefined = undefined;

  // -- config --------------------------------------------------------------
  private readonly defaultIntervalMs: number;
  private readonly maxIdleTicksValue: number;
  private readonly maxSilentDurationMs: number;
  private readonly minWakeIntervalMs: number;

  private constructor(
    defaultIntervalMs: number,
    maxIdleTicks: number,
    maxSilentDurationMs: number,
    minWakeIntervalMs: number,
    nowMs: number,
  ) {
    this.defaultIntervalMs = defaultIntervalMs;
    this.maxIdleTicksValue = maxIdleTicks;
    this.maxSilentDurationMs = maxSilentDurationMs;
    this.minWakeIntervalMs = minWakeIntervalMs;
    this.lastAnchor = nowMs;
  }

  static withConfig(config: HeartbeatConfig, nowMs: number): HeartbeatClock {
    return new HeartbeatClock(
      config.fallbackHeartbeatIntervalSecs * 1000,
      config.dormantAfterHeartbeatTurns,
      config.dormantAfterIdleTimeSecs * 1000,
      config.minimumHeartbeatLatencySecs * 1000,
      nowMs,
    );
  }

  // -- accessors ----------------------------------------------------------

  nextWake(): number | undefined {
    return this.nextWakeAt;
  }

  /** Force the next tick to fire immediately. Does not reset abandonment counters. */
  forceWake(nowMs: number): void {
    this.nextWakeAt = nowMs;
  }

  /**
   * Force the clock into dormant state. Stays dormant until a user
   * message resets it via `onUserMessage()`.
   */
  forceDormant(): void {
    this.ticksWithoutUserCount = this.maxIdleTicksValue;
    this.nextWakeAt = undefined;
  }

  /**
   * Force the clock into active state. Resets abandonment counters and
   * schedules an immediate tick. Guard will re-trip naturally if user
   * doesn't respond.
   */
  forceActive(nowMs: number): void {
    this.ticksWithoutUserCount = 0;
    this.lastUserAtMs = nowMs;
    this.nextWakeAt = nowMs;
  }

  ticksWithoutUser(): number {
    return this.ticksWithoutUserCount;
  }

  maxIdleTicks(): number {
    return this.maxIdleTicksValue;
  }

  lastUserAt(): number | undefined {
    return this.lastUserAtMs;
  }

  defaultIntervalSecs(): number {
    return this.defaultIntervalMs / 1000;
  }

  minWakeIntervalSecs(): number {
    return this.minWakeIntervalMs / 1000;
  }

  maxSilentDurationSecs(): number {
    return this.maxSilentDurationMs / 1000;
  }

  private isAbandoned(nowMs: number): boolean {
    if (this.ticksWithoutUserCount >= this.maxIdleTicksValue) {
      return true;
    }
    if (this.lastUserAtMs !== undefined) {
      if (nowMs - this.lastUserAtMs >= this.maxSilentDurationMs) {
        return true;
      }
    }
    return false;
  }

  isDormant(nowMs: number): boolean {
    return this.isAbandoned(nowMs);
  }

  /** Human-readable state label for status display and logging. */
  stateAt(nowMs: number): "Active" | "Dormant" {
    return this.isDormant(nowMs) ? "Dormant" : "Active";
  }

  // -- core ---------------------------------------------------------------

  /**
   * Called by the autonomy loop on each ~30s tick.
   *
   * Semantics:
   * 1. If `nextWakeAt` is undefined → set to `lastAnchor + defaultInterval`, return None.
   * 2. If `now < nextWakeAt` → return None.
   * 3. Deadline passed — check abandonment guard. If tripped, clear
   *    `nextWakeAt` and return None.
   * 4. Guard passes → increment counter, clear deadline, update anchor,
   *    return RunTick.
   */
  tick(nowMs: number): HeartbeatAction {
    // Step 1: bootstrap if no deadline set — but only if the guard hasn't
    // already tripped. Once abandoned, we stay dormant until reset by a
    // user message.
    if (this.nextWakeAt === undefined) {
      if (this.isAbandoned(nowMs)) {
        return HeartbeatAction.None;
      }
      this.nextWakeAt = this.lastAnchor + this.defaultIntervalMs;
      return HeartbeatAction.None;
    }

    // Step 2: not due yet.
    if (nowMs < this.nextWakeAt) {
      return HeartbeatAction.None;
    }

    // Step 3: deadline passed — check abandonment guard.
    if (this.ticksWithoutUserCount >= this.maxIdleTicksValue) {
      this.nextWakeAt = undefined;
      return HeartbeatAction.None;
    }
    if (this.lastUserAtMs !== undefined) {
      if (nowMs - this.lastUserAtMs >= this.maxSilentDurationMs) {
        this.nextWakeAt = undefined;
        return HeartbeatAction.None;
      }
    }

    // Step 4: guard passes — fire the tick.
    this.ticksWithoutUserCount += 1;
    this.nextWakeAt = undefined;
    this.lastAnchor = nowMs;
    return HeartbeatAction.RunTick;
  }

  /**
   * Called when the character invokes `set_next_wake` during a tick.
   *
   * Bounds: `MIN_WAKE_INTERVAL_MS <= (when - now) <= MAX_WAKE_INTERVAL_MS`.
   * Out-of-range values are clamped rather than rejected, so a
   * misbehaving character can never silently disable heartbeat.
   */
  schedule(whenMs: number, nowMs: number): void {
    const delta = Math.max(0, whenMs - nowMs);
    const clamped = Math.min(
      MAX_WAKE_INTERVAL_MS,
      Math.max(MIN_WAKE_INTERVAL_MS, delta),
    );
    this.nextWakeAt = nowMs + clamped;
    this.lastAnchor = nowMs;
  }

  /**
   * Called when a user message arrives.
   *
   * Semantics:
   * 1. Reset `ticksWithoutUser = 0`.
   * 2. Set `lastUserAt = now`.
   * 3. `nextWakeAt = max(nextWakeAt, now + minWakeInterval)`. If
   *    `nextWakeAt` was undefined (first message, or abandoned), this
   *    bootstraps the cycle. If the character had scheduled further out,
   *    the schedule is preserved.
   */
  onUserMessage(nowMs: number): void {
    this.ticksWithoutUserCount = 0;
    this.lastUserAtMs = nowMs;

    const minWake = nowMs + this.minWakeIntervalMs;
    const existing = this.nextWakeAt;
    if (existing !== undefined && existing > minWake) {
      this.nextWakeAt = existing;
    } else {
      this.nextWakeAt = minWake;
    }
  }

  /** Restore state from persistence (daemon restart). */
  restore(
    ticksWithoutUser: number,
    nextWakeAtMs: number | undefined,
    lastUserAtMs: number | undefined,
  ): void {
    this.ticksWithoutUserCount = ticksWithoutUser;
    if (nextWakeAtMs !== undefined) {
      this.nextWakeAt = nextWakeAtMs;
      this.lastAnchor = nextWakeAtMs;
    }
    if (lastUserAtMs !== undefined) {
      this.lastUserAtMs = lastUserAtMs;
    }
  }
}
