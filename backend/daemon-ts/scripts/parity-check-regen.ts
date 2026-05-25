#!/usr/bin/env bun
/**
 * Tier 3 parity check: deterministic regen.
 *
 * Runs the Rust daemon and the TS daemon against the same fixture and canned
 * provider response queue. The first message gets response A, regen gets
 * response B. The check passes only when:
 *
 *   1. both SWP streams expose the same generation and regen summaries;
 *   2. both daemons send the same canonical provider request bodies; and
 *   3. the post-restart persisted history matches, including regen alts.
 *
 * Usage:
 *   bun scripts/parity-check-regen.ts [--rust /usr/bin/shore-daemon] [--ts ./dist/shore-daemon]
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
  setCacheTtl,
  spawnDaemon,
} from "./parity/_lib.ts";
import {
  loadCannedResponses,
  startParityLlmProxy,
  type CannedLlmResponse,
  type CapturedLlmRequest,
} from "./parity/llm-proxy.ts";

const DEFAULT_FIXTURE = "parity-traces/fixtures/regen-basic";
const DEFAULT_RESPONSE = "parity-traces/llm-fixtures/regen-basic.json";
const DEFAULT_RUST = "/usr/bin/shore-daemon";

interface Args {
  rust: string;
  ts: string | undefined;
  fixture: string;
  response: string;
  cacheTtl: string | undefined;
}

interface StreamSummary {
  streamStarts: Array<{ regen: unknown }>;
  textChunks: string[];
  finalContent: string;
  finishReason: unknown;
  tokens: unknown;
  model: unknown;
}

interface ScenarioResult {
  first: StreamSummary;
  regen: StreamSummary;
  history: NormalizedHistory;
  requests: CapturedLlmRequest[];
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

const args = parseArgs(process.argv.slice(2));
const tsCmd = args.ts === undefined ? ["bun", "src/main.ts"] : [args.ts];
const responses = loadCannedResponses(resolvePath(args.response));
if (responses.length < 2) {
  throw new Error(`${args.response} must contain at least two canned responses for regen`);
}

const rust = await runScenario("rust", [args.rust], resolvePath(args.fixture), responses, args.cacheTtl);
const ts = await runScenario("ts", tsCmd, resolvePath(args.fixture), responses, args.cacheTtl);

let failures = 0;
failures += compareSummary("first generation summary", rust.first, ts.first);
failures += compareSummary("regen summary", rust.regen, ts.regen);
failures += compareRequests(rust.requests, ts.requests);
failures += compareHistory(rust.history, ts.history);

if (failures > 0) {
  console.error(`\n${failures} regen parity failure(s)`);
  process.exit(1);
}

console.log("\nregen parity ok");

async function runScenario(
  label: string,
  cmd: string[],
  fixtureDir: string,
  responses: CannedLlmResponse[],
  cacheTtl: string | undefined,
): Promise<ScenarioResult> {
  console.log(`-- regen: ${label} --`);
  const proxy = startParityLlmProxy({ response: responses });
  try {
    const { configDir, dataDir } = copyFixtureToTmp(fixtureDir, `shore-regen-${label}-`);
    patchProxyBaseUrl(configDir, proxy.baseUrl);
    if (cacheTtl !== undefined) setCacheTtl(configDir, cacheTtl);
    const env = buildDaemonEnv({ configDir, dataDir, prefix: `shore-regen-${label}-` });
    env["SHORE_PARITY_ANTHROPIC_KEY"] = "sk-parity";
    env["SHORE_PARITY_OPENAI_KEY"] = "sk-parity";
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
        client_name: `regen-parity-${label}`,
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
      await readUntilFinal(label, frames, framesSeen, "msg-1");

      sock.write(JSON.stringify({
        type: "regen",
        rid: "regen-1",
        stream: true,
      }) + "\n");
      await readUntilFinal(label, frames, framesSeen, "regen-1");
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
  throw new Error(`${label}: timed out waiting for final stream_end (${rid})`);
}

async function readRestartHistory(
  label: string,
  cmd: string[],
  env: Record<string, string | undefined>,
): Promise<Record<string, unknown>> {
  console.log(`-- regen: ${label} restart --`);
  const proc = spawnDaemon(cmd, env);
  try {
    const addr = await readListenAddr([proc.stdout, proc.stderr]);
    if (!addr) throw new Error(`${label}: restart daemon never printed listen address`);

    const { sock, frames } = await openConnection(addr);
    await readFrame(frames);
    sock.write(JSON.stringify({
      type: "hello",
      client_type: "cli",
      client_name: `regen-parity-${label}-restart`,
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

function compareSummary(name: string, rust: StreamSummary, ts: StreamSummary): number {
  const diffs = compareFrames(
    { type: name, ...rust },
    { type: name, ...ts },
    {},
  );
  if (diffs.length === 0) {
    console.log(`  ok    ${name}`);
    return 0;
  }
  console.error(`  FAIL  ${name}`);
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

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
