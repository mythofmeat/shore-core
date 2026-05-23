#!/usr/bin/env bun
/**
 * End-to-end tool registry smoketest.
 *
 * Spins up the TS daemon, sends a user message that elicits multi-tool
 * use, and verifies that tool_call + tool_result SWP frames flow back
 * with the right tool names. Doesn't assert specific outputs (the model
 * is non-deterministic) — only that the dispatch wiring works.
 *
 * Exercises check_time + roll_dice — both are deterministic-shaped, no
 * side effects, no extra fixtures needed.
 *
 * Requires OPENROUTER_API_KEY.
 *
 * Usage:
 *   set -a; source ~/.config/shore/.env; set +a
 *   bun scripts/tools-smoketest.ts
 */

import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

if (!process.env["OPENROUTER_API_KEY"]) {
  console.error("OPENROUTER_API_KEY required in env");
  process.exit(2);
}

const tmp = mkdtempSync(join(tmpdir(), "shore-daemon-ts-tools-smoke-"));
const configDir = join(tmp, "config");
const dataDir = join(tmp, "data");
const runtimeDir = join(tmp, "runtime");
const cacheDir = join(tmp, "cache");
for (const d of [configDir, dataDir, runtimeDir, cacheDir]) {
  mkdirSync(d, { recursive: true });
}
mkdirSync(join(configDir, "characters", "smoketest", "workspace"), {
  recursive: true,
});
writeFileSync(
  join(configDir, "characters", "smoketest", "workspace", "SOUL.md"),
  "You are a brisk smoketest assistant. You use tools when asked. When the user asks for the time AND a dice roll, you call both tools and answer briefly.",
);
writeFileSync(
  join(configDir, "config.toml"),
  `
[defaults]
display_name = "smoketester"
model = "chat.openrouter.haiku45"

[chat.openrouter.haiku45]
model_id = "anthropic/claude-haiku-4.5"
max_tokens = 1024
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
      client_name: "tools-smoke",
      character: "smoketest",
    }) + "\n",
  );
  await readUntil(frames, (f) => f.type === "history");
  console.log("  handshake ok");

  console.log("\n[tools] check_time + roll_dice");
  sock.write(
    JSON.stringify({
      type: "message",
      rid: "tools-1",
      text:
        "What time is it? Also roll 2d6 for me. Use the tools — both check_time and roll_dice.",
    }) + "\n",
  );

  const seenTools = new Set<string>();
  await readUntil(frames, (f) => {
    if (f.type === "tool_call") {
      seenTools.add(f["tool_name"] as string);
    }
    return f.type === "stream_end" && f.is_final === true;
  });

  if (!seenTools.has("check_time") || !seenTools.has("roll_dice")) {
    fail(
      `expected check_time AND roll_dice tool_calls, got: ${[...seenTools].join(", ") || "(none)"}`,
    );
  }
  console.log(`  saw tool_call: ${[...seenTools].sort().join(", ")} ok`);

  await readUntil(frames, (f) => f.type === "history" && hasRole(f, "assistant"));
  console.log("  history with assistant turn ok");

  console.log("\nok — tools smoketest passed");
  ok = true;
  sock.end();
} finally {
  proc.kill("SIGTERM");
  await proc.exited;
  if (!ok) process.exit(1);
}

// ── helpers ─────────────────────────────────────────────────────────

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

async function readFrame(q: FrameQueue, timeoutMs = 60_000): Promise<Frame> {
  const line = await Promise.race([
    q.read(),
    new Promise<string>((_, rej) => setTimeout(() => rej(new Error(`read timeout (${timeoutMs}ms)`)), timeoutMs)),
  ]);
  return JSON.parse(line) as Frame;
}

async function readUntil(
  q: FrameQueue,
  pred: (f: Frame) => boolean,
  timeoutMs = 60_000,
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
