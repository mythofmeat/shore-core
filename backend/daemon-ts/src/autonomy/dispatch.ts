import path from "node:path";

import type { LoadedConfig } from "../config/loader.ts";
import { resolveDisplayName } from "../config/loader.ts";
import type { EngineRegistry } from "../engine/engine.ts";
import type { ContentBlock, Message } from "../engine/types.ts";
import {
  enforceBudgetForCall,
  type CallType,
} from "../ledger/budget.ts";
import type { CacheForensics } from "../ledger/cache_forensics.ts";
import type { Ledger } from "../ledger/ledger.ts";
import type { PricingEngine } from "../ledger/pricing.ts";
import type { NotificationService } from "../notifications/service.ts";
import {
  buildProvider,
  cloneChatRequest,
  generateResponse,
  prepareChatRequest,
  resolveApiKey,
} from "../llm/generate.ts";
import type { Embedder } from "../llm/embed.ts";
import { loadCatalog, type ResolvedModel } from "../llm/catalog.ts";
import {
  findEffectiveModel,
  type EffectiveProviderRegistry,
} from "../llm/effective_catalog.ts";
import type { ChatEvent, ChatRequest, UsageStats } from "../llm/types.ts";
import {
  ensureActivePromptSnapshot,
  HEARTBEAT_FILE,
  loadActivePromptFile,
} from "../memory/deferred_edits.ts";
import { workspaceIndexPath } from "../memory/workspace_index.ts";
import type { ServerMessage } from "../swp/types.ts";
import { defaultRegistry } from "../tools/registry.ts";

import { CacheKeepaliveAction } from "./cache_keepalive.ts";
import { HeartbeatAction } from "./heartbeat.ts";
import type { AutonomyRegistry, TickActions } from "./registry.ts";

const WRAP_UP_NUDGE_TEXT = "[System nudge: heartbeat tool-use budget reached. Wrap up now - "
  + "if you have unfinished work, edit HEARTBEAT.md so future-you can pick it up where you left off. "
  + "Then either send a final <sendMessage> or respond HEARTBEAT_OK and stop.]";

const DEFAULT_HEARTBEAT_INSTRUCTIONS = "# HEARTBEAT\n\n"
  + "- Use this private turn however seems useful.\n"
  + "- You may use tools, schedule the next wake, or send {user} a message.\n"
  + "- If nothing needs action, respond HEARTBEAT_OK.";

export interface AutonomyDispatchOptions {
  characterName: string;
  actions: TickActions;
  engines: EngineRegistry;
  configDir: string;
  cacheDir: string;
  config: LoadedConfig;
  catalog: ReturnType<typeof loadCatalog>;
  providers: EffectiveProviderRegistry;
  ledger: Ledger;
  pricing: PricingEngine;
  autonomy: AutonomyRegistry;
  notifier?: NotificationService;
  cacheForensics?: CacheForensics;
  embedder?: Embedder;
  broadcast: (frame: ServerMessage) => void;
}

export async function runAutonomyTickActions(
  opts: AutonomyDispatchOptions,
): Promise<void> {
  if (opts.actions.heartbeat === HeartbeatAction.RunTick) {
    await executeHeartbeatTick(opts);
  }
  if (opts.actions.keepalive === CacheKeepaliveAction.Ping) {
    await executeKeepalivePing(opts);
  }
  opts.autonomy.flushHeartbeatLog(opts.characterName);
}

