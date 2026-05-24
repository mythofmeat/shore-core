/**
 * IdleTimer — wait for an idle period to elapse without activity.
 *
 * Port of `backend/daemon/src/memory/compaction/mod.rs::IdleTimer`.
 *
 * Activity notifications (via `CompactionManager.notifyActivity()` →
 * `ActivityNotify.notify()`) reset the timer. `waitForIdle()` returns when
 * the full idle period elapses with no notifications.
 */

// ---------------------------------------------------------------------------
// ActivityNotify — minimal Tokio Notify equivalent
// ---------------------------------------------------------------------------

/**
 * One-shot, edge-triggered notify. `notify()` wakes the current waiter
 * (if any); a subsequent `notified()` resolves promptly only for the next
 * notification, matching Tokio's `Notify` semantics enough for our needs.
 */
export class ActivityNotify {
  private waiter: (() => void) | undefined;

  notify(): void {
    const w = this.waiter;
    this.waiter = undefined;
    if (w !== undefined) w();
  }

  notified(): Promise<void> {
    return new Promise<void>((resolve) => {
      this.waiter = resolve;
    });
  }
}

// ---------------------------------------------------------------------------
// IdleTimer
// ---------------------------------------------------------------------------

export class IdleTimer {
  constructor(
    private readonly idleMs: number,
    private readonly notify: ActivityNotify,
  ) {}

  /**
   * Wait until the full idle period elapses without any activity.
   * Returns when compaction should be triggered.
   */
  async waitForIdle(): Promise<void> {
    while (true) {
      let timer: ReturnType<typeof setTimeout> | undefined;
      const slept = new Promise<"idle">((resolve) => {
        timer = setTimeout(() => resolve("idle"), this.idleMs);
      });
      const interrupted = this.notify.notified().then(() => "activity" as const);
      const which = await Promise.race([slept, interrupted]);
      if (timer !== undefined) clearTimeout(timer);
      if (which === "idle") return;
      // activity → reset timer by restarting loop.
    }
  }
}
