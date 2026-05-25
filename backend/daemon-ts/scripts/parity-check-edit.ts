#!/usr/bin/env bun
/**
 * Tier 1 parity check: edit command + restart-persistence.
 *
 * Replays `parity-traces/edit.jsonl` against the TS daemon. Asserts:
 *   - the edit command's two response frames (history broadcast +
 *     command_output) match the Rust daemon's, type-sorted to absorb
 *     racy wire ordering between the broadcast/direct-response tasks;
 *   - after restart, the History snapshot shows the edited content on
 *     the targeted message and all other messages untouched.
 *
 * Usage:
 *   bun scripts/parity-check-edit.ts [--fixture <dir>] [--baseline <path>] [<daemon>]
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
let fixtureDir = "parity-traces/fixtures/handshake-character";
let baselinePath = "parity-traces/edit.jsonl";
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
  "shore-edit-parity-",
);
const env = buildDaemonEnv({ configDir, dataDir, prefix: "shore-edit-parity-" });

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

    // Walk the baseline, batching consecutive same-direction s2c entries
    // so the type-sort absorbs concurrent emission order between the
    // command_output (direct_tx) and history (event_tx) frames.
    let i = 0;
    while (i < phaseEntries.length) {
      const entry = phaseEntries[i]!;
      if (entry.dir === "c2s") {
        sock.write(JSON.stringify(entry.frame) + "\n");
        i++;
        continue;
      }
      // Collect consecutive s2c entries from here.
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
      // The baseline is already type-sorted within a batch by the
      // capture script — pair index-by-index.
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
