#!/usr/bin/env bun
/**
 * Tier 3 parity check: inline compaction triggered post-generation.
 *
 * Seeded fixture commits two user turns to `active.jsonl`; the client
 * sends a third user message, the chat call returns a canned Anthropic
 * reply, and the post-generation `should_compact_now` gate fires
 * (max_turns=3, min_turns=2). The compaction LLM call returns canned
 * memory writes; both daemons should then write the same memory files,
 * archive the compacted slice into `segments/0001.jsonl`, and truncate
 * `active.jsonl` to the retained tail.
 *
 * Scope (matches the "inline compaction trigger end-to-end" item in
 * `docs/DAEMON_TS_PARITY.md`):
 *
 * - Strict diff: chat-call SWP frames (stream_start/chunk/end +
 *   `phase{compacting}`), first provider request body, post-restart
 *   history, post-compaction on-disk state.
 * - **Not** asserted: the compaction-call provider request body. Rust's
 *   `RealCompactionLlm` rebuilds the request from the cached chat
 *   prefix; TS plumbs the cached request through but
 *   `RealCompactionLlm.summarize` ignores it (audit #12). The bodies
 *   diverge by design today. Both are written to
 *   `/tmp/parity-compaction-<rust|ts>-req2.json` so the audit #12 pin
 *   has a concrete diff to start from when it lands.
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
  loadCannedResponses,
  startParityLlmProxy,
  type CannedLlmResponse,
  type CapturedLlmRequest,
} from "./parity/llm-proxy.ts";

const DEFAULT_FIXTURE = "parity-traces/fixtures/inline-compaction";
const DEFAULT_RESPONSE = "parity-traces/llm-fixtures/inline-compaction.json";
const DEFAULT_RUST = "/usr/bin/shore-daemon";

interface Args {
  rust: string;
  ts: string | undefined;
  fixture: string;
  response: string;
  cacheTtl: string | undefined;
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

interface ScenarioResult {
  summary: ChatSummary;
  snapshot: Snapshot;
  history: NormalizedHistory;
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
failures += compareSummary(rust.summary, ts.summary);
failures += compareChatRequest(rust.requests, ts.requests);
captureCompactionRequest(rust.requests, ts.requests);
failures += compareSnapshot(rust.snapshot, ts.snapshot);
failures += compareHistory(rust.history, ts.history);
failures += compareNotifications(rust.notifications, ts.notifications);

if (failures > 0) {
  console.error(`\n${failures} inline-compaction parity failure(s)`);
  process.exit(1);
}

console.log("\ninline-compaction parity ok");

async function runScenario(
  label: string,
  cmd: string[],
  fixtureDir: string,
  responses: CannedLlmResponse[],
  cacheTtl: string | undefined,
): Promise<ScenarioResult> {
  console.log(`-- inline-compaction: ${label} --`);
  const proxy = startParityLlmProxy({ response: responses });
  try {
    const { configDir, dataDir } = copyFixtureToTmp(
      fixtureDir,
      `shore-compaction-${label}-`,
    );
    patchProxyBaseUrl(configDir, proxy.baseUrl);
    if (cacheTtl !== undefined) setCacheTtl(configDir, cacheTtl);
    // Fresh notify-log per scenario — each daemon writes via the shim
    // PATH-installed by buildDaemonEnv. Compaction completion fires
    // a `notify-send --app-name=shore <title> <body>` on both daemons.
    const notifyLog = join(
      mkdtempSync(join(tmpdir(), `shore-compaction-notify-${label}-`)),
      "notify.jsonl",
    );
    fs.writeFileSync(notifyLog, "");
    const env = buildDaemonEnv({
      configDir,
      dataDir,
      prefix: `shore-compaction-${label}-`,
      notifyLog,
    });
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
        client_name: `compaction-parity-${label}`,
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
      await readUntilFinal(label, frames, framesSeen, "compact-1");
      await drainPhaseFrame(label, frames, framesSeen);
      await waitForCompactionArtifacts(label, dataDir);
      sock.end();
    } catch (e) {
      console.error(`${label} frames before failure:`);
      for (const frame of framesSeen) console.error(`  ${JSON.stringify(frame)}`);
      throw e;
    } finally {
      proc.kill("SIGTERM");
      await proc.exited;
    }

    const snapshot = readSnapshot(dataDir, configDir);
    const restartHistory = await readRestartHistory(label, cmd, env);
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

function compareNotifications(rust: NotifyCall[], ts: NotifyCall[]): number {
  // `notify-send` is fire-and-forget on both daemons; both call out
  // with `--app-name=shore <title> <body>`. The expected outcome of
  // the inline-compaction scenario is exactly one "compaction
  // complete" call on each side. Body text differences are real
  // parity issues (different summaries to the user).
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
  throw new Error(`${label}: timed out waiting for final stream_end`);
}

/**
 * Read frames until we see `phase{compacting}` or a short grace window
 * elapses. Post-`stream_end`, the daemon also emits the assistant's
 * `new_message` from the broadcast bus; the order vs the inline-
 * compaction task's `phase` emission is racy, so we have to drain until
 * we find phase rather than just reading one frame.
 *
 * Missing the phase entirely is a real parity failure — `compareSummary`
 * surfaces it deterministically since both daemons consistently emit it.
 */
