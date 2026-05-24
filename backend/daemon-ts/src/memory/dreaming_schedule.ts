/**
 * Dreaming schedule evaluation — `isDueNow`.
 *
 * Mirror of `backend/daemon/src/memory/dreaming.rs::is_due` (line 1154).
 * Wraps the `croner` library so the autonomy tick can ask "should
 * dreaming run this character now?" without each caller learning cron.
 *
 * Behaviour matches Rust:
 *   - never-ran: due as long as the cron's previous occurrence is in
 *     the past (true for any recurring schedule)
 *   - ran-before: due iff there's a scheduled occurrence strictly after
 *     `lastRunAt` and at or before `now`
 *   - invalid cron / malformed timestamp: NOT due (logged, not thrown —
 *     the autonomy loop must keep ticking)
 */

import { Cron } from "croner";

export function isDueNow(
  frequency: string,
  lastRunAt: string | undefined,
  nowOverride?: Date,
): boolean {
  let cron: Cron;
  try {
    cron = new Cron(frequency);
  } catch (e) {
    console.warn(
      `[shore-daemon-ts] dreaming: invalid cron schedule ${JSON.stringify(frequency)}: ${(e as Error).message}`,
    );
    return false;
  }

  const now = nowOverride ?? new Date();

  if (lastRunAt === undefined) {
    // Mirror Rust's "initial_due_window_start - 1min" approach: as long
    // as any scheduled occurrence has happened before now, the character
    // is due to dream on the very next tick.
    return cron.previousRuns(1, now).length > 0;
  }

  const lastRun = new Date(lastRunAt);
  if (Number.isNaN(lastRun.getTime())) {
    return cron.previousRuns(1, now).length > 0;
  }

  const next = cron.nextRun(lastRun);
  return next !== null && next.getTime() <= now.getTime();
}
