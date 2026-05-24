import fs from "node:fs";
import path from "node:path";

export type HeartbeatEventKind =
  | "tick_fired"
  | "message_sent"
  | "message_skipped"
  | "tool_use"
  | "dormant"
  | "wake"
  | "timeout"
  | "dormant_ping"
  | "recap_written"
  | "recap_missing";

export interface HeartbeatEvent {
  timestamp: string;
  kind: HeartbeatEventKind;
  detail: string;
}

const HEARTBEAT_LOG_CAPACITY = 100;

export class HeartbeatLog {
  private readonly events: HeartbeatEvent[];
  private dirty = false;

  private constructor(
    private readonly filePath: string | undefined,
    events: HeartbeatEvent[],
  ) {
    this.events = events.slice(-HEARTBEAT_LOG_CAPACITY);
  }

  static inMemory(): HeartbeatLog {
    return new HeartbeatLog(undefined, []);
  }

  static withPath(filePath: string): HeartbeatLog {
    return new HeartbeatLog(filePath, []);
  }

  static loadFrom(filePath: string): HeartbeatLog {
    let raw: string;
    try {
      raw = fs.readFileSync(filePath, "utf8");
    } catch (e) {
      if ((e as NodeJS.ErrnoException).code === "ENOENT") {
        return HeartbeatLog.withPath(filePath);
      }
      throw e;
    }
    const events: HeartbeatEvent[] = [];
    for (const line of raw.split(/\r?\n/)) {
      const trimmed = line.trim();
      if (trimmed.length === 0) continue;
      try {
        const parsed = JSON.parse(trimmed) as unknown;
        if (isHeartbeatEvent(parsed)) events.push(parsed);
      } catch {
        // Match Rust's log-and-skip behavior; tests only care that malformed
        // lines don't poison the whole log.
      }
    }
    return new HeartbeatLog(filePath, events);
  }

  push(kind: HeartbeatEventKind, detail: string): void {
    if (this.events.length >= HEARTBEAT_LOG_CAPACITY) {
      this.events.shift();
    }
    this.events.push({
      timestamp: rfc3339LocalNow(),
      kind,
      detail,
    });
    this.dirty = true;
  }

  recent(limit: number): HeartbeatEvent[] {
    return this.events.slice(Math.max(0, this.events.length - limit));
  }

  isDirty(): boolean {
    return this.dirty;
  }

  flushIfDirty(): void {
    if (!this.dirty) return;
    if (this.filePath === undefined) {
      this.dirty = false;
      return;
    }
    fs.mkdirSync(path.dirname(this.filePath), { recursive: true });
    const tmp = `${this.filePath}.tmp`;
    const body = this.events.map((e) => JSON.stringify(e)).join("\n");
    fs.writeFileSync(tmp, body.length === 0 ? "" : `${body}\n`);
    fs.renameSync(tmp, this.filePath);
    this.dirty = false;
  }
}

function isHeartbeatEvent(v: unknown): v is HeartbeatEvent {
  if (typeof v !== "object" || v === null || Array.isArray(v)) return false;
  const obj = v as Record<string, unknown>;
  return (
    typeof obj["timestamp"] === "string"
    && typeof obj["kind"] === "string"
    && typeof obj["detail"] === "string"
  );
}

function rfc3339LocalNow(): string {
  const now = new Date();
  const tzOffsetMinutes = -now.getTimezoneOffset();
  const sign = tzOffsetMinutes >= 0 ? "+" : "-";
  const abs = Math.abs(tzOffsetMinutes);
  const tzh = String(Math.floor(abs / 60)).padStart(2, "0");
  const tzm = String(abs % 60).padStart(2, "0");
  const pad = (n: number, w = 2): string => String(n).padStart(w, "0");
  const ms = String(now.getMilliseconds()).padStart(3, "0");
  return (
    `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}` +
    `T${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}.${ms}${sign}${tzh}:${tzm}`
  );
}
