#!/usr/bin/env bun
/**
 * End-to-end dreaming smoketest.
 *
 * Spawns the TS daemon against a temp config + data dir with one
 * character. Autonomy is enabled, heartbeat is OFF (we don't want to
 * spend chat tokens here), dreaming is enabled with a cron that fires
 * every minute so the very first autonomy tick (10s after handshake)
 * sees it as due. Triggers a connection to make sure `ensureState`
 * runs for the character, then waits for the librarian to write its
 * audit entry.
 *
 * Verifies:
 *   - the autonomy tick fires `runScheduledDream` (visible via the
 *     librarian's tool calls / state file)
 *   - `dreams/state.json` is written with a recent `last_run_at`
 *   - `<data>/<char>/DREAMS.md` audit entry is appended
 *   - the librarian executed at least one tool round
 *
 * Costs ~1-3 librarian rounds against haiku-4.5 via OpenRouter
 * (capped via `max_tool_rounds = 2`). Generally under a cent.
 *
 * Usage:
 *   set -a; source ~/.config/shore/.env; set +a
 *   bun scripts/dreaming-smoketest.ts
 */

import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

if (!process.env["OPENROUTER_API_KEY"]) {
  console.error("OPENROUTER_API_KEY required in env");
  process.exit(2);
}

// ── sandbox ─────────────────────────────────────────────────────────────
const tmp = mkdtempSync(join(tmpdir(), "shore-daemon-ts-dream-smoke-"));
const configDir = join(tmp, "config");
const dataDir = join(tmp, "data");
const runtimeDir = join(tmp, "runtime");
const cacheDir = join(tmp, "cache");
for (const d of [configDir, dataDir, runtimeDir, cacheDir]) mkdirSync(d, { recursive: true });

const charName = "dreamtest";
const charConfigDir = join(configDir, "characters", charName);
const charDataDir = join(dataDir, charName);
mkdirSync(join(charConfigDir, "workspace"), { recursive: true });
mkdirSync(join(charConfigDir, "workspace", "memory"), { recursive: true });
mkdirSync(charDataDir, { recursive: true });

writeFileSync(
  join(charConfigDir, "workspace", "SOUL.md"),
  "You are a terse librarian smoketest character. Keep responses minimal.",
);
// Seed a tiny MEMORY.md so the librarian has SOMETHING to look at when it
// inspects the workspace — otherwise it might just write a fallback and
// not call any tools.
writeFileSync(
  join(charConfigDir, "workspace", "MEMORY.md"),
  `# Memory Index

## Throughline
Smoketest character with minimal history.

## Notes
- Created for the dreaming smoketest.
`,
);

writeFileSync(
  join(configDir, "config.toml"),
  `
[defaults]
display_name = "dreamtester"
model = "chat.openrouter.haiku45"

[chat.openrouter.haiku45]
model_id = "anthropic/claude-haiku-4.5"
max_tokens = 256

[behavior.autonomy]
enabled = true
[behavior.autonomy.heartbeat]
enabled = false

[memory.dreaming]
enabled = true
# Every minute — guarantees the first 10s tick after handshake sees it as due.
frequency = "* * * * *"
max_tool_rounds = 2
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

  // Handshake: get the daemon to ensureState the character so the tick
  // includes it. Without this the per-character autonomy state is never
  // created and no tick fires.
  const hello = await readFrame(frames);
  if (hello.type !== "hello") fail(`expected hello, got ${JSON.stringify(hello)}`);
  console.log(`  hello ok`);

  sock.write(
    JSON.stringify({
      type: "hello",
      client_type: "cli",
      client_name: "dreaming-smoketest",
      character: charName,
    }) + "\n",
  );

  const histInitial = await readFrame(frames);
  if (histInitial.type !== "history") fail(`expected history, got ${histInitial.type}`);
  console.log(`  history ok (revision=${histInitial.revision})`);

  // The autonomy ticker fires every 10s. The cron is every minute. Wait
  // ~75s to give the first tick room + the librarian a few seconds.
  const dreamsPath = join(charDataDir, "DREAMS.md");
  const statePath = join(charDataDir, "dreams", "state.json");
  console.log(`  waiting for dreaming tick (up to 90s)…`);
  const deadline = Date.now() + 90_000;
  while (Date.now() < deadline) {
    if (existsSync(dreamsPath) && existsSync(statePath)) {
      console.log(`  dreaming artifacts present after ${Math.round((Date.now() - deadline + 90_000) / 1000)}s`);
      break;
    }
    await sleep(2_000);
  }

  if (!existsSync(dreamsPath)) {
    fail(`expected DREAMS.md at ${dreamsPath} — librarian never wrote audit entry`);
  }
  const dreamsContent = readFileSync(dreamsPath, "utf8");
  console.log(`  DREAMS.md present (${dreamsContent.length} bytes) ✓`);
  if (dreamsContent.length === 0) {
    fail(`DREAMS.md is empty`);
  }

  if (!existsSync(statePath)) {
    fail(`expected dreams/state.json at ${statePath}`);
  }
  const state = JSON.parse(readFileSync(statePath, "utf8")) as {
    last_run_at?: string;
    runs?: number;
  };
  console.log(`  state.json: runs=${state.runs}, last_run_at=${state.last_run_at} ✓`);
  if (state.runs === undefined || state.runs < 1) {
    fail(`expected state.runs >= 1; got ${state.runs}`);
  }
  if (state.last_run_at === undefined) {
    fail(`expected state.last_run_at to be set`);
  }
  const ranAt = new Date(state.last_run_at).getTime();
  const ageMs = Date.now() - ranAt;
  if (ageMs > 120_000 || ageMs < 0) {
    fail(`last_run_at ${state.last_run_at} not within last 2 min (age=${ageMs}ms)`);
  }

  console.log(`\nok — dreaming smoketest passed`);
  ok = true;
  sock.end();
} finally {
  proc.kill("SIGTERM");
  await proc.exited;
  if (!ok) process.exit(1);
}

// ── helpers ─────────────────────────────────────────────────────────────

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

async function readFrame(q: FrameQueue, timeoutMs = 10_000): Promise<Frame> {
  const line = await Promise.race([
    q.read(),
    new Promise<string>((_, rej) => setTimeout(() => rej(new Error(`read timeout (${timeoutMs}ms)`)), timeoutMs)),
  ]);
  return JSON.parse(line) as Frame;
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

function fail(msg: string): never {
  console.error(`FAIL: ${msg}`);
  process.exit(1);
}
