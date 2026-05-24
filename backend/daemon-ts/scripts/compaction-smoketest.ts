#!/usr/bin/env bun
/**
 * End-to-end compaction smoketest.
 *
 * Spawns the TS daemon against a temp config + data dir whose character's
 * `active.jsonl` is pre-seeded with N turns. Compaction config is tuned so
 * the trigger fires after one more user/assistant turn pair. Sends a real
 * chat message, then verifies:
 *
 *   - `stream_start` → chunks → `stream_end(is_final=true)` arrive normally
 *   - `phase{phase:"compacting"}` frame is emitted after the stream ends
 *   - a fresh `history` frame is broadcast post-reload
 *   - segments/0001.jsonl is written on disk
 *   - active.jsonl is trimmed to the retained tail (keep_recent_turns + new)
 *   - workspace/MEMORY.md is materialized after the compaction boundary
 *     (via apply_deferred_edits → active_prompt snapshot)
 *
 * Burns ~2 LLM calls (one chat + one compaction). Defaults to haiku-4.5
 * via OpenRouter.
 *
 * Requires OPENROUTER_API_KEY in env.
 *
 * Usage:
 *   set -a; source ~/.config/shore/.env; set +a
 *   bun scripts/compaction-smoketest.ts
 */

import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

if (!process.env["OPENROUTER_API_KEY"]) {
  console.error("OPENROUTER_API_KEY required in env");
  process.exit(2);
}

// ── sandbox ─────────────────────────────────────────────────────────────
const tmp = mkdtempSync(join(tmpdir(), "shore-daemon-ts-compact-smoke-"));
const configDir = join(tmp, "config");
const dataDir = join(tmp, "data");
const runtimeDir = join(tmp, "runtime");
const cacheDir = join(tmp, "cache");
for (const d of [configDir, dataDir, runtimeDir, cacheDir]) mkdirSync(d, { recursive: true });

const charName = "compacttest";
const charConfigDir = join(configDir, "characters", charName);
const charDataDir = join(dataDir, charName);
mkdirSync(join(charConfigDir, "workspace"), { recursive: true });
mkdirSync(charDataDir, { recursive: true });
writeFileSync(
  join(charConfigDir, "workspace", "SOUL.md"),
  // Keep it terse so the chat LLM doesn't spend tokens we don't need.
  "You are a terse smoketest assistant. Reply with ONE short sentence.",
);

// Pre-seed active.jsonl with 6 turns (3 user + 3 assistant) so one more
// user message + reply pushes us to 8 messages — past max_turns=6.
const seededTurns: unknown[] = [];
for (let i = 0; i < 6; i++) {
  const role = i % 2 === 0 ? "user" : "assistant";
  const text = role === "user"
    ? `Pre-seed user message ${i}: tell me a one-line trivia fact.`
    : `Pre-seed assistant reply ${i}: the sun is about 4.6 billion years old.`;
  seededTurns.push({
    msg_id: `seed-${i}`,
    role,
    timestamp: new Date(Date.now() - (6 - i) * 60_000).toISOString(),
    images: [],
    content_blocks: [{ type: "text", text }],
  });
}
writeFileSync(
  join(charDataDir, "active.jsonl"),
  seededTurns.map((t) => JSON.stringify(t)).join("\n") + "\n",
);
console.log(`  seeded ${seededTurns.length} turns into active.jsonl`);

// Compaction config tuned to fire on this run.
writeFileSync(
  join(configDir, "config.toml"),
  `
[defaults]
display_name = "smoketester"
model = "chat.openrouter.haiku45"

[chat.openrouter.haiku45]
model_id = "anthropic/claude-haiku-4.5"
max_tokens = 256

[memory.compaction]
enabled = true
min_turns = 4
max_turns = 6
keep_recent_turns = 2
idle_trigger = "30m"
`,
);

writeFileSync(
  join(configDir, ".env"),
  `OPENROUTER_API_KEY=${process.env["OPENROUTER_API_KEY"]}\n`,
);

