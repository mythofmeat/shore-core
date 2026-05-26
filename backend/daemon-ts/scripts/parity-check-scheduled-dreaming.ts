#!/usr/bin/env bun
/**
 * Tier 3 parity check: scheduled dreaming via the autonomy cron path.
 *
 * The fixture seeds `dreams/state.json` with a far-future `last_run_at`
 * so the first ticker pulse skips before the setup chat turn has cached
 * a completed request. After setup finishes, the check deletes that
 * state file, waits for the next ticker pulse to run the scheduled AI
 * librarian pass, then diffs the cached-prefix librarian request,
 * written dreaming artifacts, and the `dreaming` ledger row against a
 * committed baseline.
 */

import { Database } from "bun:sqlite";
import fs from "node:fs";
import { mkdirSync } from "node:fs";
import { dirname, join, resolve as resolvePath } from "node:path";

import {
  buildDaemonEnv,
  compareFrames,
  copyFixtureToTmp,
  openConnection,
  readFrame,
  redactHeartbeatMarkers,
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
const CHARACTER = "scout";
const SETUP_RID = "scheduled-dream-setup";
const FUTURE_LAST_RUN_AT = "2999-01-01T00:00:00Z";

interface Args {
  ts: string | undefined;
  fixture: string;
  response: string;
  cacheTtl: string | undefined;
  baseline: string | undefined;
  writeBaseline: string | undefined;
}

interface FrozenRequest {
  method: string;
  path: string;
  body: unknown;
}

interface FrozenScheduledDreamingBaseline {
  version: 1;
  mode: "scheduled-dreaming";
  fixture: string;
  response: string;
  cacheTtl: string | null;
  librarianRequest: FrozenRequest;
  dreamsState: unknown;
  dreamsLog: string;
  memoryIndex: string;
  ledgerRows: Record<string, unknown>[];
}

const args = parseArgs(process.argv.slice(2));
if (args.baseline === undefined && args.writeBaseline === undefined) {
  console.error("usage: parity-check-scheduled-dreaming.ts --baseline <path> | --write-baseline <path>");
  process.exit(2);
}
const tsCmd = args.ts === undefined ? ["bun", "src/main.ts"] : [args.ts];
const responses = loadCannedResponses(resolvePath(args.response));
if (responses.length < 2) {
  throw new Error(`${args.response} must contain at least two canned responses`);
}

const result = await runScenario(tsCmd, resolvePath(args.fixture), responses, args.cacheTtl);

if (args.writeBaseline !== undefined) {
  const librarian = pickLibrarianRequest(result.requests);
  writeFrozenBaseline(resolvePath(args.writeBaseline), {
    version: 1,
    mode: "scheduled-dreaming",
    fixture: args.fixture,
    response: args.response,
    cacheTtl: args.cacheTtl ?? null,
    librarianRequest: {
      method: librarian.method,
      path: librarian.path,
      body: redactHeartbeatMarkers(librarian.body),
    },
    dreamsState: result.dreamsState,
    dreamsLog: normalizeDreamsLog(result.dreamsLog),
    memoryIndex: normalizeMemoryIndex(result.memoryIndex),
    ledgerRows: result.ledgerRows,
  });
  console.log(`\nwrote scheduled-dreaming baseline: ${args.writeBaseline}`);
} else {
  const baseline = readFrozenBaseline(resolvePath(args.baseline!));
  let failures = 0;
  failures += compareDreamRequest(baseline.librarianRequest, result.requests);
  failures += compareDreamsState(baseline.dreamsState, result.dreamsState);
  failures += compareDreamsLog(baseline.dreamsLog, result.dreamsLog);
  failures += compareMemoryIndex(baseline.memoryIndex, result.memoryIndex);
  failures += compareLedgerRows(baseline.ledgerRows, result.ledgerRows);

  if (failures > 0) {
    console.error(`\n${failures} scheduled-dreaming parity failure(s)`);
    process.exit(1);
  }
  console.log("\nscheduled-dreaming parity ok");
}

async function runScenario(
  cmd: string[],
  fixtureDir: string,
  responses: CannedLlmResponse[],
  cacheTtl: string | undefined,
): Promise<{
  requests: CapturedLlmRequest[];
  dreamsState: unknown;
  dreamsLog: string;
  memoryIndex: string;
  ledgerRows: Record<string, unknown>[];
}> {
  console.log("-- scheduled-dreaming: ts --");
  const proxy = startParityLlmProxy({ response: responses });
  try {
    const { configDir, dataDir } = copyFixtureToTmp(fixtureDir, "shore-scheduled-dreaming-ts-");
    patchProxyBaseUrl(configDir, proxy.baseUrl);
    if (cacheTtl !== undefined) setCacheTtl(configDir, cacheTtl);
    const env = buildDaemonEnv({
      configDir,
      dataDir,
      prefix: "shore-scheduled-dreaming-ts-",
    });
    env["SHORE_PARITY_ANTHROPIC_KEY"] = "sk-parity";
    env["TZ"] = "UTC";

    const framesSeen: Record<string, unknown>[] = [];
    const proc = spawnDaemon(cmd, env);
    const logs = captureDaemonLogs([proc.stdout, proc.stderr]);
    try {
      const addr = await logs.listenAddr;
      if (!addr) throw new Error("ts: daemon never printed listen address");

      const { sock, frames } = await openConnection(addr);
      framesSeen.push((await readFrame(frames)) as Record<string, unknown>);
      sock.write(
        JSON.stringify({
          type: "hello",
          client_type: "cli",
          client_name: "scheduled-dreaming-parity-ts",
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
      await readUntilFinal(frames, framesSeen, SETUP_RID);

      markDreamDue(dataDir);
      await waitForScheduledDream(dataDir);
      sock.end();
    } catch (e) {
      console.error("ts frames before failure:");
      for (const frame of framesSeen) console.error(`  ${JSON.stringify(frame)}`);
      console.error(`ts provider requests before failure: ${proxy.requests.length}`);
      for (const req of proxy.requests) {
        console.error(`  ${req.key} ${req.path}`);
      }
      console.error("ts daemon logs before failure:");
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
    console.log(`  ts   s2c ${String(frame["type"])}`);
    if (frame["type"] === "error") {
      throw new Error(`ts: daemon emitted error: ${JSON.stringify(frame)}`);
    }
    if (frame["type"] === "stream_end" && frame["rid"] === rid && frame["is_final"] !== false) {
      return;
    }
  }
  throw new Error("ts: timed out waiting for setup stream_end");
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

async function waitForScheduledDream(dataDir: string): Promise<void> {
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
          console.log("  ts   scheduled dream completed");
          return;
        }
      } catch {
        // Keep polling while the daemon is in the middle of writing.
      }
    }
    await Bun.sleep(250);
  }
  throw new Error("ts: timed out waiting for scheduled dreaming artifacts");
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

function pickLibrarianRequest(requests: CapturedLlmRequest[]): CapturedLlmRequest {
  const matches = requests.filter((req) => JSON.stringify(req.body).includes("memory librarian pass"));
  if (matches.length !== 1) {
    throw new Error(
      `expected exactly one librarian request; got ${matches.length} of ${requests.length}`,
    );
  }
  return matches[0]!;
}

function compareDreamRequest(
  expected: FrozenRequest,
  actual: CapturedLlmRequest[],
): number {
  const matches = actual.filter((req) => JSON.stringify(req.body).includes("memory librarian pass"));
  if (matches.length !== 1) {
    console.error(
      `  FAIL  scheduled librarian request count: expected 1, got ${matches.length} of ${actual.length}`,
    );
    console.error(`        actual keys: ${actual.map((req) => req.key).join(", ")}`);
    return 1;
  }
  const m = matches[0]!;
  if (m.method !== expected.method) {
    console.error(`  FAIL  librarian request method: expected ${expected.method}, got ${m.method}`);
    return 1;
  }
  if (m.path !== expected.path) {
    console.error(`  FAIL  librarian request path: expected ${expected.path}, got ${m.path}`);
    return 1;
  }
  const expectedBody = canonicalizeJson(expected.body);
  const actualBody = canonicalizeJson(redactHeartbeatMarkers(m.body));
  if (actualBody === expectedBody) {
    console.log(`  ok    scheduled librarian request body (${m.key.slice(0, 12)})`);
    return 0;
  }
  console.error("  FAIL  scheduled librarian request body");
  console.error(`        expected: ${expectedBody}`);
  console.error(`        actual:   ${actualBody}`);
  return 1;
}

function compareDreamsState(expected: unknown, actual: unknown): number {
  const diffs = compareFrames(
    { type: "dreams_state", value: expected },
    { type: "dreams_state", value: actual },
    { dreams_state: ["value.last_run_at"] },
  );
  if (diffs.length === 0) {
    console.log("  ok    dreams/state.json (last_run_at fuzzy)");
    return 0;
  }
  console.error("  FAIL  dreams/state.json");
  for (const diff of diffs) console.error(`        ${diff}`);
  console.error(`        expected: ${JSON.stringify(expected)}`);
  console.error(`        actual:   ${JSON.stringify(actual)}`);
  return 1;
}

function normalizeDreamsLog(s: string): string {
  return s
    .replace(
      /## \d{4}-\d{2}-\d{2} \d{2}:\d{2} - AI librarian dreaming pass/g,
      "## <ts> - AI librarian dreaming pass",
    )
    .replace(/dream_cycle\s+[^\n]+/g, "dream_cycle <ts>")
    .replace(/AI librarian dreaming pass at `[^`]+`/g, "AI librarian dreaming pass at `<ts>`");
}

function normalizeMemoryIndex(s: string): string {
  return s.replace(/Last updated:\s+[^\n]+/g, "Last updated: <ts>");
}

function compareDreamsLog(expected: string, actual: string): number {
  const nExpected = normalizeDreamsLog(expected);
  const nActual = normalizeDreamsLog(actual);
  if (nExpected === nActual) {
    console.log("  ok    DREAMS.md (timestamp lines fuzzy)");
    return 0;
  }
  console.error("  FAIL  DREAMS.md");
  console.error(`        expected:\n${indent(expected)}`);
  console.error(`        actual:\n${indent(actual)}`);
  return 1;
}

function compareMemoryIndex(expected: string, actual: string): number {
  const nExpected = normalizeMemoryIndex(expected);
  const nActual = normalizeMemoryIndex(actual);
  if (nExpected === nActual) {
    console.log("  ok    MEMORY.md (Last updated fuzzy)");
    return 0;
  }
  console.error("  FAIL  MEMORY.md");
  console.error(`        expected:\n${indent(expected)}`);
  console.error(`        actual:\n${indent(actual)}`);
  return 1;
}

function compareLedgerRows(
  expected: Record<string, unknown>[],
  actual: Record<string, unknown>[],
): number {
  const diffs = compareFrames(
    { type: "dreaming_ledger", rows: expected },
    { type: "dreaming_ledger", rows: actual },
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
  console.error(`        expected: ${canonicalizeJson(expected)}`);
  console.error(`        actual:   ${canonicalizeJson(actual)}`);
  return 1;
}

function indent(text: string): string {
  return text
    .split("\n")
    .map((l) => `          ${l}`)
    .join("\n");
}

function readFrozenBaseline(path: string): FrozenScheduledDreamingBaseline {
  const parsed = JSON.parse(fs.readFileSync(path, "utf8")) as FrozenScheduledDreamingBaseline;
  if (parsed.version !== 1 || parsed.mode !== "scheduled-dreaming") {
    throw new Error(`${path}: unsupported scheduled-dreaming baseline`);
  }
  return parsed;
}

function writeFrozenBaseline(path: string, baseline: FrozenScheduledDreamingBaseline): void {
  mkdirSync(dirname(path), { recursive: true });
  fs.writeFileSync(path, JSON.stringify(baseline, null, 2) + "\n");
}

function patchProxyBaseUrl(configDir: string, proxyBaseUrl: string): void {
  const configPath = join(configDir, "config.toml");
  const raw = fs.readFileSync(configPath, "utf8");
  fs.writeFileSync(configPath, raw.replaceAll("{{LLM_PROXY_BASE_URL}}", proxyBaseUrl));
}

function parseArgs(argv: string[]): Args {
  const parsed: Args = {
    ts: undefined,
    fixture: DEFAULT_FIXTURE,
    response: DEFAULT_RESPONSE,
    cacheTtl: undefined,
    baseline: undefined,
    writeBaseline: undefined,
  };

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]!;
    if (arg === "--ts") parsed.ts = takeValue(argv, ++i, arg);
    else if (arg === "--fixture") parsed.fixture = takeValue(argv, ++i, arg);
    else if (arg === "--response") parsed.response = takeValue(argv, ++i, arg);
    else if (arg === "--cache-ttl") parsed.cacheTtl = takeValue(argv, ++i, arg);
    else if (arg === "--baseline") parsed.baseline = takeValue(argv, ++i, arg);
    else if (arg === "--write-baseline") parsed.writeBaseline = takeValue(argv, ++i, arg);
    else {
      console.error(`unknown arg: ${arg}`);
      process.exit(2);
    }
  }
  if (parsed.baseline !== undefined && parsed.writeBaseline !== undefined) {
    console.error("--baseline and --write-baseline are mutually exclusive");
    process.exit(2);
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
