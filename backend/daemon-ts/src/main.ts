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
import fs from "node:fs";

import { runAutonomyTickActions } from "./autonomy/dispatch.ts";
import {
  buildInlineCompactionRunner,
  type InlineCompactionRunner,
} from "./autonomy/inline_compaction.ts";
import {
  buildInlineDreamingRunner,
  type InlineDreamingRunner,
} from "./autonomy/inline_dreaming.ts";
import { AutonomyRegistry } from "./autonomy/registry.ts";
import { characterMetadata, discoverCharacters } from "./characters/registry.ts";
import { dispatchCommand } from "./commands/dispatch.ts";
import { loadProviderRegistry } from "./commands/providers.ts";
import type { RuntimeConfigState } from "./commands/types.ts";
import type { ImageRef } from "./engine/types.ts";
import {
  firstChatModelQualifiedName,
  loadConfig,
  resolveDisplayName,
  type LoadedConfig,
} from "./config/loader.ts";
import { EngineRegistry } from "./engine/engine.ts";
import type { Message } from "./engine/types.ts";
import { loadCatalog, type ResolvedModel } from "./llm/catalog.ts";
import { findEffectiveModel } from "./llm/effective_catalog.ts";
import {
  enforceBudgetForCall,
  newlyCrossedBudgetWarnings,
  type BudgetBlock,
  type CallType,
} from "./ledger/budget.ts";
import { CacheForensics } from "./ledger/cache_forensics.ts";
import { Ledger } from "./ledger/ledger.ts";
import { PricingEngine } from "./ledger/pricing.ts";
import { NotificationService } from "./notifications/service.ts";
import { resolveEmbedder, type Embedder } from "./llm/embed.ts";
import { loadConfigDotenv } from "./llm/env.ts";
import { generateResponse } from "./llm/generate.ts";
import { workspaceIndexPath } from "./memory/workspace_index.ts";
import { defaultRegistry, ToolRegistry } from "./tools/registry.ts";
import { resolveShoreDirs } from "./runtime/dirs.ts";
import { Registry } from "./runtime/registry.ts";
import { SwpServer } from "./swp/server.ts";
import type { HandshakeProvider, MessageHandler } from "./swp/server.ts";

interface ParsedArgs {
  addr: string;
  instanceId: string | undefined;
  configPath: string | undefined;
}

function parseArgs(argv: string[]): ParsedArgs {
  let addr: string | undefined;
  let instanceId: string | undefined;
  let configPath: string | undefined;

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--addr") {
      addr = takeArgValue(argv, ++i, arg);
    } else if (arg === "--instance-id") {
      instanceId = takeArgValue(argv, ++i, arg);
    } else if (arg === "--config") {
      configPath = takeArgValue(argv, ++i, arg);
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

  return { addr, instanceId, configPath };
}

function takeArgValue(argv: string[], idx: number, flag: string): string {
  const value = argv[idx];
  if (value === undefined || value.startsWith("--")) {
    console.error(`error: ${flag} requires a value`);
    process.exit(2);
  }
  return value;
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
      "  --config <PATH>       Config file to load (parent becomes config dir)",
      "  --addr <HOST:PORT>     TCP listen address (default: 127.0.0.1:0)",
      "  --instance-id <ID>     Pin the registered instance ID",
      "  -h, --help             Print this help",
      "",
      "See REWRITE.md for the current rewrite phase.",
    ].join("\n"),
  );
  process.exit(code);
}

