#!/usr/bin/env bun
/**
 * Generic capture for Tier 2 command-dispatcher parity cases.
 *
 * Driven by `parity-traces/commands/manifest.json`. For each case
 * (or one selected by `--id`), spawns the Rust daemon against the
 * named fixture, runs the standard handshake, sends the command,
 * reads `expected_frames` s2c response frames, sorts them by type
 * for deterministic ordering, and writes the trace JSONL to the
 * case's `baseline` path under `parity-traces/commands/`.
 *
 * Usage:
 *   bun scripts/capture-command.ts <rust-daemon> --all
 *   bun scripts/capture-command.ts <rust-daemon> --id <case-id>
 *   bun scripts/capture-command.ts <rust-daemon> --id <case-id> --manifest <path>
 */

import { join, resolve as resolvePath } from "node:path";

import {
  buildDaemonEnv,
  copyFixtureToTmp,
  fail,
  openConnection,
  readFrame,
  readListenAddr,
  spawnDaemon,
} from "./parity/_lib.ts";
import { caseId, loadManifest, type CommandCase } from "./parity/commands-manifest.ts";

const PARITY_ROOT = "parity-traces";
const COMMANDS_DIR = `${PARITY_ROOT}/commands`;
const FIXTURES_DIR = `${PARITY_ROOT}/fixtures`;
const DEFAULT_MANIFEST = `${COMMANDS_DIR}/manifest.json`;

const args = process.argv.slice(2);
const daemonPath = args[0];
let manifestPath = DEFAULT_MANIFEST;
let onlyId: string | undefined;
let all = false;
for (let i = 1; i < args.length; i++) {
  const a = args[i];
  if (a === "--manifest") manifestPath = args[++i]!;
  else if (a === "--id") onlyId = args[++i];
  else if (a === "--all") all = true;
  else {
    console.error(`unknown arg: ${a}`);
    process.exit(2);
  }
}
if (!daemonPath || (!onlyId && !all)) {
  console.error("usage: capture-command.ts <rust-daemon> (--all | --id <case-id>) [--manifest <path>]");
  process.exit(2);
}

const manifest = loadManifest(resolvePath(manifestPath));
const cases = onlyId
  ? manifest.cases.filter((c) => caseId(c) === onlyId)
  : manifest.cases;
if (onlyId && cases.length === 0) {
  console.error(`no case with id "${onlyId}" in ${manifestPath}`);
  process.exit(2);
}

for (const c of cases) {
  await captureCase(daemonPath, c);
}

async function captureCase(daemonBin: string, c: CommandCase): Promise<void> {
  const fixtureDir = resolvePath(join(FIXTURES_DIR, c.fixture));
  const outPath = resolvePath(join(COMMANDS_DIR, c.baseline));
  const { configDir, dataDir } = copyFixtureToTmp(fixtureDir, `shore-cmd-capture-${caseId(c)}-`);
  const env = buildDaemonEnv({ configDir, dataDir, prefix: `shore-cmd-capture-${caseId(c)}-` });
  env["SHORE_PARITY_OPENROUTER_KEY"] = "";

  const trace: Array<{ dir: "s2c" | "c2s"; frame: unknown }> = [];
  const proc = spawnDaemon([daemonBin], env);
  try {
    const addr = await readListenAddr([proc.stdout, proc.stderr]);
    if (!addr) fail(`daemon never printed listen address (capture ${caseId(c)})`);

    const { sock, frames } = await openConnection(addr);

    const hello = await readFrame(frames);
    trace.push({ dir: "s2c", frame: hello });

    const clientHello = {
      type: "hello",
      client_type: "cli",
      client_name: `cmd-capture-${caseId(c)}`,
      character: c.character,
    };
    sock.write(JSON.stringify(clientHello) + "\n");
    trace.push({ dir: "c2s", frame: clientHello });

    const history = await readFrame(frames);
    trace.push({ dir: "s2c", frame: history });

    const cmdFrame = {
      type: "command",
      ...(c.rid !== null ? { rid: c.rid ?? "r1" } : {}),
      name: c.name,
      args: c.args ?? {},
    };
    sock.write(JSON.stringify(cmdFrame) + "\n");
    trace.push({ dir: "c2s", frame: cmdFrame });

    // Read expected_frames s2c frames in any order, sort by type for
    // deterministic baseline ordering (matches the Tier 1 pattern
    // established by capture-edit.ts when the daemon emits both a
    // history broadcast and a command_output for state-mutating cmds).
    const responses: Record<string, unknown>[] = [];
    for (let i = 0; i < c.expected_frames; i++) {
      responses.push((await readFrame(frames)) as Record<string, unknown>);
    }
    const sorted = [...responses].sort((a, b) =>
      String(a["type"]).localeCompare(String(b["type"])),
    );
    for (const frame of sorted) {
      trace.push({ dir: "s2c", frame });
    }

    await new Promise((r) => setTimeout(r, 200));
    sock.end();
  } finally {
    proc.kill("SIGTERM");
    await proc.exited;
  }

  await Bun.write(outPath, trace.map((e) => JSON.stringify(e)).join("\n") + "\n");
  console.log(`[${caseId(c)}] wrote ${trace.length} frames → ${outPath}`);
  for (const entry of trace) {
    const fr = entry.frame as { type?: string };
    console.log(`   ${entry.dir}  ${fr.type}`);
  }
}
