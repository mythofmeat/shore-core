#!/usr/bin/env bun
/**
 * Capture the message-append scenario from the Rust daemon for Phase 3.
 *
 * Flow:
 *   1. Start daemon against a copy of <fixture>.
 *   2. Connect, complete handshake (selecting <character>).
 *   3. Send a ClientMessage{text: "<text>"}.
 *   4. Record one server frame — expected to be NewMessage(user_input).
 *      (The Rust daemon then tries to call an LLM; we don't care, we
 *      disconnect.)
 *   5. Kill daemon. Restart it against the SAME mutated work dir.
 *   6. Re-handshake. Record the History snapshot — it should now contain
 *      the appended user message.
 *
 * Output: a JSONL file with `dir: "s2c" | "c2s"` and `phase: "live" |
 * "restart"` so the parity-check can attribute frames to the right
 * scenario.
 *
 * Usage:
 *   bun scripts/capture-message-append.ts <rust-daemon> <out-file> \
 *     --fixture <dir> --character <name> --text "<message>"
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
let text: string | undefined;
for (let i = 2; i < args.length; i++) {
  const a = args[i];
  if (a === "--fixture") fixtureDir = resolvePath(args[++i]!);
  else if (a === "--character") character = args[++i];
  else if (a === "--text") text = args[++i];
  else {
    console.error(`unknown arg: ${a}`);
    process.exit(2);
  }
}
if (!daemonPath || !outPath || !fixtureDir || !character || text === undefined) {
  console.error(
    "usage: capture-message-append.ts <rust-daemon> <out> --fixture <dir> --character <name> --text <text>",
  );
  process.exit(2);
}

const { configDir, dataDir } = copyFixtureToTmp(fixtureDir, "shore-msg-append-");
const env = buildDaemonEnv({ configDir, dataDir, prefix: "shore-msg-" });

const trace: Array<{ dir: "s2c" | "c2s"; phase: "live" | "restart"; frame: unknown }> = [];

// ── live phase ──────────────────────────────────────────────────────────

{
  const proc = spawnDaemon([daemonPath], env);
  try {
    const addr = await readListenAddr([proc.stdout, proc.stderr]);
    if (!addr) fail("daemon never printed listen address (live)");

    const { sock, frames } = await openConnection(addr);

    // Handshake
    const hello = await readFrame(frames);
    trace.push({ dir: "s2c", phase: "live", frame: hello });

    const clientHello = {
      type: "hello",
      client_type: "cli",
      client_name: "msg-append-capture",
      character,
    };
    sock.write(JSON.stringify(clientHello) + "\n");
    trace.push({ dir: "c2s", phase: "live", frame: clientHello });

    const history = await readFrame(frames);
    trace.push({ dir: "s2c", phase: "live", frame: history });

    // Send the user message. The daemon will emit several frames in
    // response (a History broadcast from engine.append_message, a
    // NewMessage event_tx broadcast, and eventually an LLM-attempt error
    // since no model is configured). The wire ordering of these is racy
    // — broadcasts and direct responses go through different concurrent
    // tasks — so we don't try to capture them deterministically. The
    // persistence assertion (restart phase, below) is the actual signal.
    const clientMsg = { type: "message", text };
    sock.write(JSON.stringify(clientMsg) + "\n");
    trace.push({ dir: "c2s", phase: "live", frame: clientMsg });

    // Wait briefly for active.jsonl to be flushed to disk.
    await new Promise((r) => setTimeout(r, 800));

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
      client_name: "msg-append-capture-restart",
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