async function drainPhaseFrame(
  label: string,
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
      console.log(`  ${label.padEnd(4)} s2c ${String(frame["type"])} (post-stream)`);
      if (frame["type"] === "phase") return;
    } catch {
      return;
    }
  }
}

/**
 * Poll the daemon's data root for the post-compaction segment file.
 * Compaction runs in a background task so there's no SWP "done" frame;
 * `segments/0001.jsonl` is the deterministic on-disk signal.
 */
async function waitForCompactionArtifacts(label: string, dataDir: string): Promise<void> {
  const segmentPath = join(dataDir, "scout", "segments", "0001.jsonl");
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    if (fs.existsSync(segmentPath)) {
      console.log(`  ${label.padEnd(4)} compaction segment present`);
      return;
    }
    await Bun.sleep(50);
  }
  throw new Error(`${label}: timed out waiting for ${segmentPath}`);
}

async function readRestartHistory(
  label: string,
  cmd: string[],
  env: Record<string, string | undefined>,
): Promise<Record<string, unknown>> {
  console.log(`-- inline-compaction: ${label} restart --`);
  const proc = spawnDaemon(cmd, env);
  try {
    const addr = await readListenAddr([proc.stdout, proc.stderr]);
    if (!addr) throw new Error(`${label}: restart daemon never printed listen address`);

    const { sock, frames } = await openConnection(addr);
    await readFrame(frames);
    sock.write(JSON.stringify({
      type: "hello",
      client_type: "cli",
      client_name: `compaction-parity-${label}-restart`,
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

function compareSummary(rust: ChatSummary, ts: ChatSummary): number {
  const diffs = compareFrames(
    { type: "chat_summary", ...rust },
    { type: "chat_summary", ...ts },
    {},
  );
  if (diffs.length === 0) {
    console.log("  ok    chat summary + phase frames");
    return 0;
  }
  console.error("  FAIL  chat summary + phase frames");
  for (const diff of diffs) console.error(`        ${diff}`);
  console.error(`        rust: ${JSON.stringify(rust)}`);
  console.error(`        ts:   ${JSON.stringify(ts)}`);
  return 1;
}

function compareChatRequest(rust: CapturedLlmRequest[], ts: CapturedLlmRequest[]): number {
  if (rust.length < 1 || ts.length < 1) {
    console.error(`  FAIL  provider request 1 missing: rust=${rust.length}, ts=${ts.length}`);
    return 1;
  }
  const r = rust[0]!;
  const t = ts[0]!;
  if (r.canonical === t.canonical) {
    console.log(`  ok    provider request 1 / chat call (${r.key.slice(0, 12)})`);
    return 0;
  }
  console.error("  FAIL  provider request 1 / chat call");
  console.error(`        rust key: ${r.key}`);
  console.error(`        ts key:   ${t.key}`);
  console.error(`        rust: ${JSON.stringify(r.body)}`);
  console.error(`        ts:   ${JSON.stringify(t.body)}`);
  return 1;
}

/**
 * Save compaction-call bodies to /tmp for forensic diffing without
 * asserting on them. As of 2026-05-25 the only known remaining
 * divergence is the trailing user message's content form (Rust string
 * vs TS single-element array) — see DAEMON_TS_PARITY.md "Known
 * divergences" → "Compaction trailer content form". Lifting this to
 * an assertion needs the bundled live-API breakpoint-placement
 * gate; until then, capture the bodies for forensics.
 */
function captureCompactionRequest(
  rust: CapturedLlmRequest[],
  ts: CapturedLlmRequest[],
): void {
  const rustPath = "/tmp/parity-compaction-rust-req2.json";
  const tsPath = "/tmp/parity-compaction-ts-req2.json";
  if (rust.length < 2 || ts.length < 2) {
    console.log(
      `  note  provider request 2 missing: rust=${rust.length}, ts=${ts.length}` +
        " (expected — only captured when compaction actually fires; skip)",
    );
    return;
  }
  fs.writeFileSync(rustPath, JSON.stringify(rust[1]!.body, null, 2) + "\n");
  fs.writeFileSync(tsPath, JSON.stringify(ts[1]!.body, null, 2) + "\n");
  const match = rust[1]!.canonical === ts[1]!.canonical;
  console.log(
    `  note  provider request 2 / compaction call ${
      match
        ? "matches"
        : "diverges (known: trailing-user content form, see DAEMON_TS_PARITY.md)"
    }: wrote ${rustPath} + ${tsPath}`,
  );
}

function compareSnapshot(rust: Snapshot, ts: Snapshot): number {
  let failures = 0;
  // The retained tail of active.jsonl is the live chat turn that just
  // ran — it carries fresh msg_ids and timestamps that legitimately
  // diverge, so compare structurally with the same fuzzy paths the
  // restart-history diff uses.
  failures += compareJsonl(
    "active.jsonl",
    rust.activeJsonl,
    ts.activeJsonl,
    ["messages[*].msg_id", "messages[*].timestamp"],
  );
  failures += compareText("segments/0001.jsonl", rust.segment0001, ts.segment0001);
  failures += compareText(
    "memory/people/parity-user.md",
    rust.memoryPeopleParityUser,
    ts.memoryPeopleParityUser,
  );
  failures += compareText("MEMORY.md", rust.memoryIndex, ts.memoryIndex);

  const diffs = compareFrames(
    { type: "compaction_json", value: rust.compactionJson },
    { type: "compaction_json", value: ts.compactionJson },
    { compaction_json: ["value.segments[*].compacted_at"] },
  );
  if (diffs.length === 0) {
    console.log("  ok    compaction.json (compacted_at fuzzy)");
  } else {
    failures += 1;
    console.error("  FAIL  compaction.json");
    for (const diff of diffs) console.error(`        ${diff}`);
    console.error(`        rust: ${JSON.stringify(rust.compactionJson)}`);
    console.error(`        ts:   ${JSON.stringify(ts.compactionJson)}`);
  }
  return failures;
}

function compareJsonl(
  label: string,
  rust: string,
  ts: string,
  fuzzyPaths: string[],
): number {
  const parseLines = (raw: string): unknown[] =>
    raw
      .split("\n")
      .filter((l) => l.trim().length > 0)
      .map((l) => JSON.parse(l));
  const diffs = compareFrames(
    { type: "jsonl", messages: parseLines(rust) },
    { type: "jsonl", messages: parseLines(ts) },
    { jsonl: fuzzyPaths },
  );
  if (diffs.length === 0) {
    console.log(`  ok    ${label}${fuzzyPaths.length > 0 ? ` (fuzzy: ${fuzzyPaths.join(", ")})` : ""}`);
    return 0;
  }
  console.error(`  FAIL  ${label}`);
  for (const diff of diffs) console.error(`        ${diff}`);
  console.error(`        rust:\n${indent(rust)}`);
  console.error(`        ts:\n${indent(ts)}`);
  return 1;
}

function compareText(label: string, rust: string, ts: string): number {
  if (rust === ts) {
    console.log(`  ok    ${label}`);
    return 0;
  }
  console.error(`  FAIL  ${label}`);
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
