#!/usr/bin/env bun
/**
 * Tier 3 parity check: autonomous heartbeat message dispatch.
 *
 * The debug heartbeat mutators require an in-memory autonomy state, so the
 * check first sends one deterministic setup user turn. That creates the
 * autonomy state and warms `last_request`; `heartbeat_tick_now` then forces
 * the next ticker pulse to run the heartbeat LLM call. The canned heartbeat
 * response contains a <sendMessage> payload, which should be persisted,
 * broadcast as `origin:"autonomous"`, and delivered through notify-send.
 */

import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
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
  canonicalizeJson,
  loadCannedResponses,
  startParityLlmProxy,
  type CannedLlmResponse,
  type CapturedLlmRequest,
} from "./parity/llm-proxy.ts";

const DEFAULT_FIXTURE = "parity-traces/fixtures/heartbeat-tick";
const DEFAULT_RESPONSE = "parity-traces/llm-fixtures/heartbeat-tick.json";
const DEFAULT_RUST = "/usr/bin/shore-daemon";
const SETUP_RID = "heartbeat-setup";
const TICK_RID = "heartbeat-tick";

interface Args {
  rust: string;
  ts: string | undefined;
  fixture: string;
  response: string;
  cacheTtl: string | undefined;
}

interface GenerationSummary {
  streamStarts: Array<{ regen: unknown }>;
  textChunks: string[];
  finalContent: string;
  finishReason: unknown;
  tokens: unknown;
  model: unknown;
}

interface NormalizedMessage {
  role: unknown;
  content: string;
  content_blocks: unknown[];
}

interface NormalizedFrame {
  type: unknown;
  origin?: unknown;
  character?: unknown;
  role?: unknown;
  content?: string;
  content_blocks?: unknown[];
  messages?: NormalizedMessage[];
}

interface NotifyCall {
  argv: string[];
}

