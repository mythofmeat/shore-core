/**
 * Per-character prompt-cache warm/cold state machine.
 *
 * Port of `backend/ledger/src/cache_tracker.rs`.
 */

export type CacheState = "cold" | "warm";
export type CacheAnomaly = "unexpected_write" | "keepalive_miss";

export interface CacheObservation {
  ts: string;
  model: string;
  thinkingEnabled: boolean;
  cacheReadTokens: number;
  cacheWriteTokens: number;
  callType: string;
}

export interface CacheObservationResult {
  state: CacheState;
  anomaly?: CacheAnomaly;
}

export class CacheTracker {
  private state_: CacheState = "cold";
  private lastTs: Date | undefined;
  private lastModel: string | undefined;
  private lastThinking: boolean | undefined;
  private lastCallType: string | undefined;
  private lastCacheRead = 0;
  private lastToolLoopKind: string | undefined;
  private lastToolLoopCacheRead = 0;
  private ttlExpiredSinceWarm = false;

  constructor(private readonly ttlSecs = 3600) {}

  static withTtlSecs(ttlSecs: number): CacheTracker {
    return new CacheTracker(ttlSecs);
  }

  static reconstruct(
    lastTs: string,
    lastModel: string,
    lastThinking: boolean,
    lastCacheRead: number,
    ttlSecs: number,
  ): CacheTracker {
    const tracker = new CacheTracker(ttlSecs);
    const parsed = parseDate(lastTs);
    if (parsed !== undefined) {
      const elapsedSecs = (Date.now() - parsed.getTime()) / 1000;
      if (elapsedSecs < ttlSecs && lastCacheRead > 0) {
        tracker.state_ = "warm";
      }
    }
    tracker.lastTs = parsed;
    tracker.lastModel = lastModel;
    tracker.lastThinking = lastThinking;
    tracker.lastCacheRead = lastCacheRead;
    return tracker;
  }

  state(): CacheState {
    return this.state_;
  }

  lastCacheReadTokens(): number {
    return this.lastCacheRead;
  }

  observe(obs: CacheObservation): CacheObservationResult {
    const obsTs = parseDate(obs.ts);

    if (obs.callType === "compaction") {
      this.state_ = "cold";
      this.lastCacheRead = 0;
      this.clearToolLoopBaseline();
      this.ttlExpiredSinceWarm = false;
      this.updateMetadata(obsTs, obs.model, obs.thinkingEnabled);
      this.lastCallType = obs.callType;
      return { state: this.state_ };
    }

    const loopKind = toolLoopKind(obs.callType);
    const skipNormalComparison =
      obs.callType === "heartbeat" || loopKind !== undefined;

    if (this.state_ === "warm" && this.lastTs !== undefined && obsTs !== undefined) {
      const elapsedSecs = (obsTs.getTime() - this.lastTs.getTime()) / 1000;
      if (elapsedSecs > this.ttlSecs) {
        this.state_ = "cold";
        this.lastCacheRead = 0;
        this.clearToolLoopBaseline();
        this.ttlExpiredSinceWarm = true;
      }
    }

    if (this.state_ === "warm" && this.lastModel !== undefined && this.lastModel !== obs.model) {
      this.state_ = "cold";
      this.lastCacheRead = 0;
      this.clearToolLoopBaseline();
      this.ttlExpiredSinceWarm = false;
    }

    if (
      this.state_ === "warm" &&
      this.lastThinking !== undefined &&
      this.lastThinking !== obs.thinkingEnabled
    ) {
      this.state_ = "cold";
      this.lastCacheRead = 0;
      this.clearToolLoopBaseline();
      this.ttlExpiredSinceWarm = false;
    }

    let anomaly: CacheAnomaly | undefined;
    if (this.state_ === "warm") {
      anomaly = this.observeWarmCache(obs, loopKind);
    } else {
      if (obs.cacheReadTokens > 0 || obs.cacheWriteTokens > 0) {
        this.state_ = "warm";
      }
    }

    if (this.ttlExpiredSinceWarm) {
      if (obs.callType === "keepalive") {
        this.ttlExpiredSinceWarm = false;
      } else {
        anomaly ??= "keepalive_miss";
        this.ttlExpiredSinceWarm = false;
      }
    }

    if (loopKind !== undefined) {
      if (anomaly === undefined) {
        this.lastToolLoopKind = loopKind;
        this.lastToolLoopCacheRead = obs.cacheReadTokens;
      } else {
        this.clearToolLoopBaseline();
      }
    } else if (!skipNormalComparison) {
      this.lastCacheRead = obs.cacheReadTokens;
      this.clearToolLoopBaseline();
    } else if (obs.callType === "heartbeat") {
      this.clearToolLoopBaseline();
    }

    this.updateMetadata(obsTs, obs.model, obs.thinkingEnabled);
    this.lastCallType = obs.callType;

    return anomaly === undefined
      ? { state: this.state_ }
      : { state: this.state_, anomaly };
  }

  private observeWarmCache(
    obs: CacheObservation,
    loopKind: string | undefined,
  ): CacheAnomaly | undefined {
    if (loopKind !== undefined) {
      const continuedLoop =
        this.lastToolLoopKind === loopKind && this.lastCallType === obs.callType;
      const droppedWithinLoop =
        continuedLoop && obs.cacheReadTokens < this.lastToolLoopCacheRead;
      const coldWriteAfterWarmMessage =
        !continuedLoop &&
        this.lastCacheRead > 0 &&
        obs.cacheReadTokens === 0 &&
        obs.cacheWriteTokens > 0;

      if (droppedWithinLoop || coldWriteAfterWarmMessage) {
        this.state_ = "cold";
        this.lastCacheRead = 0;
        return "unexpected_write";
      }
      return undefined;
    }

    if (obs.callType === "heartbeat" || obs.cacheReadTokens >= this.lastCacheRead) {
      return undefined;
    }
    this.state_ = "cold";
    this.lastCacheRead = 0;
    return "unexpected_write";
  }

  private updateMetadata(ts: Date | undefined, model: string, thinking: boolean): void {
    this.lastTs = ts;
    this.lastModel = model;
    this.lastThinking = thinking;
  }

  private clearToolLoopBaseline(): void {
    this.lastToolLoopKind = undefined;
    this.lastToolLoopCacheRead = 0;
  }
}

function parseDate(ts: string): Date | undefined {
  const d = new Date(ts);
  return Number.isNaN(d.getTime()) ? undefined : d;
}

function toolLoopKind(callType: string): string | undefined {
  if (callType === "tool_loop") return "tool_loop";
  if (callType === "heartbeat_tool_loop") return "heartbeat_tool_loop";
  return undefined;
}
