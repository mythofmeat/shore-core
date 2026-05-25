#!/usr/bin/env bun
/**
 * Tier 3 parity check: scheduled dreaming via the autonomy cron path.
 *
 * The fixture seeds `dreams/state.json` with a far-future `last_run_at`
 * so Rust's immediate first tick and TS's first delayed tick both skip
 * before the setup chat turn has cached a completed request. After setup
 * finishes, the check deletes that state file, waits for the next ticker
 * pulse to run the scheduled AI librarian pass, then diffs the cached-prefix
 * librarian request, written dreaming artifacts, and the `dreaming` ledger row.
 */

import { Database } from "bun:sqlite";
import fs from "node:fs";
import { join, resolve as resolvePath } from "node:path";

import {
  buildDaemonEnv,
  compareFrames,
  copyFixtureToTmp,
  openConnection,
  readFrame,
  setCacheTtl,
  spawnDaemon,
  type FrameQueue,
} from "./parity/_lib.ts";
import {
  canonicalizeJson,
  loadCannedResponses,
  startParityLlmProxy,
  type CannedLlmResponse,
  type CapturedLlmRequest,
} from "./parity/llm-proxy.ts";

const DEFAULT_FIXTURE = "parity-traces/fixtures/scheduled-dreaming";
const DEFAULT_RESPONSE = "parity-traces/llm-fixtures/scheduled-dreaming.json";
const DEFAULT_RUST = "/usr/bin/shore-daemon";
const CHARACTER = "scout";
const SETUP_RID = "scheduled-dream-setup";
const FUTURE_LAST_RUN_AT = "2999-01-01T00:00:00Z";

interface Args {
  rust: string;
  ts: string | undefined;
  fixture: string;
  response: string;
  cacheTtl: string | undefined;
}

interface ScenarioResult {
  requests: CapturedLlmRequest[];
  dreamsState: unknown;
  dreamsLog: string;
  memoryIndex: string;
  ledgerRows: Record<string, unknown>[];
}

const args = parseArgs(process.argv.slice(2));
const tsCmd = args.ts === undefined ? ["bun", "src/main.ts"] : [args.ts];
const responses = loadCannedResponses(resolvePath(args.response));
if (responses.length < 2) {
  throw new Error(`${args.response} must contain at least two canned responses`);
}

const rust = await runScenario("rust", [args.rust], resolvePath(args.fixture), responses, args.cacheTtl);
const ts = await runScenario("ts", tsCmd, resolvePath(args.fixture), responses, args.cacheTtl);

let failures = 0;
failures += compareDreamRequests(rust.requests, ts.requests);
failures += compareDreamsState(rust.dreamsState, ts.dreamsState);
failures += compareDreamsLog(rust.dreamsLog, ts.dreamsLog);
failures += compareMemoryIndex(rust.memoryIndex, ts.memoryIndex);
failures += compareLedgerRows(rust.ledgerRows, ts.ledgerRows);

if (failures > 0) {
  console.error(`\n${failures} scheduled-dreaming parity failure(s)`);
  process.exit(1);
}

console.log("\nscheduled-dreaming parity ok");

