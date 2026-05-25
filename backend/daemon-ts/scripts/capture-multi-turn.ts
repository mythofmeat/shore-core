#!/usr/bin/env bun
/**
 * Capture multi-turn message-append scenario from the Rust daemon.
 *
 * Extends the single-message-append flow to N user messages so the
 * Tier 1 parity check covers the case where multiple turns accumulate
 * in `active.jsonl` before a restart.
 *
 * Flow:
 *   1. Start daemon against a copy of <fixture>.
 *   2. Handshake (selecting <character>).
 *   3. Send each ClientMessage{text} from --text, sleeping briefly
 *      between sends so each append has time to flush.
 *   4. Kill daemon. Restart against the SAME mutated work dir.
 *   5. Re-handshake. Record the History snapshot — it should contain
 *      every appended user message in order.
 *
 * Output: JSONL with `dir: "s2c" | "c2s"` and `phase: "live" | "restart"`.
 *
 * Usage:
 *   bun scripts/capture-multi-turn.ts <rust-daemon> <out-file> \
 *     --fixture <dir> --character <name> --text "<m1>" --text "<m2>" [...]
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
const texts: string[] = [];
for (let i = 2; i < args.length; i++) {
  const a = args[i];
  if (a === "--fixture") fixtureDir = resolvePath(args[++i]!);
  else if (a === "--character") character = args[++i];
  else if (a === "--text") texts.push(args[++i]!);
  else {
    console.error(`unknown arg: ${a}`);
    process.exit(2);
  }
}
if (!daemonPath || !outPath || !fixtureDir || !character || texts.length === 0) {
  console.error(
    "usage: capture-multi-turn.ts <rust-daemon> <out> --fixture <dir> --character <name> --text <m1> --text <m2> [...]",
  );
  process.exit(2);
}

const { configDir, dataDir } = copyFixtureToTmp(fixtureDir, "shore-multi-turn-");
const env = buildDaemonEnv({ configDir, dataDir, prefix: "shore-multi-turn-" });

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
      client_name: "multi-turn-capture",
      character,
    };
    sock.write(JSON.stringify(clientHello) + "\n");
    trace.push({ dir: "c2s", phase: "live", frame: clientHello });

    const history = await readFrame(frames);
    trace.push({ dir: "s2c", phase: "live", frame: history });

    // Same rationale as capture-message-append.ts: post-message s2c
    // frames are racy (broadcast vs direct response across concurrent
    // tasks), so capture only the c2s sends. The persistence
    // assertion in the restart phase is the parity signal.
    for (const text of texts) {
      const clientMsg = { type: "message", text };
      sock.write(JSON.stringify(clientMsg) + "\n");
      trace.push({ dir: "c2s", phase: "live", frame: clientMsg });
      await new Promise((r) => setTimeout(r, 600));
    }

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
      client_name: "multi-turn-capture-restart",
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
