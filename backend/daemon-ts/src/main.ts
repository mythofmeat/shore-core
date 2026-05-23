#!/usr/bin/env bun
/**
 * shore-daemon-ts entry point.
 *
 * Phase 0: start the SWP server, register in instances.json, idle until
 * SIGINT/SIGTERM. No engine, no LLM, no real messages — just enough to
 * pass the handshake with the existing Rust CLI.
 */

import { resolveShoreDirs } from "./runtime/dirs.ts";
import { Registry } from "./runtime/registry.ts";
import { SwpServer } from "./swp/server.ts";

interface ParsedArgs {
  addr: string;
  instanceId: string | undefined;
}

function parseArgs(argv: string[]): ParsedArgs {
  let addr: string | undefined;
  let instanceId: string | undefined;

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--addr") {
      addr = argv[++i];
    } else if (arg === "--instance-id") {
      instanceId = argv[++i];
    } else if (arg === "--help" || arg === "-h") {
      printHelpAndExit(0);
    } else if (arg !== undefined) {
      console.error(`error: unknown argument: ${arg}`);
      printHelpAndExit(1);
    }
  }

  if (!addr) {
    addr = process.env["SHORE_ADDR"] ?? "127.0.0.1:0";
  }

  return { addr, instanceId };
}

function printHelpAndExit(code: number): never {
  console.error(
    [
      "shore-daemon-ts — TypeScript reimplementation of shore-daemon.",
      "",
      "USAGE:",
      "  shore-daemon-ts [OPTIONS]",
      "",
      "OPTIONS:",
      "  --addr <HOST:PORT>     TCP listen address (default: 127.0.0.1:0)",
      "  --instance-id <ID>     Pin the registered instance ID",
      "  -h, --help             Print this help",
      "",
      "See REWRITE.md for the current rewrite phase.",
    ].join("\n"),
  );
  process.exit(code);
}

function splitAddr(addr: string): { host: string; port: number } {
  const idx = addr.lastIndexOf(":");
  if (idx < 0) {
    console.error(`error: --addr must be HOST:PORT, got ${addr}`);
    process.exit(2);
  }
  const host = addr.slice(0, idx);
  const portStr = addr.slice(idx + 1);
  const port = Number.parseInt(portStr, 10);
  if (!Number.isFinite(port) || port < 0 || port > 65535) {
    console.error(`error: invalid port: ${portStr}`);
    process.exit(2);
  }
  return { host, port };
}

function rfc3339Now(): string {
  return new Date().toISOString();
}

function generateInstanceId(): string {
  // RFC 4122 v4 — Bun has crypto.randomUUID built in.
  return crypto.randomUUID();
}

async function main(): Promise<void> {
  const { addr, instanceId } = parseArgs(process.argv.slice(2));
  const { host, port } = splitAddr(addr);

  const dirs = resolveShoreDirs();
  const id = instanceId ?? generateInstanceId();

  const server = new SwpServer({
    host,
    port,
    serverName: "shore-daemon-ts",
    onClient: (clientType, clientName) => {
      console.log(`[shore-daemon-ts] client connected: type=${clientType} name=${clientName}`);
    },
  });
  const listen = server.start();

  const registry = Registry.atDefault(dirs.runtime);
  registry.register({
    id,
    pid: process.pid,
    addr: `${listen.host}:${listen.port}`,
    started_at: rfc3339Now(),
    data_dir: dirs.data,
    config_dir: dirs.config,
  });

  console.log(`[shore-daemon-ts] listening on ${listen.host}:${listen.port} (id=${id}, pid=${process.pid})`);
  console.log(`[shore-daemon-ts] registry: ${registry.path()}`);

  const shutdown = (signal: string) => {
    console.log(`[shore-daemon-ts] received ${signal}, shutting down`);
    try {
      registry.unregister(id);
    } catch (e) {
      console.error(`[shore-daemon-ts] registry unregister failed: ${(e as Error).message}`);
    }
    server.stop();
    process.exit(0);
  };

  process.on("SIGINT", () => shutdown("SIGINT"));
  process.on("SIGTERM", () => shutdown("SIGTERM"));
  process.on("SIGHUP", () => shutdown("SIGHUP"));

  // Idle. Bun keeps the event loop alive while the TCP listener is open.
}

await main();
