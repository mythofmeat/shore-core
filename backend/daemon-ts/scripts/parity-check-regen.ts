#!/usr/bin/env bun
/**
 * Tier 3 parity check: deterministic regen.
 *
 * Runs the TS daemon against a fixture and canned provider response queue.
 * The first message gets response A, regen gets response B. The check
 * passes only when:
 *
 *   1. The SWP streams expose the same generation and regen summaries as
 *      the frozen baseline;
 *   2. the daemon's canonical provider request bodies match the baseline;
 *      and
 *   3. the post-restart persisted history matches the baseline (including
 *      regen alts).
 *
 * Usage:
 *   bun scripts/parity-check-regen.ts --baseline parity-traces/frozen/regen-basic.json
 *   bun scripts/parity-check-regen.ts --write-baseline parity-traces/frozen/regen-basic.json
 */

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve as resolvePath } from "node:path";

import {
  buildDaemonEnv,
  compareFrames,
  copyFixtureToTmp,
  openConnection,
  readFrame,
  readListenAddr,
  setCacheTtl,
  spawnDaemon,
} from "./parity/_lib.ts";
import {
  canonicalizeJson,
  loadCannedResponses,
  startParityLlmProxy,
  type CannedLlmResponse,
  type CapturedLlmRequest,
} from "./parity/llm-proxy.ts";

const DEFAULT_FIXTURE = "parity-traces/fixtures/regen-basic";
const DEFAULT_RESPONSE = "parity-traces/llm-fixtures/regen-basic.json";

interface Args {
  ts: string | undefined;
  fixture: string;
  response: string;
  cacheTtl: string | undefined;
  baseline: string | undefined;
  writeBaseline: string | undefined;
}

interface StreamSummary {
  streamStarts: Array<{ regen: unknown }>;
  textChunks: string[];
  finalContent: string;
  finishReason: unknown;
  tokens: unknown;
  model: unknown;
}

interface NormalizedHistory {
  messages: NormalizedMessage[];
}

interface NormalizedMessage {
  role: unknown;
  content: string;
  content_blocks: unknown[];
  alt_index: unknown;
  alt_count: unknown;
  alternatives: NormalizedAlternative[];
}

interface NormalizedAlternative {
  content: string;
  content_blocks: unknown[];
}

interface FrozenRequest {
  method: string;
  path: string;
  body: unknown;
}

interface FrozenRegenBaseline {
  version: 1;
  mode: "regen";
  fixture: string;
  response: string;
  cacheTtl: string | null;
  summary: {
    first: StreamSummary;
    regen: StreamSummary;
  };
  providerRequests: FrozenRequest[];
  history: NormalizedHistory;
}

const args = parseArgs(process.argv.slice(2));
if (args.baseline === undefined && args.writeBaseline === undefined) {
  console.error("usage: parity-check-regen.ts --baseline <path> | --write-baseline <path>");
  process.exit(2);
}
const tsCmd = args.ts === undefined ? ["bun", "src/main.ts"] : [args.ts];
const responses = loadCannedResponses(resolvePath(args.response));
if (responses.length < 2) {
  throw new Error(`${args.response} must contain at least two canned responses for regen`);
}

const result = await runScenario(tsCmd, resolvePath(args.fixture), responses, args.cacheTtl);

if (args.writeBaseline !== undefined) {
  writeFrozenBaseline(resolvePath(args.writeBaseline), {
    version: 1,
    mode: "regen",
    fixture: args.fixture,
    response: args.response,
    cacheTtl: args.cacheTtl ?? null,
    summary: { first: result.first, regen: result.regen },
    providerRequests: result.requests.map((r) => ({
      method: r.method,
      path: r.path,
      body: r.body,
    })),
    history: result.history,
  });
  console.log(`\nwrote regen baseline: ${args.writeBaseline}`);
} else {
  const baseline = readFrozenBaseline(resolvePath(args.baseline!));
  let failures = 0;
  failures += compareSummary("first generation summary", baseline.summary.first, result.first);
  failures += compareSummary("regen summary", baseline.summary.regen, result.regen);
  failures += compareRequestsToBaseline(result.requests, baseline.providerRequests);
  failures += compareHistory(baseline.history, result.history);

  if (failures > 0) {
    console.error(`\n${failures} regen parity failure(s)`);
    process.exit(1);
  }
  console.log("\nregen parity ok");
}

