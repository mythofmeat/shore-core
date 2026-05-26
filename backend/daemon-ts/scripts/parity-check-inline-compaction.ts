#!/usr/bin/env bun
/**
 * Tier 3 parity check: inline compaction triggered post-generation.
 *
 * Seeded fixture commits two user turns to `active.jsonl`; the client
 * sends a third user message, the chat call returns a canned Anthropic
 * reply, and the post-generation `should_compact_now` gate fires
 * (max_turns=3, min_turns=2). The compaction LLM call returns canned
 * memory writes; the daemon should write the same memory files,
 * archive the compacted slice into `segments/0001.jsonl`, and truncate
 * `active.jsonl` to the retained tail.
 *
 * Frozen-baseline mode compares the TS daemon's SWP frames, both
 * provider request bodies (chat + compaction), post-restart history,
 * post-compaction on-disk state, and notify-send calls against a
 * committed baseline JSON.
 */

import fs from "node:fs";
import { mkdtempSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
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

const DEFAULT_FIXTURE = "parity-traces/fixtures/inline-compaction";
const DEFAULT_RESPONSE = "parity-traces/llm-fixtures/inline-compaction.json";

interface Args {
  ts: string | undefined;
  fixture: string;
  response: string;
  cacheTtl: string | undefined;
  baseline: string | undefined;
  writeBaseline: string | undefined;
}

interface ChatSummary {
  streamStarts: Array<{ regen: unknown }>;
  textChunks: string[];
  streamEnds: Array<{
    content: string;
    finishReason: unknown;
    isFinal: unknown;
    tokens: unknown;
    model: unknown;
  }>;
  phases: Array<{ phase: unknown; model: unknown }>;
}

interface Snapshot {
  activeJsonl: string;
  compactionJson: unknown;
  segment0001: string;
  memoryPeopleParityUser: string;
  memoryIndex: string;
}

interface NormalizedHistory {
  messages: Array<{
    role: unknown;
    content: string;
    content_blocks: unknown[];
  }>;
}

interface NotifyCall {
  argv: string[];
}

interface FrozenRequest {
  method: string;
  path: string;
  body: unknown;
}

interface FrozenInlineCompactionBaseline {
  version: 1;
  mode: "inline-compaction";
  fixture: string;
  response: string;
  cacheTtl: string | null;
  summary: ChatSummary;
  providerRequests: FrozenRequest[];
  snapshot: Snapshot;
  history: NormalizedHistory;
  notifications: NotifyCall[];
}

const args = parseArgs(process.argv.slice(2));
if (args.baseline === undefined && args.writeBaseline === undefined) {
  console.error("usage: parity-check-inline-compaction.ts --baseline <path> | --write-baseline <path>");
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
    mode: "inline-compaction",
    fixture: args.fixture,
    response: args.response,
    cacheTtl: args.cacheTtl ?? null,
    summary: result.summary,
    providerRequests: result.requests.map((r) => ({
      method: r.method,
      path: r.path,
      body: redactHeartbeatMarkers(r.body),
    })),
    snapshot: result.snapshot,
    history: result.history,
    notifications: result.notifications,
  });
  console.log(`\nwrote inline-compaction baseline: ${args.writeBaseline}`);
} else {
  const baseline = readFrozenBaseline(resolvePath(args.baseline!));
  let failures = 0;
  failures += compareSummary(baseline.summary, result.summary);
  failures += compareRequests(baseline.providerRequests, result.requests);
  failures += compareSnapshot(baseline.snapshot, result.snapshot);
  failures += compareHistory(baseline.history, result.history);
  failures += compareNotifications(baseline.notifications, result.notifications);

  if (failures > 0) {
    console.error(`\n${failures} inline-compaction parity failure(s)`);
    process.exit(1);
  }
  console.log("\ninline-compaction parity ok");
}

