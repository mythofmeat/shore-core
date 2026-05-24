/**
 * ActivityTracker — per-character message rhythm statistics.
 *
 * Mirrors `backend/daemon/src/autonomy/activity.rs` line-for-line. The
 * "Instant" in Rust is `tokio::time::Instant` (a monotonic clock); the
 * TS port uses `performance.now()` (also monotonic, also millisecond
 * resolution) and stores it as a number of milliseconds. Wall-clock
 * times are stored as JS `Date` objects in the host TZ — same shape as
 * Rust's `NaiveDateTime` (no zone) since the Rust impl reads
 * `Local::now().naive_local()`.
 *
 * "PORT with care": the constants and the math need to match the Rust
 * implementation exactly so the heatmap classifications produced by the
 * two daemons agree on the same input.
 */

// ── Constants (verbatim from Rust) ──────────────────────────────────────

/** Idle gap (seconds) marking a session boundary. */
export const SESSION_GAP_SECS = 1800;

/** Minimum messages for adaptive timing. */
export const SUFFICIENT_DATA_MSGS = 5;
/** Minimum distinct days for adaptive timing. */
export const SUFFICIENT_DATA_DAYS = 2;

/** Minimum messages for hour-weighted (heatmap) timing. */
export const SUFFICIENT_HEATMAP_MSGS = 20;
/** Minimum distinct days for hour-weighted timing. */
export const SUFFICIENT_HEATMAP_DAYS = 7;

/** Below this many events on a weekday, fall back to global histogram. */
export const WEEKDAY_HEATMAP_MIN = 5;

/** Hour classified as peak if density > avg × this factor. */
export const PEAK_HOUR_THRESHOLD = 1.5;
/** Hour classified as trough if density < avg × this factor. */
export const TROUGH_HOUR_THRESHOLD = 0.5;

/** Stats cache validity in seconds. */
export const STATS_CACHE_TTL_SECS = 60;

/** Number of recent sessions used for session-median calculation. */
export const SESSION_MEDIANS_WINDOW = 30;
/** Number of response gaps tracked per session for tempo. */
export const SESSION_TEMPO_WINDOW = 10;

/** Z-score threshold for anomaly detection. */
export const ANOMALY_Z_SCORE = 1.5;

// ── Types ───────────────────────────────────────────────────────────────

export type HourClassification = "peak" | "trough" | "normal";

/** 0 = Monday .. 6 = Sunday, matching chrono `num_days_from_monday`. */
export type Weekday = 0 | 1 | 2 | 3 | 4 | 5 | 6;

export interface MessageTimestamp {
  /** Monotonic millisecond timestamp (performance.now() basis). */
  monotonic: number;
  /** Wall clock time. Compared by epoch ms across all math. */
  wallClock: Date;
  /** Day of the week, Monday-indexed. */
  weekday: Weekday;
}

export interface ActivityStats {
  engagementScore: number;
  consistency: number;
  tempoScore: number;
  sessionCount: number;
  sessionsPerDay: number;
  /** Length 24 — fraction of messages per hour-of-day. */
  hourHistogram: number[];
  /** Length 24. */
  hourClassifications: HourClassification[];
  hasSufficientData: boolean;
  hasSufficientHeatmap: boolean;
  medianSessionGap: number | undefined;
  anomalyZScore: number | undefined;
  /** Monotonic ms at which this stats snapshot was computed. */
  computedAt: number;
}

// ── ActivityTracker ─────────────────────────────────────────────────────

export class ActivityTracker {
  private readonly timestamps: MessageTimestamp[] = [];
  private cachedStats: ActivityStats | undefined;

  /** Record a new message event at the current time. */
  recordMessage(): void {
    this.recordMessageAt(monotonicNowMs(), new Date());
  }

  /** Record a message with explicit timestamps (useful for testing). */
  recordMessageAt(monotonic: number, wallClock: Date): void {
    this.timestamps.push({
      monotonic,
      wallClock,
      weekday: weekdayMon0(wallClock),
    });
    this.cachedStats = undefined;
  }

  /**
   * Backfill the tracker with historical wall-clock timestamps. No-op
   * when the tracker already has data, mirroring Rust's safety guard.
   */
  backfill(wallClocks: Date[]): void {
    if (this.timestamps.length > 0 || wallClocks.length === 0) return;
    const base = monotonicNowMs();
    for (let i = 0; i < wallClocks.length; i++) {
      const wallClock = wallClocks[i]!;
      this.timestamps.push({
        // Use nanosecond-scale offset like Rust does; ms precision is fine
        // because monotonic only matters for relative ordering during the
        // process lifetime.
        monotonic: base + i / 1e6,
        wallClock,
        weekday: weekdayMon0(wallClock),
      });
    }
    this.timestamps.sort((a, b) => a.wallClock.getTime() - b.wallClock.getTime());
    this.cachedStats = undefined;
  }

