import { describe, expect, test } from "bun:test";

import {
  CacheKeepalive,
  CacheKeepaliveAction,
} from "../src/autonomy/cache_keepalive.ts";

const BASE_MS = 1_000_000;
const HOUR_MS = 3600 * 1000;
const MINUTE_MS = 60 * 1000;

describe("CacheKeepalive", () => {
  test("new returns no action", () => {
    const ka = new CacheKeepalive();
    expect(ka.tick(BASE_MS)).toBe(CacheKeepaliveAction.None);
  });

  test("ping fires after interval", () => {
    const ka = new CacheKeepalive();
    ka.onCacheWarmed(BASE_MS);
    ka.setNextWake(BASE_MS + 4 * HOUR_MS);
    expect(ka.tick(BASE_MS + 54 * MINUTE_MS)).toBe(CacheKeepaliveAction.None);
    expect(ka.tick(BASE_MS + 55 * MINUTE_MS)).toBe(CacheKeepaliveAction.Ping);
  });

  test("ping reschedules after caller confirms", () => {
    const ka = new CacheKeepalive();
    ka.onCacheWarmed(BASE_MS);
    ka.setNextWake(BASE_MS + 4 * HOUR_MS);
    expect(ka.tick(BASE_MS + 55 * MINUTE_MS)).toBe(CacheKeepaliveAction.Ping);
    ka.onCacheWarmed(BASE_MS + 55 * MINUTE_MS);
    expect(ka.tick(BASE_MS + 109 * MINUTE_MS)).toBe(CacheKeepaliveAction.None);
    expect(ka.tick(BASE_MS + 110 * MINUTE_MS)).toBe(CacheKeepaliveAction.Ping);
  });

  test("ping retries when not confirmed", () => {
    const ka = new CacheKeepalive();
    ka.onCacheWarmed(BASE_MS);
    ka.setNextWake(BASE_MS + 4 * HOUR_MS);
    const due = BASE_MS + 55 * MINUTE_MS;
    expect(ka.tick(due)).toBe(CacheKeepaliveAction.Ping);
    ka.onPingFailed(due);
    expect(ka.tick(due + 29_000)).toBe(CacheKeepaliveAction.None);
    expect(ka.tick(due + 30_000)).toBe(CacheKeepaliveAction.Ping);
  });

  test("no ping when wake exceeds breakeven", () => {
    const ka = new CacheKeepalive();
    ka.onCacheWarmed(BASE_MS);
    ka.setNextWake(BASE_MS + 30 * HOUR_MS);
    expect(ka.tick(BASE_MS + 55 * MINUTE_MS)).toBe(CacheKeepaliveAction.None);
    expect(ka.tick(BASE_MS + 2 * HOUR_MS)).toBe(CacheKeepaliveAction.None);
  });

  test("no ping when no wake set", () => {
    const ka = new CacheKeepalive();
    ka.onCacheWarmed(BASE_MS);
    expect(ka.tick(BASE_MS + HOUR_MS)).toBe(CacheKeepaliveAction.None);
  });

  test("guard trip clears pings", () => {
    const ka = new CacheKeepalive();
    ka.onCacheWarmed(BASE_MS);
    ka.setNextWake(BASE_MS + 4 * HOUR_MS);
    ka.setNextWake(undefined);
    expect(ka.tick(BASE_MS + HOUR_MS)).toBe(CacheKeepaliveAction.None);
  });

  test("cache warm resets ping deadline", () => {
    const ka = new CacheKeepalive();
    ka.onCacheWarmed(BASE_MS);
    ka.setNextWake(BASE_MS + 4 * HOUR_MS);
    ka.onCacheWarmed(BASE_MS + 30 * MINUTE_MS);
    expect(ka.tick(BASE_MS + 55 * MINUTE_MS)).toBe(CacheKeepaliveAction.None);
    expect(ka.tick(BASE_MS + 85 * MINUTE_MS)).toBe(CacheKeepaliveAction.Ping);
  });

  test("compaction invalidation pauses and later warm resumes", () => {
    const ka = new CacheKeepalive();
    ka.onCacheWarmed(BASE_MS);
    ka.setNextWake(BASE_MS + 4 * HOUR_MS);
    ka.onCacheInvalidated();
    expect(ka.tick(BASE_MS + 55 * MINUTE_MS)).toBe(CacheKeepaliveAction.None);
    ka.onCacheWarmed(BASE_MS + HOUR_MS);
    expect(ka.tick(BASE_MS + HOUR_MS + 55 * MINUTE_MS)).toBe(CacheKeepaliveAction.Ping);
  });
});
