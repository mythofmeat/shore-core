/**
 * Per-character autonomy registry.
 *
 * Phase 8a slice: tracks user-message rhythm so `activity_heatmap` returns
 * real data.
 *
 * Phase 8b slice: also owns a `HeartbeatClock` per character so that
 * `set_next_wake` has a real implementation behind the
 * `ctx.scheduleNextWake` hook.
 *
 * Phase 8c slice: restores/saves `autonomy_state.json`, mirrors heartbeat
 * wake deadlines into `CacheKeepalive`, and can run the production ticker
 * loop. The actual async LLM work for `RunTick` / keepalive `Ping` lands
 * in 8d; `tickCharacter()` returns those actions for that driver.
 */

import fs from "node:fs";
import path from "node:path";

import { ActivityTracker, type ActivityStats } from "./activity.ts";
import {
  CacheKeepalive,
  CacheKeepaliveAction,
} from "./cache_keepalive.ts";
import {
  DEFAULT_HEARTBEAT_CONFIG,
  HeartbeatClock,
  HeartbeatAction,
  type HeartbeatConfig,
} from "./heartbeat.ts";
import {
  HeartbeatLog,
  type HeartbeatEvent,
  type HeartbeatEventKind,
} from "./heartbeat_log.ts";
import { SegmentReader } from "../engine/segments.ts";
import type { ConversationEngine } from "../engine/engine.ts";
import type { Message } from "../engine/types.ts";
import type { ChatRequest } from "../llm/types.ts";
import type { AutonomyConfig, LoadedConfig } from "../config/loader.ts";
import {
  DEFAULT_COMPACTION_CONFIG,
  type CompactionConfig,
} from "../memory/compaction/types.ts";
import {
  defaultDreamingConfig,
  type DreamingConfig,
} from "../memory/dreaming.ts";

const BACKFILL_WINDOW_DAYS = 90;
const STATE_VERSION = 4;
const STATE_FILENAME = "autonomy_state.json";
const TICK_INTERVAL_MS = 10_000;

/**
 * Per-character compaction trigger state. Mirror of the
 * compaction-related fields on Rust's `AutonomyState`
 * (`backend/daemon/src/autonomy/manager.rs:117`):
 *
 * - `triggered` — set when a trigger fires; cleared by
 *   `notifyCompactionComplete` / `notifyCompactionFailed`. Prevents
 *   re-firing the same compaction cycle from two paths (post-generation
 *   handler and idle tick) at once.
 * - `pending` — set by the idle tick when the trigger fired but no
 *   inline compaction runner was wired. The post-generation handler's
 *   `shouldCompactNow` consumes it on the user's next turn.
 * - `activeTurnCount` — last observed `engine.messageCount()`. Compared
 *   against `min_turns` / `max_turns`.
 * - `lastActivityMs` — monotonic timestamp of the most recent user or
 *   assistant message. Drives the idle trigger.
 */
interface CompactionStateEntry {
  triggered: boolean;
  pending: boolean;
  activeTurnCount: number;
  lastActivityMs: number;
}

/**
 * Per-character dreaming retry state. Mirror of the dream-related fields
 * on Rust's `AutonomyState` (manager.rs:138-141):
 *
 * - `nextAttemptAtMs` — earliest monotonic time at which the next
 *   scheduled dream attempt is allowed. Updated by
 *   `notifyDreamingFailed` to back the retry off; cleared by
 *   `notifyDreamingSuccess` or a successful skip.
 * - `failureCount` — consecutive scheduled dreaming failures.
 *   Exponential backoff via `backgroundRetryDelay`.
 * - `running` — set true while a dream is in flight so the tick doesn't
 *   double-fire (analogous to compaction's `triggered`).
 */
interface DreamingStateEntry {
  nextAttemptAtMs?: number;
  failureCount: number;
  running: boolean;
}

/**
 * Mirror of `background_retry_delay` (manager.rs:150). Exponential
 * backoff starting at 60 seconds, doubling each failure up to a 1-hour
 * cap. failureCount must be ≥1 (the success path doesn't call this).
 */
function backgroundRetryDelayMs(failureCount: number): number {
  const exponent = Math.min(6, Math.max(0, failureCount - 1));
  const secs = Math.min(3600, 60 * Math.pow(2, exponent));
  return secs * 1000;
}

const DEFAULT_AUTONOMY_CONFIG: AutonomyConfig = {
  enabled: false,
  heartbeat: {
    enabled: true,
    ...DEFAULT_HEARTBEAT_CONFIG,
    maxToolRounds: 12,
    wrapUpGraceRounds: 3,
  },
};