async function runScenario(
  cmd: string[],
  fixtureDir: string,
  responses: CannedLlmResponse[],
  cacheTtl: string | undefined,
): Promise<{
  summary: ChatSummary;
  snapshot: Snapshot;
  history: NormalizedHistory;
  requests: CapturedLlmRequest[];
  notifications: NotifyCall[];
}> {
  console.log("-- inline-compaction: ts --");
  const proxy = startParityLlmProxy({ response: responses });
  try {
    const { configDir, dataDir } = copyFixtureToTmp(fixtureDir, "shore-compaction-ts-");
    patchProxyBaseUrl(configDir, proxy.baseUrl);
    if (cacheTtl !== undefined) setCacheTtl(configDir, cacheTtl);
    const notifyLog = join(
      mkdtempSync(join(tmpdir(), "shore-compaction-notify-ts-")),
      "notify.jsonl",
    );
    fs.writeFileSync(notifyLog, "");
    const env = buildDaemonEnv({
      configDir,
      dataDir,
      prefix: "shore-compaction-ts-",
      notifyLog,
    });
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
        client_name: "compaction-parity-ts",
        capabilities: ["streaming"],
        character: "scout",
      }) + "\n");
      framesSeen.push((await readFrame(frames)) as Record<string, unknown>);

      sock.write(JSON.stringify({
        type: "message",
        rid: "compact-1",
        text: "third user message",
        stream: true,
      }) + "\n");
      await readUntilFinal(frames, framesSeen, "compact-1");
      await drainPhaseFrame(frames, framesSeen);
      await waitForCompactionArtifacts(dataDir);
      sock.end();
    } catch (e) {
      console.error("ts frames before failure:");
      for (const frame of framesSeen) console.error(`  ${JSON.stringify(frame)}`);
      throw e;
    } finally {
      proc.kill("SIGTERM");
      await proc.exited;
    }

    const snapshot = readSnapshot(dataDir, configDir);
    const restartHistory = await readRestartHistory(cmd, env);
    const notifications = readNotifyLog(notifyLog);
    return {
      summary: summarize(framesSeen, "compact-1"),
      snapshot,
      history: normalizeHistory(restartHistory),
      requests: [...proxy.requests],
      notifications,
    };
  } finally {
    await proxy.stop();
  }
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
    .filter((l) => l.trim().length > 0)
    .map((l) => JSON.parse(l) as NotifyCall);
}

function compareNotifications(expected: NotifyCall[], actual: NotifyCall[]): number {
  const diffs = compareFrames(
    { type: "notify_log", calls: expected },
    { type: "notify_log", calls: actual },
    {},
  );
  if (diffs.length === 0) {
    console.log(`  ok    notify-send calls (${expected.length})`);
    return 0;
  }
  console.error("  FAIL  notify-send calls");
  for (const diff of diffs) console.error(`        ${diff}`);
  console.error(`        expected: ${JSON.stringify(expected)}`);
  console.error(`        actual:   ${JSON.stringify(actual)}`);
  return 1;
}

async function readUntilFinal(
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

async function drainPhaseFrame(
  frames: FrameQueue,
  framesSeen: Record<string, unknown>[],
): Promise<void> {
  const deadline = Date.now() + 3000;
  while (Date.now() < deadline) {
    try {
      const frame = (await readFrame(frames, Math.max(50, deadline - Date.now()))) as Record<
        string,
        unknown
      >;
      framesSeen.push(frame);
      console.log(`  ts   s2c ${String(frame["type"])} (post-stream)`);
      if (frame["type"] === "phase") return;
    } catch {
      return;
    }
  }
}

async function waitForCompactionArtifacts(dataDir: string): Promise<void> {
  const segmentPath = join(dataDir, "scout", "segments", "0001.jsonl");
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    if (fs.existsSync(segmentPath)) {
      console.log("  ts   compaction segment present");
      return;
    }
    await Bun.sleep(50);
  }
  throw new Error(`ts: timed out waiting for ${segmentPath}`);
}

