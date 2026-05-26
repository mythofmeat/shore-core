#!/usr/bin/env bun
/**
 * Live cache-accounting test — head-to-head Rust vs TS placement.
 *
 * NOT for CI. Costs real OpenRouter tokens. Requires
 * `OPENROUTER_API_KEY` in the environment. Drives both daemons
 * (Rust at `target/release/shore-daemon`, TS via `bun src/main.ts`)
 * against four scripted scenarios that exercise the breakpoint
 * placement gates from `docs/DAEMON_TS_PARITY.md`:
 *
 *   S1 plain growing chat (5 turns) — measures whether stable-message
 *      markers accumulate cache_read across turn growth.
 *   S2 tool-loop (single round) — measures cache_read across the
 *      tool_use → tool_result frozen-history boundary.
 *   S3 memory_index rewrite mid-stream — confirms that the TS rule
 *      ("skip memory_index") preserves the system prefix when
 *      MEMORY.md content changes between turns.
 *   S4 regen + continue — measures cache behavior when the last
 *      assistant turn is replaced (the regen-friendliness gate).
 *
 * Each scenario runs once per daemon. The first user message embeds a
 * scenario-unique nonce so the prefix hash is guaranteed-cold the
 * first time we send it; later turns within the scenario inherit the
 * same nonce and should engage the cache.
 *
 * We read cache_read / cache_creation token counts straight out of
 * each daemon's `ledger.db` after shutdown, print a per-call table
 * for each scenario, and summarize totals and a weighted "net
 * savings" figure. The weights model Anthropic's published rates
 * (cache write at 1h ≈ 2× input, cache read ≈ 0.1× input).
 */

import { Database } from "bun:sqlite";
import fs from "node:fs";
import { join, resolve as resolvePath } from "node:path";

import {
  buildDaemonEnv,
  copyFixtureToTmp,
  openConnection,
  readFrame,
  readListenAddr,
  spawnDaemon,
  type FrameQueue,
} from "./parity/_lib.ts";

const DEFAULT_FIXTURE = "parity-traces/live-fixtures/cache-test";
const DEFAULT_RUST = resolvePath(
  process.cwd(),
  "..",
  "..",
  "target",
  "release",
  "shore-daemon",
);
const CHARACTER = "caseyparity";

interface Args {
  rust: string;
  ts: string | undefined;
  fixture: string;
  only: string | undefined;
}

const args = parseArgs(process.argv.slice(2));
const fixtureDir = resolvePath(args.fixture);
const tsCmd = args.ts === undefined ? ["bun", "src/main.ts"] : [args.ts];

if (!process.env["OPENROUTER_API_KEY"]) {
  console.error(
    "live-cache-accounting requires OPENROUTER_API_KEY — set it and re-run.",
  );
  process.exit(2);
}
if (!fs.existsSync(args.rust)) {
  console.error(`Rust daemon binary not found at ${args.rust}`);
  console.error("Build it with: cargo build --release -p shore-daemon");
  process.exit(2);
}

const NONCE = process.env["SHORE_TEST_NONCE"] ?? crypto.randomUUID();

interface ScenarioStep {
  kind: "message" | "regen" | "rewrite-memory";
  text?: string;
  newMemory?: string;
}

interface Scenario {
  name: string;
  description: string;
  steps: ScenarioStep[];
}

const NEW_MEMORY_BODY = `# Memory Index

This file is Casey's map of long-term memory. It is not the memory itself — it is a pointer to the files where memory lives.

## Memory areas

- \`memory/log.md\` — chronological session log.
- \`memory/people.md\` — named NPCs and their current state.

## Current campaign

The party has moved to a new arc in the city of Caer Drûn — a fortified mountain capital several weeks' travel from Aldermere. The previous Aldermere arc has been resolved: the missing villagers were freed and the forest entity is dormant.

## Currently relevant files

- \`memory/log.md\`: last updated session 7, the party arrived at Caer Drûn and was granted audience with Lord Senedh.
- \`memory/people.md\`: Lord Senedh (city ruler, formal, cautious), Mira (a courier the party met on the road, friendly), Captain Berek (city guard captain, brusque).

## Throughlines

- **Caer Drûn court intrigue**: the party has been hired to investigate a string of poisonings among the Lord's advisors.
- **Old debts**: a merchant from Aldermere recognized the party in the marketplace and reminded them of a small unpaid favor.

## Needs review

- Whether Mira is connected to the poisoning case remains unresolved.
`;

