#!/usr/bin/env bun
/**
 * Tier 2 parity check: SWP command dispatcher round-trips.
 *
 * Replays every case in `parity-traces/commands/manifest.json` against
 * the TS daemon. Each case starts a fresh daemon against a fresh copy of
 * its named fixture, replays the captured Rust trace, and diffs all s2c
 * frames with manifest-controlled fuzzy paths.
 *
 * Usage:
 *   bun scripts/parity-check-commands.ts [<daemon>]
 *   bun scripts/parity-check-commands.ts [<daemon>] --manifest <path>
 */

import { readFileSync } from "node:fs";
import { join, resolve as resolvePath } from "node:path";

import {
  buildDaemonEnv,
  compareFrames,
  copyFixtureToTmp,
  type FuzzyDiffs,
  openConnection,
  readFrame,
  readListenAddr,
  spawnDaemon,
} from "./parity/_lib.ts";
import { caseId, loadManifest, mergeFuzzy, type CommandCase } from "./parity/commands-manifest.ts";

const PARITY_ROOT = "parity-traces";
const COMMANDS_DIR = `${PARITY_ROOT}/commands`;
const FIXTURES_DIR = `${PARITY_ROOT}/fixtures`;
const DEFAULT_MANIFEST = `${COMMANDS_DIR}/manifest.json`;

const GLOBAL_FUZZY: FuzzyDiffs = {
  hello: ["server_name"],
  history: ["messages[*].timestamp"],
};

interface TraceEntry {
  dir: "s2c" | "c2s";
  frame: Record<string, unknown>;
}

const positional: string[] = [];
let manifestPath = DEFAULT_MANIFEST;
const args = process.argv.slice(2);
for (let i = 0; i < args.length; i++) {
  const a = args[i]!;
  if (a === "--manifest") manifestPath = args[++i]!;
  else positional.push(a);
}

const daemonBin = positional[0];
const cmd: string[] = daemonBin ? [daemonBin] : ["bun", "src/main.ts"];
const manifest = loadManifest(resolvePath(manifestPath));

let failures = 0;
for (const c of manifest.cases) {
  failures += await checkCase(c);
}

if (failures > 0) {
  console.error(`\n${failures} command parity failure(s)`);
  process.exit(1);
}

console.log(`\ncommand parity ok (${manifest.cases.length} cases)`);

async function checkCase(c: CommandCase): Promise<number> {
  const id = caseId(c);
  console.log(`── command: ${id} ──`);

  let caseFailures = 0;
  const baselinePath = resolvePath(join(COMMANDS_DIR, c.baseline));
  const baseline = loadBaseline(baselinePath);
  const baselinePostFrames = postCommandS2cFrames(baseline);
  if (baselinePostFrames !== c.expected_frames) {
    console.error(
      `  FAIL  manifest expected_frames=${c.expected_frames}, baseline has ${baselinePostFrames} post-command s2c frame(s)`,
    );
    caseFailures++;
  }

  const fixtureDir = resolvePath(join(FIXTURES_DIR, c.fixture));
  const { configDir, dataDir } = copyFixtureToTmp(fixtureDir, `shore-cmd-${id}-`);
  const env = buildDaemonEnv({ configDir, dataDir, prefix: `shore-cmd-${id}-` });
  env["SHORE_PARITY_OPENROUTER_KEY"] = "";
  const fuzzy = mergeFuzzy(GLOBAL_FUZZY, c.fuzzy);
  const postCommandActuals: Record<string, unknown>[] = [];

  const proc = spawnDaemon(cmd, env);
  try {
    const addr = await readListenAddr([proc.stdout, proc.stderr]);
    if (!addr) throw new Error("daemon never printed listen address");

    const { sock, frames } = await openConnection(addr);
    let sawCommand = false;

    let i = 0;
    while (i < baseline.length) {
      const entry = baseline[i]!;
      if (entry.dir === "c2s") {
        sock.write(JSON.stringify(entry.frame) + "\n");
        if (entry.frame["type"] === "command") sawCommand = true;
        i++;
        continue;
      }

      const batchStart = i;
      while (i < baseline.length && baseline[i]!.dir === "s2c") i++;
      const expectedBatch = baseline.slice(batchStart, i);
      const actuals: Record<string, unknown>[] = [];
      for (let k = 0; k < expectedBatch.length; k++) {
        actuals.push((await readFrame(frames)) as Record<string, unknown>);
      }
      if (sawCommand) postCommandActuals.push(...actuals);

      const sortedExpected = [...expectedBatch].sort((a, b) =>
        String(a.frame["type"]).localeCompare(String(b.frame["type"])),
      );
      const sortedActual = [...actuals].sort((a, b) =>
        String(a["type"]).localeCompare(String(b["type"])),
      );

      for (let k = 0; k < sortedExpected.length; k++) {
        const expected = sortedExpected[k]!;
        const actual = sortedActual[k]!;
        const diff = compareFrames(expected.frame, actual, fuzzy);
        if (diff.length === 0) {
          console.log(`  ok    s2c ${actual["type"]}`);
        } else {
          caseFailures++;
          console.error(`  FAIL  s2c ${actual["type"]}`);
          for (const d of diff) console.error(`          ${d}`);
          console.error(`          baseline: ${JSON.stringify(expected.frame)}`);
          console.error(`          actual:   ${JSON.stringify(actual)}`);
        }
      }
    }

    const hasError = postCommandActuals.some((frame) => frame["type"] === "error");
    const hasOutput = postCommandActuals.some((frame) => frame["type"] === "command_output");
    if (c.outcome === "error" && !hasError) {
      caseFailures++;
      console.error("  FAIL  outcome=error but no post-command error frame was emitted");
    }
    if (c.outcome === "ok" && !hasOutput) {
      caseFailures++;
      console.error("  FAIL  outcome=ok but no post-command command_output frame was emitted");
    }

    sock.end();
  } catch (e) {
    caseFailures++;
    console.error(`  FAIL  ${e instanceof Error ? e.message : String(e)}`);
  } finally {
    proc.kill("SIGTERM");
    await proc.exited;
  }

  return caseFailures;
}

function loadBaseline(path: string): TraceEntry[] {
  return readFileSync(path, "utf8")
    .split("\n")
    .filter((l) => l.trim() !== "")
    .map((l) => JSON.parse(l) as TraceEntry);
}

function postCommandS2cFrames(entries: TraceEntry[]): number {
  let sawCommand = false;
  let count = 0;
  for (const entry of entries) {
    if (entry.dir === "c2s" && entry.frame["type"] === "command") {
      sawCommand = true;
      continue;
    }
    if (sawCommand && entry.dir === "s2c") count++;
  }
  return count;
}