interface ActivityWithCount {
  stats: ActivityStats;
  messageCount: number;
}

interface PersistedState {
  version: number;
  ticks_without_user: number;
  next_wake_at?: string | null;
  last_user_at?: string | null;
}

export interface AutonomyStatus {
  paused: boolean;
  heartbeat_state: string;
  ticks_without_user: number;
  dormant_after_heartbeat_turns: number;
  effective_interval_secs: number;
  next_wake_at?: string;
  seconds_until_wake?: number;
  last_user_at?: string;
  seconds_since_user?: number;
  minimum_heartbeat_latency_secs: number;
  dormant_after_idle_time_secs: number;
  recent_events: HeartbeatEvent[];
}

interface RuntimeResources {
  llmClient: unknown;
  pushTx: unknown;
  loadedConfig: LoadedConfig;
  notifier: unknown;
}

export interface TickActions {
  heartbeat: HeartbeatAction;
  keepalive: CacheKeepaliveAction;
  guardTripped: boolean;
  /**
   * True when the tick detected a compaction trigger (max_turns or
   * idle_trigger) AND the registry has an `onIdleCompaction` callback
   * wired — i.e. the tick is committing to run compaction inline. When
   * no callback is wired, the trigger sets `pending` on the per-character
   * state instead and this stays false; the post-generation handler picks
   * up the pending flag on the user's next message.
   */
  runIdleCompaction: boolean;
  /**
   * True when the tick detected that dreaming is past its retry-backoff
   * window AND `onScheduledDream` is wired. The runner itself re-checks
   * the cron schedule (the tick only enforces backoff + the autonomy +
   * dreaming enabled flags). Skip vs. real-work decision lives inside
   * the runner so the same gating logic isn't duplicated in two places.
   */
  runScheduledDream: boolean;
}

export interface AutonomyRegistryOptions {
  autonomyConfig?: AutonomyConfig;
  /** Compaction config — drives the idle/max-turns trigger inside the tick. */
  compactionConfig?: CompactionConfig;
  /** Dreaming config — drives the scheduled-dream tick + cron gate. */
  dreamingConfig?: DreamingConfig;
  /** Start the per-character 10s ticker as states are first ensured. */
  autoStartTicker?: boolean;
  /** Injectable monotonic clock for deterministic tests. */
  nowMs?: () => number;
  /** Injectable wall clock for persistence tests. */
  wallNow?: () => Date;
  /** Async 8d driver for returned heartbeat/keepalive actions. */
  onTickActions?: (characterName: string, actions: TickActions) => Promise<void> | void;
  /**
   * Idle-compaction runner. Invoked from the per-character tick when the
   * compaction trigger fires AND this callback is wired. When the callback
   * is undefined, the tick sets the per-character `pending` flag instead;
   * the next user message's post-generation hook picks it up. Mirror of
   * the `execute_idle_compaction` arm in
   * `backend/daemon/src/autonomy/manager.rs:1172`.
   */
  onIdleCompaction?: (characterName: string) => Promise<void> | void;
  /**
   * Scheduled-dream runner. Invoked from the per-character tick when
   * `autonomy.enabled && dreaming.enabled` AND the per-character
   * backoff window is past. The runner is expected to itself call
   * `runLibrarianSweep`, which re-checks the cron schedule via
   * `isDueNow`; the tick does not duplicate that gate. Mirror of
   * `execute_scheduled_dream` (manager.rs:1279).
   */
  onScheduledDream?: (characterName: string) => Promise<void> | void;
}

export class AutonomyRegistry {
  private readonly trackers = new Map<string, ActivityTracker>();
  private readonly clocks = new Map<string, HeartbeatClock>();
  private readonly keepalives = new Map<string, CacheKeepalive>();
  private readonly heartbeatLogs = new Map<string, HeartbeatLog>();
  private readonly lastRequests = new Map<string, ChatRequest>();
  private readonly compactionStates = new Map<string, CompactionStateEntry>();
  private readonly dreamingStates = new Map<string, DreamingStateEntry>();
  private readonly paused = new Map<string, boolean>();
  private readonly dataDirs = new Map<string, string>();
  private readonly dirty = new Set<string>();
  private readonly tickers = new Map<string, ReturnType<typeof setInterval>>();
  private readonly ticking = new Set<string>();
  private readonly inFlightTicks = new Map<string, Promise<void>>();
  private autonomyConfig: AutonomyConfig;
  private compactionConfig: CompactionConfig;
  private dreamingConfig: DreamingConfig;
  private resources: RuntimeResources | undefined;
  private loadedConfig: LoadedConfig | undefined;
  private readonly autoStartTicker: boolean;
  private readonly nowMs: () => number;
  private readonly wallNow: () => Date;
  private readonly onTickActions:
    | ((characterName: string, actions: TickActions) => Promise<void> | void)
    | undefined;
  private readonly onIdleCompaction:
    | ((characterName: string) => Promise<void> | void)
    | undefined;
  private readonly onScheduledDream:
    | ((characterName: string) => Promise<void> | void)
    | undefined;