async function runScenario(
  cmd: string[],
  fixtureDir: string,
  responses: CannedLlmResponse[],
  cacheTtl: string | undefined,
): Promise<{
  first: StreamSummary;
  regen: StreamSummary;
  history: NormalizedHistory;
  requests: CapturedLlmRequest[];
}> {
  console.log("-- regen: ts --");
  const proxy = startParityLlmProxy({ response: responses });
  try {
    const { configDir, dataDir } = copyFixtureToTmp(fixtureDir, "shore-regen-ts-");
    patchProxyBaseUrl(configDir, proxy.baseUrl);
    if (cacheTtl !== undefined) setCacheTtl(configDir, cacheTtl);
    const env = buildDaemonEnv({ configDir, dataDir, prefix: "shore-regen-ts-" });
    env["SHORE_PARITY_ANTHROPIC_KEY"] = "sk-parity";
    env["SHORE_PARITY_OPENAI_KEY"] = "sk-parity";
    env["TZ"] = "UTC";

    const framesSeen: Record<string, unknown>[] = [];
    const proc = spawnDaemon(cmd, env);
    try {
      const addr = await readListenAddr([proc.stdout, proc.stderr]);
      if (!addr) throw new Error("ts: daemon never printed listen address");

      const { sock, frames } = await openConnection(addr);

      framesSeen.push((await readFrame(frames)) as Record<string, unknown>);
      sock.write(JSON.stringify({
        type: "hello",
        client_type: "cli",
        client_name: "regen-parity-ts",
        capabilities: ["streaming"],
        character: "scout",
      }) + "\n");
      framesSeen.push((await readFrame(frames)) as Record<string, unknown>);

      sock.write(JSON.stringify({
        type: "message",
        rid: "msg-1",
        text: "Please reply with the regen parity fixture response.",
        stream: true,
      }) + "\n");
      await readUntilFinal(frames, framesSeen, "msg-1");

      sock.write(JSON.stringify({
        type: "regen",
        rid: "regen-1",
        stream: true,
      }) + "\n");
      await readUntilFinal(frames, framesSeen, "regen-1");
      sock.end();
    } catch (e) {
      console.error("ts frames before failure:");
      for (const frame of framesSeen) console.error(`  ${JSON.stringify(frame)}`);
      throw e;
    } finally {
      proc.kill("SIGTERM");
      await proc.exited;
    }

    const restartHistory = await readRestartHistory(cmd, env);
    return {
      first: summarize(framesSeen, "msg-1"),
      regen: summarize(framesSeen, "regen-1"),
      history: normalizeHistory(restartHistory),
      requests: [...proxy.requests],
    };
  } finally {
    await proxy.stop();
  }
}

async function readUntilFinal(
  frames: Parameters<typeof readFrame>[0],
  framesSeen: Record<string, unknown>[],
  rid: string,
): Promise<void> {
  const deadline = Date.now() + 15_000;
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
  throw new Error(`ts: timed out waiting for final stream_end (${rid})`);
}

async function readRestartHistory(
  cmd: string[],
  env: Record<string, string | undefined>,
): Promise<Record<string, unknown>> {
  console.log("-- regen: ts restart --");
  const proc = spawnDaemon(cmd, env);
  try {
    const addr = await readListenAddr([proc.stdout, proc.stderr]);
    if (!addr) throw new Error("ts: restart daemon never printed listen address");

    const { sock, frames } = await openConnection(addr);
    await readFrame(frames);
    sock.write(JSON.stringify({
      type: "hello",
      client_type: "cli",
      client_name: "regen-parity-ts-restart",
      capabilities: ["streaming"],
      character: "scout",
    }) + "\n");
    const history = (await readFrame(frames)) as Record<string, unknown>;
    sock.end();
    if (history["type"] !== "history") {
      throw new Error(`ts: expected restart history, got ${JSON.stringify(history)}`);
    }
    return history;
  } finally {
    proc.kill("SIGTERM");
    await proc.exited;
  }
}

function summarize(frames: Record<string, unknown>[], rid: string): StreamSummary {
  const starts = frames
    .filter((f) => f["type"] === "stream_start" && f["rid"] === rid)
    .map((f) => ({ regen: f["regen"] }));
  const chunks = frames
    .filter((f) => f["type"] === "stream_chunk" && f["rid"] === rid)
    .filter((f) => f["content_type"] === undefined || f["content_type"] === "text")
    .map((f) => String(f["text"] ?? ""));
  const final = frames
    .filter((f) => f["type"] === "stream_end" && f["rid"] === rid && f["is_final"] !== false)
    .at(-1);
  if (final === undefined) throw new Error(`missing final stream_end for ${rid}`);
  const metadata = isObject(final["metadata"]) ? final["metadata"] : {};
  return {
    streamStarts: starts,
    textChunks: chunks,
    finalContent: String(final["content"] ?? ""),
    finishReason: final["finish_reason"],
    tokens: metadata["tokens"],
    model: metadata["model"],
  };
}

