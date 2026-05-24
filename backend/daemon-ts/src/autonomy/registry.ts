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
import type { AutonomyConfig } from "../config/loader.ts";

const BACKFILL_WINDOW_DAYS = 90;
const STATE_VERSION = 4;
const STATE_FILENAME = "autonomy_state.json";
const TICK_INTERVAL_MS = 10_000;

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

export interface TickActions {
  heartbeat: HeartbeatAction;
  keepalive: CacheKeepaliveAction;
  guardTripped: boolean;
}

export interface AutonomyRegistryOptions {
  autonomyConfig?: AutonomyConfig;
  /** Start the per-character 10s ticker as states are first ensured. */
  autoStartTicker?: boolean;
  /** Injectable monotonic clock for deterministic tests. */
  nowMs?: () => number;
  /** Injectable wall clock for persistence tests. */
  wallNow?: () => Date;
  /** Async 8d driver for returned heartbeat/keepalive actions. */
  onTickActions?: (characterName: string, actions: TickActions) => Promise<void> | void;
}

export class AutonomyRegistry {
  private readonly trackers = new Map<string, ActivityTracker>();
  private readonly clocks = new Map<string, HeartbeatClock>();
  private readonly keepalives = new Map<string, CacheKeepalive>();
  private readonly heartbeatLogs = new Map<string, HeartbeatLog>();
  private readonly lastRequests = new Map<string, ChatRequest>();
  private readonly dataDirs = new Map<string, string>();
  private readonly dirty = new Set<string>();
  private readonly tickers = new Map<string, ReturnType<typeof setInterval>>();
  private readonly ticking = new Set<string>();
  private readonly autonomyConfig: AutonomyConfig;
  private readonly autoStartTicker: boolean;
  private readonly nowMs: () => number;
  private readonly wallNow: () => Date;
  private readonly onTickActions:
    | ((characterName: string, actions: TickActions) => Promise<void> | void)
    | undefined;

  constructor(options: AutonomyRegistryOptions = {}) {
    this.autonomyConfig = options.autonomyConfig ?? DEFAULT_AUTONOMY_CONFIG;
    this.autoStartTicker = options.autoStartTicker ?? false;
    this.nowMs = options.nowMs ?? (() => performance.now());
    this.wallNow = options.wallNow ?? (() => new Date());
    this.onTickActions = options.onTickActions;
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

    const timestamps = collectBackfillTimestamps(engine);
    if (timestamps.length > 0) {
      tracker.backfill(timestamps);
    }
    if (this.autoStartTicker) {
      this.startTicker(name);
    }
    return tracker;
  }

  /** Record a fresh user turn. Mirrors `notify_user_message`. */
  notifyUserMessage(characterName: string): void {
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
    this.markDirty(characterName);
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
    if (this.autonomyConfig.enabled && this.autonomyConfig.heartbeat.enabled) {
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
    this.saveState(characterName);
    return { heartbeat, keepalive: keepaliveAction, guardTripped };
  }

  startTicker(characterName: string): void {
    if (this.tickers.has(characterName)) return;
    const timer = setInterval(() => {
      void this.runTickerPulse(characterName);
    }, TICK_INTERVAL_MS);
    this.tickers.set(characterName, timer);
  }

  stopAll(): void {
    for (const timer of this.tickers.values()) {
      clearInterval(timer);
    }
    this.tickers.clear();
    for (const name of this.clocks.keys()) {
      this.markDirty(name);
      this.saveState(name);
      this.flushHeartbeatLog(name);
    }
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