  constructor(options: AutonomyRegistryOptions = {}) {
    this.autonomyConfig = options.autonomyConfig ?? DEFAULT_AUTONOMY_CONFIG;
    this.compactionConfig = sanitizeCompactionConfig(
      options.compactionConfig ?? DEFAULT_COMPACTION_CONFIG,
    );
    this.dreamingConfig = options.dreamingConfig ?? defaultDreamingConfig();
    this.autoStartTicker = options.autoStartTicker ?? false;
    this.nowMs = options.nowMs ?? (() => performance.now());
    this.wallNow = options.wallNow ?? (() => new Date());
    this.onTickActions = options.onTickActions;
    this.onIdleCompaction = options.onIdleCompaction;
    this.onScheduledDream = options.onScheduledDream;
  }

  /**
   * Set dependency handles used by autonomous background work. The TS port
   * keeps the concrete capabilities opaque here; call sites wire the actual
   * runners separately, but this mirrors Rust's manager-level resource seam.
   */
  setResources(
    llmClient: unknown,
    pushTx: unknown,
    loadedConfig: LoadedConfig,
    notifier: unknown,
  ): void {
    this.resources = {
      llmClient,
      pushTx,
      loadedConfig,
      notifier,
    };
    this.loadedConfig = loadedConfig;
  }

  /** Reload autonomy + compaction runtime config after config_reset. */
  reloadRuntimeConfig(newLoadedConfig: LoadedConfig): void {
    this.autonomyConfig = cloneAutonomyConfig(
      newLoadedConfig.app.behavior.autonomy,
    );
    this.compactionConfig = sanitizeCompactionConfig({
      ...newLoadedConfig.memory.compaction,
    });
    this.dreamingConfig = { ...newLoadedConfig.memory.dreaming };
    this.loadedConfig = newLoadedConfig;
    if (this.resources !== undefined) {
      this.resources = {
        ...this.resources,
        loadedConfig: newLoadedConfig,
      };
    }
  }

  /** Idempotent — first call backfills from on-disk history. */
  ensureState(engine: ConversationEngine): ActivityTracker {
    const name = engine.name();
    const existing = this.trackers.get(name);
    if (existing !== undefined) return existing;

    const tracker = new ActivityTracker();
    this.trackers.set(name, tracker);
    this.dataDirs.set(name, engine.dataDir());

    if (!this.clocks.has(name)) {
      const clock = HeartbeatClock.withConfig(
        heartbeatClockConfig(this.autonomyConfig.heartbeat),
        this.nowMs(),
      );
      const persisted = loadPersistedState(statePath(engine.dataDir()));
      if (persisted !== undefined) {
        restoreFromPersisted(persisted, clock, this.nowMs(), this.wallNow());
      }
      this.clocks.set(name, clock);
    }

    const keepalive = new CacheKeepalive();
    const wake = this.clocks.get(name)?.nextWake();
    if (wake !== undefined) {
      keepalive.setNextWake(wake);
      keepalive.onCacheWarmed(this.nowMs());
    }
    this.keepalives.set(name, keepalive);
    this.heartbeatLogs.set(
      name,
      HeartbeatLog.loadFrom(path.join(engine.dataDir(), "heartbeat.jsonl")),
    );

    // Seed compaction state from on-disk message count so the idle trigger
    // can fire for an existing character right after process restart, before
    // the user sends a fresh message. Matches the Rust behavior: the autonomy
    // tick's compaction arm uses `active_turn_count`, which is updated on
    // every user / assistant notify but starts at whatever the engine reports.
    this.compactionStates.set(name, {
      triggered: false,
      pending: false,
      activeTurnCount: engine.messageCount(),
      lastActivityMs: this.nowMs(),
    });
    this.dreamingStates.set(name, { failureCount: 0, running: false });
    this.paused.set(name, false);

    const timestamps = collectBackfillTimestamps(engine);
    if (timestamps.length > 0) {
      tracker.backfill(timestamps);
    }
    if (this.autoStartTicker) {
      this.startTicker(name);
    }
    return tracker;
  }

