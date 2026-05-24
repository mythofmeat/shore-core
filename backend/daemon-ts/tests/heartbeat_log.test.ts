import { describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { HeartbeatLog } from "../src/autonomy/heartbeat_log.ts";

describe("HeartbeatLog", () => {
  test("push marks dirty but does not write until flush", () => {
    const dir = mkdtempSync(join(tmpdir(), "heartbeat-log-test-"));
    const file = join(dir, "heartbeat.jsonl");
    const log = HeartbeatLog.withPath(file);
    log.push("tick_fired", "test");
    expect(log.isDirty()).toBe(true);
    expect(existsSync(file)).toBe(false);
    log.flushIfDirty();
    expect(existsSync(file)).toBe(true);
    expect(log.isDirty()).toBe(false);
  });

  test("load skips malformed lines and keeps recent ordering", () => {
    const dir = mkdtempSync(join(tmpdir(), "heartbeat-log-load-test-"));
    const file = join(dir, "heartbeat.jsonl");
    writeFileSync(
      file,
      [
        JSON.stringify({ timestamp: "2026-05-24T00:00:00Z", kind: "tick_fired", detail: "a" }),
        "{bad",
        JSON.stringify({ timestamp: "2026-05-24T00:00:01Z", kind: "message_sent", detail: "b" }),
        "",
      ].join("\n"),
    );
    const log = HeartbeatLog.loadFrom(file);
    expect(log.recent(10).map((e) => e.detail)).toEqual(["a", "b"]);
  });

  test("flush rewrites as jsonl", () => {
    const dir = mkdtempSync(join(tmpdir(), "heartbeat-log-flush-test-"));
    const file = join(dir, "heartbeat.jsonl");
    const log = HeartbeatLog.withPath(file);
    log.push("dormant_ping", "ping");
    log.flushIfDirty();
    const lines = readFileSync(file, "utf8").trim().split("\n");
    expect(lines).toHaveLength(1);
    expect(JSON.parse(lines[0]!).detail).toBe("ping");
  });
});
