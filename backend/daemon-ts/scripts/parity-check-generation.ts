#!/usr/bin/env bun
/**
 * Tier 3 parity check: one deterministic LLM generation.
 *
 * Runs the Rust daemon and the TS daemon against the same fixture and the
 * same in-process LLM proxy. The proxy returns a canned provider SSE stream
 * and captures each daemon's canonical provider request. The check passes only
 * when:
 *
 *   1. both SWP streams expose the same assistant text/tokens/finish reason;
 *   2. both daemons made exactly one provider request; and
 *   3. those provider request bodies are byte-identical after JSON key sorting.
 *
 * Usage:
 *   bun scripts/parity-check-generation.ts [--rust /usr/bin/shore-daemon] [--ts ./dist/shore-daemon]
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
  loadCannedResponse,
  startParityLlmProxy,
  type CapturedLlmRequest,
} from "./parity/llm-proxy.ts";

const DEFAULT_FIXTURE = "parity-traces/fixtures/generation-basic";
const DEFAULT_RESPONSE = "parity-traces/llm-fixtures/generation-basic.json";
const DEFAULT_RUST = "/usr/bin/shore-daemon";

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

const args = parseArgs(process.argv.slice(2));
const tsCmd = args.ts === undefined ? ["bun", "src/main.ts"] : [args.ts];
const response = loadCannedResponse(resolvePath(args.response));
const proxy = startParityLlmProxy({ response });

try {
  const rust = await runScenario("rust", [args.rust], resolvePath(args.fixture), proxy.baseUrl, args.cacheTtl);
  const ts = await runScenario("ts", tsCmd, resolvePath(args.fixture), proxy.baseUrl, args.cacheTtl);

  let failures = 0;
  failures += compareSummary("generation summary", rust.summary, ts.summary);
  failures += compareRequests(proxy.requests);

  if (failures > 0) {
    console.error(`\n${failures} generation parity failure(s)`);
    process.exit(1);
  }

  console.log("\ngeneration parity ok");
} finally {
  await proxy.stop();
}

async function runScenario(
  label: string,
  cmd: string[],
  fixtureDir: string,
  proxyBaseUrl: string,
  cacheTtl: string | undefined,
): Promise<{ summary: GenerationSummary; frames: Record<string, unknown>[] }> {
  console.log(`-- generation: ${label} --`);
  const { configDir, dataDir } = copyFixtureToTmp(fixtureDir, `shore-gen-${label}-`);
  patchProxyBaseUrl(configDir, proxyBaseUrl);
  if (cacheTtl !== undefined) setCacheTtl(configDir, cacheTtl);
  const env = buildDaemonEnv({ configDir, dataDir, prefix: `shore-gen-${label}-` });
  env["SHORE_PARITY_ANTHROPIC_KEY"] = "sk-parity";
  env["SHORE_PARITY_OPENAI_KEY"] = "sk-parity";
  env["TZ"] = "UTC";

  const proc = spawnDaemon(cmd, env);
  const framesSeen: Record<string, unknown>[] = [];
  try {
    const addr = await readListenAddr([proc.stdout, proc.stderr]);
    if (!addr) throw new Error(`${label}: daemon never printed listen address`);

    const { sock, frames } = await openConnection(addr);

    framesSeen.push((await readFrame(frames)) as Record<string, unknown>);
    const hello = {
      type: "hello",
      client_type: "cli",
      client_name: `generation-parity-${label}`,
      capabilities: ["streaming"],
      character: "scout",
    };
    sock.write(JSON.stringify(hello) + "\n");
    framesSeen.push((await readFrame(frames)) as Record<string, unknown>);

    const msg = {
      type: "message",
      rid: "r1",
      text: "Please reply with the parity fixture response.",
      stream: true,
    };
    sock.write(JSON.stringify(msg) + "\n");

    const deadline = Date.now() + 15_000;
    while (Date.now() < deadline) {
      const frame = (await readFrame(frames, Math.max(100, deadline - Date.now()))) as Record<
        string,
        unknown
      >;
      framesSeen.push(frame);
      console.log(`  ${label.padEnd(4)} s2c ${String(frame["type"])}`);
      if (frame["type"] === "stream_end" && frame["rid"] === "r1" && frame["is_final"] !== false) {
        sock.end();
        return { summary: summarize(framesSeen), frames: framesSeen };
      }
      if (frame["type"] === "error") {
        throw new Error(`${label}: daemon emitted error: ${JSON.stringify(frame)}`);
      }
    }

    throw new Error(`${label}: timed out waiting for final stream_end`);
  } catch (e) {
    console.error(`${label} frames before failure:`);
    for (const frame of framesSeen) console.error(`  ${JSON.stringify(frame)}`);
    throw e;
  } finally {
    proc.kill("SIGTERM");
    await proc.exited;
  }
}

function summarize(frames: Record<string, unknown>[]): GenerationSummary {
  const starts = frames
    .filter((f) => f["type"] === "stream_start" && f["rid"] === "r1")
    .map((f) => ({ regen: f["regen"] }));
  const chunks = frames
    .filter((f) => f["type"] === "stream_chunk" && f["rid"] === "r1")
    .filter((f) => f["content_type"] === undefined || f["content_type"] === "text")
    .map((f) => String(f["text"] ?? ""));
  const final = frames
    .filter((f) => f["type"] === "stream_end" && f["rid"] === "r1" && f["is_final"] !== false)
    .at(-1);
  if (final === undefined) throw new Error("missing final stream_end");
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

function compareSummary(name: string, rust: GenerationSummary, ts: GenerationSummary): number {
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

function compareRequests(requests: CapturedLlmRequest[]): number {
  if (requests.length !== 2) {
    console.error(`  FAIL  provider request count: expected 2, got ${requests.length}`);
    for (const req of requests) console.error(`        ${req.key} ${req.path}`);
    return 1;
  }

  const [rust, ts] = requests;
  if (rust === undefined || ts === undefined) return 1;
  if (rust.canonical === ts.canonical) {
    console.log(`  ok    provider request body (${rust.key.slice(0, 12)})`);
    return 0;
  }

  console.error("  FAIL  provider request body");
  console.error(`        rust key: ${rust.key}`);
  console.error(`        ts key:   ${ts.key}`);
  console.error(`        rust: ${JSON.stringify(rust.body)}`);
  console.error(`        ts:   ${JSON.stringify(ts.body)}`);
  return 1;
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
