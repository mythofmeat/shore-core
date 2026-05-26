#!/usr/bin/env bun
/**
 * Tier 3 parity check: deterministic Anthropic tool loop.
 *
 * The canned provider queue first asks for the `read` tool, then returns final
 * text after the daemon appends the assistant tool_use and user tool_result
 * turns to the second provider request.
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

const DEFAULT_FIXTURE = "parity-traces/fixtures/tool-loop-read";
const DEFAULT_RESPONSE = "parity-traces/llm-fixtures/tool-loop-read.json";

interface Args {
  ts: string | undefined;
  fixture: string;
  response: string;
  cacheTtl: string | undefined;
  baseline: string | undefined;
  writeBaseline: string | undefined;
}

interface ToolLoopSummary {
  streamStarts: Array<{ regen: unknown }>;
  textChunks: string[];
  streamEnds: Array<{
    content: string;
    finishReason: unknown;
    isFinal: unknown;
    tokens: unknown;
    model: unknown;
  }>;
  toolCalls: Array<{ toolId: unknown; toolName: unknown; input: unknown }>;
  toolResults: Array<{
    toolId: unknown;
    toolName: unknown;
    output: string;
    isError: unknown;
  }>;
}

interface NormalizedHistory {
  messages: Array<{
    role: unknown;
    content: string;
    content_blocks: unknown[];
  }>;
}

interface FrozenRequest {
  method: string;
  path: string;
  body: unknown;
}

interface FrozenToolLoopBaseline {
  version: 1;
  mode: "tool-loop";
  fixture: string;
  response: string;
  cacheTtl: string | null;
  summary: ToolLoopSummary;
  providerRequests: FrozenRequest[];
  history: NormalizedHistory;
}

const args = parseArgs(process.argv.slice(2));
if (args.baseline === undefined && args.writeBaseline === undefined) {
  console.error("usage: parity-check-tool-loop.ts --baseline <path> | --write-baseline <path>");
  process.exit(2);
}
const tsCmd = args.ts === undefined ? ["bun", "src/main.ts"] : [args.ts];
const responses = loadCannedResponses(resolvePath(args.response));
if (responses.length < 2) {
  throw new Error(`${args.response} must contain at least two canned responses`);
}

const result = await runScenario(tsCmd, resolvePath(args.fixture), responses, args.cacheTtl);

if (args.writeBaseline !== undefined) {
  writeFrozenBaseline(resolvePath(args.writeBaseline), {
    version: 1,
    mode: "tool-loop",
    fixture: args.fixture,
    response: args.response,
    cacheTtl: args.cacheTtl ?? null,
    summary: result.summary,
    providerRequests: result.requests.map((r) => ({
      method: r.method,
      path: r.path,
      body: r.body,
    })),
    history: result.history,
  });
  console.log(`\nwrote tool-loop baseline: ${args.writeBaseline}`);
} else {
  const baseline = readFrozenBaseline(resolvePath(args.baseline!));
  let failures = 0;
  failures += compareSummary(baseline.summary, result.summary);
  failures += compareRequestsToBaseline(result.requests, baseline.providerRequests);
  failures += compareHistory(baseline.history, result.history);

  if (failures > 0) {
    console.error(`\n${failures} tool-loop parity failure(s)`);
    process.exit(1);
  }
  console.log("\ntool-loop parity ok");
}

async function runScenario(
  cmd: string[],
  fixtureDir: string,
  responses: CannedLlmResponse[],
  cacheTtl: string | undefined,
): Promise<{
  summary: ToolLoopSummary;
  history: NormalizedHistory;
  requests: CapturedLlmRequest[];
}> {
  console.log("-- tool-loop: ts --");
  const proxy = startParityLlmProxy({ response: responses });
  try {
    const { configDir, dataDir } = copyFixtureToTmp(fixtureDir, "shore-tool-loop-ts-");
    patchProxyBaseUrl(configDir, proxy.baseUrl);
    if (cacheTtl !== undefined) setCacheTtl(configDir, cacheTtl);
    const env = buildDaemonEnv({ configDir, dataDir, prefix: "shore-tool-loop-ts-" });
    env["SHORE_PARITY_ANTHROPIC_KEY"] = "sk-parity";
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
        client_name: "tool-loop-parity-ts",
        capabilities: ["streaming"],
        character: "scout",
      }) + "\n");
      framesSeen.push((await readFrame(frames)) as Record<string, unknown>);

      sock.write(JSON.stringify({
        type: "message",
        rid: "tool-loop-1",
        text: "Please read the parity tool fixture file before answering.",
        stream: true,
      }) + "\n");
      await readUntilFinal(frames, framesSeen, "tool-loop-1");
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
      summary: summarize(framesSeen, "tool-loop-1"),
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
  throw new Error("ts: timed out waiting for final stream_end");
}

async function readRestartHistory(
  cmd: string[],
  env: Record<string, string | undefined>,
): Promise<Record<string, unknown>> {
  console.log("-- tool-loop: ts restart --");
  const proc = spawnDaemon(cmd, env);
  try {
    const addr = await readListenAddr([proc.stdout, proc.stderr]);
    if (!addr) throw new Error("ts: restart daemon never printed listen address");

    const { sock, frames } = await openConnection(addr);
    await readFrame(frames);
    sock.write(JSON.stringify({
      type: "hello",
      client_type: "cli",
      client_name: "tool-loop-parity-ts-restart",
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

function summarize(frames: Record<string, unknown>[], rid: string): ToolLoopSummary {
  return {
    streamStarts: frames
      .filter((f) => f["type"] === "stream_start" && f["rid"] === rid)
      .map((f) => ({ regen: f["regen"] })),
    textChunks: frames
      .filter((f) => f["type"] === "stream_chunk" && f["rid"] === rid)
      .filter((f) => f["content_type"] === undefined || f["content_type"] === "text")
      .map((f) => String(f["text"] ?? "")),
    streamEnds: frames
      .filter((f) => f["type"] === "stream_end" && f["rid"] === rid)
      .map((f) => {
        const metadata = isObject(f["metadata"]) ? f["metadata"] : {};
        return {
          content: String(f["content"] ?? ""),
          finishReason: f["finish_reason"],
          isFinal: f["is_final"] ?? true,
          tokens: metadata["tokens"],
          model: metadata["model"],
        };
      }),
    toolCalls: frames
      .filter((f) => f["type"] === "tool_call" && f["rid"] === rid)
      .map((f) => ({
        toolId: f["tool_id"],
        toolName: f["tool_name"],
        input: f["input"],
      })),
    toolResults: frames
      .filter((f) => f["type"] === "tool_result" && f["rid"] === rid)
      .map((f) => ({
        toolId: f["tool_id"],
        toolName: f["tool_name"],
        output: String(f["output"] ?? ""),
        isError: f["is_error"] ?? false,
      })),
  };
}

function compareSummary(expected: ToolLoopSummary, actual: ToolLoopSummary): number {
  const diffs = compareFrames(
    { type: "tool_loop_summary", ...expected },
    { type: "tool_loop_summary", ...actual },
    {},
  );
  if (diffs.length === 0) {
    console.log("  ok    tool-loop summary");
    return 0;
  }
  console.error("  FAIL  tool-loop summary");
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
    messages: messages.filter(isObject).map((msg) => {
      const blocks = normalizeBlocks(msg["content_blocks"]);
      return {
        role: msg["role"],
        content: normalizeContent(msg["content"], blocks),
        content_blocks: blocks,
      };
    }),
  };
}

function normalizeBlocks(value: unknown): unknown[] {
  if (!Array.isArray(value)) return [];
  return value.map((block) => {
    if (!isObject(block)) return block;
    if (block["type"] === "text") {
      return { type: "text", text: String(block["text"] ?? "") };
    }
    if (block["type"] === "tool_use") {
      return {
        type: "tool_use",
        id: block["id"],
        name: block["name"],
        input: block["input"],
      };
    }
    if (block["type"] === "tool_result") {
      return {
        type: "tool_result",
        tool_use_id: block["tool_use_id"],
        content: String(block["content"] ?? ""),
        is_error: block["is_error"] ?? false,
      };
    }
    return block;
  });
}

function normalizeContent(value: unknown, blocks: unknown[]): string {
  if (typeof value === "string" && value.length > 0) return value;
  return blocks
    .filter(isObject)
    .filter((block) => block["type"] === "text" || block["type"] === "tool_result")
    .map((block) => String(block["text"] ?? block["content"] ?? ""))
    .join("");
}

function readFrozenBaseline(path: string): FrozenToolLoopBaseline {
  const parsed = JSON.parse(readFileSync(path, "utf8")) as FrozenToolLoopBaseline;
  if (parsed.version !== 1 || parsed.mode !== "tool-loop") {
    throw new Error(`${path}: unsupported tool-loop baseline`);
  }
  return parsed;
}

function writeFrozenBaseline(path: string, baseline: FrozenToolLoopBaseline): void {
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
