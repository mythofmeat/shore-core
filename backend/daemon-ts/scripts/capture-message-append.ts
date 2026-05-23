#!/usr/bin/env bun
/**
 * Capture the message-append scenario from the Rust daemon for Phase 3.
 *
 * Flow:
 *   1. Start daemon against a copy of <fixture>.
 *   2. Connect, complete handshake (selecting <character>).
 *   3. Send a ClientMessage{text: "<text>"}.
 *   4. Record one server frame — expected to be NewMessage(user_input).
 *      (The Rust daemon then tries to call an LLM; we don't care, we
 *      disconnect.)
 *   5. Kill daemon. Restart it against the SAME mutated work dir.
 *   6. Re-handshake. Record the History snapshot — it should now contain
 *      the appended user message.
 *
 * Output: a JSONL file with `dir: "s2c" | "c2s"` and `phase: "live" |
 * "restart"` so the parity-check can attribute frames to the right
 * scenario.
 *
 * Usage:
 *   bun scripts/capture-message-append.ts <rust-daemon> <out-file> \
 *     --fixture <dir> --character <name> --text "<message>"
 */

import { cpSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve as resolvePath } from "node:path";

const args = process.argv.slice(2);
const daemonPath = args[0];
const outPath = args[1];
let fixtureDir: string | undefined;
let character: string | undefined;
let text: string | undefined;
for (let i = 2; i < args.length; i++) {
  const a = args[i];
  if (a === "--fixture") fixtureDir = resolvePath(args[++i]!);
  else if (a === "--character") character = args[++i];
  else if (a === "--text") text = args[++i];
  else {
    console.error(`unknown arg: ${a}`);
    process.exit(2);
  }
}
if (!daemonPath || !outPath || !fixtureDir || !character || text === undefined) {
  console.error(
    "usage: capture-message-append.ts <rust-daemon> <out> --fixture <dir> --character <name> --text <text>",
  );
  process.exit(2);
}

const workDir = mkdtempSync(join(tmpdir(), "shore-msg-append-"));
cpSync(join(fixtureDir, "config"), join(workDir, "config"), { recursive: true });
cpSync(join(fixtureDir, "data"), join(workDir, "data"), { recursive: true });

const env = {
  ...process.env,
  SHORE_RUNTIME_DIR: mkdtempSync(join(tmpdir(), "shore-msg-runtime-")),
  SHORE_CACHE_DIR: mkdtempSync(join(tmpdir(), "shore-msg-cache-")),
  SHORE_CONFIG_DIR: join(workDir, "config"),
  SHORE_DATA_DIR: join(workDir, "data"),
};

const trace: Array<{ dir: "s2c" | "c2s"; phase: "live" | "restart"; frame: unknown }> = [];

// ── live phase ──────────────────────────────────────────────────────────

{
  const proc = spawnDaemon();
  try {
    const addr = await readListenAddr([proc.stdout, proc.stderr]);
    if (!addr) fail("daemon never printed listen address (live)");

    const { sock, frames } = await openConnection(addr);

    // Handshake
    const hello = await readFrame(frames);
    trace.push({ dir: "s2c", phase: "live", frame: hello });

    const clientHello = {
      type: "hello",
      client_type: "cli",
      client_name: "msg-append-capture",
      character,
    };
    sock.write(JSON.stringify(clientHello) + "\n");
    trace.push({ dir: "c2s", phase: "live", frame: clientHello });

    const history = await readFrame(frames);
    trace.push({ dir: "s2c", phase: "live", frame: history });

    // Send the user message. The daemon will emit several frames in
    // response (a History broadcast from engine.append_message, a
    // NewMessage event_tx broadcast, and eventually an LLM-attempt error
    // since no model is configured). The wire ordering of these is racy
    // — broadcasts and direct responses go through different concurrent
    // tasks — so we don't try to capture them deterministically. The
    // persistence assertion (restart phase, below) is the actual signal.
    const clientMsg = { type: "message", text };
    sock.write(JSON.stringify(clientMsg) + "\n");
    trace.push({ dir: "c2s", phase: "live", frame: clientMsg });

    // Wait briefly for active.jsonl to be flushed to disk.
    await new Promise((r) => setTimeout(r, 800));

    sock.end();
  } finally {
    proc.kill("SIGTERM");
    await proc.exited;
  }
}

// ── restart phase ───────────────────────────────────────────────────────

{
  const proc = spawnDaemon();
  try {
    const addr = await readListenAddr([proc.stdout, proc.stderr]);
    if (!addr) fail("daemon never printed listen address (restart)");

    const { sock, frames } = await openConnection(addr);

    const hello = await readFrame(frames);
    trace.push({ dir: "s2c", phase: "restart", frame: hello });

    const clientHello = {
      type: "hello",
      client_type: "cli",
      client_name: "msg-append-capture-restart",
      character,
    };
    sock.write(JSON.stringify(clientHello) + "\n");
    trace.push({ dir: "c2s", phase: "restart", frame: clientHello });

    const history = await readFrame(frames);
    trace.push({ dir: "s2c", phase: "restart", frame: history });

    sock.end();
  } finally {
    proc.kill("SIGTERM");
    await proc.exited;
  }
}

await Bun.write(outPath, trace.map((e) => JSON.stringify(e)).join("\n") + "\n");

console.log(`wrote ${trace.length} frames → ${outPath}`);
for (const entry of trace) {
  const fr = entry.frame as { type?: string };
  console.log(`  ${entry.phase.padEnd(7)} ${entry.dir}  ${fr.type}`);
}

// ── helpers ────────────────────────────────────────────────────────────

function spawnDaemon() {
  return Bun.spawn({
    cmd: [daemonPath!, "--addr", "127.0.0.1:0"],
    env,
    stdout: "pipe",
    stderr: "pipe",
  });
}

async function openConnection(addr: { host: string; port: number }): Promise<{ sock: any; frames: FrameQueue }> {
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
  return { sock, frames };
}

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
