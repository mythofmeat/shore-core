#!/usr/bin/env bun
/**
 * Tier 1 parity check: alt command + restart-persistence.
 *
 * Replays `parity-traces/alt.jsonl` against the TS daemon. Same shape
 * as the edit/delete checks: concurrent-emission batching for the post-
 * command response, restart-phase history assertion that the selected
 * alternative survives the daemon teardown.
 *
 * Usage:
 *   bun scripts/parity-check-alt.ts [--fixture <dir>] [--baseline <path>] [<daemon>]
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
let fixtureDir = "parity-traces/fixtures/alt-cycle";
let baselinePath = "parity-traces/alt.jsonl";
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
  "shore-alt-parity-",
);
const env = buildDaemonEnv({ configDir, dataDir, prefix: "shore-alt-parity-" });

const FUZZY_DIFFS: FuzzyDiffs = {
  hello: ["server_name"],
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

    let i = 0;
    while (i < phaseEntries.length) {
      const entry = phaseEntries[i]!;
      if (entry.dir === "c2s") {
        sock.write(JSON.stringify(entry.frame) + "\n");
        i++;
        continue;
      }
      const batchStart = i;
      while (i < phaseEntries.length && phaseEntries[i]!.dir === "s2c") i++;
      const batch = phaseEntries.slice(batchStart, i);
      const actuals: Record<string, unknown>[] = [];
      for (let k = 0; k < batch.length; k++) {
        actuals.push((await readFrame(frames)) as Record<string, unknown>);
      }
      const sortedActual = [...actuals].sort((a, b) =>
        String(a["type"]).localeCompare(String(b["type"])),
      );
      for (let k = 0; k < batch.length; k++) {
        const expected = batch[k]!;
        const actual = sortedActual[k]!;
        const diff = compareFrames(expected.frame, actual, FUZZY_DIFFS);
        if (diff.length === 0) {
          console.log(`  ok    s2c ${actual["type"]}`);
        } else {
          failures++;
          console.error(`  FAIL  s2c ${actual["type"]}`);
          for (const d of diff) console.error(`          ${d}`);
          console.error(`          baseline: ${JSON.stringify(expected.frame)}`);
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