  /**
   * Record a fresh user turn. Mirror of
   * `AutonomyManager::notify_user_message` (manager.rs:507).
   *
   * `messageCount` is the post-append `engine.messageCount()` snapshot —
   * the same value Rust stores in `active_turn_count` and compares against
   * `min_turns`/`max_turns`.
   */
  notifyUserMessage(characterName: string, messageCount: number): void {
    const tracker = this.trackers.get(characterName);
    if (tracker === undefined) return;
    tracker.recordMessage();
    const clock = this.clocks.get(characterName);
    const now = this.nowMs();
    clock?.onUserMessage(now);
    const keepalive = this.keepalives.get(characterName);
    const wake = clock?.nextWake();
    if (keepalive !== undefined && wake !== undefined) {
      keepalive.setNextWake(wake);
    }
    keepalive?.onCacheWarmed(now);
    const compaction = this.compactionStates.get(characterName);
    if (compaction !== undefined) {
      compaction.activeTurnCount = messageCount;
      compaction.lastActivityMs = now;
    }
    this.markDirty(characterName);
  }

  /**
   * Record a fresh assistant turn. Mirror of
   * `AutonomyManager::notify_assistant_message` (manager.rs:535).
   *
   * Updates compaction bookkeeping so the idle trigger's "no activity for N
   * seconds" check uses the most recent message, not just user input. The
   * post-generation handler calls this after persisting the assistant turn
   * and any tool-loop intermediates.
   */
  notifyAssistantMessage(characterName: string, messageCount?: number): void {
    const compaction = this.compactionStates.get(characterName);
    if (compaction === undefined) return;
    if (messageCount !== undefined) {
      compaction.activeTurnCount = messageCount;
    }
    compaction.lastActivityMs = this.nowMs();
    this.markDirty(characterName);
  }

  /**
   * Compaction trigger check called from the post-generation handler. Mirror of
   * `AutonomyManager::should_compact_now` (manager.rs:619).
   *
   * Returns true if any trigger fires. Sets the per-character `triggered`
   * flag so the autonomy tick won't double-fire while compaction runs, and
   * consumes the `pending` flag set by an idle tick that ran without an
   * inline compaction runner.
   *
   * Order matches Rust: max_turns → max_context_tokens → idle pending.
   */
  shouldCompactNow(
    characterName: string,
    turnCount: number,
    contextTokens: number,
  ): boolean {
    const cfg = this.compactionConfig;
    if (!cfg.enabled) return false;
    const compaction = this.compactionStates.get(characterName);
    if (compaction === undefined) return false;

    if (
      cfg.maxTurns > 0
      && turnCount >= cfg.maxTurns
      && turnCount >= cfg.minTurns
    ) {
      compaction.triggered = true;
      this.markDirty(characterName);
      return true;
    }
    if (
      cfg.maxContextTokens > 0
      && contextTokens >= cfg.maxContextTokens
      && turnCount >= cfg.minTurns
    ) {
      compaction.triggered = true;
      this.markDirty(characterName);
      return true;
    }
    if (compaction.pending) {
      compaction.pending = false;
      this.markDirty(characterName);
      return true;
    }
    return false;
  }

  /**
   * Called by the compaction runner after `runCompaction` returns
   * successfully and the engine has been reloaded. Mirror of
   * `AutonomyManager::notify_compaction_complete` (manager.rs:575).
   *
   * Invalidates the cached request — its message tail is now stale relative
   * to the freshly compacted active.jsonl. Clears the trigger flags so the
   * next compaction cycle can fire, and re-anchors the idle timer.
   */
  notifyCompactionComplete(characterName: string, newTurnCount: number): void {
    const compaction = this.compactionStates.get(characterName);
    if (compaction === undefined) return;
    compaction.triggered = false;
    compaction.pending = false;
    compaction.activeTurnCount = newTurnCount;
    compaction.lastActivityMs = this.nowMs();
    this.lastRequests.delete(characterName);
    this.markDirty(characterName);
  }

  /**
   * Called by the compaction runner after `runCompaction` throws. Mirror of
   * `AutonomyManager::notify_compaction_failed` (manager.rs:602).
   *
   * Clears the trigger flag so a retry can fire; re-anchors the idle timer
   * so the failed attempt doesn't immediately re-trigger on the next tick.
   */
  notifyCompactionFailed(characterName: string): void {
    const compaction = this.compactionStates.get(characterName);
    if (compaction === undefined) return;
    compaction.triggered = false;
    compaction.lastActivityMs = this.nowMs();
    this.markDirty(characterName);
  }