async function runScenario(
  label: string,
  cmd: string[],
  fixtureDir: string,
  responses: CannedLlmResponse[],
  cacheTtl: string | undefined,
): Promise<ScenarioResult> {
  console.log(`-- scheduled-dreaming: ${label} --`);
  const proxy = startParityLlmProxy({ response: responses });
  try {
    const { configDir, dataDir } = copyFixtureToTmp(
      fixtureDir,
      `shore-scheduled-dreaming-${label}-`,
    );
    patchProxyBaseUrl(configDir, proxy.baseUrl);
    if (cacheTtl !== undefined) setCacheTtl(configDir, cacheTtl);
    const env = buildDaemonEnv({
      configDir,
      dataDir,
      prefix: `shore-scheduled-dreaming-${label}-`,
    });
    env["SHORE_PARITY_ANTHROPIC_KEY"] = "sk-parity";
    env["TZ"] = "UTC";

    const framesSeen: Record<string, unknown>[] = [];
    const proc = spawnDaemon(cmd, env);
    const logs = captureDaemonLogs([proc.stdout, proc.stderr]);
    try {
      const addr = await logs.listenAddr;
      if (!addr) throw new Error(`${label}: daemon never printed listen address`);

      const { sock, frames } = await openConnection(addr);
      framesSeen.push((await readFrame(frames)) as Record<string, unknown>);
      sock.write(
        JSON.stringify({
          type: "hello",
          client_type: "cli",
          client_name: `scheduled-dreaming-parity-${label}`,
          capabilities: ["streaming"],
          character: CHARACTER,
        }) + "\n",
      );
      framesSeen.push((await readFrame(frames)) as Record<string, unknown>);

      sock.write(
        JSON.stringify({
          type: "message",
          rid: SETUP_RID,
          text: "Please seed scheduled dreaming parity state.",
          stream: true,
        }) + "\n",
      );
      await readUntilFinal(label, frames, framesSeen, SETUP_RID);

      markDreamDue(dataDir);
      await waitForScheduledDream(label, dataDir);
      sock.end();
    } catch (e) {
      console.error(`${label} frames before failure:`);
      for (const frame of framesSeen) console.error(`  ${JSON.stringify(frame)}`);
      console.error(
        `${label} provider requests before failure: ${proxy.requests.length}`,
      );
      for (const req of proxy.requests) {
        console.error(`  ${req.key} ${req.path}`);
      }
      console.error(`${label} daemon logs before failure:`);
      for (const line of logs.lines.slice(-80)) console.error(`  ${line}`);
      throw e;
    } finally {
      proc.kill("SIGTERM");
      await proc.exited;
    }

    return {
      requests: [...proxy.requests],
      dreamsState: readDreamsState(dataDir),
      dreamsLog: readDreamsLog(dataDir),
      memoryIndex: readMemoryIndex(configDir),
      ledgerRows: readDreamingLedgerRows(dataDir),
    };
  } finally {
    await proxy.stop();
  }
}

async function readUntilFinal(
  label: string,
  frames: FrameQueue,
  framesSeen: Record<string, unknown>[],
  rid: string,
): Promise<void> {
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    const frame = (await readFrame(frames, Math.max(100, deadline - Date.now()))) as Record<
      string,
      unknown
    >;
    framesSeen.push(frame);
    console.log(`  ${label.padEnd(4)} s2c ${String(frame["type"])}`);
    if (frame["type"] === "error") {
      throw new Error(`${label}: daemon emitted error: ${JSON.stringify(frame)}`);
    }
    if (frame["type"] === "stream_end" && frame["rid"] === rid && frame["is_final"] !== false) {
      return;
    }
  }
  throw new Error(`${label}: timed out waiting for setup stream_end`);
}

function captureDaemonLogs(streams: ReadableStream<Uint8Array>[]): {
  lines: string[];
  listenAddr: Promise<{ host: string; port: number } | undefined>;
} {
  const lines: string[] = [];
  let resolved = false;
  let resolveAddr: (addr: { host: string; port: number } | undefined) => void = () => {};
  const listenAddr = new Promise<{ host: string; port: number } | undefined>((resolve) => {
    resolveAddr = resolve;
  });
  const timeout = setTimeout(() => {
    if (!resolved) {
      resolved = true;
      resolveAddr(undefined);
    }
  }, 10_000);

  for (const stream of streams) {
    void (async () => {
      const decoder = new TextDecoder();
      let acc = "";
      const reader = stream.getReader();
      try {
        while (true) {
          const { value, done } = await reader.read();
          if (done) break;
          const chunk = decoder.decode(value, { stream: true });
          acc += chunk;
          for (const line of chunk.split(/\r?\n/)) {
            if (line.trim().length > 0) lines.push(line);
          }
          if (!resolved) {
            for (const m of acc.matchAll(/(\d+\.\d+\.\d+\.\d+):(\d+)/g)) {
              const port = Number(m[2]);
              if (port > 0) {
                resolved = true;
                clearTimeout(timeout);
                resolveAddr({ host: m[1]!, port });
                break;
              }
            }
          }
        }
      } finally {
        reader.releaseLock();
      }
    })();
  }

  return { lines, listenAddr };
}

function markDreamDue(dataDir: string): void {
  fs.rmSync(join(dataDir, CHARACTER, "dreams", "state.json"), { force: true });
}