interface ScenarioResult {
  setup: GenerationSummary;
  tickCommand: unknown;
  tickFrames: NormalizedFrame[];
  activeMessages: NormalizedMessage[];
  history: NormalizedMessage[];
  requests: CapturedLlmRequest[];
  notifications: NotifyCall[];
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
failures += compareSetup(rust.setup, ts.setup);
failures += compareTickCommand(rust.tickCommand, ts.tickCommand);
failures += compareTickFrames(rust.tickFrames, ts.tickFrames);
failures += compareRequests(rust.requests, ts.requests);
failures += compareMessages("active.jsonl", rust.activeMessages, ts.activeMessages);
failures += compareMessages("restart history", rust.history, ts.history);
failures += compareNotifications(rust.notifications, ts.notifications);

if (failures > 0) {
  console.error(`\n${failures} heartbeat-tick parity failure(s)`);
  process.exit(1);
}

console.log("\nheartbeat-tick parity ok");

async function runScenario(
  label: string,
  cmd: string[],
  fixtureDir: string,
  responses: CannedLlmResponse[],
  cacheTtl: string | undefined,
): Promise<ScenarioResult> {
  console.log(`-- heartbeat-tick: ${label} --`);
  const proxy = startParityLlmProxy({ response: responses });
  try {
    const { configDir, dataDir } = copyFixtureToTmp(
      fixtureDir,
      `shore-heartbeat-${label}-`,
    );
    patchProxyBaseUrl(configDir, proxy.baseUrl);
    if (cacheTtl !== undefined) setCacheTtl(configDir, cacheTtl);
    const notifyLog = join(
      mkdtempSync(join(tmpdir(), `shore-heartbeat-notify-${label}-`)),
      "notify.jsonl",
    );
    fs.writeFileSync(notifyLog, "");

    const env = buildDaemonEnv({
      configDir,
      dataDir,
      prefix: `shore-heartbeat-${label}-`,
      notifyLog,
    });
    env["SHORE_PARITY_ANTHROPIC_KEY"] = "sk-parity";
    env["TZ"] = "UTC";

    const framesSeen: Record<string, unknown>[] = [];
    const proc = spawnDaemon(cmd, env);
    let setup: GenerationSummary | undefined;
    let tickCommand: unknown;
    let tickFrames: NormalizedFrame[] = [];
    try {
      const addr = await readListenAddr([proc.stdout, proc.stderr]);
      if (!addr) throw new Error(`${label}: daemon never printed listen address`);

      const { sock, frames } = await openConnection(addr);
      framesSeen.push((await readFrame(frames)) as Record<string, unknown>);
      sock.write(JSON.stringify({
        type: "hello",
        client_type: "cli",
        client_name: `heartbeat-parity-${label}`,
        capabilities: ["streaming"],
        character: "scout",
      }) + "\n");
      framesSeen.push((await readFrame(frames)) as Record<string, unknown>);

      sock.write(JSON.stringify({
        type: "message",
        rid: SETUP_RID,
        text: "Please set up heartbeat parity state.",
        stream: true,
      }) + "\n");
      await readUntilFinal(label, frames, framesSeen, SETUP_RID);
      setup = summarizeSetup(framesSeen);

      sock.write(JSON.stringify({
        type: "command",
        rid: TICK_RID,
        name: "heartbeat_tick_now",
        args: {},
      }) + "\n");
      tickCommand = await readUntilCommandOutput(label, frames, framesSeen, TICK_RID);
      tickFrames = await readUntilAutonomousMessage(label, frames, framesSeen);
      await waitForNotifyCalls(label, notifyLog, 1);
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
      setup: setup ?? missingSetup(),
      tickCommand,
      tickFrames,
      activeMessages: readActiveMessages(dataDir),
      history: normalizeHistory(restartHistory),
      requests: [...proxy.requests],
      notifications: readNotifyLog(notifyLog),
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
  throw new Error(`${label}: timed out waiting for setup stream_end`);
}

async function readUntilCommandOutput(
  label: string,
  frames: FrameQueue,
  framesSeen: Record<string, unknown>[],
  rid: string,
): Promise<unknown> {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    const frame = (await readFrame(frames, Math.max(100, deadline - Date.now()))) as Record<
      string,
      unknown
    >;
    framesSeen.push(frame);
    console.log(`  ${label.padEnd(4)} s2c ${String(frame["type"])} (command)`);
    if (frame["type"] === "error") {
      throw new Error(`${label}: daemon emitted error: ${JSON.stringify(frame)}`);
    }
    if (frame["type"] === "command_output" && frame["rid"] === rid) {
      return frame["data"];
    }
  }
  throw new Error(`${label}: timed out waiting for heartbeat_tick_now output`);
}

async function readUntilAutonomousMessage(
  label: string,
  frames: FrameQueue,
  framesSeen: Record<string, unknown>[],
): Promise<NormalizedFrame[]> {
  const tickFrames: Record<string, unknown>[] = [];
  const deadline = Date.now() + 25_000;
  while (Date.now() < deadline) {
    const frame = (await readFrame(frames, Math.max(100, deadline - Date.now()))) as Record<
      string,
      unknown
    >;
    framesSeen.push(frame);
    tickFrames.push(frame);
    console.log(`  ${label.padEnd(4)} s2c ${String(frame["type"])} (tick)`);
    if (frame["type"] === "error") {
      throw new Error(`${label}: daemon emitted error during tick: ${JSON.stringify(frame)}`);
    }
    if (frame["type"] === "new_message" && frame["origin"] === "autonomous") {
      return tickFrames.map(normalizeFrame);
    }
  }
  throw new Error(`${label}: timed out waiting for autonomous new_message`);
}

async function readRestartHistory(
  label: string,
  cmd: string[],
  env: Record<string, string | undefined>,
): Promise<Record<string, unknown>> {
  console.log(`-- heartbeat-tick: ${label} restart --`);
  const proc = spawnDaemon(cmd, env);
  try {
    const addr = await readListenAddr([proc.stdout, proc.stderr]);
    if (!addr) throw new Error(`${label}: restart daemon never printed listen address`);

    const { sock, frames } = await openConnection(addr);
    await readFrame(frames);
    sock.write(JSON.stringify({
      type: "hello",
      client_type: "cli",
      client_name: `heartbeat-parity-${label}-restart`,
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

async function waitForNotifyCalls(label: string, notifyLog: string, count: number): Promise<void> {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    const calls = readNotifyLog(notifyLog);
    if (calls.length >= count) return;
    await Bun.sleep(50);
  }
  throw new Error(`${label}: timed out waiting for ${count} notify-send call(s)`);
}

function summarizeSetup(frames: Record<string, unknown>[]): GenerationSummary {
  const starts = frames
    .filter((f) => f["type"] === "stream_start" && f["rid"] === SETUP_RID)
    .map((f) => ({ regen: f["regen"] }));
  const chunks = frames
    .filter((f) => f["type"] === "stream_chunk" && f["rid"] === SETUP_RID)
    .filter((f) => f["content_type"] === undefined || f["content_type"] === "text")
    .map((f) => String(f["text"] ?? ""));
  const final = frames
    .filter((f) => f["type"] === "stream_end" && f["rid"] === SETUP_RID && f["is_final"] !== false)
    .at(-1);
  if (final === undefined) throw new Error("missing setup stream_end");
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

function missingSetup(): GenerationSummary {
  return {
    streamStarts: [],
    textChunks: [],
    finalContent: "",
    finishReason: null,
    tokens: null,
    model: null,
  };
}

function normalizeFrame(frame: Record<string, unknown>): NormalizedFrame {
  if (frame["type"] === "history") {
    return {
      type: "history",
      messages: normalizeHistory(frame),
    };
  }
  if (frame["type"] === "new_message") {
    const blocks = normalizeBlocks(frame["content_blocks"]);
    return {
      type: "new_message",
      origin: frame["origin"],
      character: frame["character"],
      role: frame["role"],
      content: normalizeContent(frame["content"], blocks),
      content_blocks: blocks,
    };
  }
  return { type: frame["type"] };
}

function readActiveMessages(dataDir: string): NormalizedMessage[] {
  const active = fs.readFileSync(join(dataDir, "scout", "active.jsonl"), "utf8");
  return active
    .split("\n")
    .filter((line) => line.trim().length > 0)
    .map((line) => normalizeMessage(JSON.parse(line) as Record<string, unknown>));
}

function normalizeHistory(frame: Record<string, unknown>): NormalizedMessage[] {
  const messages = Array.isArray(frame["messages"]) ? frame["messages"] : [];
  return messages.filter(isObject).map(normalizeMessage);
}

function normalizeMessage(msg: Record<string, unknown>): NormalizedMessage {
  const blocks = normalizeBlocks(msg["content_blocks"]);
  return {
    role: msg["role"],
    content: normalizeContent(msg["content"], blocks),
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

function compareSetup(rust: GenerationSummary, ts: GenerationSummary): number {
  const diffs = compareFrames(
    { type: "setup_summary", ...rust },
    { type: "setup_summary", ...ts },
    {},
  );
  if (diffs.length === 0) {
    console.log("  ok    setup generation summary");
    return 0;
  }
  console.error("  FAIL  setup generation summary");
  for (const diff of diffs) console.error(`        ${diff}`);
  console.error(`        rust: ${JSON.stringify(rust)}`);
  console.error(`        ts:   ${JSON.stringify(ts)}`);
  return 1;
}

function compareTickCommand(rust: unknown, ts: unknown): number {
  const diffs = compareFrames(
    { type: "tick_command", data: rust },
    { type: "tick_command", data: ts },
    {},
  );
  if (diffs.length === 0) {
    console.log("  ok    heartbeat_tick_now command output");
    return 0;
  }
  console.error("  FAIL  heartbeat_tick_now command output");
  for (const diff of diffs) console.error(`        ${diff}`);
  console.error(`        rust: ${JSON.stringify(rust)}`);
  console.error(`        ts:   ${JSON.stringify(ts)}`);
  return 1;
}

function compareTickFrames(rust: NormalizedFrame[], ts: NormalizedFrame[]): number {
  const diffs = compareFrames(
    { type: "tick_frames", frames: rust },
    { type: "tick_frames", frames: ts },
    {},
  );
  if (diffs.length === 0) {
    console.log("  ok    autonomous SWP frames");
    return 0;
  }
  console.error("  FAIL  autonomous SWP frames");
  for (const diff of diffs) console.error(`        ${diff}`);
  console.error(`        rust: ${JSON.stringify(rust)}`);
  console.error(`        ts:   ${JSON.stringify(ts)}`);
  return 1;
}

function compareRequests(rust: CapturedLlmRequest[], ts: CapturedLlmRequest[]): number {
  let failures = 0;
  failures += compareRequestAt(0, "provider request 1 / setup chat", rust, ts, false);
  failures += compareRequestAt(1, "provider request 2 / heartbeat call", rust, ts, true);
  if (rust.length !== 2 || ts.length !== 2) {
    console.error(`  FAIL  provider request count: rust=${rust.length}, ts=${ts.length}, expected 2 each`);
    failures += 1;
  }
  return failures;
}

function compareRequestAt(
  index: number,
  label: string,
  rust: CapturedLlmRequest[],
  ts: CapturedLlmRequest[],
  normalizeTime: boolean,
): number {
  const r = rust[index];
  const t = ts[index];
  if (r === undefined || t === undefined) {
    console.error(`  FAIL  ${label} missing`);
    return 1;
  }
  const rCanonical = normalizeTime ? normalizedRequestCanonical(r) : r.canonical;
  const tCanonical = normalizeTime ? normalizedRequestCanonical(t) : t.canonical;
  if (rCanonical === tCanonical) {
    const note = normalizeTime ? " (current-time fuzzy)" : "";
    console.log(`  ok    ${label}${note} (${r.key.slice(0, 12)} / ${t.key.slice(0, 12)})`);
    return 0;
  }
  console.error(`  FAIL  ${label}`);
  console.error(`        rust key: ${r.key}`);
  console.error(`        ts key:   ${t.key}`);
  console.error(`        rust: ${JSON.stringify(r.body)}`);
  console.error(`        ts:   ${JSON.stringify(t.body)}`);
  return 1;
}

function normalizedRequestCanonical(req: CapturedLlmRequest): string {
  return [
    req.method.toUpperCase(),
    req.path,
    canonicalizeJson(normalizeHeartbeatCurrentTime(req.body)),
  ].join("\n");
}

function normalizeHeartbeatCurrentTime(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(normalizeHeartbeatCurrentTime);
  if (value === null || typeof value !== "object") {
    if (typeof value !== "string") return value;
    return value.replace(/\[Current time: [^\]]+\]/g, "[Current time: <dynamic>]");
  }
  const out: Record<string, unknown> = {};
  for (const [key, child] of Object.entries(value as Record<string, unknown>)) {
    out[key] = normalizeHeartbeatCurrentTime(child);
  }
  return out;
}

function compareMessages(label: string, rust: NormalizedMessage[], ts: NormalizedMessage[]): number {
  const diffs = compareFrames(
    { type: "messages", messages: rust },
    { type: "messages", messages: ts },
    {},
  );
  if (diffs.length === 0) {
    console.log(`  ok    ${label}`);
    return 0;
  }
  console.error(`  FAIL  ${label}`);
  for (const diff of diffs) console.error(`        ${diff}`);
  console.error(`        rust: ${JSON.stringify(rust)}`);
  console.error(`        ts:   ${JSON.stringify(ts)}`);
  return 1;
}

function readNotifyLog(path: string): NotifyCall[] {
  let raw: string;
  try {
    raw = fs.readFileSync(path, "utf8");
  } catch {
    return [];
  }
  return raw
    .split("\n")
    .filter((line) => line.trim().length > 0)
    .map((line) => JSON.parse(line) as NotifyCall);
}

function compareNotifications(rust: NotifyCall[], ts: NotifyCall[]): number {
  const diffs = compareFrames(
    { type: "notify_log", calls: rust },
    { type: "notify_log", calls: ts },
    {},
  );
  if (diffs.length === 0) {
    console.log(`  ok    notify-send calls (${rust.length} per daemon)`);
    return 0;
  }
  console.error("  FAIL  notify-send calls");
  for (const diff of diffs) console.error(`        ${diff}`);
  console.error(`        rust: ${JSON.stringify(rust)}`);
  console.error(`        ts:   ${JSON.stringify(ts)}`);
  return 1;
}

function patchProxyBaseUrl(configDir: string, proxyBaseUrl: string): void {
  const configPath = path.join(configDir, "config.toml");
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

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
