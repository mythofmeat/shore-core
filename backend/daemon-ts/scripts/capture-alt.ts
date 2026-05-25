#!/usr/bin/env bun
/**
 * Capture the alt-command scenario from the Rust daemon.
 *
 * Sends `alt {ref, index}` to switch the active alternative on a
 * pre-seeded assistant message with multiple alternatives. The alt
 * fixture has m2 with two alternatives ("hi there" at index 0,
 * "hello back" at index 1).
 *
 * Usage:
 *   bun scripts/capture-alt.ts <rust-daemon> <out-file> \
 *     --fixture <dir> --character <name> --ref <msg_id> --index <n>
 */

import { resolve as resolvePath } from "node:path";

import {
  buildDaemonEnv,
  copyFixtureToTmp,
  fail,
  openConnection,
  readFrame,
  readListenAddr,
  spawnDaemon,
} from "./parity/_lib.ts";

const args = process.argv.slice(2);
const daemonPath = args[0];
const outPath = args[1];
let fixtureDir: string | undefined;
let character: string | undefined;
let ref: string | undefined;
let indexStr: string | undefined;
for (let i = 2; i < args.length; i++) {
  const a = args[i];
  if (a === "--fixture") fixtureDir = resolvePath(args[++i]!);
  else if (a === "--character") character = args[++i];
  else if (a === "--ref") ref = args[++i];
  else if (a === "--index") indexStr = args[++i];
  else {
    console.error(`unknown arg: ${a}`);
    process.exit(2);
  }
}
if (!daemonPath || !outPath || !fixtureDir || !character || !ref || indexStr === undefined) {
  console.error(
    "usage: capture-alt.ts <rust-daemon> <out> --fixture <dir> --character <name> --ref <msg_id> --index <n>",
  );
  process.exit(2);
}
const index = Number(indexStr);
if (!Number.isInteger(index) || index < 0) {
  console.error(`--index must be a non-negative integer (got ${indexStr})`);
  process.exit(2);
}

const { configDir, dataDir } = copyFixtureToTmp(fixtureDir, "shore-alt-");
const env = buildDaemonEnv({ configDir, dataDir, prefix: "shore-alt-" });

const trace: Array<{ dir: "s2c" | "c2s"; phase: "live" | "restart"; frame: unknown }> = [];

// ── live phase ──────────────────────────────────────────────────────────

{
  const proc = spawnDaemon([daemonPath], env);
  try {
    const addr = await readListenAddr([proc.stdout, proc.stderr]);
    if (!addr) fail("daemon never printed listen address (live)");

    const { sock, frames } = await openConnection(addr);

    const hello = await readFrame(frames);
    trace.push({ dir: "s2c", phase: "live", frame: hello });

    const clientHello = {
      type: "hello",
      client_type: "cli",
      client_name: "alt-capture",
      character,
    };
    sock.write(JSON.stringify(clientHello) + "\n");
    trace.push({ dir: "c2s", phase: "live", frame: clientHello });

    const history = await readFrame(frames);
    trace.push({ dir: "s2c", phase: "live", frame: history });

    const altCmd = {
      type: "command",
      rid: "r1",
      name: "alt",
      args: { ref, index },
    };
    sock.write(JSON.stringify(altCmd) + "\n");
    trace.push({ dir: "c2s", phase: "live", frame: altCmd });

    // Two-frame response (history broadcast + command_output) — sort by
    // type for deterministic ordering.
    const r1 = (await readFrame(frames)) as Record<string, unknown>;
    const r2 = (await readFrame(frames)) as Record<string, unknown>;
    const sorted = [r1, r2].sort((a, b) =>
      String(a["type"]).localeCompare(String(b["type"])),
    );
    for (const frame of sorted) {
      trace.push({ dir: "s2c", phase: "live", frame });
    }

    await new Promise((r) => setTimeout(r, 400));

    sock.end();
  } finally {
    proc.kill("SIGTERM");
    await proc.exited;
  }
}

// ── restart phase ───────────────────────────────────────────────────────

{
  const proc = spawnDaemon([daemonPath], env);
  try {
    const addr = await readListenAddr([proc.stdout, proc.stderr]);
    if (!addr) fail("daemon never printed listen address (restart)");

    const { sock, frames } = await openConnection(addr);

    const hello = await readFrame(frames);
    trace.push({ dir: "s2c", phase: "restart", frame: hello });

    const clientHello = {
      type: "hello",
      client_type: "cli",
      client_name: "alt-capture-restart",
      character,
    };
    sock.write(JSON.stringify(clientHello) + "\n");
    trace.push({ dir: "c2s", phase: "restart", frame: clientHello });

    const history = await readFrame(frames);
    trace.push({ dir: "s2c", phase: "restart", frame: history });

    sock.end();
  } finally {
    proc.kill("SIGTERM");
    await proc.exited;
  }
}

await Bun.write(outPath, trace.map((e) => JSON.stringify(e)).join("\n") + "\n");

console.log(`wrote ${trace.length} frames → ${outPath}`);
for (const entry of trace) {
  const fr = entry.frame as { type?: string };
  console.log(`  ${entry.phase.padEnd(7)} ${entry.dir}  ${fr.type}`);
}
