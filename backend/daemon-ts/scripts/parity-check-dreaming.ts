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
 * Both `cache_ttl=""` (default) and `cache_ttl="1h"` (via `--cache-ttl 1h`)
 * variants run in CI; the cached variant exercises the cache-prefix +
 * breakpoint-placement code paths the no-cache path skips.
 */

import fs from "node:fs";
import { join, resolve as resolvePath } from "node:path";

import {
  buildDaemonEnv,
  compareFrames,
  copyFixtureToTmp,
  openConnection,
  readFrame,
  readListenAddr,
  setCacheTtl,
  spawnDaemon,
  type FrameQueue,
} from "./parity/_lib.ts";
import {
  loadCannedResponses,
  startParityLlmProxy,
  type CannedLlmResponse,
  type CapturedLlmRequest,
} from "./parity/llm-proxy.ts";

const DEFAULT_FIXTURE = "parity-traces/fixtures/dreaming-cmd";
const DEFAULT_RESPONSE = "parity-traces/llm-fixtures/dreaming-cmd.json";
const DEFAULT_RUST = "/usr/bin/shore-daemon";
const COMMAND_RID = "dream-1";
const CHARACTER = "scout";

interface Args {
  rust: string;
  ts: string | undefined;
  fixture: string;
  response: string;
  cacheTtl: string | undefined;
}

interface ScenarioResult {
  commandOutput: unknown;
  requests: CapturedLlmRequest[];
  dreamsState: unknown;
  dreamsLog: string;
  memoryIndex: string;
}

const args = parseArgs(process.argv.slice(2));
const tsCmd = args.ts === undefined ? ["bun", "src/main.ts"] : [args.ts];
const responses = loadCannedResponses(resolvePath(args.response));
if (responses.length < 1) {
  throw new Error(`${args.response} must contain at least one canned response`);
}

const rust = await runScenario("rust", [args.rust], resolvePath(args.fixture), responses, args.cacheTtl);
const ts = await runScenario("ts", tsCmd, resolvePath(args.fixture), responses, args.cacheTtl);

let failures = 0;
failures += compareCommandOutput(rust.commandOutput, ts.commandOutput);
failures += compareRequests(rust.requests, ts.requests);
failures += compareDreamsState(rust.dreamsState, ts.dreamsState);
failures += compareDreamsLog(rust.dreamsLog, ts.dreamsLog);
failures += compareMemoryIndex(rust.memoryIndex, ts.memoryIndex);

if (failures > 0) {
  console.error(`\n${failures} dreaming parity failure(s)`);
  process.exit(1);
}

console.log("\ndreaming parity ok");

async function runScenario(
  label: string,
  cmd: string[],
  fixtureDir: string,
  responses: CannedLlmResponse[],
  cacheTtl: string | undefined,
): Promise<ScenarioResult> {
  console.log(`-- dreaming: ${label} --`);
  const proxy = startParityLlmProxy({ response: responses });
  try {
    const { configDir, dataDir } = copyFixtureToTmp(fixtureDir, `shore-dreaming-${label}-`);
    patchProxyBaseUrl(configDir, proxy.baseUrl);
    if (cacheTtl !== undefined) setCacheTtl(configDir, cacheTtl);
    const env = buildDaemonEnv({
      configDir,
      dataDir,
      prefix: `shore-dreaming-${label}-`,
    });
    env["SHORE_PARITY_ANTHROPIC_KEY"] = "sk-parity";
    env["TZ"] = "UTC";

    const framesSeen: Record<string, unknown>[] = [];
    let commandOutput: unknown;
    const proc = spawnDaemon(cmd, env);
    try {
      const addr = await readListenAddr([proc.stdout, proc.stderr]);
      if (!addr) throw new Error(`${label}: daemon never printed listen address`);

      const { sock, frames } = await openConnection(addr);
      framesSeen.push((await readFrame(frames)) as Record<string, unknown>);
      sock.write(
        JSON.stringify({
          type: "hello",
          client_type: "cli",
          client_name: `dreaming-parity-${label}`,
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
      commandOutput = await readUntilCommandOutput(label, frames, framesSeen, COMMAND_RID);
      sock.end();
    } catch (e) {
      console.error(`${label} frames before failure:`);
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
  label: string,
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
    console.log(`  ${label.padEnd(4)} s2c ${String(frame["type"])}`);
    if (frame["type"] === "error") {
      throw new Error(`${label}: daemon emitted error: ${JSON.stringify(frame)}`);
    }
    if (frame["type"] === "command_output" && frame["rid"] === rid) {
      return frame["data"];
    }
  }
  throw new Error(`${label}: timed out waiting for memory_dream command_output`);
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

function compareCommandOutput(rust: unknown, ts: unknown): number {
  // Many fields legitimately differ between scenarios:
  //   - `ran_at`: current time, captured at call time on each side.
  //   - absolute paths (`paths_written`, `staged_path`, `dreams_path`,
  //     `memory_path`, `phase_summaries[*].paths[*]`): each scenario
  //     uses its own tmp dir.
  const diffs = compareFrames(
    { type: "memory_dream", data: rust },
    { type: "memory_dream", data: ts },
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
    console.log("  ok    memory_dream command_output");
    return 0;
  }
  console.error("  FAIL  memory_dream command_output");
  for (const diff of diffs) console.error(`        ${diff}`);
  console.error(`        rust: ${JSON.stringify(rust)}`);
  console.error(`        ts:   ${JSON.stringify(ts)}`);
  return 1;
}

function compareRequests(rust: CapturedLlmRequest[], ts: CapturedLlmRequest[]): number {
  if (rust.length < 1 || ts.length < 1) {
    console.error(`  FAIL  librarian request missing: rust=${rust.length}, ts=${ts.length}`);
    return 1;
  }
  if (rust.length !== ts.length) {
    console.error(`  FAIL  librarian request count diverged: rust=${rust.length}, ts=${ts.length}`);
    return 1;
  }
  const r = rust[0]!;
  const t = ts[0]!;
  if (r.canonical === t.canonical) {
    console.log(`  ok    librarian request body (${r.key.slice(0, 12)})`);
    return 0;
  }
  console.error("  FAIL  librarian request body");
  console.error(`        rust key: ${r.key}`);
  console.error(`        ts key:   ${t.key}`);
  console.error(`        rust: ${JSON.stringify(r.body)}`);
  console.error(`        ts:   ${JSON.stringify(t.body)}`);
  return 1;
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
  // DREAMS.md is dream_cycle-prefixed markdown with timestamps in the
  // heading + the "AI librarian dreaming pass at `<ts>`" body line.
  // Normalize both substitution sites to a sentinel so the body of the
  // entry is the actual byte-for-byte check.
  const normalize = (s: string): string =>
    s
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
  // The fallback MEMORY.md writer stamps "Last updated: <ran_at>" into
  // the file. Fuzz that one line; the rest is byte-stable content
  // produced by both daemons' fallback path.
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
