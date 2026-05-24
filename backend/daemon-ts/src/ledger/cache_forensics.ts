/**
 * Prompt-cache forensic JSONL log.
 *
 * Mirrors the Rust `shore_llm::cache_forensics` response-side behavior:
 * best-effort append-only writes to `{cache_dir}/cache_forensics.jsonl`.
 * Request-side breakpoint logging can be added once the TS request mirror
 * grows a stable placement hook.
 */

import fs from "node:fs";
import path from "node:path";

export interface ResponseLog {
  callId: number;
  model: string;
  character: string;
  callType: string;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
}

export interface RequestLog {
  callId: number;
  character?: string;
  model: string;
  msgCount: number;
  msgBreakpoints: number[];
  sysBreakpoints: number[];
  sysBlocks: number;
  prefixHash: string;
  hasExistingMarkers: boolean;
  cacheEnabled: boolean;
  rid?: string;
}

export interface ErrorLog {
  callId: number;
  model: string;
  character: string;
  callType: string;
  error: string;
}

export class CacheForensics {
  private nextId = 0;
  private readonly filePath: string;

  private constructor(cacheDir: string) {
    fs.mkdirSync(cacheDir, { recursive: true });
    this.filePath = path.join(cacheDir, "cache_forensics.jsonl");
  }

  static open(cacheDir: string): CacheForensics {
    return new CacheForensics(cacheDir);
  }

  nextCallId(): number {
    const id = this.nextId;
    this.nextId += 1;
    return id;
  }

  logRequest(entry: RequestLog): void {
    this.writeEntry({
      ts: rfc3339LocalNow(),
      type: "request",
      call_id: entry.callId,
      character: entry.character ?? null,
      model: entry.model,
      msg_count: entry.msgCount,
      msg_breakpoints: entry.msgBreakpoints,
      sys_breakpoints: entry.sysBreakpoints,
      sys_blocks: entry.sysBlocks,
      prefix_hash: entry.prefixHash,
      has_existing_markers: entry.hasExistingMarkers,
      cache_enabled: entry.cacheEnabled,
      rid: entry.rid ?? null,
    });
  }

  logResponse(entry: ResponseLog): void {
    this.writeEntry({
      ts: rfc3339LocalNow(),
      type: "response",
      call_id: entry.callId,
      model: entry.model,
      character: entry.character,
      call_type: entry.callType,
      input_tokens: entry.inputTokens,
      output_tokens: entry.outputTokens,
      cache_read_tokens: entry.cacheReadTokens,
      cache_creation_tokens: entry.cacheCreationTokens,
    });
  }

  logError(entry: ErrorLog): void {
    this.writeEntry({
      ts: rfc3339LocalNow(),
      type: "error",
      call_id: entry.callId,
      model: entry.model,
      character: entry.character,
      call_type: entry.callType,
      error: entry.error,
    });
  }

  private writeEntry(entry: Record<string, unknown>): void {
    try {
      fs.appendFileSync(this.filePath, `${JSON.stringify(entry)}\n`, "utf8");
    } catch {
      // Diagnostic-only. Never fail the main generation path on I/O.
    }
  }
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
