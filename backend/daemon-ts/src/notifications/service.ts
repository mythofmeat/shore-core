/**
 * notify-send dispatcher.
 *
 * Fire-and-forget: `notify` returns immediately and spawns `notify-send`
 * in the background. Dispatch failures are logged but don't propagate —
 * a missing/broken `notify-send` binary must never break a real user
 * flow.
 *
 * Spawn-based, no shell. Title and body pass through as separate argv
 * entries, so shell metacharacters from LLM-generated content are inert.
 */

import {
  DEFAULT_NOTIFICATIONS_CONFIG,
  type NotificationEvent,
  type NotificationsConfig,
} from "./types.ts";

const NOTIFY_BODY_MAX = 200;

export type NotifySendFn = (title: string, body: string) => Promise<void> | void;

/**
 * Default spawner. Replaceable via the constructor so tests can capture
 * the calls without invoking `notify-send` for real.
 */
const defaultNotifySend: NotifySendFn = async (title, body) => {
  try {
    const proc = Bun.spawn(["notify-send", "--app-name=shore", title, body], {
      stdout: "ignore",
      stderr: "ignore",
    });
    await proc.exited;
  } catch (e) {
    console.warn(`[shore-daemon-ts] notify-send dispatch failed: ${(e as Error).message}`);
  }
};

export class NotificationService {
  private config: NotificationsConfig;
  private readonly send: NotifySendFn;

  constructor(config?: NotificationsConfig, send: NotifySendFn = defaultNotifySend) {
    this.config = config ?? { ...DEFAULT_NOTIFICATIONS_CONFIG };
    this.send = send;
  }

  notify(event: NotificationEvent, title: string, body: string): void {
    if (!this.config.enabled) return;
    if (!this.config.events[event]) return;
    void this.send(title, truncateSummary(body, NOTIFY_BODY_MAX));
  }

  /**
   * Fire a `message_complete` notification only if generation took at
   * least `generation_threshold_ms`. Mirrors Rust's
   * `notify_message_complete` short-circuit.
   */
  notifyMessageComplete(title: string, body: string, totalMs: number): void {
    if (this.config.generation_threshold_ms > 0 && totalMs < this.config.generation_threshold_ms) {
      return;
    }
    this.notify("message_complete", title, body);
  }

  /** Atomic config swap. Used by `config_reset` once hot-reload paths exist. */
  reload(next: NotificationsConfig): void {
    this.config = next;
  }
}

function truncateSummary(s: string, max: number): string {
  if (s.length <= max) return s;
  return `${s.slice(0, max - 1)}…`;
}