  messageCount(): number {
    return this.timestamps.length;
  }

  /** Get cached stats, recomputing if stale or absent. */
  stats(): ActivityStats {
    const cached = this.cachedStats;
    if (cached !== undefined && (monotonicNowMs() - cached.computedAt) / 1000 < STATS_CACHE_TTL_SECS) {
      return cached;
    }
    const fresh = this.computeStats();
    this.cachedStats = fresh;
    return fresh;
  }

  /** Force recompute (bypass TTL). */
  recomputeStats(): ActivityStats {
    this.cachedStats = this.computeStats();
    return this.cachedStats;
  }

  /** Test-only accessor for cache state assertions. */
  hasCachedStats(): boolean {
    return this.cachedStats !== undefined;
  }

  // ── Internal computations ────────────────────────────────────────────

  private computeStats(): ActivityStats {
    const now = monotonicNowMs();
    const currentWeekday = weekdayMon0(new Date());

    const distinctDays = this.distinctDays();
    const msgCount = this.timestamps.length;

    const hasSufficientData = msgCount >= SUFFICIENT_DATA_MSGS && distinctDays >= SUFFICIENT_DATA_DAYS;
    const hasSufficientHeatmap = msgCount >= SUFFICIENT_HEATMAP_MSGS && distinctDays >= SUFFICIENT_HEATMAP_DAYS;

    const sessions = this.detectSessions();
    const sessionCount = sessions.length;
    const sessionsPerDay = distinctDays > 0 ? sessionCount / distinctDays : 0;

    const consistency = this.computeConsistency();
    const sessionGaps = this.computeSessionGaps(sessions);
    const medianSessionGap = median(sessionGaps);

    const tempoGaps = this.computeTempoGaps(sessions);
    const tempoScore = computeTempoScore(tempoGaps);

    const engagementScore = 0.6 * consistency + 0.4 * tempoScore;

    const hourHistogram = this.computeHourHistogram(currentWeekday);
    const hourClassifications = classifyHours(hourHistogram);

    const anomalyZScore = this.computeAnomalyZScore(sessions);

    return {
      engagementScore,
      consistency,
      tempoScore,
      sessionCount,
      sessionsPerDay,
      hourHistogram,
      hourClassifications,
      hasSufficientData,
      hasSufficientHeatmap,
      medianSessionGap,
      anomalyZScore,
      computedAt: now,
    };
  }

  private distinctDays(): number {
    const days = new Set<string>();
    for (const ts of this.timestamps) {
      days.add(dateKey(ts.wallClock));
    }
    return days.size;
  }

  private computeConsistency(): number {
    if (this.timestamps.length < 2) {
      return this.timestamps.length === 0 ? 0 : 1;
    }
    const first = this.timestamps[0]!.wallClock;
    const last = this.timestamps[this.timestamps.length - 1]!.wallClock;
    const spanDays = daysBetween(first, last) + 1;
    if (spanDays <= 0) return 1;
    const activeDays = this.distinctDays();
    return clamp(activeDays / spanDays, 0, 1);
  }

  detectSessions(): number[][] {
    if (this.timestamps.length === 0) return [];

    const sessions: number[][] = [];
    let current: number[] = [0];

    for (let i = 1; i < this.timestamps.length; i++) {
      const gap = Math.abs(
        Math.trunc((this.timestamps[i]!.wallClock.getTime() - this.timestamps[i - 1]!.wallClock.getTime()) / 1000),
      );
      if (gap >= SESSION_GAP_SECS) {
        sessions.push(current);
        current = [];
      }
      current.push(i);
    }
    sessions.push(current);

    if (sessions.length > SESSION_MEDIANS_WINDOW) {
      sessions.splice(0, sessions.length - SESSION_MEDIANS_WINDOW);
    }
    return sessions;
  }

  private computeSessionGaps(sessions: number[][]): number[] {
    if (sessions.length < 2) return [];
    const gaps: number[] = [];
    for (let i = 0; i < sessions.length - 1; i++) {
      const prev = sessions[i]!;
      const next = sessions[i + 1]!;
      const lastOfPrev = prev[prev.length - 1]!;
      const firstOfNext = next[0]!;
      const gap = Math.abs(
        Math.trunc((this.timestamps[firstOfNext]!.wallClock.getTime() - this.timestamps[lastOfPrev]!.wallClock.getTime()) / 1000),
      );
      gaps.push(gap);
    }
    return gaps;
  }

