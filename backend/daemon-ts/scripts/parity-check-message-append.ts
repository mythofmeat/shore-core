#!/usr/bin/env bun
/**
 * Phase 3 parity check: message-append + restart-persistence.
 *
 * Replays the captured client side of `parity-traces/message-append.jsonl`
 * against the TS daemon and diffs the recorded server frames. The trace
 * is split into two phases (live, restart): we tear the daemon down and
 * restart it between phases, against the same mutated work dir. This
 * exercises Phase 3's exit criterion — "send a user message, restart the
 * daemon, see the message in the next handshake's History".
 *
 * Non-deterministic fields in the restart-phase History (`msg_id` and
 * `timestamp` of the appended message) are matched fuzzily: same type,
 * same field path, value not compared.
 *
 * Usage:
 *   bun scripts/parity-check-message-append.ts [--fixture <dir>] [--text <s>] [<daemon>]
 */

import { cpSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve as resolvePath } from "node:path";

const args = process.argv.slice(2);
let fixtureDir = "parity-traces/fixtures/message-append";
let baselinePath = "parity-traces/message-append.jsonl";
let text = "hello daemon";
let daemonBin: string | undefined;
for (let i = 0; i < args.length; i++) {
  const a = args[i];
  if (a === "--fixture") fixtureDir = args[++i]!;
  else if (a === "--baseline") baselinePath = args[++i]!;
  else if (a === "--text") text = args[++i]!;
  else daemonBin = a;
}
const cmd: string[] = daemonBin ? [daemonBin] : ["bun", "src/main.ts"];

interface TraceEntry {
  dir: "s2c" | "c2s";
  phase: "live" | "restart";
  frame: Record<string, unknown>;
}

const baseline: TraceEntry[] = readFileSync(resolvePath(baselinePath), "utf8")
  .split("\n")
  .filter((l) => l.trim() !== "")
  .map((l) => JSON.parse(l) as TraceEntry);

// Copy fixture into one work dir that survives across both phases.
const workDir = mkdtempSync(join(tmpdir(), "shore-msg-append-parity-"));
cpSync(join(resolvePath(fixtureDir), "config"), join(workDir, "config"), { recursive: true });
cpSync(join(resolvePath(fixtureDir), "data"), join(workDir, "data"), { recursive: true });

const env = {
  ...process.env,
  SHORE_RUNTIME_DIR: mkdtempSync(join(tmpdir(), "shore-msg-runtime-")),
  SHORE_CACHE_DIR: mkdtempSync(join(tmpdir(), "shore-msg-cache-")),
  SHORE_CONFIG_DIR: join(workDir, "config"),
  SHORE_DATA_DIR: join(workDir, "data"),
};

// Per-frame-type ignore lists. Paths use dotted notation with [n] for
// array indices. Values at these paths must be the same type but the
// content is not compared.
const FUZZY_DIFFS: Record<string, string[]> = {
  hello: ["server_name"],
  history: ["messages[*].msg_id", "messages[*].timestamp"],
};

const phases: Array<"live" | "restart"> = ["live", "restart"];
let failures = 0;

for (const phase of phases) {
  console.log(`── phase: ${phase} ──`);
  const phaseEntries = baseline.filter((e) => e.phase === phase);

  const proc = Bun.spawn({
    cmd: [...cmd, "--addr", "127.0.0.1:0"],
    env,
    stdout: "pipe",
    stderr: "pipe",
  });

  try {
    const addr = await readListenAddr([proc.stdout, proc.stderr]);
    if (!addr) fail("daemon never printed listen address");

    const frames = new FrameQueue();
    const sock = await Bun.connect({
      hostname: addr.host,
      port: addr.port,
      socket: {
        data: (_s, chunk) => frames.push(chunk),
        open: () => {},
        close: () => frames.eof(),
        error: (_s, e) => frames.error(e),
      },
    });

    for (const entry of phaseEntries) {
      if (entry.dir === "c2s") {
        sock.write(JSON.stringify(entry.frame) + "\n");
        // The post-message send is async on our side too; give it a
        // moment to flush before the next phase tears the daemon down.
        if (entry.frame["type"] === "message") {
          await new Promise((r) => setTimeout(r, 500));
        }
      } else {
        const actual = (await readFrame(frames)) as Record<string, unknown>;
        const diff = compareFrames(entry.frame, actual);
        if (diff.length === 0) {
          console.log(`  ok    ${entry.dir} ${actual["type"]}`);
        } else {
          failures++;
          console.error(`  FAIL  ${entry.dir} ${actual["type"]}`);
          for (const d of diff) console.error(`          ${d}`);
          console.error(`          baseline: ${JSON.stringify(entry.frame)}`);
          console.error(`          actual:   ${JSON.stringify(actual)}`);
        }
      }
    }

    sock.end();
  } finally {
    proc.kill("SIGTERM");
    await proc.exited;
  }
}

if (failures > 0) {
  console.error(`\n${failures} divergence(s)`);
  process.exit(1);
}
console.log("\nparity ok");

// ── compare ────────────────────────────────────────────────────────────

function compareFrames(a: Record<string, unknown>, b: Record<string, unknown>): string[] {
  const type = String(a["type"] ?? "");
  const fuzzy = (FUZZY_DIFFS[type] ?? []).map(pathToMatcher);
  const out: string[] = [];
  walk(a, b, "", out, fuzzy);
  return out;
}

