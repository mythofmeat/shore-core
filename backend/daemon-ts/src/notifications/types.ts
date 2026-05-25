/**
 * Notification config + event types.
 *
 * Port of `backend/daemon/src/notifications.rs` + the `[notifications]`
 * section of `core/config/src/app.rs`. Reduced surface vs. Rust:
 *
 *   - Only the `notify-send` backend is supported. The Rust daemon's
 *     `[notifications.ntfy]` and `[notifications.command]` tables are
 *     intentionally dropped — users who want ntfy push can script around
 *     the Rust CLI's `shore notify` listener (see REWRITE.md #7).
 *   - `cache_warning` is omitted: the Rust enum defined it but no call
 *     site ever fired it. Add it back the day a fire-site exists.
 */

export type NotificationEvent =
  | "autonomous_message"
  | "compaction_complete"
  | "error"
  | "message_complete"
  | "usage_warning";

export interface NotificationEventsConfig {
  autonomous_message: boolean;
  compaction_complete: boolean;
  error: boolean;
  message_complete: boolean;
  usage_warning: boolean;
}

export interface NotificationsConfig {
  enabled: boolean;
  /**
   * Only fire `message_complete` notifications when generation took
   * longer than this threshold. 0 means always notify. Matches Rust's
   * `generation_threshold`.
   */
  generation_threshold_ms: number;
  events: NotificationEventsConfig;
}

/** Matches `NotificationEventsConfig::default()` in Rust. */
export const DEFAULT_NOTIFICATION_EVENTS: NotificationEventsConfig = {
  autonomous_message: true,
  compaction_complete: true,
  error: true,
  message_complete: false,
  usage_warning: true,
};

/** Matches `NotificationsConfig::default()` in Rust. */
export const DEFAULT_NOTIFICATIONS_CONFIG: NotificationsConfig = {
  enabled: false,
  generation_threshold_ms: 0,
  events: { ...DEFAULT_NOTIFICATION_EVENTS },
};