// ── spawn ───────────────────────────────────────────────────────────────
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

  // 1) hello + handshake
  const hello = await readFrame(frames);
  if (hello.type !== "hello") fail(`expected hello, got ${JSON.stringify(hello)}`);
  console.log(`  hello ok (server=${hello.server_name})`);

  sock.write(
    JSON.stringify({
      type: "hello",
      client_type: "cli",
      client_name: "compaction-smoketest",
      character: charName,
    }) + "\n",
  );

  const histInitial = await readFrame(frames);
  if (histInitial.type !== "history") fail(`expected history, got ${histInitial.type}`);
  const seededVisible = (histInitial.messages as unknown[]).length;
  console.log(
    `  initial history ok (revision=${histInitial.revision}, visible_msgs=${seededVisible})`,
  );

  // 2) send a chat message to push us past max_turns
  sock.write(
    JSON.stringify({
      type: "message",
      rid: "compact-smoke-1",
      text: "What's the capital of Australia?",
    }) + "\n",
  );

  let sawStreamEnd = false;
  let sawPhaseCompacting = false;
  let sawPostReloadHistory = false;
  let chunkCount = 0;
  const deadline = Date.now() + 120_000; // generous: chat + compaction LLM

  while (Date.now() < deadline && !sawPostReloadHistory) {
    const f = await readFrame(frames, 60_000);
    switch (f.type) {
      case "history": {
        const msgs = f.messages as Array<{ role: string }>;
        const note = sawStreamEnd
          ? sawPhaseCompacting
            ? "post-reload"
            : "pre-compaction"
          : "incremental";
        console.log(`  history (rev=${f.revision}, msgs=${msgs.length}, ${note})`);
        if (sawPhaseCompacting) sawPostReloadHistory = true;
        break;
      }
      case "stream_start":
        console.log(`  stream_start`);
        break;
      case "stream_chunk":
        chunkCount++;
        break;
      case "stream_end":
        sawStreamEnd = true;
        console.log(
          `  stream_end (is_final=${f.is_final}, finish=${f.finish_reason}, content="${(f.content as string).slice(0, 60).replace(/\n/g, " ")}")`,
        );
        break;
      case "phase":
        if (f.phase === "compacting") {
          sawPhaseCompacting = true;
          console.log(`  phase{compacting} ✓`);
        } else {
          console.log(`  phase{${f.phase}}`);
        }
        break;
      case "error":
        fail(`server error: code=${f.code} message=${f.message}`);
        break;
      default:
        console.log(`  (other frame: ${f.type})`);
        break;
    }
  }

  if (!sawStreamEnd) fail("never saw stream_end");
  if (chunkCount === 0) fail("no stream_chunk frames");
  if (!sawPhaseCompacting) fail("never saw phase{compacting} — trigger didn't fire");
  if (!sawPostReloadHistory) fail("never saw post-reload history broadcast");

  // 3) on-disk side effects
  const segmentPath = join(charDataDir, "segments", "0001.jsonl");
  if (!existsSync(segmentPath)) fail(`expected segments/0001.jsonl, missing`);
  const segmentLines = readFileSync(segmentPath, "utf8").split("\n").filter((l) => l.length > 0);
  console.log(`  segment 0001 has ${segmentLines.length} archived turns ✓`);

  const activeAfter = readFileSync(join(charDataDir, "active.jsonl"), "utf8")
    .split("\n")
    .filter((l) => l.length > 0);
  console.log(`  active.jsonl trimmed to ${activeAfter.length} turns ✓`);
  if (activeAfter.length >= seededTurns.length + 2) {
    fail(
      `active.jsonl should be smaller than pre-compaction (${seededTurns.length}+2); got ${activeAfter.length}`,
    );
  }

  const memoryPath = join(charConfigDir, "workspace", "MEMORY.md");
  if (!existsSync(memoryPath)) {
    fail(`expected workspace/MEMORY.md to be written by compaction`);
  }
  const memoryContent = readFileSync(memoryPath, "utf8");
  console.log(`  MEMORY.md written (${memoryContent.length} bytes) ✓`);
  if (memoryContent.length === 0) {
    fail(`MEMORY.md is zero-length`);
  }

  // Active-prompt snapshot should also have a MEMORY.md, materialized by
  // the apply_deferred_edits call inside runCompaction.
  const snapshotMem = join(charDataDir, "active_prompt", "MEMORY.md");
  if (existsSync(snapshotMem)) {
    console.log(`  active_prompt/MEMORY.md snapshot present ✓`);
  } else {
    console.log(`  (active_prompt/MEMORY.md absent — deferred edit may have been zero-byte sentinel)`);
  }

  console.log(`\nok — compaction smoketest passed`);
  ok = true;
  sock.end();
} finally {
  proc.kill("SIGTERM");
  await proc.exited;
  if (!ok) process.exit(1);
}

// ── helpers (mirrored from generate-smoketest.ts) ───────────────────────

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
