/**
 * Per-character autonomy registry.
 *
 * Phase 8a slice: tracks user-message rhythm so `activity_heatmap` returns
 * real data.
 *
 * Phase 8b slice: also owns a `HeartbeatClock` per character so that
 * `set_next_wake` has a real implementation behind the
 * `ctx.scheduleNextWake` hook. The production driver — the ~30s ticker
 * loop, persistence, and the actual LLM dispatch on `RunTick` — lands in
 * 8c/8d. Until then the clock is exercised only by tests; the
 * user-message path keeps `scheduleNextWake: undefined` in
 * `GenerateOptions`.
 */

import { ActivityTracker, type ActivityStats } from "./activity.ts";
import {
  HeartbeatClock,
  type HeartbeatConfig,
} from "./heartbeat.ts";
import { SegmentReader } from "../engine/segments.ts";
import type { ConversationEngine } from "../engine/engine.ts";
import type { Message } from "../engine/types.ts";

const BACKFILL_WINDOW_DAYS = 90;

/**
 * Default HeartbeatConfig used until the config-loader hookup lands in
 * 8c. Mirrors the Rust defaults in `shore-config::app::HeartbeatConfig`.
 */
const DEFAULT_HEARTBEAT_CONFIG: HeartbeatConfig = {
  fallbackHeartbeatIntervalSecs: 6 * 3600, // 6h
  dormantAfterHeartbeatTurns: 12,
  dormantAfterIdleTimeSecs: 7 * 86400, // 7 days
  minimumHeartbeatLatencySecs: 3600, // 1h
};

interface ActivityWithCount {
  stats: ActivityStats;
  messageCount: number;
}

export class AutonomyRegistry {
  private readonly trackers = new Map<string, ActivityTracker>();
  private readonly clocks = new Map<string, HeartbeatClock>();
  private readonly heartbeatConfig: HeartbeatConfig;

  constructor(heartbeatConfig: HeartbeatConfig = DEFAULT_HEARTBEAT_CONFIG) {
    this.heartbeatConfig = heartbeatConfig;
  }

  /** Idempotent — first call backfills from on-disk history. */
  ensureState(engine: ConversationEngine): ActivityTracker {
    const name = engine.name();
    const existing = this.trackers.get(name);
    if (existing !== undefined) return existing;

    const tracker = new ActivityTracker();
    this.trackers.set(name, tracker);

    if (!this.clocks.has(name)) {
      this.clocks.set(
        name,
        HeartbeatClock.withConfig(this.heartbeatConfig, performance.now()),
      );
    }

    const timestamps = collectBackfillTimestamps(engine);
    if (timestamps.length > 0) {
      tracker.backfill(timestamps);
    }
    return tracker;
  }

  /** Record a fresh user turn. Mirrors `notify_user_message`. */
  notifyUserMessage(characterName: string): void {
    const tracker = this.trackers.get(characterName);
    if (tracker === undefined) return;
    tracker.recordMessage();
    const clock = this.clocks.get(characterName);
    clock?.onUserMessage(performance.now());
  }

  /**
   * Mirrors `AutonomyManager::activity_stats` — returns the cached stats
   * and the raw message count, or `undefined` if the character has no
   * state yet. The `messageCount` value is the tool's `total_messages` /
   * `total_turns`.
   */
  activityStats(characterName: string): ActivityWithCount | undefined {
    const tracker = this.trackers.get(characterName);
    if (tracker === undefined) return undefined;
    return {
      stats: tracker.stats(),
      messageCount: tracker.messageCount(),
    };
  }

  /**
   * Implementation behind the `set_next_wake` tool hook. Mirrors
   * `schedule_next_wake_in_state` in `backend/daemon/src/autonomy/manager.rs`.
   *
   * Returns the same plain-string payload as Rust (`json!(format!(...))`)
   * so the tool's downstream `JSON.stringify(result)` produces identical
   * wire output to the Rust daemon.
   *
   * `set_next_wake`'s execute has already clamped `hoursFromNow` into
   * `[1, 48]` before calling us. Re-clamping happens at the clock layer
   * (`HeartbeatClock.schedule` enforces `MIN/MAX_WAKE_INTERVAL_MS`), so
   * the bounds are guaranteed regardless of caller.
   */
  scheduleNextWake(
    characterName: string,
    hoursFromNow: number,
    _reason: string,
  ): string {
    const clock = this.clocks.get(characterName);
    if (clock === undefined) {
      throw new Error(
        `scheduleNextWake: no autonomy state for character "${characterName}"`,
      );
    }
    const clamped = Math.min(48, Math.max(1, hoursFromNow));
    const now = performance.now();
    const when = now + clamped * 3600 * 1000;
    clock.schedule(when, now);
    return `Scheduled next moment in ${clamped.toFixed(1)} hours.`;
  }

  /** Test/debug accessor. */
  hasState(characterName: string): boolean {
    return this.trackers.has(characterName);
  }

  /** Test/debug accessor for the per-character heartbeat clock. */
  heartbeatClock(characterName: string): HeartbeatClock | undefined {
    return this.clocks.get(characterName);
  }
}

/**
 * Walk the engine's active messages plus all frozen segments, filter to
 * non-tool-result user turns within the last 90 days, parse RFC3339
 * timestamps, return them as `Date`s. Mirrors `task.rs::is_new_autonomy_state`.
 */
function collectBackfillTimestamps(engine: ConversationEngine): Date[] {
  const cutoff = new Date(Date.now() - BACKFILL_WINDOW_DAYS * 86_400_000);
  const out: Date[] = [];

  const collect = (messages: Message[]): void => {
    for (const msg of messages) {
      if (msg.role !== "user") continue;
      if (isToolResultOnly(msg)) continue;
      const dt = new Date(msg.timestamp);
      if (Number.isNaN(dt.getTime())) continue;
      if (dt < cutoff) continue;
      out.push(dt);
    }
  };

  collect(engine.historySnapshot().messages);

  try {
    const segments = SegmentReader.load(engine.dataDir());
    for (let i = 0; i < segments.segmentCount(); i++) {
      try {
        collect(segments.readSegment(i));
      } catch {
        // Match Rust: log-and-continue is the model, but we want backfill
        // to be best-effort here so a single bad segment doesn't strand
        // the whole tracker.
      }
    }
  } catch {
    // No segments dir or unreadable manifest — leave the active-only
    // backfill as-is.
  }

  return out;
}

function isToolResultOnly(msg: Message): boolean {
  return (
    msg.content_blocks.length > 0
    && msg.content_blocks.every((b) => b.type === "tool_result")
  );
}