function compareSummary(name: string, expected: StreamSummary, actual: StreamSummary): number {
  const diffs = compareFrames(
    { type: name, ...expected },
    { type: name, ...actual },
    {},
  );
  if (diffs.length === 0) {
    console.log(`  ok    ${name}`);
    return 0;
  }
  console.error(`  FAIL  ${name}`);
  for (const diff of diffs) console.error(`        ${diff}`);
  console.error(`        expected: ${JSON.stringify(expected)}`);
  console.error(`        actual:   ${JSON.stringify(actual)}`);
  return 1;
}

function compareRequestsToBaseline(
  actual: CapturedLlmRequest[],
  expected: FrozenRequest[],
): number {
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
    const actualBody = canonicalizeJson(a.body);
    if (actualBody === expectedBody) {
      console.log(`  ok    provider request ${i + 1} (${a.key.slice(0, 12)})`);
    } else {
      console.error(`  FAIL  provider request ${i + 1} body`);
      console.error(`        expected: ${expectedBody}`);
      console.error(`        actual:   ${actualBody}`);
      failures++;
    }
  }
  return failures;
}

function compareHistory(expected: NormalizedHistory, actual: NormalizedHistory): number {
  const diffs = compareFrames(
    { type: "restart_history", ...expected },
    { type: "restart_history", ...actual },
    {},
  );
  if (diffs.length === 0) {
    console.log("  ok    restart history");
    return 0;
  }
  console.error("  FAIL  restart history");
  for (const diff of diffs) console.error(`        ${diff}`);
  console.error(`        expected: ${JSON.stringify(expected)}`);
  console.error(`        actual:   ${JSON.stringify(actual)}`);
  return 1;
}

function normalizeHistory(frame: Record<string, unknown>): NormalizedHistory {
  const messages = Array.isArray(frame["messages"]) ? frame["messages"] : [];
  return {
    messages: messages
      .filter(isObject)
      .map(normalizeMessage),
  };
}

function normalizeMessage(msg: Record<string, unknown>): NormalizedMessage {
  const blocks = normalizeBlocks(msg["content_blocks"]);
  const alternatives = Array.isArray(msg["alternatives"])
    ? msg["alternatives"].filter(isObject).map(normalizeAlternative)
    : [];
  return {
    role: msg["role"],
    content: normalizeContent(msg["content"], blocks),
    content_blocks: blocks,
    alt_index: msg["alt_index"] ?? null,
    alt_count: msg["alt_count"] ?? null,
    alternatives,
  };
}

function normalizeAlternative(alt: Record<string, unknown>): NormalizedAlternative {
  const blocks = normalizeBlocks(alt["content_blocks"]);
  return {
    content: normalizeContent(alt["content"], blocks),
    content_blocks: blocks,
  };
}

function normalizeBlocks(value: unknown): unknown[] {
  if (!Array.isArray(value)) return [];
  return value.map((block) => {
    if (!isObject(block)) return block;
    if (block["type"] === "text") {
      return { type: "text", text: String(block["text"] ?? "") };
    }
    return block;
  });
}

function normalizeContent(value: unknown, blocks: unknown[]): string {
  if (typeof value === "string" && value.length > 0) return value;
  return blocks
    .filter(isObject)
    .filter((block) => block["type"] === "text")
    .map((block) => String(block["text"] ?? ""))
    .join("");
}

function readFrozenBaseline(path: string): FrozenRegenBaseline {
  const parsed = JSON.parse(readFileSync(path, "utf8")) as FrozenRegenBaseline;
  if (parsed.version !== 1 || parsed.mode !== "regen") {
    throw new Error(`${path}: unsupported regen baseline`);
  }
  return parsed;
}

function writeFrozenBaseline(path: string, baseline: FrozenRegenBaseline): void {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, JSON.stringify(baseline, null, 2) + "\n");
}

function patchProxyBaseUrl(configDir: string, proxyBaseUrl: string): void {
  const configPath = join(configDir, "config.toml");
  const raw = readFileSync(configPath, "utf8");
  writeFileSync(configPath, raw.replaceAll("{{LLM_PROXY_BASE_URL}}", proxyBaseUrl));
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

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