async function executeHeartbeatTick(opts: AutonomyDispatchOptions): Promise<void> {
  const resolved = resolveActiveModel(opts);
  if (resolved === undefined) return;
  const engine = opts.engines.get(opts.characterName);
  opts.autonomy.ensureState(engine);
  opts.autonomy.pushHeartbeatEvent(opts.characterName, "tick_fired", "Heartbeat tick fired");
  void opts.pricing.getOrFetch(resolved.providerKey, resolved.modelId);

  const blocked = enforceBudgetForCall(
    opts.ledger,
    opts.config.app.usage,
    {
      provider: resolved.providerKey,
      model: resolved.modelId,
      call_type: "heartbeat",
      character: opts.characterName,
    },
    new Date(),
  );
  if (blocked !== undefined) {
    opts.autonomy.pushHeartbeatEvent(
      opts.characterName,
      "message_skipped",
      `Heartbeat skipped by usage budget: ${blocked.message}`,
    );
    return;
  }

  try {
    ensureActivePromptSnapshot(engine.dataDir(), opts.configDir, opts.characterName);
  } catch (e) {
    console.warn(
      `[shore-daemon-ts] heartbeat snapshot prep failed for ${opts.characterName}: ${(e as Error).message}`,
    );
  }

  const displayName = resolveDisplayName(opts.config);
  const heartbeat = opts.config.app.behavior.autonomy.heartbeat;
  const systemSuffix = buildHeartbeatSystemSuffix(
    engine.dataDir(),
    displayName,
    heartbeat.fallbackHeartbeatIntervalSecs,
  );
  const registry = defaultRegistry({
    characterName: opts.characterName,
    displayName,
  });

  try {
    const result = await generateResponse({
      engine,
      characterConfigDir: path.join(opts.configDir, "characters", opts.characterName),
      configDir: opts.configDir,
      displayName,
      resolved,
      registry,
      broadcast: () => {},
      ledger: opts.ledger,
      pricing: opts.pricing,
      ...(opts.cacheForensics !== undefined ? { cacheForensics: opts.cacheForensics } : {}),
      retrievalConfig: opts.config.memory.retrieval,
      ...(opts.embedder !== undefined ? { embedder: opts.embedder } : {}),
      ...(opts.embedder !== undefined
        ? { workspaceIndexPath: workspaceIndexPath(opts.cacheDir, opts.characterName) }
        : {}),
      activityStats: (name) => {
        const snap = opts.autonomy.activityStats(name);
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
      scheduleNextWake: (hours, reason) =>
        opts.autonomy.scheduleNextWake(opts.characterName, hours, reason),
      systemSuffix,
      persistTurns: false,
      maxIterations: heartbeat.maxToolRounds + heartbeat.wrapUpGraceRounds,
      ledgerCallTypes: { first: "heartbeat", loop: "heartbeat_tool_loop" },
      ...(heartbeat.wrapUpGraceRounds > 0
        ? {
            wrapUp: {
              afterIterations: heartbeat.maxToolRounds,
              text: WRAP_UP_NUDGE_TEXT,
              onNudge: () => {
                opts.autonomy.pushHeartbeatEvent(
                  opts.characterName,
                  "tool_use",
                  "Wrap-up nudge: budget reached, model asked to summarize",
                );
              },
            },
          }
        : {}),
    });

    opts.autonomy.onCacheWarmed(opts.characterName);
    const sendText = extractSendMessageFromTurns(result.newTurns);
    if (sendText !== undefined) {
      await persistAutonomousMessage(opts, sendText);
    } else {
      opts.autonomy.pushHeartbeatEvent(
        opts.characterName,
        "message_skipped",
        "Tick completed - no message sent",
      );
    }
  } catch (e) {
    opts.autonomy.pushHeartbeatEvent(
      opts.characterName,
      "message_skipped",
      `Heartbeat failed: ${(e as Error).message}`,
    );
    console.warn(
      `[shore-daemon-ts] heartbeat failed for ${opts.characterName}: ${(e as Error).message}`,
    );
  }
}

async function executeKeepalivePing(opts: AutonomyDispatchOptions): Promise<void> {
  const resolved = resolveActiveModel(opts);
  if (resolved === undefined) {
    opts.autonomy.onKeepaliveFailed(opts.characterName);
    return;
  }
  void opts.pricing.getOrFetch(resolved.providerKey, resolved.modelId);
  const blocked = enforceBudgetForCall(
    opts.ledger,
    opts.config.app.usage,
    {
      provider: resolved.providerKey,
      model: resolved.modelId,
      call_type: "keepalive",
      character: opts.characterName,
    },
    new Date(),
  );
  if (blocked !== undefined) {
    opts.autonomy.onKeepaliveFailed(opts.characterName);
    opts.autonomy.pushHeartbeatEvent(
      opts.characterName,
      "dormant_ping",
      `Cache keepalive ping skipped by usage budget: ${blocked.message}`,
    );
    return;
  }

  const base = buildKeepaliveBaseRequest(opts, resolved);
  if (base === undefined) {
    opts.autonomy.onKeepaliveFailed(opts.characterName);
    opts.autonomy.pushHeartbeatEvent(
      opts.characterName,
      "dormant_ping",
      "Cache keepalive ping skipped: no cached or rebuildable request",
    );
    return;
  }

  const request = buildKeepalivePing(base, opts.characterName);
  if (opts.cacheForensics !== undefined) {
    request.cacheForensics = opts.cacheForensics;
    request.forensicCharacter = opts.characterName;
  }
  try {
    const provider = buildProvider(resolved.sdk);
    const call = await streamOnce(provider.stream(request));
    opts.ledger.recordCall({
      provider: resolved.providerKey,
      model: resolved.modelId,
      callType: "keepalive",
      character: opts.characterName,
      inputTokens: call.usage.inputTokens,
      outputTokens: call.usage.outputTokens,
      cacheReadTokens: call.usage.cacheReadInputTokens,
      cacheWriteTokens: call.usage.cacheCreationInputTokens,
      totalMs: call.totalMs,
      ttftMs: call.ttftMs,
      finishReason: call.stopReason,
      thinkingEnabled: request.thinking.enabled,
      ...(resolved.cacheTtl !== undefined ? { cacheTtl: resolved.cacheTtl } : {}),
      pricing: opts.pricing,
    }, opts.cacheForensics);
    opts.autonomy.onCacheWarmed(opts.characterName);
    opts.autonomy.pushHeartbeatEvent(
      opts.characterName,
      "dormant_ping",
      `Cache refresh ping (cache_read: ${call.usage.cacheReadInputTokens}, input: ${call.usage.inputTokens})`,
    );
    opts.autonomy.pushHeartbeatEvent(
      opts.characterName,
      "dormant_ping",
      "Cache keepalive ping",
    );
  } catch (e) {
    opts.autonomy.onKeepaliveFailed(opts.characterName);
    opts.autonomy.pushHeartbeatEvent(
      opts.characterName,
      "dormant_ping",
      `Cache keepalive ping failed: ${(e as Error).message}`,
    );
  }
}

function resolveActiveModel(opts: AutonomyDispatchOptions): ResolvedModel | undefined {
  const modelName = opts.config.app.defaults.model;
  if (!modelName) return undefined;
  try {
    return findEffectiveModel(
      {
        catalog: opts.catalog,
        providers: opts.providers,
        cacheDir: opts.cacheDir,
      },
      modelName,
      true,
    );
  } catch (e) {
    console.warn(
      `[shore-daemon-ts] autonomy model resolution failed for ${opts.characterName}: ${(e as Error).message}`,
    );
    return undefined;
  }
}

function buildHeartbeatSystemSuffix(
  characterDataDir: string,
  displayName: string,
  defaultIntervalSecs: number,
): string {
  const instructions = (loadActivePromptFile(characterDataDir, HEARTBEAT_FILE)
    ?? DEFAULT_HEARTBEAT_INSTRUCTIONS).replaceAll("{user}", displayName);
  return `${instructions}\n\n${buildHeartbeatPrompt(displayName, formatInterval(defaultIntervalSecs))}`;
}

function buildHeartbeatPrompt(userName: string, defaultInterval: string): string {
  const now = formatHeartbeatNow();
  return `[Current time: ${now}]\n\n`
    + "[This is a private heartbeat turn governed by the active HEARTBEAT.md content above. "
    + "You have real tools and can search or write workspace and memory files, search "
    + "your conversation history, check the web, generate images, and schedule the next wake.\n\n"
    + "In addition, you can:\n\n"
    + "- Schedule your next heartbeat session: use set_next_wake(hours_from_now, reason). "
    + "The minimum is 1 hour, the maximum is 48 hours. Sooner if you want to come back "
    + "to something, later if you'd rather rest. If you don't schedule, your next moment "
    + `will arrive in ${defaultInterval}. This is the next opportunity you will have to send `
    + `${userName} an autonomous message or to continue any unfinished or ongoing work from `
    + "this current heartbeat session.\n\n"
    + `- Send a message to ${userName}: wrap it in <sendMessage>...</sendMessage>. `
    + `You have the ability to autonomously and spontaneously send messages to ${userName}. `
    + `Any text included in the sendMessage tags will be delivered to ${userName}.\n\n`
    + "Thoughts, tool-use results, and any text in your response that is not part of "
    + "<sendMessage> tags are private and ephemeral. If you want to carry something "
    + "forward, write it down with a workspace tool.\n\n"
    + "If you have a multi-step task in progress and want future-you to pick it up, "
    + "edit HEARTBEAT.md to record what you were doing and what to come back to. "
    + "HEARTBEAT.md is read into your prompt at the start of every heartbeat tick, "
    + "so notes you leave there will be visible to your next session.\n\n"
    + "Changes you make to workspace files, including files under memory/, will persist. "
    + "If nothing needs doing right now, respond with HEARTBEAT_OK and stop.]";
}

function formatHeartbeatNow(): string {
  const now = new Date();
  return new Intl.DateTimeFormat(undefined, {
    weekday: "long",
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "numeric",
    minute: "2-digit",
  }).format(now);
}

function formatInterval(secs: number): string {
  if (secs >= 3600 && secs % 3600 === 0) {
    const h = secs / 3600;
    return h === 1 ? "1 hour" : `${h} hours`;
  }
  return `${Math.max(1, Math.round(secs / 60))} minutes`;
}

function extractSendMessageFromTurns(turns: Array<{ role: string; content: ContentBlock[] }>): string | undefined {
  let out: string | undefined;
  for (const turn of turns) {
    if (turn.role !== "assistant") continue;
    const text = turn.content
      .filter((b): b is Extract<ContentBlock, { type: "text" }> => b.type === "text")
      .map((b) => b.text)
      .join("");
    const extracted = extractTag(text, "<sendMessage>", "</sendMessage>");
    if (extracted !== undefined) out = extracted;
  }
  return out;
}

function extractTag(content: string, startTag: string, endTag: string): string | undefined {
  let result: string | undefined;
  let searchFrom = 0;
  while (true) {
    const start = content.indexOf(startTag, searchFrom);
    if (start < 0) break;
    const innerStart = start + startTag.length;
    const end = content.indexOf(endTag, innerStart);
    if (end < 0) break;
    const inner = content.slice(innerStart, end).trim();
    if (inner.length > 0) result = inner;
    searchFrom = end + endTag.length;
  }
  return result;
}

async function persistAutonomousMessage(
  opts: AutonomyDispatchOptions,
  text: string,
): Promise<void> {
  const engine = opts.engines.get(opts.characterName);
  const msg: Message = {
    msg_id: `m_${crypto.randomUUID()}`,
    role: "assistant",
    content: text,
    images: [],
    content_blocks: [{ type: "text", text }],
    timestamp: rfc3339LocalNow(),
  };
  await engine.appendMessage(msg);
  const revision = engine.historySnapshot().revision;
  opts.broadcast({
    type: "new_message",
    revision,
    character: opts.characterName,
    origin: "autonomous",
    msg_id: msg.msg_id,
    role: msg.role,
    content: msg.content,
    images: msg.images,
    content_blocks: msg.content_blocks,
    timestamp: msg.timestamp,
  });
  opts.autonomy.pushHeartbeatEvent(
    opts.characterName,
    "message_sent",
    `Autonomous message sent: ${text.slice(0, 80)}`,
  );
  opts.notifier?.notify(
    "autonomous_message",
    `Shore — ${opts.characterName}`,
    text,
  );
}

function buildKeepaliveBaseRequest(
  opts: AutonomyDispatchOptions,
  resolved: ResolvedModel,
): ChatRequest | undefined {
  const cached = opts.autonomy.cachedLastRequest(opts.characterName);
  if (cached !== undefined) return cached;

  const engine = opts.engines.get(opts.characterName);
  const messages = engine.historySnapshot().messages;
  if (messages.length === 0 || messages[messages.length - 1]?.role !== "assistant") {
    return undefined;
  }
  const displayName = resolveDisplayName(opts.config);
  const registry = defaultRegistry({
    characterName: opts.characterName,
    displayName,
  });
  let apiKey: string;
  try {
    apiKey = resolveApiKey(resolved);
  } catch {
    return undefined;
  }
  const request = prepareChatRequest({
    engine,
    characterConfigDir: path.join(opts.configDir, "characters", opts.characterName),
    configDir: opts.configDir,
    displayName,
    resolved,
    registry,
    apiKey,
  });
  opts.autonomy.notifyLastRequest(opts.characterName, request);
  return cloneChatRequest(request);
}

function buildKeepalivePing(base: ChatRequest, character: string): ChatRequest {
  const ping = cloneChatRequest(base);
  ping.maxTokens = 1;
  ping.forensicCharacter = character;
  ping.messages = [
    ...ping.messages,
    { role: "user", content: [{ type: "text", text: "." }] },
  ];
  return ping;
}

async function streamOnce(events: AsyncIterable<ChatEvent>): Promise<{
  usage: UsageStats;
  stopReason: string;
  totalMs: number;
  ttftMs: number;
}> {
  const started = Date.now();
  let firstOutputAt: number | undefined;
  for await (const event of events) {
    if (event.kind !== "done" && firstOutputAt === undefined) {
      firstOutputAt = Date.now();
    }
    if (event.kind === "done") {
      const totalMs = Date.now() - started;
      return {
        usage: event.usage,
        stopReason: event.stopReason,
        totalMs,
        ttftMs: firstOutputAt === undefined ? totalMs : firstOutputAt - started,
      };
    }
  }
  throw new Error("provider stream ended without a done event");
}

function rfc3339LocalNow(): string {
  const now = new Date();
  const tzOffsetMinutes = -now.getTimezoneOffset();
  const sign = tzOffsetMinutes >= 0 ? "+" : "-";
  const abs = Math.abs(tzOffsetMinutes);
  const tzh = String(Math.floor(abs / 60)).padStart(2, "0");
  const tzm = String(abs % 60).padStart(2, "0");
  const pad = (n: number, w = 2): string => String(n).padStart(w, "0");
  const ms = String(now.getMilliseconds()).padStart(3, "0");
  return (
    `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}` +
    `T${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}.${ms}${sign}${tzh}:${tzm}`
  );
}
