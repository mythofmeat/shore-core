/**
 * IdleTimer tests — mirror of
 * `backend/daemon/src/memory/compaction/mod.rs::test_idle_timer_fires_after_duration`
 * and `test_idle_timer_resets_on_activity`.
 *
 * Rust uses `tokio::time::pause` / `advance` for deterministic scheduling.
 * Bun doesn't have an equivalent built-in fake-time API, so we use a short
 * real-time idle period and bounded waits instead.
 */
import { describe, expect, it } from "bun:test";

import {
  ActivityNotify,
  IdleTimer,
} from "../src/memory/compaction/idle_timer.ts";

const sleep = (ms: number): Promise<void> =>
  new Promise((r) => setTimeout(r, ms));

describe("IdleTimer", () => {
  it("fires after the idle duration with no activity", async () => {
    const notify = new ActivityNotify();
    const timer = new IdleTimer(50, notify);
    let fired = false;
    const handle = timer.waitForIdle().then(() => {
      fired = true;
    });

    await sleep(20);
    expect(fired).toBe(false);

    await handle;
    expect(fired).toBe(true);
  });

  it("resets on activity", async () => {
    const notify = new ActivityNotify();
    const timer = new IdleTimer(60, notify);
    let fired = false;
    const handle = timer.waitForIdle().then(() => {
      fired = true;
    });

    // Tick once before idle elapses → should NOT fire.
    await sleep(30);
    expect(fired).toBe(false);
    notify.notify();

    // Tick again before the reset idle elapses → still should NOT fire.
    await sleep(30);
    expect(fired).toBe(false);
    notify.notify();

    // Now leave it alone for a full idle period; it should fire.
    await handle;
    expect(fired).toBe(true);
  });
});
