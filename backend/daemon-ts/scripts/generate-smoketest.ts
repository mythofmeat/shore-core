#!/usr/bin/env bun
/**
 * End-to-end generation smoketest.
 *
 * Spawns the daemon against a temp config dir containing one minimal
 * character + a haiku-4.5 chat model entry, connects, sends a chat
 * message, and verifies the frame sequence:
 *
 *   server.hello → server.history (initial empty)
 *   ← client.hello → server.history (with character)
 *   ← client.message
 *   → server.history (with new user msg)
 *   → server.stream_start
 *   → server.stream_chunk × N
 *   → server.stream_end (is_final=true)
 *   → server.history (with new assistant msg)
 *
 * Requires OPENROUTER_API_KEY in env.
 *
 * Usage:
 *   set -a; source ~/.config/shore/.env; set +a
 *   bun scripts/generate-smoketest.ts
 */

import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

if (!process.env["OPENROUTER_API_KEY"]) {
  console.error("OPENROUTER_API_KEY required in env");
  process.exit(2);
}

// ── set up a sandbox config dir ─────────────────────────────────────────
const tmp = mkdtempSync(join(tmpdir(), "shore-daemon-ts-gen-smoke-"));
const configDir = join(tmp, "config");
const dataDir = join(tmp, "data");
const runtimeDir = join(tmp, "runtime");
const cacheDir = join(tmp, "cache");
for (const d of [configDir, dataDir, runtimeDir, cacheDir]) mkdirSync(d, { recursive: true });

// Minimal character: a workspace/SOUL.md so it discovers, but nothing
// fancy — the model only needs to produce a text reply.
const charDir = join(configDir, "characters", "smoketest");
mkdirSync(join(charDir, "workspace"), { recursive: true });
writeFileSync(
  join(charDir, "workspace", "SOUL.md"),
  "You are a terse smoketest assistant. Reply with exactly the word 'PONG'.",
);

// Point default model at haiku via OpenRouter — the catalog prefix
// default routes anthropic/* through the Anthropic SDK.
writeFileSync(
  join(configDir, "config.toml"),
  `
[defaults]
display_name = "smoketester"
model = "chat.openrouter.haiku45"

[chat.openrouter.haiku45]
model_id = "anthropic/claude-haiku-4.5"
max_tokens = 256
`,
);

// Pass through the API key from current env to the spawned daemon.
writeFileSync(
  join(configDir, ".env"),
  `OPENROUTER_API_KEY=${process.env["OPENROUTER_API_KEY"]}\n`,
);

// ── spawn the daemon ─────────────────────────────────────────────────────
const env = {
  ...process.env,
  SHORE_CONFIG_DIR: configDir,
  SHORE_DATA_DIR: dataDir,
  SHORE_RUNTIME_DIR: runtimeDir,
  SHORE_CACHE_DIR: cacheDir,
};
const proc = Bun.spawn({
  cmd: ["bun", "src/main.ts", "--addr", "127.0.0.1:0"],
  env,
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

  // 1) server.hello with characters
  const hello = await readFrame(frames);
  if (hello.type !== "hello") fail(`expected hello, got ${JSON.stringify(hello)}`);
  if (!Array.isArray(hello.characters) || hello.characters.length !== 1) {
    fail(`expected 1 character, got ${JSON.stringify(hello.characters)}`);
  }
  console.log(`  hello ok (server=${hello.server_name}, characters=${(hello.characters as Array<{name:string}>).map((c) => c.name).join(",")})`);

  // 2) client.hello → server.history (with selected character)
  sock.write(
    JSON.stringify({
      type: "hello",
      client_type: "cli",
      client_name: "generate-smoketest",
      character: "smoketest",
    }) + "\n",
  );
  const hist0 = await readFrame(frames);
  if (hist0.type !== "history") fail(`expected history, got ${hist0.type}`);
  if (hist0.selected_character !== "smoketest") fail(`unexpected character: ${hist0.selected_character}`);
  console.log(`  history ok (revision=${hist0.revision}, messages=${(hist0.messages as unknown[]).length})`);

  // 3) client.message → expect stream + history frames
  sock.write(
    JSON.stringify({
      type: "message",
      rid: "smoke-1",
      text: "ping",
    }) + "\n",
  );

  let sawStreamStart = false;
  let sawStreamEnd = false;
  let sawAssistantHistory = false;
  let chunkCount = 0;
  let assistantText = "";
  const deadline = Date.now() + 60_000;

  while (Date.now() < deadline && !sawAssistantHistory) {
    const f = await readFrame(frames, 30_000);
    switch (f.type) {
      case "history": {
        const msgs = f.messages as Array<{ role: string; content: string }>;
        const hasAsst = msgs.some((m) => m.role === "assistant");
        console.log(`  history (rev=${f.revision}, msgs=${msgs.length}, has_asst=${hasAsst})`);
        if (hasAsst) {
          sawAssistantHistory = true;
          assistantText = msgs.filter((m) => m.role === "assistant").map((m) => m.content).join(" / ");
        }
        break;
      }
      case "stream_start":
        sawStreamStart = true;
        console.log(`  stream_start (rid=${f.rid})`);
        break;
      case "stream_chunk":
        chunkCount++;
        break;
      case "stream_end":
        sawStreamEnd = true;
        console.log(`  stream_end (is_final=${f.is_final}, finish=${f.finish_reason}, content="${(f.content as string).slice(0, 40)}", model=${(f.metadata as {model:string}).model})`);
        break;
      case "error":
        fail(`server error: code=${f.code} message=${f.message}`);
        break;
      default:
        console.log(`  (other frame: ${f.type})`);
        break;
    }
  }

  if (!sawStreamStart) fail("never saw stream_start");
  if (!sawStreamEnd) fail("never saw stream_end");
  if (!sawAssistantHistory) fail("never saw history with assistant turn");
  if (chunkCount === 0) fail("no stream_chunk frames");

  console.log(`\nok — end-to-end generation passed`);
  console.log(`  chunks=${chunkCount}  assistantText="${assistantText}"`);
  ok = true;
  sock.end();
} finally {
  proc.kill("SIGTERM");
  await proc.exited;
  if (!ok) process.exit(1);
}

// ── helpers (copied from handshake-smoketest.ts) ────────────────────────

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

async function readFrame(q: FrameQueue, timeoutMs = 5000): Promise<Frame> {
  const line = await Promise.race([
    q.read(),
    new Promise<string>((_, rej) => setTimeout(() => rej(new Error(`read timeout (${timeoutMs}ms)`)), timeoutMs)),
  ]);
  return JSON.parse(line) as Frame;
}

function fail(msg: string): never {
  console.error(`FAIL: ${msg}`);
  process.exit(1);
}