  /**
   * Called by the dreaming runner after `runLibrarianSweep` returns
   * (success OR a skip from the cron gate). Mirror of the Ok(_) arms of
   * `execute_scheduled_dream` (manager.rs:1301-1322): clear retry
   * backoff so the next tick is free to re-check the cron gate.
   */
  notifyDreamingSuccess(characterName: string): void {
    const dreaming = this.dreamingStates.get(characterName);
    if (dreaming === undefined) return;
    dreaming.failureCount = 0;
    delete dreaming.nextAttemptAtMs;
    dreaming.running = false;
    this.markDirty(characterName);
  }

  /**
   * Called by the dreaming runner after `runLibrarianSweep` throws.
   * Mirror of the Err(e) arm (manager.rs:1323-1337): increment failure
   * count and back the next attempt off exponentially via
   * `backgroundRetryDelay`.
   */
  notifyDreamingFailed(characterName: string): void {
    const dreaming = this.dreamingStates.get(characterName);
    if (dreaming === undefined) return;
    dreaming.failureCount += 1;
    dreaming.nextAttemptAtMs = this.nowMs() + backgroundRetryDelayMs(dreaming.failureCount);
    dreaming.running = false;
    this.markDirty(characterName);
  }

  /** Test/debug accessor for dreaming backoff state. */
  dreamingState(characterName: string): {
    failureCount: number;
    nextAttemptAtMs: number | undefined;
    running: boolean;
  } | undefined {
    const d = this.dreamingStates.get(characterName);
    if (d === undefined) return undefined;
    return {
      failureCount: d.failureCount,
      nextAttemptAtMs: d.nextAttemptAtMs,
      running: d.running,
    };
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

  heartbeatTickNow(characterName: string): boolean | undefined {
    const clock = this.clocks.get(characterName);
    if (clock === undefined) return undefined;
    const dormant = clock.isDormant(this.nowMs());
    clock.forceWake(this.nowMs());
    this.markDirty(characterName);
    return dormant;
  }

  heartbeatSetDormant(characterName: string): boolean {
    const clock = this.clocks.get(characterName);
    if (clock === undefined) return false;
    clock.forceDormant();
    this.markDirty(characterName);
    return true;
  }

  heartbeatSetActive(characterName: string): boolean {
    const clock = this.clocks.get(characterName);
    if (clock === undefined) return false;
    clock.forceActive(this.nowMs());
    this.markDirty(characterName);
    return true;
  }

  setPaused(characterName: string, paused: boolean): boolean | undefined {
    if (!this.trackers.has(characterName)) return undefined;
    this.paused.set(characterName, paused);
    this.markDirty(characterName);
    return paused;
  }

  status(characterName: string): AutonomyStatus | undefined {
    const clock = this.clocks.get(characterName);
    const log = this.heartbeatLogs.get(characterName);
    if (clock === undefined || log === undefined) return undefined;

    const now = this.nowMs();
    const wall = this.wallNow();
    const nextWake = clock.nextWake();
    const lastUser = clock.lastUserAt();
    const status: AutonomyStatus = {
      paused: this.paused.get(characterName) ?? false,
      heartbeat_state: clock.stateAt(now),
      ticks_without_user: clock.ticksWithoutUser(),
      dormant_after_heartbeat_turns: clock.maxIdleTicks(),
      effective_interval_secs: clock.defaultIntervalSecs(),
      minimum_heartbeat_latency_secs: clock.minWakeIntervalSecs(),
      dormant_after_idle_time_secs: clock.maxSilentDurationSecs(),
      recent_events: log.recent(5),
    };
    if (nextWake !== undefined) {
      status.next_wake_at = monotonicToRfc3339(nextWake, now, wall);
      status.seconds_until_wake = secondsDelta(now, nextWake);
    }
    if (lastUser !== undefined) {
      status.last_user_at = monotonicToRfc3339(lastUser, now, wall);
      status.seconds_since_user = Math.floor((now - lastUser) / 1000);
    }
    return status;
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
    const now = this.nowMs();
    const when = now + clamped * 3600 * 1000;
    clock.schedule(when, now);
    const scheduled = clock.nextWake() ?? when;
    this.keepalives.get(characterName)?.setNextWake(scheduled);
    this.markDirty(characterName);
    return `Scheduled next moment in ${clamped.toFixed(1)} hours.`;
  }

  /**
   * One synchronous ticker pulse for a character. The async driver in 8d
   * consumes returned actions and performs the LLM work outside the state
   * mutation path.
   *
   * Compaction triggers (max_turns + idle_trigger) are evaluated here too.
   * If a trigger fires AND an `onIdleCompaction` runner is wired,
   * `runIdleCompaction` is set in the returned actions and the driver
   * dispatches it. If no runner is wired (test contexts, or production
   * before the dependency wiring lands), the per-character `pending` flag
   * is set instead — the post-generation handler's `shouldCompactNow`
   * picks it up on the user's next message. Matches the
   * `have_deps ? run_compaction_now : compaction_pending = true` branch in
   * `autonomy/manager.rs:1046`.
   */
  tickCharacter(characterName: string, now = this.nowMs()): TickActions {
    const clock = this.clocks.get(characterName);
    const keepalive = this.keepalives.get(characterName);
    if (clock === undefined || keepalive === undefined) {
      throw new Error(`tickCharacter: no autonomy state for "${characterName}"`);
    }

    const hadDeadline = clock.nextWake() !== undefined;
    let heartbeat = HeartbeatAction.None;
    let guardTripped = false;
    if (
      this.autonomyConfig.enabled
      && this.autonomyConfig.heartbeat.enabled
      && !(this.paused.get(characterName) ?? false)
    ) {
      heartbeat = clock.tick(now);
      if (heartbeat !== HeartbeatAction.None) {
        this.markDirty(characterName);
      }
      guardTripped =
        hadDeadline
        && heartbeat === HeartbeatAction.None
        && clock.nextWake() === undefined;
      if (guardTripped) {
        keepalive.setNextWake(undefined);
        this.pushHeartbeatEvent(
          characterName,
          "dormant",
          `Abandonment guard tripped (ticks without user: ${clock.ticksWithoutUser()})`,
        );
        this.markDirty(characterName);
      }
    }

    const keepaliveAction = keepalive.tick(now);

    let runIdleCompaction = false;
    const compaction = this.compactionStates.get(characterName);
    const cfg = this.compactionConfig;
    if (
      compaction !== undefined
      && this.autonomyConfig.enabled
      && cfg.enabled
      && !compaction.triggered
    ) {
      let shouldFire = false;
      if (
        cfg.maxTurns > 0
        && compaction.activeTurnCount >= cfg.maxTurns
        && compaction.activeTurnCount >= cfg.minTurns
      ) {
        shouldFire = true;
      } else if (
        compaction.activeTurnCount >= cfg.minTurns
        && cfg.idleTriggerSecs > 0
      ) {
        const idleSecs = (now - compaction.lastActivityMs) / 1000;
        if (idleSecs >= cfg.idleTriggerSecs) {
          shouldFire = true;
        }
      }
      if (shouldFire) {
        compaction.triggered = true;
        if (this.onIdleCompaction !== undefined) {
          runIdleCompaction = true;
        } else {
          compaction.pending = true;
        }
        this.markDirty(characterName);
      }
    }

    let runScheduledDream = false;
    const dreaming = this.dreamingStates.get(characterName);
    if (
      dreaming !== undefined
      && !dreaming.running
      && this.autonomyConfig.enabled
      && this.dreamingConfig.enabled
      && this.onScheduledDream !== undefined
    ) {
      const past = dreaming.nextAttemptAtMs === undefined
        || now >= dreaming.nextAttemptAtMs;
      if (past) {
        // Mark `running` so subsequent ticks don't re-fire while the
        // dispatch is in flight. The runner clears it via
        // notifyDreamingSuccess / notifyDreamingFailed.
        dreaming.running = true;
        runScheduledDream = true;
        this.markDirty(characterName);
      }
    }

    this.saveState(characterName);
    return {
      heartbeat,
      keepalive: keepaliveAction,
      guardTripped,
      runIdleCompaction,
      runScheduledDream,
    };
  }

  startTicker(characterName: string): void {
    if (this.tickers.has(characterName)) return;
    const timer = setInterval(() => {
      this.startTickerPulse(characterName);
    }, TICK_INTERVAL_MS);
    this.tickers.set(characterName, timer);
  }

  stopAll(): void {
    this.clearTickers();
    this.persistAllStates();
  }

  async shutdown(): Promise<void> {
    this.clearTickers();
    const pending = Array.from(this.inFlightTicks.values());
    await Promise.allSettled(pending);
    this.persistAllStates();
  }

  saveState(characterName: string): void {
    if (!this.dirty.has(characterName)) return;
    const clock = this.clocks.get(characterName);
    const dataDir = this.dataDirs.get(characterName);
    if (clock === undefined || dataDir === undefined) return;
    const now = this.nowMs();
    const wall = this.wallNow();
    const persisted: PersistedState = {
      version: STATE_VERSION,
      ticks_without_user: clock.ticksWithoutUser(),
      next_wake_at: clock.nextWake() === undefined
        ? null
        : monotonicToRfc3339(clock.nextWake()!, now, wall),
      last_user_at: clock.lastUserAt() === undefined
        ? null
        : monotonicToRfc3339(clock.lastUserAt()!, now, wall),
    };
    const file = statePath(dataDir);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, `${JSON.stringify(persisted, null, 2)}\n`);
    this.dirty.delete(characterName);
  }