function resolveExplicitConfigPath(raw: string | undefined): string | undefined {
  if (raw === undefined) return undefined;
  if (!fs.existsSync(raw)) {
    console.error(`error: invalid --config path ${raw}: file does not exist`);
    process.exit(2);
  }
  if (fs.statSync(raw).isDirectory()) {
    console.error(`error: invalid --config path ${raw}: expected a config.toml file, not a directory`);
    process.exit(2);
  }
  return raw;
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
  const { addr, instanceId, configPath } = parseArgs(process.argv.slice(2));
  const { host, port } = splitAddr(addr);

  const explicitConfigFile = resolveExplicitConfigPath(configPath);
  const dirs = resolveShoreDirs(explicitConfigFile);
  const id = instanceId ?? generateInstanceId();

  // Load .env into process.env so provider clients can resolve API
  // keys via process.env[<api_key_env>]. Override semantics matches
  // dotenvy::from_path_override in the Rust daemon.
  loadConfigDotenv(dirs.config);

  const configSource = explicitConfigFile === undefined
    ? { configDir: dirs.config }
    : { configDir: dirs.config, configFile: explicitConfigFile };
  const runtime: RuntimeConfigState = {
    config: loadConfig(configSource),
    catalog: loadCatalog(configSource),
    providers: loadProviderRegistry(configSource),
  };
  let config = runtime.config;
  let catalog = runtime.catalog;
  const ledger = Ledger.open(path.join(dirs.data, "ledger.db"));
  const pricing = new PricingEngine(ledger);
  const notifier = new NotificationService(config.app.notifications);
  const cacheForensics = config.app.advanced.cache_forensics
    ? CacheForensics.open(dirs.cache)
    : undefined;
  if (cacheForensics !== undefined) {
    console.log(`[shore-daemon-ts] cache forensics: ${path.join(dirs.cache, "cache_forensics.jsonl")}`);
  }

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
  let autonomy: AutonomyRegistry;
  let inlineCompaction: InlineCompactionRunner;
  let inlineDreaming: InlineDreamingRunner;
  autonomy = new AutonomyRegistry({
    autonomyConfig: config.app.behavior.autonomy,
    compactionConfig: config.memory.compaction,
    dreamingConfig: config.memory.dreaming,
    autoStartTicker: true,
    onIdleCompaction: (characterName) => inlineCompaction(characterName),
    onScheduledDream: (characterName) => inlineDreaming(characterName),
    onTickActions: (characterName, actions): Promise<void> => {
      const embedder = resolveOptionalEmbedder(config);
      return runAutonomyTickActions({
        characterName,
        actions,
        engines,
        configDir: dirs.config,
        cacheDir: dirs.cache,
        config,
        catalog,
        providers: runtime.providers,
        ledger,
        pricing,
        autonomy,
        notifier,
        ...(cacheForensics !== undefined ? { cacheForensics } : {}),
        ...(embedder !== undefined ? { embedder } : {}),
        broadcast: (frame) => serverRef?.broadcast(frame),
      });
    },
  });
  inlineCompaction = buildInlineCompactionRunner({
    engines,
    config,
    dataDir: dirs.data,
    configDir: dirs.config,
    compactionConfig: config.memory.compaction,
    catalog,
    ledger,
    autonomy,
    notifier,
    ...(cacheForensics !== undefined ? { cacheForensics } : {}),
    broadcast: (frame) => serverRef?.broadcast(frame),
  });
  inlineDreaming = buildInlineDreamingRunner({
    engines,
    config,
    dataDir: dirs.data,
    configDir: dirs.config,
    cacheDir: dirs.cache,
    dreamingConfig: config.memory.dreaming,
    catalog,
    ledger,
    autonomy,
    ...(cacheForensics !== undefined ? { cacheForensics } : {}),
    ...((): { embedder?: Embedder } => {
      const e = resolveOptionalEmbedder(config);
      return e !== undefined ? { embedder: e } : {};
    })(),
    retrievalConfig: config.memory.retrieval,
  });

  const handshake = buildHandshakeProvider(config, dirs.config, engines, autonomy);
  const onMessage = buildMessageHandler(
    engines,
    dirs.config,
    dirs.cache,
    config,
    catalog,
    runtime.providers,
    ledger,
    pricing,
    autonomy,
    notifier,
    cacheForensics,
    () => serverRef,
    (characterName, rid) => inlineCompaction(characterName, rid),
  );
  const onRegen = buildRegenHandler(
    engines,
    dirs.config,
    dirs.cache,
    config,
    catalog,
    runtime.providers,
    ledger,
    pricing,
    autonomy,
    notifier,
    cacheForensics,
    () => serverRef,
    (characterName, rid) => inlineCompaction(characterName, rid),
  );
  const onCommand = buildCommandHandler(
    engines,
    dirs.config,
    dirs.data,
    dirs.cache,
    configSource,
    runtime,
    autonomy,
    ledger,
    pricing,
    (next) => {
      runtime.config = next.config;
      runtime.catalog = next.catalog;
      runtime.providers = next.providers;
      config = next.config;
      catalog = next.catalog;
    },
    () => serverRef,
  );

  const server = new SwpServer({
    host,
    port,
    serverName: "shore-daemon-ts",
    handshake,
    onMessage,
    onRegen,
    onCommand,
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
    autonomy.stopAll();
    ledger.close();
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
  autonomy: AutonomyRegistry,
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
      const engine = engines.get(selectedCharacter);
      const snap = engine.historySnapshot();
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
  cacheDir: string,
  config: LoadedConfig,
  catalog: ReturnType<typeof loadCatalog>,
  providers: RuntimeConfigState["providers"],
  ledger: Ledger,
  pricing: PricingEngine,
  autonomy: AutonomyRegistry,
  notifier: NotificationService,
  cacheForensics: CacheForensics | undefined,
  getServer: () => SwpServer | undefined,
  inlineCompaction: InlineCompactionRunner,
): MessageHandler {
  return async (session, msg) => {
    if (session.character === undefined) {
      throw new Error("client sent a message before selecting a character");
    }
    const engine = engines.get(session.character);
    // Lazy autonomy state: first message of the process lifetime triggers
    // a 90-day history backfill so heatmap data isn't empty after restart.
    autonomy.ensureState(engine);
    const images = buildImageRefs(msg.images, msg.image_data);
    const userMsg: Message = {
      msg_id: `m_${crypto.randomUUID()}`,
      role: "user",
      content: msg.text,
      images,
      content_blocks: [{ type: "text", text: msg.text }],
      timestamp: rfc3339LocalNow(),
    };
    await engine.appendMessage(userMsg);
    const userRevision = engine.historySnapshot().revision;
    broadcastNewMessage(
      getServer,
      session.character,
      "user_input",
      userRevision,
      userMsg,
    );
    autonomy.notifyUserMessage(session.character, engine.messageCount());

    const modelName = config.app.defaults.model;
    if (!modelName) {
      console.error("[shore-daemon-ts] no app.defaults.model set; skipping generation");
      return;
    }
    let resolved: ResolvedModel;
    try {
      resolved = findEffectiveModel({ catalog, providers, cacheDir }, modelName, true);
    } catch (e) {
      console.error(`[shore-daemon-ts] could not resolve model: ${(e as Error).message}`);
      return;
    }

    const characterConfigDir = path.join(configDir, "characters", session.character);
    const displayName = resolveDisplayName(config);
    const embedder = resolveOptionalEmbedder(config);
    const broadcast = (frame: Parameters<NonNullable<ReturnType<typeof getServer>>["broadcast"]>[0]): void => {
      getServer()?.broadcast(frame);
    };

    // Best-effort pricing warm-up: populates the catalog cache so the
    // ledger row gets per-component costs without blocking generation if
    // OpenRouter is slow or unreachable.
    void pricing.getOrFetch(resolved.providerKey, resolved.modelId);

    const block = enforceBudgetForCall(
      ledger,
      config.app.usage,
      {
        provider: resolved.providerKey,
        model: resolved.modelId,
        call_type: "message",
        character: session.character,
      },
      new Date(),
    );
    if (block !== undefined) {
      emitBudgetBlock(broadcast, msg.rid, block);
      return;
    }

    let generateResult: import("./llm/generate.ts").GenerateResult | undefined;
    const startedAtMs = Date.now();
    try {
      generateResult = await generateResponse({
        engine,
        characterConfigDir,
        configDir,
        displayName,
        resolved,
        registry: registryForGeneration(config, session.character, displayName),
        maxIterations: config.app.behavior.tool_use.max_iterations,
        broadcast,
        ledger,
        pricing,
        ...(cacheForensics !== undefined ? { cacheForensics } : {}),
        retrievalConfig: config.memory.retrieval,
        ...(embedder !== undefined ? { embedder } : {}),
        ...(embedder !== undefined
          ? { workspaceIndexPath: workspaceIndexPath(cacheDir, session.character) }
          : {}),
        activityStats: (name) => {
          const snap = autonomy.activityStats(name);
          if (snap === undefined) return undefined;
          return {
            hourHistogram: snap.stats.hourHistogram,
            hourClassifications: snap.stats.hourClassifications,
            hasSufficientHeatmap: snap.stats.hasSufficientHeatmap,
            engagementScore: snap.stats.engagementScore,
            sessionsPerDay: snap.stats.sessionsPerDay,
            turnCount: snap.messageCount,
          };
        },
        signal: msg.signal,
        ...(msg.rid !== undefined ? { rid: msg.rid } : {}),
        ...(msg.overrides ? { overrides: msg.overrides } : {}),
        onPreparedRequest: (request) =>
          autonomy.notifyLastRequest(session.character!, request),
      });
    } catch (e) {
      handleGenerationError(broadcast, msg.rid, e);
      notifier.notify(
        "error",
        `Shore — ${session.character}`,
        (e as Error).message,
      );
    }

    if (generateResult !== undefined) {
      notifier.notifyMessageComplete(
        `Shore — ${session.character}`,
        generateResult.finalText,
        Date.now() - startedAtMs,
      );
    }

    // Post-generation compaction trigger — mirror of
    // `handler/task.rs:349-376`. Uses the fresh engine message count plus
    // the final provider call's input + cache tokens; either max_turns,
    // max_context_tokens, or a tick-pending flag can fire the check.
    // Compaction is fire-and-forget here so the next user message isn't
    // gated on the compaction LLM call completing.
    if (generateResult !== undefined) {
      const messageCount = engine.messageCount();
      const usage = generateResult.finalUsage;
      const contextTokens = usage !== undefined
        ? usage.inputTokens
          + usage.cacheReadInputTokens
          + usage.cacheCreationInputTokens
        : 0;
      if (
        autonomy.shouldCompactNow(session.character, messageCount, contextTokens)
      ) {
        void inlineCompaction(
          session.character,
          ...(msg.rid !== undefined ? [msg.rid] : []),
        );
      }
    }

    emitBudgetWarnings(broadcast, ledger, config.app.usage, notifier);
  };
}

function emitBudgetBlock(
  broadcast: (frame: import("./swp/types.ts").ServerMessage) => void,
  rid: string | undefined,
  block: BudgetBlock,
): void {
  console.warn(
    `[shore-daemon-ts] budget block: ${block.budget_name} ($${block.current_cost.toFixed(2)}/$${block.cost_limit.toFixed(2)} ${block.period}); action=${block.action}`,
  );
  broadcast({
    type: "error",
    ...(rid !== undefined ? { rid } : {}),
    code: "usage_budget_blocked",
    message: block.message,
  });
}

function emitBudgetWarnings(
  broadcast: (frame: import("./swp/types.ts").ServerMessage) => void,
  ledger: Ledger,
  config: import("./ledger/budget.ts").UsageConfig,
  notifier: NotificationService,
): void {
  let events;
  try {
    events = newlyCrossedBudgetWarnings(ledger, config, new Date());
  } catch (e) {
    console.warn(`[shore-daemon-ts] budget warning check failed: ${(e as Error).message}`);
    return;
  }
  for (const event of events) {
    console.warn(`[shore-daemon-ts] budget warning: ${event.message}`);
    broadcast({
      type: "command_output",
      name: "usage_budget_warning",
      data: event as unknown as Record<string, unknown>,
    });
    notifier.notify("usage_warning", "Shore usage warning", event.message);
  }
}

function broadcastNewMessage(
  getServer: () => SwpServer | undefined,
  character: string,
  origin: "user_input" | "assistant_reply" | "autonomous",
  revision: number,
  message: Message,
): void {
  getServer()?.broadcast({
    type: "new_message",
    revision,
    character,
    origin,
    ...message,
  } as import("./swp/types.ts").ServerMessage);
}

/**
 * Decide what to do with a generation error. `AbortError` from the
 * AbortSignal pathway is expected — clients see only the cancellation
 * sentinel, not an internal_error frame. Anything else is surfaced.
 */
function handleGenerationError(
  broadcast: (frame: import("./swp/types.ts").ServerMessage) => void,
  rid: string | undefined,
  e: unknown,
): void {
  const err = e as Error & { name?: string };
  if (err.name === "AbortError" || /aborted/i.test(err.message ?? "")) {
    broadcast({
      type: "stream_end",
      ...(rid !== undefined ? { rid } : {}),
      content: "",
      metadata: {
        tokens: { input: 0, output: 0, cache_read: 0, cache_write: 0 },
        timing: { total_ms: 0, ttft_ms: 0 },
        model: "",
      },
      finish_reason: "cancelled",
      is_final: true,
    });
    return;
  }
  console.error(`[shore-daemon-ts] generation failed: ${err.message}`);
  broadcast({
    type: "error",
    ...(rid !== undefined ? { rid } : {}),
    code: "internal_error",
    message: `generation failed: ${err.message}`,
  });
}

function registryForGeneration(
  config: LoadedConfig,
  characterName: string,
  displayName: string,
): ToolRegistry {
  if (!config.app.behavior.tool_use.enabled) return new ToolRegistry();
  return defaultRegistry({ characterName, displayName });
}

function resolveOptionalEmbedder(config: LoadedConfig): Embedder | undefined {
  if (
    config.app.defaults.embedding === undefined &&
    Object.keys(config.embedding).length === 0
  ) {
    return undefined;
  }
  try {
    return resolveEmbedder(config.app.defaults.embedding, config.embedding);
  } catch (e) {
    console.warn(
      `[shore-daemon-ts] embedder unavailable; semantic file_search disabled: ${(e as Error).message}`,
    );
    return undefined;
  }
}

/**
 * Regen handler. Mirrors Rust's successful-regen path: build the provider
 * request from history through the last real user turn, then atomically replace
 * the old assistant tail after the fresh response completes. The replaced
 * response is preserved as an alternate on the new assistant message.
 */
function buildRegenHandler(
  engines: EngineRegistry,
  configDir: string,
  cacheDir: string,
  config: LoadedConfig,
  catalog: ReturnType<typeof loadCatalog>,
  providers: RuntimeConfigState["providers"],
  ledger: Ledger,
  pricing: PricingEngine,
  autonomy: AutonomyRegistry,
  notifier: NotificationService,
  cacheForensics: CacheForensics | undefined,
  getServer: () => SwpServer | undefined,
  inlineCompaction: InlineCompactionRunner,
): import("./swp/server.ts").RegenHandler {
  return async (session, msg) => {
    if (session.character === undefined) {
      throw new Error("client sent regen before selecting a character");
    }
    const engine = engines.get(session.character);
    autonomy.ensureState(engine);
    const pendingRegenAlt = engine.pendingRegenAlt();
    if (pendingRegenAlt === undefined) {
      throw new Error("nothing to regen: no trailing assistant turn");
    }

    const modelName = config.app.defaults.model;
    if (!modelName) {
      throw new Error("no app.defaults.model set");
    }
    const resolved = findEffectiveModel({ catalog, providers, cacheDir }, modelName, true);
    const characterConfigDir = path.join(configDir, "characters", session.character);
    const displayName = resolveDisplayName(config);
    const embedder = resolveOptionalEmbedder(config);
    const broadcast = (frame: import("./swp/types.ts").ServerMessage): void => {
      getServer()?.broadcast(frame);
    };

    void pricing.getOrFetch(resolved.providerKey, resolved.modelId);

    const block = enforceBudgetForCall(
      ledger,
      config.app.usage,
      {
        provider: resolved.providerKey,
        model: resolved.modelId,
        call_type: "message",
        character: session.character,
      },
      new Date(),
    );
    if (block !== undefined) {
      emitBudgetBlock(broadcast, msg.rid, block);
      return;
    }

    let generateResult: import("./llm/generate.ts").GenerateResult | undefined;
    const startedAtMs = Date.now();
    try {
      generateResult = await generateResponse({
        engine,
        characterConfigDir,
        configDir,
        displayName,
        resolved,
        registry: registryForGeneration(config, session.character, displayName),
        maxIterations: config.app.behavior.tool_use.max_iterations,
        broadcast,
        ledger,
        pricing,
        ...(cacheForensics !== undefined ? { cacheForensics } : {}),
        retrievalConfig: config.memory.retrieval,
        ...(embedder !== undefined ? { embedder } : {}),
        ...(embedder !== undefined
          ? { workspaceIndexPath: workspaceIndexPath(cacheDir, session.character) }
          : {}),
        activityStats: (name) => {
          const snap = autonomy.activityStats(name);
          if (snap === undefined) return undefined;
          return {
            hourHistogram: snap.stats.hourHistogram,
            hourClassifications: snap.stats.hourClassifications,
            hasSufficientHeatmap: snap.stats.hasSufficientHeatmap,
            engagementScore: snap.stats.engagementScore,
            sessionsPerDay: snap.stats.sessionsPerDay,
            turnCount: snap.messageCount,
          };
        },
        signal: msg.signal,
        regen: true,
        regenAlt: pendingRegenAlt,
        ...(msg.rid !== undefined ? { rid: msg.rid } : {}),
        onPreparedRequest: (request) =>
          autonomy.notifyLastRequest(session.character!, request),
      });
    } catch (e) {
      handleGenerationError(broadcast, msg.rid, e);
      notifier.notify(
        "error",
        `Shore — ${session.character}`,
        (e as Error).message,
      );
    }

    if (generateResult !== undefined) {
      notifier.notifyMessageComplete(
        `Shore — ${session.character}`,
        generateResult.finalText,
        Date.now() - startedAtMs,
      );
    }

    // Same post-generation compaction trigger as the message handler; a
    // regen produces new turns and can cross max_turns / max_context_tokens
    // just like a fresh user message.
    if (generateResult !== undefined) {
      const messageCount = engine.messageCount();
      const usage = generateResult.finalUsage;
      const contextTokens = usage !== undefined
        ? usage.inputTokens
          + usage.cacheReadInputTokens
          + usage.cacheCreationInputTokens
        : 0;
      if (
        autonomy.shouldCompactNow(session.character, messageCount, contextTokens)
      ) {
        void inlineCompaction(
          session.character,
          ...(msg.rid !== undefined ? [msg.rid] : []),
        );
      }
    }

    emitBudgetWarnings(broadcast, ledger, config.app.usage, notifier);
  };
}

function buildCommandHandler(
  engines: EngineRegistry,
  configDir: string,
  dataDir: string,
  cacheDir: string,
  configSource: { configDir: string; configFile?: string },
  runtime: RuntimeConfigState,
  autonomy: AutonomyRegistry,
  ledger: Ledger,
  pricing: PricingEngine,
  reloadRuntimeConfig: (next: RuntimeConfigState) => void,
  getServer: () => SwpServer | undefined,
): import("./swp/server.ts").CommandHandler {
  return async (session, msg) => {
    const send = session.send ?? ((frame: import("./swp/types.ts").ServerMessage): void => {
      getServer()?.broadcast(frame);
    });
    const alwaysCharacterless =
      msg.name === "list_characters"
      || msg.name === "list_providers"
      || msg.name === "list_provider_models";
    const characterless = alwaysCharacterless
      || (session.character === undefined
        && (
          msg.name === "list_models"
          || msg.name === "refresh_provider_models"
          || msg.name === "refresh_all_provider_models"
        ));
    const engine = characterless || session.character === undefined
      ? undefined
      : engines.get(session.character);

    const ctx = {
      configSource,
      runtime,
      dataDir,
      cacheDir,
      engines,
      autonomy,
      ledger,
      pricing,
      ...(session.character !== undefined ? { characterName: session.character } : {}),
      reloadRuntimeConfig,
    };

    try {
      const data = await dispatchCommand({
        ctx,
        ...(engine !== undefined ? { engine } : {}),
        name: msg.name,
        args: msg.args,
      });

      if (msg.name === "switch_character" && isRecord(data)) {
        const selected = typeof data["character"] === "string" ? data["character"] : undefined;
        if (selected !== undefined) {
          session.setCharacter?.(selected);
          const selectedEngine = engines.get(selected);
          autonomy.ensureState(selectedEngine);
          const activeModel = runtime.config.app.defaults.model ?? firstChatModelQualifiedName(runtime.config) ?? null;
          const snap = selectedEngine.historySnapshot({ active_model: activeModel, private: false });
          data["selected_character"] = selected;
          data["active_model"] = activeModel;
          data["private"] = false;
          send({
            type: "history",
            ...(msg.rid !== undefined ? { rid: msg.rid } : {}),
            messages: snap.messages,
            ...(snap.active_start !== 0 ? { active_start: snap.active_start } : {}),
            config: snap.config,
            selected_character: snap.selected_character,
            revision: snap.revision,
          });
        }
      }

      if (msg.name === "config_reset" && isRecord(data) && session.character !== undefined) {
        const invalidated = isRecord(data["invalidated"]) ? data["invalidated"] : {};
        invalidated["character_discovery"] = false;
        invalidated["merged_character_configs"] = true;
        invalidated["removed_character_engines"] = 0;
        data["invalidated"] = invalidated;

        const selectedEngine = engines.get(session.character);
        const activeModel = runtime.config.app.defaults.model ?? firstChatModelQualifiedName(runtime.config) ?? null;
        const snap = selectedEngine.historySnapshot({ active_model: activeModel, private: false });
        send({
          type: "history",
          messages: snap.messages,
          ...(snap.active_start !== 0 ? { active_start: snap.active_start } : {}),
          config: snap.config,
          selected_character: snap.selected_character,
          revision: snap.revision,
        });
      }

      send({
        type: "command_output",
        ...(msg.rid !== undefined ? { rid: msg.rid } : {}),
        name: msg.name,
        data,
      });
    } catch (e) {
      const err = e as Error & { code?: string };
      send({
        type: "error",
        ...(msg.rid !== undefined ? { rid: msg.rid } : {}),
        code: err.code ?? "internal_error",
        message: err.message,
      });
    }
  };
}

/**
 * Produce an RFC3339 timestamp with the local timezone offset, matching
 * `chrono::Local::now().to_rfc3339()` in the Rust daemon. Node's
 * `Date.toISOString()` always emits UTC (`Z`), so we format manually.
 */
/**
 * Materialize the ClientMessage's `images` (file paths) + `image_data`
 * (inline base64) into `ImageRef[]`. Inline data wins when both name the
 * same file. The daemon strips `data` before persisting, matching Rust's
 * `ImageRef::serialize` (skip-if-none); we mimic that by clearing `data`
 * on disk later — but at message-build time we want the data attached so
 * the provider can read it without going back to the filesystem.
 */
function buildImageRefs(
  paths: string[],
  inline: Array<{ filename: string; data: string }>,
): ImageRef[] {
  const out: ImageRef[] = [];
  for (const p of paths) out.push({ path: p });
  for (const i of inline) out.push({ path: i.filename, data: i.data });
  return out;
}

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

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

await main();
