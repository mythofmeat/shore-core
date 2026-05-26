#!/usr/bin/env bun
/**
 * Tier 3 parity check: `memory_dream` command end-to-end.
 *
 * Sends a single `memory_dream` SWP command with `force=true` and a
 * canned text-only librarian response. The librarian invokes no tools,
 * so the daemon's MEMORY.md fallback path is exercised: fallback content
 * is written, the DREAMS.md audit entry is appended, and
 * `dreams/state.json` is updated. Diffs the command_output payload, the
 * librarian's outbound LLM request body, and the on-disk artifacts
 * (`dreams/state.json`, `DREAMS.md`, fallback `MEMORY.md`).
 *
 * Frozen-baseline mode compares the TS daemon's full memory_dream flow
 * against a committed baseline.
 */

import fs from "node:fs";
import { mkdirSync } from "node:fs";
import { dirname, join, resolve as resolvePath } from "node:path";

import {
  buildDaemonEnv,
  compareFrames,
  copyFixtureToTmp,
  openConnection,
  readFrame,
  readListenAddr,
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

const DEFAULT_FIXTURE = "parity-traces/fixtures/dreaming-cmd";
const DEFAULT_RESPONSE = "parity-traces/llm-fixtures/dreaming-cmd.json";
const COMMAND_RID = "dream-1";
const CHARACTER = "scout";

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

interface FrozenDreamingBaseline {
  version: 1;
  mode: "dreaming";
  fixture: string;
  response: string;
  cacheTtl: string | null;
  commandOutput: unknown;
  providerRequests: FrozenRequest[];
  dreamsState: unknown;
  dreamsLog: string;
  memoryIndex: string;
}

const args = parseArgs(process.argv.slice(2));
if (args.baseline === undefined && args.writeBaseline === undefined) {
  console.error("usage: parity-check-dreaming.ts --baseline <path> | --write-baseline <path>");
  process.exit(2);
}
const tsCmd = args.ts === undefined ? ["bun", "src/main.ts"] : [args.ts];
const responses = loadCannedResponses(resolvePath(args.response));
if (responses.length < 1) {
  throw new Error(`${args.response} must contain at least one canned response`);
}

const result = await runScenario(tsCmd, resolvePath(args.fixture), responses, args.cacheTtl);

if (args.writeBaseline !== undefined) {
  writeFrozenBaseline(resolvePath(args.writeBaseline), {
    version: 1,
    mode: "dreaming",
    fixture: args.fixture,
    response: args.response,
    cacheTtl: args.cacheTtl ?? null,
    commandOutput: result.commandOutput,
    providerRequests: result.requests.map((r) => ({
      method: r.method,
      path: r.path,
      body: redactHeartbeatMarkers(r.body),
    })),
    dreamsState: result.dreamsState,
    dreamsLog: normalizeDreamsLog(result.dreamsLog),
    memoryIndex: normalizeMemoryIndex(result.memoryIndex),
  });
  console.log(`\nwrote dreaming baseline: ${args.writeBaseline}`);
} else {
  const baseline = readFrozenBaseline(resolvePath(args.baseline!));
  let failures = 0;
  failures += compareCommandOutput(baseline.commandOutput, result.commandOutput);
  failures += compareRequests(baseline.providerRequests, result.requests);
  failures += compareDreamsState(baseline.dreamsState, result.dreamsState);
  failures += compareDreamsLog(baseline.dreamsLog, result.dreamsLog);
  failures += compareMemoryIndex(baseline.memoryIndex, result.memoryIndex);

  if (failures > 0) {
    console.error(`\n${failures} dreaming parity failure(s)`);
    process.exit(1);
  }
  console.log("\ndreaming parity ok");
}

async function runScenario(
  cmd: string[],
  fixtureDir: string,
  responses: CannedLlmResponse[],
  cacheTtl: string | undefined,
): Promise<{
  commandOutput: unknown;
  requests: CapturedLlmRequest[];
  dreamsState: unknown;
  dreamsLog: string;
  memoryIndex: string;
}> {
  console.log("-- dreaming: ts --");
  const proxy = startParityLlmProxy({ response: responses });
  try {
    const { configDir, dataDir } = copyFixtureToTmp(fixtureDir, "shore-dreaming-ts-");
    patchProxyBaseUrl(configDir, proxy.baseUrl);
    if (cacheTtl !== undefined) setCacheTtl(configDir, cacheTtl);
    const env = buildDaemonEnv({
      configDir,
      dataDir,
      prefix: "shore-dreaming-ts-",
    });
    env["SHORE_PARITY_ANTHROPIC_KEY"] = "sk-parity";
    env["TZ"] = "UTC";

    const framesSeen: Record<string, unknown>[] = [];
    let commandOutput: unknown;
    const proc = spawnDaemon(cmd, env);
    try {
      const addr = await readListenAddr([proc.stdout, proc.stderr]);
      if (!addr) throw new Error("ts: daemon never printed listen address");

      const { sock, frames } = await openConnection(addr);
      framesSeen.push((await readFrame(frames)) as Record<string, unknown>);
      sock.write(
        JSON.stringify({
          type: "hello",
          client_type: "cli",
          client_name: "dreaming-parity-ts",
          capabilities: ["streaming"],
          character: CHARACTER,
        }) + "\n",
      );
      framesSeen.push((await readFrame(frames)) as Record<string, unknown>);

      sock.write(
        JSON.stringify({
          type: "command",
          rid: COMMAND_RID,
          name: "memory_dream",
          args: { force: true },
        }) + "\n",
      );
      commandOutput = await readUntilCommandOutput(frames, framesSeen, COMMAND_RID);
      sock.end();
    } catch (e) {
      console.error("ts frames before failure:");
      for (const frame of framesSeen) console.error(`  ${JSON.stringify(frame)}`);
      throw e;
    } finally {
      proc.kill("SIGTERM");
      await proc.exited;
    }

    return {
      commandOutput,
      requests: [...proxy.requests],
      dreamsState: readDreamsState(dataDir),
      dreamsLog: readDreamsLog(dataDir),
      memoryIndex: readMemoryIndex(configDir),
    };
  } finally {
    await proxy.stop();
  }
}

async function readUntilCommandOutput(
  frames: FrameQueue,
  framesSeen: Record<string, unknown>[],
  rid: string,
): Promise<unknown> {
  const deadline = Date.now() + 30_000;
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
    if (frame["type"] === "command_output" && frame["rid"] === rid) {
      return frame["data"];
    }
  }
  throw new Error("ts: timed out waiting for memory_dream command_output");
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

function compareCommandOutput(expected: unknown, actual: unknown): number {
  const diffs = compareFrames(
    { type: "memory_dream", data: expected },
    { type: "memory_dream", data: actual },
    {
      memory_dream: [
        "data.ran_at",
        "data.paths_written[*]",
        "data.staged_path",
        "data.dreams_path",
        "data.memory_path",
        "data.phase_summaries[*].paths[*]",
      ],
    },
  );
  if (diffs.length === 0) {
    console.log("  ok    memory_dream command_output (paths + ran_at fuzzy)");
    return 0;
  }
  console.error("  FAIL  memory_dream command_output");
  for (const diff of diffs) console.error(`        ${diff}`);
  console.error(`        expected: ${JSON.stringify(expected)}`);
  console.error(`        actual:   ${JSON.stringify(actual)}`);
  return 1;
}

function compareRequests(expected: FrozenRequest[], actual: CapturedLlmRequest[]): number {
  let failures = 0;
  if (actual.length !== expected.length) {
    console.error(`  FAIL  provider request count: expected ${expected.length}, got ${actual.length}`);
    failures++;
  }
  const n = Math.min(actual.length, expected.length);
  for (let i = 0; i < n; i++) {
    const a = actual[i]!;
    const e = expected[i]!;
    if (a.method !== e.method) {
      console.error(`  FAIL  provider request ${i + 1} method: expected ${e.method}, got ${a.method}`);
      failures++;
    }
    if (a.path !== e.path) {
      console.error(`  FAIL  provider request ${i + 1} path: expected ${e.path}, got ${a.path}`);
      failures++;
    }
    const expectedBody = canonicalizeJson(e.body);
    const actualBody = canonicalizeJson(redactHeartbeatMarkers(a.body));
    if (actualBody === expectedBody) {
      console.log(`  ok    librarian request body (${a.key.slice(0, 12)})`);
    } else {
      console.error(`  FAIL  librarian request body`);
      console.error(`        expected: ${expectedBody}`);
      console.error(`        actual:   ${actualBody}`);
      failures++;
    }
  }
  return failures;
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

function indent(text: string): string {
  return text
    .split("\n")
    .map((l) => `          ${l}`)
    .join("\n");
}

function readFrozenBaseline(path: string): FrozenDreamingBaseline {
  const parsed = JSON.parse(fs.readFileSync(path, "utf8")) as FrozenDreamingBaseline;
  if (parsed.version !== 1 || parsed.mode !== "dreaming") {
    throw new Error(`${path}: unsupported dreaming baseline`);
  }
  return parsed;
}

function writeFrozenBaseline(path: string, baseline: FrozenDreamingBaseline): void {
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
