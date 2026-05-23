#!/usr/bin/env bun
/**
 * End-to-end smoketest covering the four polish items beyond basic
 * generation: image messages, regen, cancel, and command.
 *
 *   1. plain generation, observe stream + history
 *   2. regen — observe history shrinks then re-grows
 *   3. cancel — abort mid-flight, observe stream_end finish=cancelled
 *   4. command (inject_system_message) — observe command_output + history
 *
 * Image messages aren't exercised here (would need a real image-capable
 * model + a real image); the wire-shape test in `tests/images.test.ts`
 * covers the adapter encoding.
 *
 * Requires OPENROUTER_API_KEY.
 *
 * Usage:
 *   set -a; source ~/.config/shore/.env; set +a
 *   bun scripts/full-features-smoketest.ts
 */

import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

if (!process.env["OPENROUTER_API_KEY"]) {
  console.error("OPENROUTER_API_KEY required in env");
  process.exit(2);
}

// ── sandbox setup ───────────────────────────────────────────────────────
const tmp = mkdtempSync(join(tmpdir(), "shore-daemon-ts-full-smoke-"));
const configDir = join(tmp, "config");
const dataDir = join(tmp, "data");
const runtimeDir = join(tmp, "runtime");
const cacheDir = join(tmp, "cache");
for (const d of [configDir, dataDir, runtimeDir, cacheDir]) mkdirSync(d, { recursive: true });
mkdirSync(join(configDir, "characters", "smoketest", "workspace"), { recursive: true });
writeFileSync(
  join(configDir, "characters", "smoketest", "workspace", "SOUL.md"),
  "You are a slow, deliberate smoketest assistant. Always count to ten in words before answering. Reply briefly after counting.",
);
writeFileSync(
  join(configDir, "config.toml"),
  `
[defaults]
display_name = "smoketester"
model = "chat.openrouter.haiku45"

[chat.openrouter.haiku45]
model_id = "anthropic/claude-haiku-4.5"
max_tokens = 512
`,
);
writeFileSync(
  join(configDir, ".env"),
  `OPENROUTER_API_KEY=${process.env["OPENROUTER_API_KEY"]}\n`,
);

const proc = Bun.spawn({
  cmd: ["bun", "src/main.ts", "--addr", "127.0.0.1:0"],
  env: {
    ...process.env,
    SHORE_CONFIG_DIR: configDir,
    SHORE_DATA_DIR: dataDir,
    SHORE_RUNTIME_DIR: runtimeDir,
    SHORE_CACHE_DIR: cacheDir,
  },
  stdout: "pipe",
  stderr: "inherit",
});