const SCENARIOS: Scenario[] = [
  {
    name: "S1-plain-chat",
    description: "5-turn growing chat, no tools intended",
    steps: [
      { kind: "message", text: nonceMsg("S1", "Hi Casey. Give me a one-sentence summary of where my party currently is in the campaign.") },
      { kind: "message", text: "What kind of place is Aldermere — paint me a quick sensory picture." },
      { kind: "message", text: "Tell me one thing about Edric that's worth keeping in mind." },
      { kind: "message", text: "And what's the marshal's deal — why is he suspicious of us?" },
      { kind: "message", text: "Given everything, what should I be doing next? Just suggest, don't roll." },
    ],
  },
  {
    name: "S2-tool-loop",
    description: "Single roll-dice tool call mid-fiction",
    steps: [
      { kind: "message", text: nonceMsg("S2", "Casey, I'm sneaking up to Edric's cabin at dusk. Roll 1d20 for stealth and narrate what happens based on the result.") },
    ],
  },
  {
    name: "S3-memory-rewrite",
    description: "memory_index changes between turn 2 and turn 3",
    steps: [
      { kind: "message", text: nonceMsg("S3", "Hi Casey. Quick recap: what's our campaign about right now?") },
      { kind: "message", text: "Who are the most important NPCs we know about?" },
      { kind: "rewrite-memory", newMemory: NEW_MEMORY_BODY },
      { kind: "message", text: "Okay, anything else worth keeping in mind right now?" },
    ],
  },
  {
    name: "S4-regen",
    description: "Two turns, then regen the last assistant, then continue",
    steps: [
      { kind: "message", text: nonceMsg("S4", "Hi Casey. Set the scene at Aldermere for me — open the session.") },
      { kind: "message", text: "What's the weather like today there?" },
      { kind: "regen" },
      { kind: "message", text: "Got it. Where should I head first, just narratively?" },
    ],
  },
];

function nonceMsg(scenarioName: string, body: string): string {
  return `[cache-test ${NONCE} ${scenarioName}]\n\n${body}`;
}

interface CallRow {
  id: number;
  callType: string;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
}

interface ScenarioResult {
  daemon: "rust" | "ts";
  scenario: string;
  rows: CallRow[];
}

const results: ScenarioResult[] = [];

const allScenarios = args.only
  ? SCENARIOS.filter((s) => s.name === args.only)
  : SCENARIOS;

if (allScenarios.length === 0) {
  console.error(`unknown scenario: ${args.only}`);
  process.exit(2);
}

console.log(`live-cache-accounting nonce=${NONCE}`);
console.log(`scenarios: ${allScenarios.map((s) => s.name).join(", ")}`);
console.log();

for (const scenario of allScenarios) {
  console.log(`========== ${scenario.name} ==========`);
  console.log(scenario.description);
  console.log();
  for (const daemon of ["rust", "ts"] as const) {
    const cmd = daemon === "rust" ? [args.rust] : tsCmd;
    const rows = await runScenario(daemon, cmd, scenario);
    results.push({ daemon, scenario: scenario.name, rows });
  }
}

printReport(results);

// ── runner ────────────────────────────────────────────────────────────

