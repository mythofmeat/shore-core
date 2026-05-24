/**
 * CacheKeepalive — prompt-cache bridge during quiet stretches.
 *
 * Mirrors `backend/daemon/src/cache_keepalive.rs`. It is deliberately
 * ignorant of heartbeat/autonomy policy: callers tell it when a prompt
 * cache was warmed and what the next scheduled wake is, then poll `tick()`
 * from the background loop.
 */

export enum CacheKeepaliveAction {
  None = "None",
  Ping = "Ping",
}

const KEEPALIVE_BREAKEVEN_MS = 18 * 3600 * 1000;
const DEFAULT_PING_INTERVAL_SECS = 55 * 60;

export function pingIntervalMs(): number {
  const raw = process.env["SHORE_KEEPALIVE_INTERVAL_SECS"];
  if (raw === undefined) return DEFAULT_PING_INTERVAL_SECS * 1000;
  const parsed = Number.parseInt(raw, 10);
  if (!Number.isFinite(parsed) || parsed < 0) {
    return DEFAULT_PING_INTERVAL_SECS * 1000;
  }
  return parsed * 1000;
}

function retryDelayMs(failureCount: number): number {
  const exponent = Math.min(5, Math.max(0, failureCount - 1));
  const secs = Math.min(15 * 60, 30 * 2 ** exponent);
  return secs * 1000;
}

export class CacheKeepalive {
  private nextPingAtMs: number | undefined = undefined;
  private nextWakeAtMs: number | undefined = undefined;
  private failureCountValue = 0;

  onCacheWarmed(nowMs: number): void {
    this.nextPingAtMs = nowMs + pingIntervalMs();
    this.failureCountValue = 0;
  }

  onCacheInvalidated(): void {
    this.nextPingAtMs = undefined;
    this.failureCountValue = 0;
  }

  setNextWake(atMs: number | undefined): void {
    this.nextWakeAtMs = atMs;
    if (atMs === undefined) {
      this.nextPingAtMs = undefined;
      this.failureCountValue = 0;
    }
  }

  tick(nowMs: number): CacheKeepaliveAction {
    if (this.nextPingAtMs === undefined || nowMs < this.nextPingAtMs) {
      return CacheKeepaliveAction.None;
    }
    if (this.nextWakeAtMs === undefined) {
      return CacheKeepaliveAction.None;
    }
    if (
      this.nextWakeAtMs > nowMs
      && this.nextWakeAtMs - nowMs >= KEEPALIVE_BREAKEVEN_MS
    ) {
      this.nextPingAtMs = undefined;
      return CacheKeepaliveAction.None;
    }
    return CacheKeepaliveAction.Ping;
  }

  onPingFailed(nowMs: number): void {
    this.failureCountValue = Math.min(
      Number.MAX_SAFE_INTEGER,
      this.failureCountValue + 1,
    );
    this.nextPingAtMs = nowMs + retryDelayMs(this.failureCountValue);
  }

  nextPingAt(): number | undefined {
    return this.nextPingAtMs;
  }

  nextWakeAt(): number | undefined {
    return this.nextWakeAtMs;
  }

  failureCount(): number {
    return this.failureCountValue;
  }
}