let ok = false;
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

  await readUntil(frames, (f) => f.type === "hello");
  sock.write(
    JSON.stringify({
      type: "hello",
      client_type: "cli",
      client_name: "full-smoke",
      character: "smoketest",
    }) + "\n",
  );
  await readUntil(frames, (f) => f.type === "history");
  console.log("  handshake ok");

  // ── 1. plain generation ────────────────────────────────────────────
  console.log("\n[1] plain generation");
  sock.write(
    JSON.stringify({
      type: "message",
      rid: "gen-1",
      text: "Say PONG and nothing else.",
    }) + "\n",
  );
  const gen1End = await readUntil(frames, (f) => f.type === "stream_end" && f.is_final === true);
  console.log(`  stream_end finish=${gen1End.finish_reason} content="${(gen1End.content as string).slice(0, 30)}"`);
  await readUntil(frames, (f) => f.type === "history" && hasRole(f, "assistant"));
  console.log("  history with assistant turn ok");

  // ── 2. regen ───────────────────────────────────────────────────────
  console.log("\n[2] regen");
  sock.write(JSON.stringify({ type: "regen", rid: "regen-1" }) + "\n");
  // The truncate-then-regen flow emits: history (smaller) → stream_start → ... → stream_end → history (with new asst).
  // We just need to confirm a new stream_end fires and the history has assistant again.
  const regenEnd = await readUntil(frames, (f) => f.type === "stream_end" && f.is_final === true && (f as { rid?: string }).rid === "regen-1");
  console.log(`  regen stream_end finish=${regenEnd.finish_reason}`);
  await readUntil(frames, (f) => f.type === "history" && hasRole(f, "assistant"));
  console.log("  history with new assistant turn ok");

  // ── 3. cancel ──────────────────────────────────────────────────────
  console.log("\n[3] cancel (abort mid-stream)");
  sock.write(
    JSON.stringify({
      type: "message",
      rid: "cancel-1",
      text: "Count slowly from one to twenty in plain English. Take your time.",
    }) + "\n",
  );
  // Wait until the model has actually started streaming (we see at least
  // one stream_chunk), then abort.
  await readUntil(frames, (f) => f.type === "stream_chunk");
  sock.write(JSON.stringify({ type: "cancel" }) + "\n");
  const cancelEnd = await readUntil(
    frames,
    (f) => f.type === "stream_end" && (f as { rid?: string }).rid === "cancel-1",
    20_000,
  );
  if (cancelEnd.finish_reason !== "cancelled") {
    fail(`expected finish_reason=cancelled, got "${cancelEnd.finish_reason}"`);
  }
  console.log(`  cancel stream_end finish=${cancelEnd.finish_reason} ok`);

  // ── 4. command (inject_system_message) ────────────────────────────
  console.log("\n[4] command: inject_system_message");
  sock.write(
    JSON.stringify({
      type: "command",
      rid: "cmd-1",
      name: "inject_system_message",
      args: { text: "Remember: always end your reply with the word PEACH." },
    }) + "\n",
  );
  // inject_system_message emits the history broadcast FIRST (from the
  // engine.appendMessage call) and the command_output AFTER. The single
  // readUntil consumes both — we just need to land on the latter.
  const cmdOut = await readUntil(frames, (f) => f.type === "command_output" && (f as { rid?: string }).rid === "cmd-1");
  const cmdData = cmdOut.data as { injected?: boolean };
  if (!cmdData.injected) fail("command_output missing injected:true");
  console.log("  command_output ok");

  console.log("\nok — all four flows passed");
  ok = true;
  sock.end();
} finally {
  proc.kill("SIGTERM");
  await proc.exited;
  if (!ok) process.exit(1);
}

// ── helpers ──────────────────────────────────────────────────────────

interface Frame { type: string; [k: string]: unknown }

function hasRole(f: Frame, role: string): boolean {
  const msgs = f.messages as Array<{ role: string }> | undefined;
  return Array.isArray(msgs) && msgs.some((m) => m.role === role);
}

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
  eof(): void { this.closed = true; this.drain(); }
  error(e: Error): void { this.err = e; this.drain(); }
  read(): Promise<string> {
    return new Promise((resolve, reject) => {
      this.waiters.push({ resolve, reject });
      this.drain();
    });
  }
  private drain(): void {
    while (this.waiters.length > 0) {
      const w = this.waiters[0]!;
      if (this.err) { this.waiters.shift(); w.reject(this.err); continue; }
      const nl = this.buf.indexOf(0x0a);
      if (nl < 0) {
        if (this.closed) { this.waiters.shift(); w.reject(new Error("connection closed")); continue; }
        return;
      }
      const line = this.buf.subarray(0, nl).toString("utf8");
      this.buf = this.buf.subarray(nl + 1);
      this.waiters.shift();
      w.resolve(line);
    }
  }
}

async function readFrame(q: FrameQueue, timeoutMs = 30_000): Promise<Frame> {
  const line = await Promise.race([
    q.read(),
    new Promise<string>((_, rej) => setTimeout(() => rej(new Error(`read timeout (${timeoutMs}ms)`)), timeoutMs)),
  ]);
  return JSON.parse(line) as Frame;
}

/** Read frames until one matches the predicate. Returns that frame. */
async function readUntil(
  q: FrameQueue,
  pred: (f: Frame) => boolean,
  timeoutMs = 30_000,
): Promise<Frame> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const f = await readFrame(q, Math.max(1, deadline - Date.now()));
    if (pred(f)) return f;
  }
  throw new Error("readUntil timed out");
}

function fail(msg: string): never {
  console.error(`FAIL: ${msg}`);
  process.exit(1);
}
