#!/usr/bin/env bun
/**
 * Capture a handshake trace from the Rust shore-daemon for parity baseline.
 *
 * Usage:
 *   bun scripts/capture-rust-trace.ts <rust-daemon-path> <out-file>
 *
 * Spawns the Rust daemon against an empty SHORE_DATA_DIR/SHORE_CONFIG_DIR,
 * connects, performs a minimal SWP handshake, and writes both directions of
 * the exchange to <out-file> as JSONL with a `dir` field ("s2c" or "c2s").
 *
 * These traces are the source of truth for "did the TS daemon emit the same
 * bytes" parity checks in later phases. The empty-character / empty-history
 * case is Phase 0's exit criterion baseline.
 */

import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const daemonPath = process.argv[2];
const outPath = process.argv[3];
if (!daemonPath || !outPath) {
  console.error("usage: capture-rust-trace.ts <rust-daemon-path> <out-file>");
  process.exit(2);
}

const tmp = mkdtempSync(join(tmpdir(), "shore-rust-trace-"));
const env = {
  ...process.env,
  SHORE_RUNTIME_DIR: join(tmp, "runtime"),
  SHORE_DATA_DIR: join(tmp, "data"),
  SHORE_CONFIG_DIR: join(tmp, "config"),
  SHORE_CACHE_DIR: join(tmp, "cache"),
};

const proc = Bun.spawn({
  cmd: [daemonPath, "--addr", "127.0.0.1:0"],
  env,
  stdout: "pipe",
  // Rust shore-daemon emits its startup logs (including the resolved
  // listen addr) to stderr. Merge it into stdout so we can scan one stream.
  stderr: "pipe",
});

const trace: Array<{ dir: "s2c" | "c2s"; frame: unknown }> = [];

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

  // 1) ServerHello
  const hello = await readFrame(frames);
  trace.push({ dir: "s2c", frame: hello });

  // 2) ClientHello
  const clientHello = {
    type: "hello",
    client_type: "cli",
    client_name: "rust-trace-capture",
  };
  sock.write(JSON.stringify(clientHello) + "\n");
  trace.push({ dir: "c2s", frame: clientHello });

  // 3) History
  const history = await readFrame(frames);
  trace.push({ dir: "s2c", frame: history });

  sock.end();

  const file = Bun.file(outPath).writer();
  for (const entry of trace) {
    file.write(JSON.stringify(entry) + "\n");
  }
  await file.end();

  console.log(`wrote ${trace.length} frames → ${outPath}`);
  for (const entry of trace) {
    const fr = entry.frame as { type?: string };
    console.log(`  ${entry.dir}  ${fr.type}`);
  }
} finally {
  proc.kill("SIGTERM");
  await proc.exited;
}

// ── helpers ────────────────────────────────────────────────────────────

async function readListenAddr(
  streams: ReadableStream<Uint8Array>[],
): Promise<{ host: string; port: number } | undefined> {
  // Race all streams; first to yield a host:port with a non-zero port wins.
  // The Rust daemon logs `bind_addr=127.0.0.1:0` (the requested addr) before
  // it logs the resolved `addr=127.0.0.1:<port>`, so skip port-0 matches.
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
