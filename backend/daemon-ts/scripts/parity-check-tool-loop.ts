#!/usr/bin/env bun
/**
 * Tier 3 parity check: deterministic Anthropic tool loop.
 *
 * The canned provider queue first asks for the `read` tool, then returns final
 * text after the daemon appends the assistant tool_use and user tool_result
 * turns to the second provider request.
 */

import { readFileSync, writeFileSync } from "node:fs";
import { join, resolve as resolvePath } from "node:path";

import {
  buildDaemonEnv,
  compareFrames,
  copyFixtureToTmp,
  openConnection,
  readFrame,
  readListenAddr,
  spawnDaemon,
} from "./parity/_lib.ts";
import {
  loadCannedResponses,
  startParityLlmProxy,
  type CannedLlmResponse,
  type CapturedLlmRequest,
} from "./parity/llm-proxy.ts";

const DEFAULT_FIXTURE = "parity-traces/fixtures/tool-loop-read";
const DEFAULT_RESPONSE = "parity-traces/llm-fixtures/tool-loop-read.json";
const DEFAULT_RUST = "/usr/bin/shore-daemon";

interface Args {
  rust: string;
  ts: string | undefined;
  fixture: string;
  response: string;
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

interface ScenarioResult {
  summary: ToolLoopSummary;
  history: NormalizedHistory;
  requests: CapturedLlmRequest[];
}

interface NormalizedHistory {
  messages: Array<{
    role: unknown;
    content: string;
    content_blocks: unknown[];
  }>;
}

const args = parseArgs(process.argv.slice(2));
const tsCmd = args.ts === undefined ? ["bun", "src/main.ts"] : [args.ts];
const responses = loadCannedResponses(resolvePath(args.response));
if (responses.length < 2) {
  throw new Error(`${args.response} must contain at least two canned responses`);
}

const rust = await runScenario("rust", [args.rust], resolvePath(args.fixture), responses);
const ts = await runScenario("ts", tsCmd, resolvePath(args.fixture), responses);

let failures = 0;
failures += compareSummary(rust.summary, ts.summary);
failures += compareRequests(rust.requests, ts.requests);
failures += compareHistory(rust.history, ts.history);

if (failures > 0) {
  console.error(`\n${failures} tool-loop parity failure(s)`);
  process.exit(1);
}

console.log("\ntool-loop parity ok");

async function runScenario(
  label: string,
  cmd: string[],
  fixtureDir: string,
  responses: CannedLlmResponse[],
): Promise<ScenarioResult> {
  console.log(`-- tool-loop: ${label} --`);
  const proxy = startParityLlmProxy({ response: responses });
  try {
    const { configDir, dataDir } = copyFixtureToTmp(fixtureDir, `shore-tool-loop-${label}-`);
    patchProxyBaseUrl(configDir, proxy.baseUrl);
    const env = buildDaemonEnv({ configDir, dataDir, prefix: `shore-tool-loop-${label}-` });
    env["SHORE_PARITY_ANTHROPIC_KEY"] = "sk-parity";
    env["TZ"] = "UTC";

    const framesSeen: Record<string, unknown>[] = [];
    const proc = spawnDaemon(cmd, env);
    try {
      const addr = await readListenAddr([proc.stdout, proc.stderr]);
      if (!addr) throw new Error(`${label}: daemon never printed listen address`);

      const { sock, frames } = await openConnection(addr);
      framesSeen.push((await readFrame(frames)) as Record<string, unknown>);
      sock.write(JSON.stringify({
        type: "hello",
        client_type: "cli",
        client_name: `tool-loop-parity-${label}`,
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
      await readUntilFinal(label, frames, framesSeen, "tool-loop-1");
      sock.end();
    } catch (e) {
      console.error(`${label} frames before failure:`);
      for (const frame of framesSeen) console.error(`  ${JSON.stringify(frame)}`);
      throw e;
    } finally {
      proc.kill("SIGTERM");
      await proc.exited;
    }

    const restartHistory = await readRestartHistory(label, cmd, env);
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
  label: string,
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
    console.log(`  ${label.padEnd(4)} s2c ${String(frame["type"])}`);
    if (frame["type"] === "error") {
      throw new Error(`${label}: daemon emitted error: ${JSON.stringify(frame)}`);
    }
    if (frame["type"] === "stream_end" && frame["rid"] === rid && frame["is_final"] !== false) {
      return;
    }
  }
  throw new Error(`${label}: timed out waiting for final stream_end`);
}

async function readRestartHistory(
  label: string,
  cmd: string[],
  env: Record<string, string | undefined>,
): Promise<Record<string, unknown>> {
  console.log(`-- tool-loop: ${label} restart --`);
  const proc = spawnDaemon(cmd, env);
  try {
    const addr = await readListenAddr([proc.stdout, proc.stderr]);
    if (!addr) throw new Error(`${label}: restart daemon never printed listen address`);

    const { sock, frames } = await openConnection(addr);
    await readFrame(frames);
    sock.write(JSON.stringify({
      type: "hello",
      client_type: "cli",
      client_name: `tool-loop-parity-${label}-restart`,
      capabilities: ["streaming"],
      character: "scout",
    }) + "\n");
    const history = (await readFrame(frames)) as Record<string, unknown>;
    sock.end();
    if (history["type"] !== "history") {
      throw new Error(`${label}: expected restart history, got ${JSON.stringify(history)}`);
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

function compareSummary(rust: ToolLoopSummary, ts: ToolLoopSummary): number {
  const diffs = compareFrames(
    { type: "tool_loop_summary", ...rust },
    { type: "tool_loop_summary", ...ts },
    {},
  );
  if (diffs.length === 0) {
    console.log("  ok    tool-loop summary");
    return 0;
  }
  console.error("  FAIL  tool-loop summary");
  for (const diff of diffs) console.error(`        ${diff}`);
  console.error(`        rust: ${JSON.stringify(rust)}`);
  console.error(`        ts:   ${JSON.stringify(ts)}`);
  return 1;
}

function compareRequests(rust: CapturedLlmRequest[], ts: CapturedLlmRequest[]): number {
  let failures = 0;
  if (rust.length !== 2 || ts.length !== 2) {
    console.error(`  FAIL  provider request count: rust=${rust.length}, ts=${ts.length}, expected=2`);
    failures++;
  }

  const n = Math.min(rust.length, ts.length);
  for (let i = 0; i < n; i++) {
    const r = rust[i]!;
    const t = ts[i]!;
    if (r.canonical === t.canonical) {
      console.log(`  ok    provider request ${i + 1} (${r.key.slice(0, 12)})`);
      continue;
    }
    failures++;
    console.error(`  FAIL  provider request ${i + 1}`);
    console.error(`        rust key: ${r.key}`);
    console.error(`        ts key:   ${t.key}`);
    console.error(`        rust: ${JSON.stringify(r.body)}`);
    console.error(`        ts:   ${JSON.stringify(t.body)}`);
  }
  return failures;
}

function compareHistory(rust: NormalizedHistory, ts: NormalizedHistory): number {
  const diffs = compareFrames(
    { type: "restart_history", ...rust },
    { type: "restart_history", ...ts },
    {},
  );
  if (diffs.length === 0) {
    console.log("  ok    restart history");
    return 0;
  }
  console.error("  FAIL  restart history");
  for (const diff of diffs) console.error(`        ${diff}`);
  console.error(`        rust: ${JSON.stringify(rust)}`);
  console.error(`        ts:   ${JSON.stringify(ts)}`);
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

function patchProxyBaseUrl(configDir: string, proxyBaseUrl: string): void {
  const configPath = join(configDir, "config.toml");
  const raw = readFileSync(configPath, "utf8");
  writeFileSync(configPath, raw.replaceAll("{{LLM_PROXY_BASE_URL}}", proxyBaseUrl));
}

function parseArgs(argv: string[]): Args {
  const parsed: Args = {
    rust: DEFAULT_RUST,
    ts: undefined,
    fixture: DEFAULT_FIXTURE,
    response: DEFAULT_RESPONSE,
  };

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]!;
    if (arg === "--rust") parsed.rust = takeValue(argv, ++i, arg);
    else if (arg === "--ts") parsed.ts = takeValue(argv, ++i, arg);
    else if (arg === "--fixture") parsed.fixture = takeValue(argv, ++i, arg);
    else if (arg === "--response") parsed.response = takeValue(argv, ++i, arg);
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

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
