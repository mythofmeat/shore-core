#!/usr/bin/env bun
/**
 * Phase 3 parity check: message-append + restart-persistence.
 *
 * Replays the captured client side of `parity-traces/message-append.jsonl`
 * against the TS daemon and diffs the recorded server frames. The trace
 * is split into two phases (live, restart): we tear the daemon down and
 * restart it between phases, against the same mutated work dir. This
 * exercises Phase 3's exit criterion — "send a user message, restart the
 * daemon, see the message in the next handshake's History".
 *
 * Non-deterministic fields in the restart-phase History (`msg_id` and
 * `timestamp` of the appended message) are matched fuzzily: same type,
 * same field path, value not compared.
 *
 * Usage:
 *   bun scripts/parity-check-message-append.ts [--fixture <dir>] [--text <s>] [<daemon>]
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
let baselinePath = "parity-traces/message-append.jsonl";
let daemonBin: string | undefined;
for (let i = 0; i < args.length; i++) {
  const a = args[i];
  if (a === "--fixture") fixtureDir = args[++i]!;
  else if (a === "--baseline") baselinePath = args[++i]!;
  else if (a === "--text") args[++i]; // accepted for capture-script parity; not used here
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

// Copy fixture into one work dir that survives across both phases.
const { configDir, dataDir } = copyFixtureToTmp(
  resolvePath(fixtureDir),
  "shore-msg-append-parity-",
);
const env = buildDaemonEnv({ configDir, dataDir, prefix: "shore-msg-append-" });

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
        // The post-message send is async on our side too; give it a
        // moment to flush before the next phase tears the daemon down.
        if (entry.frame["type"] === "message") {
          await new Promise((r) => setTimeout(r, 500));
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
