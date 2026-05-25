#!/usr/bin/env bun
/**
 * Capture the edit-command scenario from the Rust daemon.
 *
 * Flow:
 *   1. Start daemon against a copy of <fixture>.
 *   2. Handshake (selecting <character>).
 *   3. Send Command{name: "edit", args: {ref, content}}.
 *   4. Read the command response (deterministic — direct response, not
 *      an async broadcast).
 *   5. Kill daemon. Restart against the same mutated work dir.
 *   6. Re-handshake. Record History — message <ref> should show the new
 *      content; all other messages should be untouched.
 *
 * Output: JSONL with `dir: "s2c" | "c2s"` and `phase: "live" | "restart"`.
 *
 * Usage:
 *   bun scripts/capture-edit.ts <rust-daemon> <out-file> \
 *     --fixture <dir> --character <name> --ref <msg_id_or_index> \
 *     --content "<new text>"
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
let content: string | undefined;
for (let i = 2; i < args.length; i++) {
  const a = args[i];
  if (a === "--fixture") fixtureDir = resolvePath(args[++i]!);
  else if (a === "--character") character = args[++i];
  else if (a === "--ref") ref = args[++i];
  else if (a === "--content") content = args[++i];
  else {
    console.error(`unknown arg: ${a}`);
    process.exit(2);
  }
}
if (!daemonPath || !outPath || !fixtureDir || !character || !ref || content === undefined) {
  console.error(
    "usage: capture-edit.ts <rust-daemon> <out> --fixture <dir> --character <name> --ref <msg_id_or_index> --content <text>",
  );
  process.exit(2);
}

const { configDir, dataDir } = copyFixtureToTmp(fixtureDir, "shore-edit-");
const env = buildDaemonEnv({ configDir, dataDir, prefix: "shore-edit-" });

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
      client_name: "edit-capture",
      character,
    };
    sock.write(JSON.stringify(clientHello) + "\n");
    trace.push({ dir: "c2s", phase: "live", frame: clientHello });

    const history = await readFrame(frames);
    trace.push({ dir: "s2c", phase: "live", frame: history });

    const editCmd = {
      type: "command",
      rid: "r1",
      name: "edit",
      args: { ref, content },
    };
    sock.write(JSON.stringify(editCmd) + "\n");
    trace.push({ dir: "c2s", phase: "live", frame: editCmd });

    // The edit command emits two frames after the command: a History
    // broadcast (via event_tx, contains the mutated message) and a
    // command_output (via direct_tx, contains the ack payload). They
    // can arrive in either order across the two concurrent tasks. We
    // read both and sort by type so the baseline is order-independent.
    const r1 = (await readFrame(frames)) as Record<string, unknown>;
    const r2 = (await readFrame(frames)) as Record<string, unknown>;
    const sorted = [r1, r2].sort((a, b) =>
      String(a["type"]).localeCompare(String(b["type"])),
    );
    for (const frame of sorted) {
      trace.push({ dir: "s2c", phase: "live", frame });
    }

    // Brief flush window before tearing the daemon down.
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
      client_name: "edit-capture-restart",
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