  notifyLastRequest(characterName: string, request: ChatRequest): void {
    this.lastRequests.set(characterName, cloneChatRequestForCache(request));
  }

  cachedLastRequest(characterName: string): ChatRequest | undefined {
    const request = this.lastRequests.get(characterName);
    return request === undefined ? undefined : cloneChatRequestForCache(request);
  }

  onCacheWarmed(characterName: string): void {
    const keepalive = this.keepalives.get(characterName);
    if (keepalive === undefined) return;
    keepalive.onCacheWarmed(this.nowMs());
    this.markDirty(characterName);
  }

  onKeepaliveFailed(characterName: string): void {
    const keepalive = this.keepalives.get(characterName);
    if (keepalive === undefined) return;
    keepalive.onPingFailed(this.nowMs());
    this.markDirty(characterName);
  }

  pushHeartbeatEvent(
    characterName: string,
    kind: HeartbeatEventKind,
    detail: string,
  ): void {
    this.heartbeatLogs.get(characterName)?.push(kind, detail);
  }

  flushHeartbeatLog(characterName: string): void {
    this.heartbeatLogs.get(characterName)?.flushIfDirty();
  }

  heartbeatLog(characterName: string, limit: number): HeartbeatEvent[] {
    return this.heartbeatLogs.get(characterName)?.recent(limit) ?? [];
  }

