#!/usr/bin/env bun
/**
 * Parity check: spawn the TS daemon, replay the captured client side of the
 * trace, diff the emitted server-to-client frames against the baseline
 * produced by `capture-rust-trace.ts`.
 *
 * Exits non-zero on any structural divergence. Differences in
 * `server_name` are expected (we want "shore-daemon-ts" vs "shore-daemon").
 *
 * Usage:
 *   bun scripts/parity-check.ts <baseline.jsonl> [<daemon-bin>] [--fixture <dir>]
 *
 *   daemon-bin defaults to running `bun src/main.ts`.
 *   --fixture points SHORE_CONFIG_DIR / SHORE_DATA_DIR at <dir>/config and
 *   <dir>/data (matches capture-rust-trace.ts).
 */

import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve as resolvePath } from "node:path";

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

const FUZZY_DIFFS: FuzzyDiffs = {
  hello: ["server_name"],
};

const positional: string[] = [];
let fixtureDir: string | undefined;
for (let i = 2; i < process.argv.length; i++) {
  const a = process.argv[i]!;
  if (a === "--fixture") fixtureDir = resolvePath(process.argv[++i]!);
  else positional.push(a);
}
const baselinePath = positional[0];
const daemonBin = positional[1];
if (!baselinePath) {
  console.error("usage: parity-check.ts <baseline.jsonl> [<daemon-bin>] [--fixture <dir>]");
  process.exit(2);
}
const cmd: string[] = daemonBin ? [daemonBin] : ["bun", "src/main.ts"];

const baseline = readFileSync(baselinePath, "utf8")
  .split("\n")
  .filter((l) => l.trim() !== "")
  .map((l) => JSON.parse(l) as { dir: "s2c" | "c2s"; frame: Record<string, unknown> });

let configDir: string;
let dataDir: string;
if (fixtureDir) {
  ({ configDir, dataDir } = copyFixtureToTmp(fixtureDir, "shore-daemon-ts-parity-fixture-"));
} else {
  const tmp = mkdtempSync(join(tmpdir(), "shore-daemon-ts-parity-"));
  configDir = join(tmp, "config");
  dataDir = join(tmp, "data");
}
const env = buildDaemonEnv({ configDir, dataDir, prefix: "shore-daemon-ts-parity-" });

const proc = spawnDaemon(cmd, env);

let failures = 0;
try {
  const addr = await readListenAddr([proc.stdout, proc.stderr]);
  if (!addr) fail("daemon never printed listen address");

  const { sock, frames } = await openConnection(addr);

  for (const entry of baseline) {
    if (entry.dir === "c2s") {
      sock.write(JSON.stringify(entry.frame) + "\n");
    } else {
      const actual = (await readFrame(frames)) as Record<string, unknown>;
      const diff = compareFrames(entry.frame, actual, FUZZY_DIFFS);
      if (diff.length === 0) {
        console.log(`ok    s2c ${actual["type"]}`);
      } else {
        failures++;
        console.error(`FAIL  s2c ${actual["type"]}`);
        for (const d of diff) console.error(`        ${d}`);
        console.error(`        baseline: ${JSON.stringify(entry.frame)}`);
        console.error(`        actual:   ${JSON.stringify(actual)}`);
      }
    }
  }

  sock.end();
} finally {
  proc.kill("SIGTERM");
  await proc.exited;
}

if (failures > 0) {
  console.error(`\n${failures} divergence(s)`);
  process.exit(1);
}
console.log("\nparity ok");