async function waitForScheduledDream(label: string, dataDir: string): Promise<void> {
  const statePath = join(dataDir, CHARACTER, "dreams", "state.json");
  const dreamsPath = join(dataDir, CHARACTER, "DREAMS.md");
  const deadline = Date.now() + 40_000;
  while (Date.now() < deadline) {
    if (fs.existsSync(statePath) && fs.existsSync(dreamsPath)) {
      try {
        const state = JSON.parse(fs.readFileSync(statePath, "utf8")) as Record<string, unknown>;
        if (
          typeof state["last_run_at"] === "string"
          && state["last_run_at"] !== FUTURE_LAST_RUN_AT
        ) {
          console.log(`  ${label.padEnd(4)} scheduled dream completed`);
          return;
        }
      } catch {
        // Keep polling while the daemon is in the middle of writing.
      }
    }
    await Bun.sleep(250);
  }
  throw new Error(`${label}: timed out waiting for scheduled dreaming artifacts`);
}

function readDreamsState(dataDir: string): unknown {
  const path = join(dataDir, CHARACTER, "dreams", "state.json");
  if (!fs.existsSync(path)) return undefined;
  return JSON.parse(fs.readFileSync(path, "utf8"));
}

function readDreamsLog(dataDir: string): string {
  const path = join(dataDir, CHARACTER, "DREAMS.md");
  if (!fs.existsSync(path)) return "";
  return fs.readFileSync(path, "utf8");
}

function readMemoryIndex(configDir: string): string {
  const path = join(configDir, "characters", CHARACTER, "workspace", "MEMORY.md");
  if (!fs.existsSync(path)) return "";
  return fs.readFileSync(path, "utf8");
}

function readDreamingLedgerRows(dataDir: string): Record<string, unknown>[] {
  const path = join(dataDir, "ledger.db");
  if (!fs.existsSync(path)) return [];
  const db = new Database(path);
  try {
    return db
      .query<Record<string, unknown>, []>(
        `SELECT
           id, ts, character, provider, api_key_name, model, call_type,
           input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
           cache_ttl, total_ms, ttft_ms, finish_reason, thinking_enabled,
           cache_state, cache_anomaly, input_cost, output_cost,
           cache_read_cost, cache_write_cost, cost_source, total_cost
         FROM calls
         WHERE call_type = 'dreaming'
         ORDER BY id`,
      )
      .all();
  } finally {
    db.close();
  }
}

function compareDreamRequests(
  rust: CapturedLlmRequest[],
  ts: CapturedLlmRequest[],
): number {
  const r = dreamRequests(rust);
  const t = dreamRequests(ts);
  if (r.length !== 1 || t.length !== 1) {
    console.error(
      `  FAIL  scheduled librarian request count: rust=${r.length}/${rust.length}, ts=${t.length}/${ts.length}`,
    );
    console.error(`        rust keys: ${rust.map((req) => req.key).join(", ")}`);
    console.error(`        ts keys:   ${ts.map((req) => req.key).join(", ")}`);
    return 1;
  }
  if (r[0]!.canonical === t[0]!.canonical) {
    console.log(`  ok    scheduled librarian request body (${r[0]!.key.slice(0, 12)})`);
    return 0;
  }
  console.error("  FAIL  scheduled librarian request body");
  console.error(`        rust key: ${r[0]!.key}`);
  console.error(`        ts key:   ${t[0]!.key}`);
  console.error(`        rust: ${JSON.stringify(r[0]!.body)}`);
  console.error(`        ts:   ${JSON.stringify(t[0]!.body)}`);
  return 1;
}

function dreamRequests(requests: CapturedLlmRequest[]): CapturedLlmRequest[] {
  return requests.filter((req) => JSON.stringify(req.body).includes("memory librarian pass"));
}

function compareDreamsState(rust: unknown, ts: unknown): number {
  const diffs = compareFrames(
    { type: "dreams_state", value: rust },
    { type: "dreams_state", value: ts },
    { dreams_state: ["value.last_run_at"] },
  );
  if (diffs.length === 0) {
    console.log("  ok    dreams/state.json (last_run_at fuzzy)");
    return 0;
  }
  console.error("  FAIL  dreams/state.json");
  for (const diff of diffs) console.error(`        ${diff}`);
  console.error(`        rust: ${JSON.stringify(rust)}`);
  console.error(`        ts:   ${JSON.stringify(ts)}`);
  return 1;
}