  /** Test/debug accessor. */
  hasState(characterName: string): boolean {
    return this.trackers.has(characterName);
  }

  /** Test/debug accessor for the per-character heartbeat clock. */
  heartbeatClock(characterName: string): HeartbeatClock | undefined {
    return this.clocks.get(characterName);
  }

  /** Test/debug accessor for the per-character cache keepalive clock. */
  cacheKeepalive(characterName: string): CacheKeepalive | undefined {
    return this.keepalives.get(characterName);
  }

  private markDirty(characterName: string): void {
    if (this.clocks.has(characterName)) {
      this.dirty.add(characterName);
    }
  }

  private clearTickers(): void {
    for (const timer of this.tickers.values()) {
      clearInterval(timer);
    }
    this.tickers.clear();
  }

  private persistAllStates(): void {
    for (const name of this.clocks.keys()) {
      this.markDirty(name);
      this.saveState(name);
      this.flushHeartbeatLog(name);
    }
  }

  private startTickerPulse(characterName: string): void {
    if (this.inFlightTicks.has(characterName)) return;
    const pulse = this.runTickerPulse(characterName);
    this.inFlightTicks.set(characterName, pulse);
    void pulse.finally(() => {
      if (this.inFlightTicks.get(characterName) === pulse) {
        this.inFlightTicks.delete(characterName);
      }
    });
  }

  private async runTickerPulse(characterName: string): Promise<void> {
    if (this.ticking.has(characterName)) return;
    this.ticking.add(characterName);
    try {
      const actions = this.tickCharacter(characterName);
      if (
        this.onTickActions !== undefined
        && (actions.heartbeat !== HeartbeatAction.None
          || actions.keepalive !== CacheKeepaliveAction.None)
      ) {
        await this.onTickActions(characterName, actions);
      }
      if (actions.runIdleCompaction && this.onIdleCompaction !== undefined) {
        // The runner is responsible for calling notifyCompactionComplete /
        // notifyCompactionFailed to clear the triggered flag — until then,
        // the per-character `triggered` state set inside tickCharacter
        // keeps future ticks from double-firing.
        try {
          await this.onIdleCompaction(characterName);
        } catch (e) {
          console.warn(
            `[shore-daemon-ts] idle compaction failed for ${characterName}: ${(e as Error).message}`,
          );
          this.notifyCompactionFailed(characterName);
        }
      }
      if (actions.runScheduledDream && this.onScheduledDream !== undefined) {
        // Same pattern as compaction: the runner clears `running` via
        // notifyDreamingSuccess / notifyDreamingFailed. A bare throw
        // here (runner forgot to notify) is converted to "failed" so
        // the state doesn't get stuck with `running=true` forever.
        try {
          await this.onScheduledDream(characterName);
        } catch (e) {
          console.warn(
            `[shore-daemon-ts] scheduled dream failed for ${characterName}: ${(e as Error).message}`,
          );
          this.notifyDreamingFailed(characterName);
        }
      }
    } catch (e) {
      console.warn(
        `[shore-daemon-ts] autonomy tick failed for ${characterName}: ${(e as Error).message}`,
      );
    } finally {
      this.flushHeartbeatLog(characterName);
      this.saveState(characterName);
      this.ticking.delete(characterName);
    }
  }
}

