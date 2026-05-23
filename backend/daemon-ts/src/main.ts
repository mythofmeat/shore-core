#!/usr/bin/env bun
/**
 * shore-daemon-ts entry point.
 *
 * Phase 3: handshake snapshot reads from a persistent EngineRegistry,
 * ClientMessage appends to the active.jsonl via the engine, and engine
 * broadcasts fan out to all connected clients as History frames. No LLM
 * call yet — that's Phase 4.
 */

import path from "node:path";

import { characterMetadata, discoverCharacters } from "./characters/registry.ts";
import { loadConfig, firstChatModelQualifiedName, type LoadedConfig } from "./config/loader.ts";
import { EngineRegistry } from "./engine/engine.ts";
import type { Message } from "./engine/types.ts";
import { loadCatalog, resolveModel, type ResolvedModel } from "./llm/catalog.ts";
import { loadConfigDotenv } from "./llm/env.ts";
import { generateResponse } from "./llm/generate.ts";
import { defaultRegistry } from "./llm/tools/registry.ts";
import { resolveShoreDirs } from "./runtime/dirs.ts";
import { Registry } from "./runtime/registry.ts";
import { SwpServer } from "./swp/server.ts";
import type { HandshakeProvider, MessageHandler } from "./swp/server.ts";

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

  // Load .env into process.env so provider clients can resolve API
  // keys via process.env[<api_key_env>]. Override semantics matches
  // dotenvy::from_path_override in the Rust daemon.
  loadConfigDotenv(dirs.config);

  const config = loadConfig(dirs.config);
  const catalog = loadCatalog(dirs.config);

  // EngineRegistry is constructed before the server so we can wire the
  // broadcast callback at engine-construction time (engines are lazily
  // created on first use; each one captures the broadcast target).
  let serverRef: SwpServer | undefined;
  const engines = new EngineRegistry(dirs.data, {
    onBroadcast: (snapshot) => {
      if (!serverRef) return;
      serverRef.broadcast({
        type: "history",
        messages: snapshot.messages,
        ...(snapshot.active_start !== 0 ? { active_start: snapshot.active_start } : {}),
        // engine.broadcast_history in Rust emits config={} (the
        // active_model/private fields are only added at handshake time).
        config: {},
        selected_character: snapshot.selected_character,
        revision: snapshot.revision,
      });
    },
  });

  const handshake = buildHandshakeProvider(config, dirs.config, engines);
  const onMessage = buildMessageHandler(engines, dirs.config, config, catalog, () => serverRef);

  const server = new SwpServer({
    host,
    port,
    serverName: "shore-daemon-ts",
    handshake,
    onMessage,
    onClient: (clientType, clientName, character) => {
      console.log(
        `[shore-daemon-ts] client connected: type=${clientType} name=${clientName} character=${character ?? "<none>"}`,
      );
    },
  });
  serverRef = server;
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

/**
 * Build the handshake provider that mirrors
 * `backend/daemon/src/handshake.rs::build_handshake_provider`.
 *
 * Re-walks character discovery on every connect so newly-added characters
 * appear without a daemon restart. History snapshot returns the no-engine
 * shape when no character is selected (matching Rust's `None => HistorySnapshot`).
 */
function buildHandshakeProvider(
  config: LoadedConfig,
  configDir: string,
  engines: EngineRegistry,
): HandshakeProvider {
  const activeModel = (): string | null =>
    config.app.defaults.model ?? firstChatModelQualifiedName(config) ?? null;

  return {
    helloSnapshot() {
      const names = discoverCharacters(configDir);
      return { characters: names.map((n) => characterMetadata(configDir, n)) };
    },
    historySnapshot(selectedCharacter) {
      const baseConfig = { active_model: activeModel(), private: false };
      if (selectedCharacter === undefined) {
        return {
          messages: [],
          config: baseConfig,
          revision: 0,
        };
      }
      const snap = engines.get(selectedCharacter).historySnapshot();
      return {
        messages: snap.messages,
        ...(snap.active_start !== 0 ? { active_start: snap.active_start } : {}),
        config: baseConfig,
        selected_character: snap.selected_character,
        revision: snap.revision,
      };
    },
  };
}

/**
 * ClientMessage handler. Builds the user-turn `Message` matching the
 * Rust handler in `backend/daemon/src/handler/task.rs` (msg_id format,
 * timestamp format, role, single Text block), appends via the engine,
 * and then drives the assistant generation through the LLM call layer.
 *
 * Phase 4c.1 wires the engine → catalog → provider → tool_loop path
 * end-to-end. Images and the `overrides` field are still ignored.
 */
function buildMessageHandler(
  engines: EngineRegistry,
  configDir: string,
  config: LoadedConfig,
  catalog: ReturnType<typeof loadCatalog>,
  getServer: () => SwpServer | undefined,
): MessageHandler {
  return async (session, msg) => {
    if (session.character === undefined) {
      throw new Error("client sent a message before selecting a character");
    }
    const engine = engines.get(session.character);
    const userMsg: Message = {
      msg_id: `m_${crypto.randomUUID()}`,
      role: "user",
      content: msg.text,
      images: [],
      content_blocks: [{ type: "text", text: msg.text }],
      timestamp: rfc3339LocalNow(),
    };
    await engine.appendMessage(userMsg);

    const modelName = config.app.defaults.model;
    if (!modelName) {
      console.error("[shore-daemon-ts] no app.defaults.model set; skipping generation");
      return;
    }
    let resolved: ResolvedModel;
    try {
      resolved = resolveModel(catalog, modelName);
    } catch (e) {
      console.error(`[shore-daemon-ts] could not resolve model: ${(e as Error).message}`);
      return;
    }

    const characterConfigDir = path.join(configDir, "characters", session.character);
    const displayName = process.env["USER"] ?? "user";
    const broadcast = (frame: Parameters<NonNullable<ReturnType<typeof getServer>>["broadcast"]>[0]): void => {
      getServer()?.broadcast(frame);
    };

    try {
      await generateResponse({
        engine,
        characterConfigDir,
        displayName,
        resolved,
        registry: defaultRegistry(),
        broadcast,
        ...(msg.rid !== undefined ? { rid: msg.rid } : {}),
      });
    } catch (e) {
      console.error(`[shore-daemon-ts] generation failed: ${(e as Error).message}`);
      broadcast({
        type: "error",
        code: "internal_error",
        message: `generation failed: ${(e as Error).message}`,
      });
    }
  };
}

/**
 * Produce an RFC3339 timestamp with the local timezone offset, matching
 * `chrono::Local::now().to_rfc3339()` in the Rust daemon. Node's
 * `Date.toISOString()` always emits UTC (`Z`), so we format manually.
 */
function rfc3339LocalNow(): string {
  const now = new Date();
  const tzOffsetMinutes = -now.getTimezoneOffset();
  const sign = tzOffsetMinutes >= 0 ? "+" : "-";
  const abs = Math.abs(tzOffsetMinutes);
  const tzh = String(Math.floor(abs / 60)).padStart(2, "0");
  const tzm = String(abs % 60).padStart(2, "0");
  const pad = (n: number, w = 2) => String(n).padStart(w, "0");
  const ms = String(now.getMilliseconds()).padStart(3, "0");
  return (
    `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}` +
    `T${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}.${ms}${sign}${tzh}:${tzm}`
  );
}

await main();
