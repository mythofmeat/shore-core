#!/usr/bin/env bun
/**
 * Phase 0/1 exit-criterion smoketest.
 *
 * Spawns the daemon, waits for it to print the listen address, opens a TCP
 * connection, completes the SWP handshake, and exits non-zero if anything
 * deviates from the expected sequence.
 *
 * Usage:
 *   bun scripts/handshake-smoketest.ts                         # runs `bun src/main.ts`
 *   bun scripts/handshake-smoketest.ts dist/shore-daemon       # runs compiled binary
 *   bun scripts/handshake-smoketest.ts dist/shore-daemon -- --config /tmp/shore/config.toml
 */

import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const rawArgs = process.argv.slice(2);
const sep = rawArgs.indexOf("--");
const positional = sep >= 0
  ? rawArgs.slice(0, sep)
  : rawArgs[0]?.startsWith("--")
    ? []
    : rawArgs.slice(0, 1);
const daemonArgs = sep >= 0
  ? rawArgs.slice(sep + 1)
  : rawArgs[0]?.startsWith("--")
    ? rawArgs
    : rawArgs.slice(1);
if (positional.length > 1) {
  console.error("usage: handshake-smoketest.ts [daemon-bin] [-- <daemon-args>...]");
  process.exit(2);
}
const cmdArg = positional[0];
const cmd: string[] = cmdArg ? [cmdArg] : ["bun", "src/main.ts"];

const tmp = mkdtempSync(join(tmpdir(), "shore-daemon-ts-smoketest-"));
const env = {
  ...process.env,
  SHORE_RUNTIME_DIR: join(tmp, "runtime"),
  SHORE_DATA_DIR: join(tmp, "data"),
  SHORE_CONFIG_DIR: join(tmp, "config"),
  SHORE_CACHE_DIR: join(tmp, "cache"),
};

const proc = Bun.spawn({
  cmd: [...cmd, ...daemonArgs, "--addr", "127.0.0.1:0"],
  env,
  stdout: "pipe",
  stderr: "inherit",
});

try {
  const addr = await readListenAddr(proc.stdout);
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
  if (hello.type !== "hello") fail(`expected hello, got ${JSON.stringify(hello)}`);
  if (hello.v !== 1) fail(`expected v=1, got ${hello.v}`);
  if (typeof hello.server_name !== "string") fail("server_name missing");

  // 2) ClientHello → server
  sock.write(JSON.stringify({
    type: "hello",
    client_type: "cli",
    client_name: "handshake-smoketest",
  }) + "\n");

  // 3) History
  const history = await readFrame(frames);
  if (history.type !== "history") fail(`expected history, got ${JSON.stringify(history)}`);
  if (!Array.isArray(history.messages)) fail("history.messages not array");

  sock.end();
  console.log(`ok — handshake completed against ${cmd.join(" ")}; server_name=${hello.server_name}`);
} finally {
  proc.kill("SIGTERM");
  await proc.exited;
}

// ── helpers ────────────────────────────────────────────────────────────

interface Frame { type: string; [k: string]: unknown }

async function readListenAddr(
  stream: ReadableStream<Uint8Array>,
): Promise<{ host: string; port: number } | undefined> {
  const decoder = new TextDecoder();
  let acc = "";
  const reader = stream.getReader();
  const deadline = Date.now() + 10_000;
  try {
    while (Date.now() < deadline) {
      const { value, done } = await reader.read();
      if (done) return undefined;
      acc += decoder.decode(value, { stream: true });
      const m = acc.match(/listening on (\d+\.\d+\.\d+\.\d+):(\d+)/);
      if (m) return { host: m[1]!, port: Number(m[2]) };
    }
  } finally {
    reader.releaseLock();
  }
  return undefined;
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

async function readFrame(q: FrameQueue, timeoutMs = 5000): Promise<Frame> {
  const line = await Promise.race([
    q.read(),
    new Promise<string>((_, rej) => setTimeout(() => rej(new Error("read timeout")), timeoutMs)),
  ]);
  return JSON.parse(line) as Frame;
}

function fail(msg: string): never {
  console.error(`FAIL: ${msg}`);
  process.exit(1);
}