function compareDreamsLog(rust: string, ts: string): number {
  // Three timestamp sites to fuzz: the markdown heading
  // (`## YYYY-MM-DD HH:MM - AI librarian dreaming pass`), the
  // dream_cycle frontmatter, and the body line. The heading can flip a
  // minute between the rust and ts runs when they straddle the wall
  // clock — same flakiness window the MEMORY.md `Last updated:`
  // normalizer covers.
  const normalize = (s: string): string =>
    s
      .replace(
        /## \d{4}-\d{2}-\d{2} \d{2}:\d{2} - AI librarian dreaming pass/g,
        "## <ts> - AI librarian dreaming pass",
      )
      .replace(/dream_cycle\s+[^\n]+/g, "dream_cycle <ts>")
      .replace(/AI librarian dreaming pass at `[^`]+`/g, "AI librarian dreaming pass at `<ts>`");
  const nRust = normalize(rust);
  const nTs = normalize(ts);
  if (nRust === nTs) {
    console.log("  ok    DREAMS.md (timestamp lines fuzzy)");
    return 0;
  }
  console.error("  FAIL  DREAMS.md");
  console.error(`        rust:\n${indent(rust)}`);
  console.error(`        ts:\n${indent(ts)}`);
  return 1;
}

function compareMemoryIndex(rust: string, ts: string): number {
  const normalize = (s: string): string =>
    s.replace(/Last updated:\s+[^\n]+/g, "Last updated: <ts>");
  const nRust = normalize(rust);
  const nTs = normalize(ts);
  if (nRust === nTs) {
    console.log("  ok    MEMORY.md (Last updated fuzzy)");
    return 0;
  }
  console.error("  FAIL  MEMORY.md");
  console.error(`        rust:\n${indent(rust)}`);
  console.error(`        ts:\n${indent(ts)}`);
  return 1;
}

function compareLedgerRows(
  rust: Record<string, unknown>[],
  ts: Record<string, unknown>[],
): number {
  const diffs = compareFrames(
    { type: "dreaming_ledger", rows: rust },
    { type: "dreaming_ledger", rows: ts },
    {
      dreaming_ledger: [
        "rows[*].id",
        "rows[*].ts",
        "rows[*].total_ms",
        "rows[*].ttft_ms",
      ],
    },
  );
  if (diffs.length === 0) {
    console.log("  ok    dreaming ledger row (timing fuzzy)");
    return 0;
  }
  console.error("  FAIL  dreaming ledger row");
  for (const diff of diffs) console.error(`        ${diff}`);
  console.error(`        rust: ${canonicalizeJson(rust)}`);
  console.error(`        ts:   ${canonicalizeJson(ts)}`);
  return 1;
}

function indent(text: string): string {
  return text
    .split("\n")
    .map((l) => `          ${l}`)
    .join("\n");
}

function patchProxyBaseUrl(configDir: string, proxyBaseUrl: string): void {
  const configPath = join(configDir, "config.toml");
  const raw = fs.readFileSync(configPath, "utf8");
  fs.writeFileSync(configPath, raw.replaceAll("{{LLM_PROXY_BASE_URL}}", proxyBaseUrl));
}

function parseArgs(argv: string[]): Args {
  const parsed: Args = {
    rust: DEFAULT_RUST,
    ts: undefined,
    fixture: DEFAULT_FIXTURE,
    response: DEFAULT_RESPONSE,
    cacheTtl: undefined,
  };

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]!;
    if (arg === "--rust") parsed.rust = takeValue(argv, ++i, arg);
    else if (arg === "--ts") parsed.ts = takeValue(argv, ++i, arg);
    else if (arg === "--fixture") parsed.fixture = takeValue(argv, ++i, arg);
    else if (arg === "--response") parsed.response = takeValue(argv, ++i, arg);
    else if (arg === "--cache-ttl") parsed.cacheTtl = takeValue(argv, ++i, arg);
    else {
      console.error(`unknown arg: ${arg}`);
      process.exit(2);
    }
  }
  return parsed;
}

function takeValue(argv: string[], idx: number, flag: string): string {
  const value = argv[idx];
  if (value === undefined || value.startsWith("--")) {
    console.error(`${flag} requires a value`);
    process.exit(2);
  }
  return value;
}