/**
 * Mirror of `sanitize_compaction_config` (manager.rs:270): disables
 * compaction if `min_turns` / `max_turns` are not strictly greater than
 * `keep_recent_turns`, or if `max_turns < min_turns`. Logged-but-not-thrown
 * here matches the Rust behavior: misconfigured users still get a working
 * daemon (just without compaction).
 */
function sanitizeCompactionConfig(config: CompactionConfig): CompactionConfig {
  if (!config.enabled) return config;
  const k = config.keepRecentTurns;
  if (config.minTurns <= k || config.maxTurns <= k) {
    console.error(
      `[shore-daemon-ts] Compaction disabled: min_turns (${config.minTurns}) and max_turns (${config.maxTurns}) must be greater than keep_recent_turns (${k})`,
    );
    return { ...config, enabled: false };
  }
  if (config.maxTurns < config.minTurns) {
    console.error(
      `[shore-daemon-ts] Compaction disabled: max_turns (${config.maxTurns}) must be >= min_turns (${config.minTurns})`,
    );
    return { ...config, enabled: false };
  }
  return config;
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

function heartbeatClockConfig(config: HeartbeatConfig): HeartbeatConfig {
  return {
    fallbackHeartbeatIntervalSecs: config.fallbackHeartbeatIntervalSecs,
    dormantAfterHeartbeatTurns: config.dormantAfterHeartbeatTurns,
    dormantAfterIdleTimeSecs: config.dormantAfterIdleTimeSecs,
    minimumHeartbeatLatencySecs: config.minimumHeartbeatLatencySecs,
  };
}

function cloneAutonomyConfig(config: AutonomyConfig): AutonomyConfig {
  return {
    enabled: config.enabled,
    heartbeat: {
      ...config.heartbeat,
    },
  };
}

function secondsDelta(nowMs: number, targetMs: number): number {
  const delta = targetMs - nowMs;
  const seconds = Math.floor(Math.abs(delta) / 1000);
  return delta >= 0 ? seconds : -seconds;
}

function statePath(characterDataDir: string): string {
  return path.join(characterDataDir, STATE_FILENAME);
}

function loadPersistedState(file: string): PersistedState | undefined {
  let raw: string;
  try {
    raw = fs.readFileSync(file, "utf8");
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw e;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return undefined;
  }
  if (!isPlainObject(parsed)) return undefined;
  if (parsed["version"] !== STATE_VERSION) return undefined;
  const ticks = parsed["ticks_without_user"];
  if (typeof ticks !== "number" || !Number.isFinite(ticks)) return undefined;
  return {
    version: STATE_VERSION,
    ticks_without_user: ticks,
    next_wake_at: typeof parsed["next_wake_at"] === "string"
      ? parsed["next_wake_at"]
      : null,
    last_user_at: typeof parsed["last_user_at"] === "string"
      ? parsed["last_user_at"]
      : null,
  };
}

function restoreFromPersisted(
  persisted: PersistedState,
  clock: HeartbeatClock,
  nowMs: number,
  wallNow: Date,
): void {
  clock.restore(
    persisted.ticks_without_user,
    rfc3339ToMonotonic(persisted.next_wake_at, nowMs, wallNow),
    rfc3339ToMonotonic(persisted.last_user_at, nowMs, wallNow),
  );
}

function monotonicToRfc3339(instantMs: number, nowMs: number, wallNow: Date): string {
  return new Date(wallNow.getTime() + (instantMs - nowMs)).toISOString();
}

function rfc3339ToMonotonic(
  raw: string | null | undefined,
  nowMs: number,
  wallNow: Date,
): number | undefined {
  if (raw === null || raw === undefined) return undefined;
  const parsed = Date.parse(raw);
  if (!Number.isFinite(parsed)) return undefined;
  return nowMs + (parsed - wallNow.getTime());
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function cloneChatRequestForCache(request: ChatRequest): ChatRequest {
  const {
    signal: _signal,
    cacheForensics: _cacheForensics,
    ...rest
  } = request;
  return {
    ...rest,
    messages: request.messages.map((m) => ({
      ...m,
      content: m.content.map((b) => ({ ...b }) as Message["content_blocks"][number]),
      ...(m.images !== undefined ? { images: m.images.map((i) => ({ ...i })) } : {}),
    })),
    tools: request.tools.map((t) => ({
      ...t,
      inputSchema: { ...t.inputSchema },
    })),
  };
}