async function readRestartHistory(
  cmd: string[],
  env: Record<string, string | undefined>,
): Promise<Record<string, unknown>> {
  console.log("-- inline-compaction: ts restart --");
  const proc = spawnDaemon(cmd, env);
  try {
    const addr = await readListenAddr([proc.stdout, proc.stderr]);
    if (!addr) throw new Error("ts: restart daemon never printed listen address");

    const { sock, frames } = await openConnection(addr);
    await readFrame(frames);
    sock.write(JSON.stringify({
      type: "hello",
      client_type: "cli",
      client_name: "compaction-parity-ts-restart",
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

function readSnapshot(dataDir: string, configDir: string): Snapshot {
  const charDir = join(dataDir, "scout");
  const workspace = join(configDir, "characters", "scout", "workspace");
  const compactionJsonRaw = fs.readFileSync(join(charDir, "compaction.json"), "utf8");
  return {
    activeJsonl: fs.readFileSync(join(charDir, "active.jsonl"), "utf8"),
    compactionJson: JSON.parse(compactionJsonRaw),
    segment0001: fs.readFileSync(join(charDir, "segments", "0001.jsonl"), "utf8"),
    memoryPeopleParityUser: fs.readFileSync(
      join(workspace, "memory", "people", "parity-user.md"),
      "utf8",
    ),
    memoryIndex: fs.readFileSync(join(workspace, "MEMORY.md"), "utf8"),
  };
}

function summarize(frames: Record<string, unknown>[], rid: string): ChatSummary {
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
    phases: frames
      .filter((f) => f["type"] === "phase")
      .map((f) => ({ phase: f["phase"], model: f["model"] })),
  };
}

function compareSummary(expected: ChatSummary, actual: ChatSummary): number {
  const diffs = compareFrames(
    { type: "chat_summary", ...expected },
    { type: "chat_summary", ...actual },
    {},
  );
  if (diffs.length === 0) {
    console.log("  ok    chat summary + phase frames");
    return 0;
  }
  console.error("  FAIL  chat summary + phase frames");
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
      const label = i === 0 ? "chat call" : "compaction call";
      console.log(`  ok    provider request ${i + 1} / ${label} (${a.key.slice(0, 12)})`);
    } else {
      console.error(`  FAIL  provider request ${i + 1} body`);
      console.error(`        expected: ${expectedBody}`);
      console.error(`        actual:   ${actualBody}`);
      failures++;
    }
  }
  return failures;
}

function compareSnapshot(expected: Snapshot, actual: Snapshot): number {
  let failures = 0;
  failures += compareJsonl(
    "active.jsonl",
    expected.activeJsonl,
    actual.activeJsonl,
    ["messages[*].msg_id", "messages[*].timestamp"],
  );
  failures += compareText("segments/0001.jsonl", expected.segment0001, actual.segment0001);
  failures += compareText(
    "memory/people/parity-user.md",
    expected.memoryPeopleParityUser,
    actual.memoryPeopleParityUser,
  );
  failures += compareText("MEMORY.md", expected.memoryIndex, actual.memoryIndex);

  const diffs = compareFrames(
    { type: "compaction_json", value: expected.compactionJson },
    { type: "compaction_json", value: actual.compactionJson },
    { compaction_json: ["value.segments[*].compacted_at"] },
  );
  if (diffs.length === 0) {
    console.log("  ok    compaction.json (compacted_at fuzzy)");
  } else {
    failures += 1;
    console.error("  FAIL  compaction.json");
    for (const diff of diffs) console.error(`        ${diff}`);
    console.error(`        expected: ${JSON.stringify(expected.compactionJson)}`);
    console.error(`        actual:   ${JSON.stringify(actual.compactionJson)}`);
  }
  return failures;
}

function compareJsonl(
  label: string,
  expected: string,
  actual: string,
  fuzzyPaths: string[],
): number {
  const parseLines = (raw: string): unknown[] =>
    raw
      .split("\n")
      .filter((l) => l.trim().length > 0)
      .map((l) => JSON.parse(l));
  const diffs = compareFrames(
    { type: "jsonl", messages: parseLines(expected) },
    { type: "jsonl", messages: parseLines(actual) },
    { jsonl: fuzzyPaths },
  );
  if (diffs.length === 0) {
    console.log(`  ok    ${label}${fuzzyPaths.length > 0 ? ` (fuzzy: ${fuzzyPaths.join(", ")})` : ""}`);
    return 0;
  }
  console.error(`  FAIL  ${label}`);
  for (const diff of diffs) console.error(`        ${diff}`);
  console.error(`        expected:\n${indent(expected)}`);
  console.error(`        actual:\n${indent(actual)}`);
  return 1;
}

function compareText(label: string, expected: string, actual: string): number {
  if (expected === actual) {
    console.log(`  ok    ${label}`);
    return 0;
  }
  console.error(`  FAIL  ${label}`);
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

function readFrozenBaseline(path: string): FrozenInlineCompactionBaseline {
  const parsed = JSON.parse(readFileSyncStr(path)) as FrozenInlineCompactionBaseline;
  if (parsed.version !== 1 || parsed.mode !== "inline-compaction") {
    throw new Error(`${path}: unsupported inline-compaction baseline`);
  }
  return parsed;
}

function writeFrozenBaseline(path: string, baseline: FrozenInlineCompactionBaseline): void {
  mkdirSync(dirname(path), { recursive: true });
  fs.writeFileSync(path, JSON.stringify(baseline, null, 2) + "\n");
}

function readFileSyncStr(path: string): string {
  return fs.readFileSync(path, "utf8");
}

function patchProxyBaseUrl(configDir: string, proxyBaseUrl: string): void {
  const configPath = path.join(configDir, "config.toml");
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

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
