#!/usr/bin/env bun
/**
 * Parity check: spawn the TS daemon, replay the captured client side of the
 * trace, diff the emitted server-to-client frames against the baseline
 * produced by `capture-rust-trace.ts`.
 *
 * Exits non-zero on any structural divergence. Differences in
 * `server_name` are expected (we want "shore-daemon-ts" vs "shore-daemon").
 *
 * Usage:
 *   bun scripts/parity-check.ts <baseline.jsonl> [<daemon-bin>] [--fixture <dir>]
 *
 *   daemon-bin defaults to running `bun src/main.ts`.
 *   --fixture points SHORE_CONFIG_DIR / SHORE_DATA_DIR at <dir>/config and
 *   <dir>/data (matches capture-rust-trace.ts).
 */

import { cpSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve as resolvePath } from "node:path";

/**
 * Expected differences between the Rust daemon and the TS daemon that
 * should NOT count as a failure. Keyed by frame type, mapping to a list of
 * dotted-paths to ignore. As phases progress and we add new fields, this
 * list should shrink toward empty.
 */
const EXPECTED_DIFFS: Record<string, string[]> = {
  hello: ["server_name"],
};

const positional: string[] = [];
let fixtureDir: string | undefined;
for (let i = 2; i < process.argv.length; i++) {
  const a = process.argv[i]!;
  if (a === "--fixture") fixtureDir = resolvePath(process.argv[++i]!);
  else positional.push(a);
}
const baselinePath = positional[0];
const daemonBin = positional[1];
if (!baselinePath) {
  console.error("usage: parity-check.ts <baseline.jsonl> [<daemon-bin>] [--fixture <dir>]");
  process.exit(2);
}
const cmd: string[] = daemonBin ? [daemonBin] : ["bun", "src/main.ts"];

const baseline = readFileSync(baselinePath, "utf8")
  .split("\n")
  .filter((l) => l.trim() !== "")
  .map((l) => JSON.parse(l) as { dir: "s2c" | "c2s"; frame: Record<string, unknown> });

const tmp = mkdtempSync(join(tmpdir(), "shore-daemon-ts-parity-"));
// Copy the fixture into a tmp working directory so the daemon can't
// pollute the committed state (see capture-rust-trace.ts for the same
// rationale).
let configDir: string;
let dataDir: string;
if (fixtureDir) {
  const workDir = mkdtempSync(join(tmpdir(), "shore-daemon-ts-parity-fixture-"));
  cpSync(join(fixtureDir, "config"), join(workDir, "config"), { recursive: true });
  cpSync(join(fixtureDir, "data"), join(workDir, "data"), { recursive: true });
  configDir = join(workDir, "config");
  dataDir = join(workDir, "data");
} else {
  configDir = join(tmp, "config");
  dataDir = join(tmp, "data");
}
const env = {
  ...process.env,
  SHORE_RUNTIME_DIR: join(tmp, "runtime"),
  SHORE_CACHE_DIR: join(tmp, "cache"),
  SHORE_CONFIG_DIR: configDir,
  SHORE_DATA_DIR: dataDir,
};

const proc = Bun.spawn({
  cmd: [...cmd, "--addr", "127.0.0.1:0"],
  env,
  stdout: "pipe",
  stderr: "pipe",
});

let failures = 0;
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

  for (const entry of baseline) {
    if (entry.dir === "c2s") {
      sock.write(JSON.stringify(entry.frame) + "\n");
    } else {
      const actual = (await readFrame(frames)) as Record<string, unknown>;
      const diff = compareFrames(entry.frame, actual);
      if (diff.length === 0) {
        console.log(`ok    s2c ${actual["type"]}`);
      } else {
        failures++;
        console.error(`FAIL  s2c ${actual["type"]}`);
        for (const d of diff) console.error(`        ${d}`);
        console.error(`        baseline: ${JSON.stringify(entry.frame)}`);
        console.error(`        actual:   ${JSON.stringify(actual)}`);
      }
    }
  }

  sock.end();
} finally {
  proc.kill("SIGTERM");
  await proc.exited;
}

if (failures > 0) {
  console.error(`\n${failures} divergence(s)`);
  process.exit(1);
}
console.log("\nparity ok");

// ── compare ────────────────────────────────────────────────────────────

function compareFrames(a: Record<string, unknown>, b: Record<string, unknown>): string[] {
  const type = String(a["type"] ?? "");
  const ignore = new Set(EXPECTED_DIFFS[type] ?? []);
  const diffs: string[] = [];
  walk(a, b, "", diffs, ignore);
  return diffs;
}

function walk(
  a: unknown,
  b: unknown,
  path: string,
  out: string[],
  ignore: Set<string>,
): void {
  if (path && ignore.has(path)) return;
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
    for (let i = 0; i < a.length; i++) walk(a[i], b[i], `${path}[${i}]`, out, ignore);
    return;
  }
  if (typeof a === "object") {
    const ao = a as Record<string, unknown>;
    const bo = b as Record<string, unknown>;
    const keys = new Set([...Object.keys(ao), ...Object.keys(bo)]);
    for (const k of keys) {
      const sub = path ? `${path}.${k}` : k;
      if (ignore.has(sub)) continue;
      if (!(k in ao)) out.push(`${sub}: missing in baseline`);
      else if (!(k in bo)) out.push(`${sub}: missing in actual`);
      else walk(ao[k], bo[k], sub, out, ignore);
    }
    return;
  }
  if (a !== b) out.push(`${path || "<root>"}: ${JSON.stringify(a)} !== ${JSON.stringify(b)}`);
}

// ── plumbing copied from handshake-smoketest.ts ────────────────────────

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