type PathMatcher = (path: string) => boolean;

function pathToMatcher(pattern: string): PathMatcher {
  // Translate dotted+`[*]` patterns into anchored regexes. Use a sentinel
  // for `[*]` before regex-escaping so the brackets and `*` don't get
  // mangled, then swap the sentinel for the digit-index pattern.
  const SENTINEL = "\x00IDX\x00";
  const withSentinel = pattern.replaceAll("[*]", SENTINEL);
  const escaped = withSentinel.replace(/[.+?^${}()|[\]\\*]/g, "\\$&");
  const rxSrc = escaped.replaceAll(SENTINEL, "\\[\\d+\\]");
  const rx = new RegExp(`^${rxSrc}$`);
  return (p) => rx.test(p);
}

function walk(
  a: unknown,
  b: unknown,
  path: string,
  out: string[],
  fuzzy: PathMatcher[],
): void {
  if (path && fuzzy.some((m) => m(path))) {
    // Fuzzy match: just require the types align (both undefined or both same typeof).
    if (typeof a !== typeof b) {
      out.push(`${path}: fuzzy type mismatch (${typeof a} vs ${typeof b})`);
    }
    return;
  }
  if (a === b) return;
  if (a === null || b === null || typeof a !== typeof b) {
    out.push(`${path || "<root>"}: ${JSON.stringify(a)} !== ${JSON.stringify(b)}`);
    return;
  }
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) {
      out.push(`${path || "<root>"}: array shape differs`);
      return;
    }
    for (let i = 0; i < a.length; i++) walk(a[i], b[i], `${path}[${i}]`, out, fuzzy);
    return;
  }
  if (typeof a === "object") {
    const ao = a as Record<string, unknown>;
    const bo = b as Record<string, unknown>;
    const keys = new Set([...Object.keys(ao), ...Object.keys(bo)]);
    for (const k of keys) {
      const sub = path ? `${path}.${k}` : k;
      if (fuzzy.some((m) => m(sub))) {
        if (typeof ao[k] !== typeof bo[k]) {
          out.push(`${sub}: fuzzy type mismatch (${typeof ao[k]} vs ${typeof bo[k]})`);
        }
        continue;
      }
      if (!(k in ao)) out.push(`${sub}: missing in baseline`);
      else if (!(k in bo)) out.push(`${sub}: missing in actual`);
      else walk(ao[k], bo[k], sub, out, fuzzy);
    }
    return;
  }
  if (a !== b) out.push(`${path || "<root>"}: ${JSON.stringify(a)} !== ${JSON.stringify(b)}`);
}

// ── plumbing ─────────────────────────────────────────────────────────

async function readListenAddr(
  streams: ReadableStream<Uint8Array>[],
): Promise<{ host: string; port: number } | undefined> {
  return new Promise((resolve) => {
    const deadline = setTimeout(() => resolve(undefined), 10_000);
    let resolved = false;
    const finish = (v: { host: string; port: number } | undefined) => {
      if (resolved) return;
      resolved = true;
      clearTimeout(deadline);
      resolve(v);
    };
    for (const stream of streams) {
      void (async () => {
        const decoder = new TextDecoder();
        let acc = "";
        const reader = stream.getReader();
        try {
          while (!resolved) {
            const { value, done } = await reader.read();
            if (done) return;
            acc += decoder.decode(value, { stream: true });
            for (const m of acc.matchAll(/(\d+\.\d+\.\d+\.\d+):(\d+)/g)) {
              const port = Number(m[2]);
              if (port > 0) {
                finish({ host: m[1]!, port });
                return;
              }
            }
          }
        } finally {
          reader.releaseLock();
        }
      })();
    }
  });
}

class FrameQueue {
  private buf = Buffer.alloc(0);
  private waiters: Array<{ resolve: (l: string) => void; reject: (e: Error) => void }> = [];
  private closed = false;
  private err: Error | undefined;
  push(chunk: Buffer | Uint8Array): void {
    const b = Buffer.from(chunk);
    this.buf = this.buf.length === 0 ? b : Buffer.concat([this.buf, b]);
    this.drain();
  }
  eof(): void {
    this.closed = true;
    this.drain();
  }
  error(e: Error): void {
    this.err = e;
    this.drain();
  }
  read(): Promise<string> {
    return new Promise((resolve, reject) => {
      this.waiters.push({ resolve, reject });
      this.drain();
    });
  }
  private drain(): void {
    while (this.waiters.length > 0) {
      const w = this.waiters[0]!;
      if (this.err) {
        this.waiters.shift();
        w.reject(this.err);
        continue;
      }
      const nl = this.buf.indexOf(0x0a);
      if (nl < 0) {
        if (this.closed) {
          this.waiters.shift();
          w.reject(new Error("connection closed"));
          continue;
        }
        return;
      }
      const line = this.buf.subarray(0, nl).toString("utf8");
      this.buf = this.buf.subarray(nl + 1);
      this.waiters.shift();
      w.resolve(line);
    }
  }
}

async function readFrame(q: FrameQueue, timeoutMs = 5000): Promise<unknown> {
  const line = await Promise.race([
    q.read(),
    new Promise<string>((_, rej) => setTimeout(() => rej(new Error("read timeout")), timeoutMs)),
  ]);
  return JSON.parse(line);
}

function fail(msg: string): never {
  console.error(`FAIL: ${msg}`);
  process.exit(1);
}
