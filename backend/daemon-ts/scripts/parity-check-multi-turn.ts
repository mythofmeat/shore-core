#!/usr/bin/env bun
/**
 * Tier 1 parity check: multi-turn message append + restart-persistence.
 *
 * Replays the captured client side of `parity-traces/multi-turn.jsonl`
 * against the TS daemon and diffs the recorded server frames. Extends
 * the single-message-append check to verify that N user messages
 * accumulate in `active.jsonl` correctly and all survive a restart.
 *
 * Non-deterministic fields in the restart-phase History (`msg_id` and
 * `timestamp` per message) are matched fuzzily.
 *
 * Usage:
 *   bun scripts/parity-check-multi-turn.ts [--fixture <dir>] [--baseline <path>] [<daemon>]
 */

import { readFileSync } from "node:fs";
import { resolve as resolvePath } from "node:path";

import {
  buildDaemonEnv,
  compareFrames,
  copyFixtureToTmp,
  fail,
  type FuzzyDiffs,
  openConnection,
  readFrame,
  readListenAddr,
  spawnDaemon,
} from "./parity/_lib.ts";

const args = process.argv.slice(2);
let fixtureDir = "parity-traces/fixtures/message-append";
let baselinePath = "parity-traces/multi-turn.jsonl";
let daemonBin: string | undefined;
for (let i = 0; i < args.length; i++) {
  const a = args[i];
  if (a === "--fixture") fixtureDir = args[++i]!;
  else if (a === "--baseline") baselinePath = args[++i]!;
  else daemonBin = a;
}
const cmd: string[] = daemonBin ? [daemonBin] : ["bun", "src/main.ts"];

interface TraceEntry {
  dir: "s2c" | "c2s";
  phase: "live" | "restart";
  frame: Record<string, unknown>;
}

const baseline: TraceEntry[] = readFileSync(resolvePath(baselinePath), "utf8")
  .split("\n")
  .filter((l) => l.trim() !== "")
  .map((l) => JSON.parse(l) as TraceEntry);

const { configDir, dataDir } = copyFixtureToTmp(
  resolvePath(fixtureDir),
  "shore-multi-turn-parity-",
);
const env = buildDaemonEnv({ configDir, dataDir, prefix: "shore-multi-turn-parity-" });

const FUZZY_DIFFS: FuzzyDiffs = {
  hello: ["server_name"],
  history: ["messages[*].msg_id", "messages[*].timestamp"],
};

const phases: Array<"live" | "restart"> = ["live", "restart"];
let failures = 0;

for (const phase of phases) {
  console.log(`── phase: ${phase} ──`);
  const phaseEntries = baseline.filter((e) => e.phase === phase);

  const proc = spawnDaemon(cmd, env);

  try {
    const addr = await readListenAddr([proc.stdout, proc.stderr]);
    if (!addr) fail("daemon never printed listen address");

    const { sock, frames } = await openConnection(addr);

    for (const entry of phaseEntries) {
      if (entry.dir === "c2s") {
        sock.write(JSON.stringify(entry.frame) + "\n");
        if (entry.frame["type"] === "message") {
          // Same flush delay the capture script used per send.
          await new Promise((r) => setTimeout(r, 600));
        }
      } else {
        const actual = (await readFrame(frames)) as Record<string, unknown>;
        const diff = compareFrames(entry.frame, actual, FUZZY_DIFFS);
        if (diff.length === 0) {
          console.log(`  ok    ${entry.dir} ${actual["type"]}`);
        } else {
          failures++;
          console.error(`  FAIL  ${entry.dir} ${actual["type"]}`);
          for (const d of diff) console.error(`          ${d}`);
          console.error(`          baseline: ${JSON.stringify(entry.frame)}`);
          console.error(`          actual:   ${JSON.stringify(actual)}`);
        }
      }
    }

    sock.end();
  } finally {
    proc.kill("SIGTERM");
    await proc.exited;
  }
}

if (failures > 0) {
  console.error(`\n${failures} divergence(s)`);
  process.exit(1);
}
console.log("\nparity ok");