  private computeTempoGaps(sessions: number[][]): number[] {
    const all: number[] = [];
    for (const session of sessions) {
      for (let i = 0; i < session.length - 1; i++) {
        const a = session[i]!;
        const b = session[i + 1]!;
        const gap = Math.abs(
          Math.trunc((this.timestamps[b]!.wallClock.getTime() - this.timestamps[a]!.wallClock.getTime()) / 1000),
        );
        all.push(gap);
      }
    }
    if (all.length > SESSION_TEMPO_WINDOW) {
      all.splice(0, all.length - SESSION_TEMPO_WINDOW);
    }
    return all;
  }

  computeHourHistogram(currentWeekday: Weekday): number[] {
    const weekdayEvents = this.timestamps.filter((ts) => ts.weekday === currentWeekday);
    const source: number[] =
      weekdayEvents.length >= WEEKDAY_HEATMAP_MIN
        ? weekdayEvents.map((ts) => ts.wallClock.getHours())
        : this.timestamps.map((ts) => ts.wallClock.getHours());

    const histogram = new Array<number>(24).fill(0);
    for (const hour of source) {
      histogram[hour]! += 1;
    }
    const total = histogram.reduce((acc, v) => acc + v, 0);
    if (total > 0) {
      for (let h = 0; h < 24; h++) histogram[h]! /= total;
    }
    return histogram;
  }

  private computeAnomalyZScore(sessions: number[][]): number | undefined {
    const gaps = this.computeSessionGaps(sessions);
    if (gaps.length < 3) return undefined;
    const mean = gaps.reduce((acc, g) => acc + g, 0) / gaps.length;
    const variance = gaps.reduce((acc, g) => acc + (g - mean) ** 2, 0) / gaps.length;
    const stdDev = Math.sqrt(variance);
    if (stdDev < Number.EPSILON) return 0;
    const last = gaps[gaps.length - 1]!;
    return (last - mean) / stdDev;
  }
}

// ── Free functions ──────────────────────────────────────────────────────

/** Logistic tempo score: `1 / (1 + e^((median_gap - 900) / 400))`. */
export function computeTempoScore(gaps: number[]): number {
  const med = median(gaps);
  if (med === undefined) return 0.5;
  return 1 / (1 + Math.exp((med - 900) / 400));
}

/** Classify each hour as Peak / Trough / Normal by density vs. average. */
export function classifyHours(histogram: number[]): HourClassification[] {
  const nonZero = histogram.filter((d) => d > 0);
  const avg = nonZero.length === 0 ? 0 : nonZero.reduce((acc, v) => acc + v, 0) / nonZero.length;

  const result: HourClassification[] = new Array<HourClassification>(24).fill("normal");
  if (avg < Number.EPSILON) return result;

  for (let i = 0; i < 24; i++) {
    const density = histogram[i] ?? 0;
    if (density > avg * PEAK_HOUR_THRESHOLD) {
      result[i] = "peak";
    } else if (density < avg * TROUGH_HOUR_THRESHOLD) {
      result[i] = "trough";
    }
  }
  return result;
}

export function median(values: number[]): number | undefined {
  if (values.length === 0) return undefined;
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  if (sorted.length % 2 === 0) {
    return (sorted[mid - 1]! + sorted[mid]!) / 2;
  }
  return sorted[mid]!;
}

// ── helpers ─────────────────────────────────────────────────────────────

function monotonicNowMs(): number {
  // performance.now is monotonic and ms-resolution in Bun/Node.
  return performance.now();
}

function weekdayMon0(d: Date): Weekday {
  // JS `getDay()` is 0=Sun..6=Sat; Rust chrono's `num_days_from_monday`
  // is 0=Mon..6=Sun. Shift accordingly.
  return (((d.getDay() + 6) % 7) as Weekday);
}

function dateKey(d: Date): string {
  // Group by host-local calendar date (matches Rust's Local TZ).
  return `${d.getFullYear()}-${d.getMonth() + 1}-${d.getDate()}`;
}

function daysBetween(a: Date, b: Date): number {
  const dayMs = 86_400_000;
  const ka = Date.UTC(a.getFullYear(), a.getMonth(), a.getDate());
  const kb = Date.UTC(b.getFullYear(), b.getMonth(), b.getDate());
  return Math.round((kb - ka) / dayMs);
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, v));
}
