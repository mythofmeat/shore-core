#!/usr/bin/env bun
/**
 * Capture a handshake trace from the Rust shore-daemon for parity baseline.
 *
 * Usage:
 *   bun scripts/capture-rust-trace.ts <rust-daemon-path> <out-file> \
 *     [--fixture <dir>] [--character <name>]
 *
 *   --fixture <dir>   Path with `config/` and `data/` subdirs to populate
 *                     SHORE_CONFIG_DIR / SHORE_DATA_DIR. Copied into a tmp
 *                     dir before the daemon runs — the Rust daemon scaffolds
 *                     bootstrap files on startup which would otherwise
 *                     pollute the committed fixture.
 *   --character <n>   Send `character: "<n>"` in ClientHello.
 *
 * Writes both directions of the SWP exchange to <out-file> as JSONL with a
 * `dir` field ("s2c" or "c2s"). These traces are the source of truth for
 * "did the TS daemon emit the same bytes" parity checks in later phases.
 */

import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
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

const args = process.argv.slice(2);
const daemonPath = args[0];
const outPath = args[1];
let fixtureDir: string | undefined;
let character: string | undefined;
for (let i = 2; i < args.length; i++) {
  const a = args[i];
  if (a === "--fixture") fixtureDir = resolvePath(args[++i]!);
  else if (a === "--character") character = args[++i];
  else {
    console.error(`unknown arg: ${a}`);
    process.exit(2);
  }
}
if (!daemonPath || !outPath) {
  console.error(
    "usage: capture-rust-trace.ts <rust-daemon-path> <out-file> [--fixture <dir>] [--character <name>]",
  );
  process.exit(2);
}

let configDir: string;
let dataDir: string;
if (fixtureDir) {
  ({ configDir, dataDir } = copyFixtureToTmp(fixtureDir, "shore-rust-trace-fixture-"));
} else {
  configDir = mkdtempSync(join(tmpdir(), "shore-rust-trace-config-"));
  dataDir = mkdtempSync(join(tmpdir(), "shore-rust-trace-data-"));
}
const env = buildDaemonEnv({ configDir, dataDir, prefix: "shore-rust-trace-" });

const proc = spawnDaemon([daemonPath], env);

const trace: Array<{ dir: "s2c" | "c2s"; frame: unknown }> = [];

try {
  const addr = await readListenAddr([proc.stdout, proc.stderr]);
  if (!addr) fail("daemon never printed listen address");

  const { sock, frames } = await openConnection(addr);

  // 1) ServerHello
  const hello = await readFrame(frames);
  trace.push({ dir: "s2c", frame: hello });

  // 2) ClientHello
  const clientHello: Record<string, unknown> = {
    type: "hello",
    client_type: "cli",
    client_name: "rust-trace-capture",
  };
  if (character) clientHello["character"] = character;
  sock.write(JSON.stringify(clientHello) + "\n");
  trace.push({ dir: "c2s", frame: clientHello });

  // 3) History
  const history = await readFrame(frames);
  trace.push({ dir: "s2c", frame: history });

  sock.end();

  await Bun.write(outPath, trace.map((e) => JSON.stringify(e)).join("\n") + "\n");

  console.log(`wrote ${trace.length} frames → ${outPath}`);
  for (const entry of trace) {
    const fr = entry.frame as { type?: string };
    console.log(`  ${entry.dir}  ${fr.type}`);
  }
} finally {
  proc.kill("SIGTERM");
  await proc.exited;
}
