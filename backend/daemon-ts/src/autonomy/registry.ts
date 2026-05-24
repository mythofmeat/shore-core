/**
 * Per-character ActivityTracker registry.
 *
 * Phase 8a slice of the autonomy manager: tracks user-message rhythm so
 * `activity_heatmap` returns real data. The full state machine
 * (heartbeat, compaction triggers, keepalive timing) lands in 8b/8c.
 *
 * Mirrors the trim of `AutonomyManager` that `tools/activity.rs` needs:
 * `activity_stats(character)` returning `(stats, message_count)`.
 */

import { ActivityTracker, type ActivityStats } from "./activity.ts";
import { SegmentReader } from "../engine/segments.ts";
import type { ConversationEngine } from "../engine/engine.ts";
import type { Message } from "../engine/types.ts";

const BACKFILL_WINDOW_DAYS = 90;

interface ActivityWithCount {
  stats: ActivityStats;
  messageCount: number;
}

export class AutonomyRegistry {
  private readonly trackers = new Map<string, ActivityTracker>();

  /** Idempotent — first call backfills from on-disk history. */
  ensureState(engine: ConversationEngine): ActivityTracker {
    const name = engine.name();
    const existing = this.trackers.get(name);
    if (existing !== undefined) return existing;

    const tracker = new ActivityTracker();
    this.trackers.set(name, tracker);

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

  /** Test/debug accessor. */
  hasState(characterName: string): boolean {
    return this.trackers.has(characterName);
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
