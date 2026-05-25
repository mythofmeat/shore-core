import { describe, expect, it } from "bun:test";

import { NotificationService } from "../src/notifications/service.ts";
import {
  DEFAULT_NOTIFICATIONS_CONFIG,
  type NotificationEvent,
  type NotificationsConfig,
} from "../src/notifications/types.ts";

type Capture = Array<{ title: string; body: string }>;

function makeService(
  patch: Partial<NotificationsConfig> = {},
): { svc: NotificationService; calls: Capture } {
  const calls: Capture = [];
  const config: NotificationsConfig = {
    ...DEFAULT_NOTIFICATIONS_CONFIG,
    ...patch,
    events: { ...DEFAULT_NOTIFICATIONS_CONFIG.events, ...(patch.events ?? {}) },
  };
  const svc = new NotificationService(config, (title, body) => {
    calls.push({ title, body });
  });
  return { svc, calls };
}

const ALL_EVENTS: NotificationEvent[] = [
  "autonomous_message",
  "compaction_complete",
  "error",
  "message_complete",
  "usage_warning",
];

describe("notifications/service", () => {
  it("does not dispatch when the master switch is off", () => {
    const { svc, calls } = makeService({ enabled: false });
    for (const event of ALL_EVENTS) svc.notify(event, "t", "b");
    expect(calls).toEqual([]);
  });

  it("respects per-event toggles when enabled", () => {
    const { svc, calls } = makeService({
      enabled: true,
      events: {
        autonomous_message: true,
        compaction_complete: false,
        error: true,
        message_complete: false,
        usage_warning: false,
      },
    });
    for (const event of ALL_EVENTS) svc.notify(event, "t", `body-${event}`);
    expect(calls.map((c) => c.body)).toEqual([
      "body-autonomous_message",
      "body-error",
    ]);
  });

  it("truncates long bodies to 200 chars with ellipsis", () => {
    const { svc, calls } = makeService({
      enabled: true,
      events: { ...DEFAULT_NOTIFICATIONS_CONFIG.events, error: true },
    });
    const longBody = "x".repeat(500);
    svc.notify("error", "title", longBody);
    expect(calls[0].body.length).toBe(200);
    expect(calls[0].body.endsWith("…")).toBe(true);
  });

  it("notifyMessageComplete respects the threshold", () => {
    const { svc, calls } = makeService({
      enabled: true,
      generation_threshold_ms: 5000,
      events: { ...DEFAULT_NOTIFICATIONS_CONFIG.events, message_complete: true },
    });
    svc.notifyMessageComplete("t", "b", 1000);
    expect(calls).toHaveLength(0);
    svc.notifyMessageComplete("t", "b", 6000);
    expect(calls).toHaveLength(1);
  });

  it("notifyMessageComplete with threshold=0 always fires (when toggle on)", () => {
    const { svc, calls } = makeService({
      enabled: true,
      generation_threshold_ms: 0,
      events: { ...DEFAULT_NOTIFICATIONS_CONFIG.events, message_complete: true },
    });
    svc.notifyMessageComplete("t", "b", 0);
    expect(calls).toHaveLength(1);
  });

  it("notifyMessageComplete is still gated by the per-event toggle", () => {
    const { svc, calls } = makeService({
      enabled: true,
      events: { ...DEFAULT_NOTIFICATIONS_CONFIG.events, message_complete: false },
    });
    svc.notifyMessageComplete("t", "b", 99999);
    expect(calls).toEqual([]);
  });

  it("reload swaps config atomically", () => {
    const calls: Capture = [];
    const svc = new NotificationService(
      { ...DEFAULT_NOTIFICATIONS_CONFIG, enabled: false },
      (title, body) => {
        calls.push({ title, body });
      },
    );
    svc.notify("error", "t", "b");
    expect(calls).toEqual([]);
    svc.reload({
      ...DEFAULT_NOTIFICATIONS_CONFIG,
      enabled: true,
      events: { ...DEFAULT_NOTIFICATIONS_CONFIG.events, error: true },
    });
    svc.notify("error", "t", "b");
    expect(calls).toHaveLength(1);
  });

  it("defaults: master switch off, all events disabled in practice", () => {
    const calls: Capture = [];
    const svc = new NotificationService(undefined, (title, body) => {
      calls.push({ title, body });
    });
    for (const event of ALL_EVENTS) svc.notify(event, "t", "b");
    expect(calls).toEqual([]);
  });
});