async function runScenario(
  label: "rust" | "ts",
  cmd: string[],
  scenario: Scenario,
): Promise<CallRow[]> {
  const { configDir, dataDir } = copyFixtureToTmp(
    fixtureDir,
    `shore-live-cache-${scenario.name}-${label}-`,
  );
  const env = buildDaemonEnv({
    configDir,
    dataDir,
    prefix: `shore-live-cache-${scenario.name}-${label}-`,
  });
  env["TZ"] = "UTC";

  console.log(`-- ${scenario.name} : ${label} --`);
  const proc = spawnDaemon(cmd, env);
  try {
    const addr = await readListenAddr([proc.stdout, proc.stderr]);
    if (!addr) throw new Error(`${label}: daemon never printed listen address`);

    const { sock, frames } = await openConnection(addr);
    await readFrame(frames); // hello frame (history or status)
    sock.write(
      JSON.stringify({
        type: "hello",
        client_type: "cli",
        client_name: `live-cache-${label}`,
        capabilities: ["streaming"],
        character: CHARACTER,
      }) + "\n",
    );
    await readFrame(frames); // post-hello frame (history)

    let stepIndex = 0;
    for (const step of scenario.steps) {
      stepIndex++;
      const rid = `${scenario.name}-${label}-${stepIndex}`;
      if (step.kind === "rewrite-memory") {
        const newBody = step.newMemory!;
        // Write through both the canonical workspace MEMORY.md and the
        // active-prompt snapshot copy — the snapshot is what the engine
        // actually reads at prompt-assembly time, and it does not
        // refresh outside of compaction boundaries.
        const workspace = join(
          configDir,
          "characters",
          CHARACTER,
          "workspace",
          "MEMORY.md",
        );
        const snapshot = join(
          dataDir,
          CHARACTER,
          "active_prompt",
          "MEMORY.md",
        );
        fs.writeFileSync(workspace, newBody);
        fs.mkdirSync(join(dataDir, CHARACTER, "active_prompt"), {
          recursive: true,
        });
        fs.writeFileSync(snapshot, newBody);
        console.log(`  ${label.padEnd(4)} step ${stepIndex} rewrite-memory (${newBody.length}B)`);
        continue;
      }
      if (step.kind === "regen") {
        sock.write(
          JSON.stringify({ type: "regen", rid, stream: true }) + "\n",
        );
        console.log(`  ${label.padEnd(4)} step ${stepIndex} regen ${rid}`);
      } else {
        sock.write(
          JSON.stringify({
            type: "message",
            rid,
            text: step.text!,
            stream: true,
          }) + "\n",
        );
        console.log(
          `  ${label.padEnd(4)} step ${stepIndex} message ${rid} (${step.text!.length}B)`,
        );
      }
      await readUntilFinal(label, frames, rid);
    }
    sock.end();
  } finally {
    proc.kill("SIGTERM");
    await proc.exited;
  }

  return readLedger(dataDir);
}

async function readUntilFinal(
  label: string,
  frames: FrameQueue,
  rid: string,
): Promise<void> {
  // Generous deadline: cold-start cache_creation on a ~6k-token system
  // prompt + adaptive-thinking can run 10–20s on Haiku via OpenRouter.
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    const frame = (await readFrame(
      frames,
      Math.max(500, deadline - Date.now()),
    )) as Record<string, unknown>;
    if (frame["type"] === "error") {
      throw new Error(
        `${label}: daemon emitted error: ${JSON.stringify(frame)}`,
      );
    }
    if (
      frame["type"] === "stream_end"
      && frame["rid"] === rid
      && frame["is_final"] !== false
    ) {
      return;
    }
  }
  throw new Error(`${label}: timed out waiting for final stream_end (${rid})`);
}

function readLedger(dataDir: string): CallRow[] {
  const dbPath = join(dataDir, "ledger.db");
  if (!fs.existsSync(dbPath)) return [];
  const db = new Database(dbPath);
  try {
    return db
      .query<CallRow, []>(
        `SELECT
           id, call_type AS callType,
           input_tokens AS inputTokens,
           output_tokens AS outputTokens,
           cache_read_tokens AS cacheReadTokens,
           cache_write_tokens AS cacheWriteTokens
         FROM calls
         ORDER BY id`,
      )
      .all();
  } finally {
    db.close();
  }
}

// ── reporting ─────────────────────────────────────────────────────────

function printReport(results: ScenarioResult[]): void {
  console.log();
  console.log("============================================================");
  console.log("                      RESULTS");
  console.log("============================================================");

  const byScenario = new Map<string, { rust: CallRow[]; ts: CallRow[] }>();
  for (const r of results) {
    const slot = byScenario.get(r.scenario) ?? { rust: [], ts: [] };
    slot[r.daemon] = r.rows;
    byScenario.set(r.scenario, slot);
  }

  let totalRustNet = 0;
  let totalTsNet = 0;

  for (const [scenario, { rust, ts }] of byScenario) {
    console.log();
    console.log(`### ${scenario}`);
    console.log();
    const n = Math.max(rust.length, ts.length);
    console.log(
      "  call  type           rust_in  rust_out  rust_read  rust_write   |   ts_in  ts_out  ts_read  ts_write",
    );
    for (let i = 0; i < n; i++) {
      const r = rust[i];
      const t = ts[i];
      const cell = (row: CallRow | undefined): string => {
        if (!row) return "  -      -        -        -      ";
        return `${pad(row.inputTokens, 6)}  ${pad(row.outputTokens, 6)}  ${pad(
          row.cacheReadTokens,
          7,
        )}  ${pad(row.cacheWriteTokens, 8)}`;
      };
      console.log(
        `  ${pad(i + 1, 3)}   ${(r?.callType ?? t?.callType ?? "?").padEnd(13)}  ${cell(r)}   |   ${cell(t)}`,
      );
    }
    const rustTot = totals(rust);
    const tsTot = totals(ts);
    console.log();
    console.log(
      `  totals          rust  read=${rustTot.read}  write=${rustTot.write}  net=${rustTot.net.toFixed(0)}   |   ts  read=${tsTot.read}  write=${tsTot.write}  net=${tsTot.net.toFixed(0)}`,
    );
    const delta = tsTot.net - rustTot.net;
    const winner =
      Math.abs(delta) < 0.01 ? "tie" : delta > 0 ? "TS wins" : "RUST wins";
    console.log(
      `  delta           ts.net - rust.net = ${delta.toFixed(0)}  →  ${winner}`,
    );
    totalRustNet += rustTot.net;
    totalTsNet += tsTot.net;
  }

  console.log();
  console.log("============================================================");
  console.log(
    `OVERALL  rust.net = ${totalRustNet.toFixed(0)}   ts.net = ${totalTsNet.toFixed(0)}   Δ = ${(
      totalTsNet - totalRustNet
    ).toFixed(0)}`,
  );
  console.log("============================================================");
  console.log();
  console.log(
    "Net savings formula: (cache_read × 0.9) − (cache_write × 1.0).",
  );
  console.log(
    "  read saves 0.9× input cost (read = 0.1× base, vs paying base).",
  );
  console.log("  write costs +1.0× input cost (1h write = 2.0× base, vs base).");
  console.log("  positive net = strategy saves money in this scenario.");
}

function totals(rows: CallRow[]): { read: number; write: number; net: number } {
  let read = 0;
  let write = 0;
  for (const r of rows) {
    read += r.cacheReadTokens;
    write += r.cacheWriteTokens;
  }
  const net = read * 0.9 - write * 1.0;
  return { read, write, net };
}

function pad(n: number | string, width: number): string {
  const s = String(n);
  return s.length >= width ? s : " ".repeat(width - s.length) + s;
}

// ── arg parsing ───────────────────────────────────────────────────────

function parseArgs(argv: string[]): Args {
  const parsed: Args = {
    rust: DEFAULT_RUST,
    ts: undefined,
    fixture: DEFAULT_FIXTURE,
    only: undefined,
  };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]!;
    if (arg === "--rust") parsed.rust = takeValue(argv, ++i, arg);
    else if (arg === "--ts") parsed.ts = takeValue(argv, ++i, arg);
    else if (arg === "--fixture") parsed.fixture = takeValue(argv, ++i, arg);
    else if (arg === "--only") parsed.only = takeValue(argv, ++i, arg);
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
