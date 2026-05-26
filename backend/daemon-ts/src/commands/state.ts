import fs from "node:fs";
import path from "node:path";

import type { ConversationEngine } from "../engine/engine.ts";
import type { Message } from "../engine/types.ts";
import { loadConfig, resolveDisplayName } from "../config/loader.ts";
import { loadCatalog, type ResolvedModel } from "../llm/catalog.ts";
import {
  findEffectiveModel,
  listEffectiveModels,
  type EffectiveModel,
} from "../llm/effective_catalog.ts";
import { buildProvider, resolveApiKey } from "../llm/generate.ts";
import { MarkdownMemoryStore, type MarkdownEntry } from "../memory/markdown_store.ts";
import { runCompaction, type RunCompactionResult } from "../memory/compaction/background.ts";
import { RealCompactionLlm } from "../memory/compaction/llm.ts";
import { runLibrarianSweep } from "../memory/dreaming.ts";
import { isDueNow } from "../memory/dreaming_schedule.ts";
import { dreamsLogPath, recentDreamEntries } from "../memory/dreams_log.ts";
import {
  characterPreferencesPath,
  defaultModelPreferences,
  globalPreferencesPath,
  loadForCharacter,
  loadPreferences,
  modelPreference,
  resetModelSelection,
  setModelSetting as persistModelSetting,
  switchModelSelection,
  type SamplerKey,
} from "../preferences/index.ts";
import {
  resolveBackgroundModel,
  resolveChatModelForCharacter,
  resolveSamplerScopes,
  resolveSamplerSettings,
} from "../preferences/resolve.ts";
import {
  SAMPLER_KEYS,
  isSelectedModelSet,
  type SamplerScopes,
  type SamplerSettingValue,
  type SamplerSettings,
} from "../preferences/types.ts";
import {
  asArgs,
  CommandError,
  mapUnknownError,
  toSnakeModel,
  type CommandContext,
  type RuntimeConfigState,
} from "./types.ts";
import {
  loadProviderRegistry,
  providersForPreferences,
} from "./providers.ts";

export function status(ctx: CommandContext, engine: ConversationEngine): Record<string, unknown> {
  const pending = pendingDeferredEditPaths(engine.dataDir());
  const active = ctx.activeModel
    ?? resolveActiveModel(ctx, true)?.qualifiedName
    ?? ctx.runtime.config.app.defaults.model
    ?? null;
  const activity = ctx.autonomy.activityStats(engine.name());
  return {
    character: engine.name(),
    message_count: engine.turnCount(),
    turn_count: engine.turnCount(),
    active_model: active,
    config_dir: ctx.configSource.configDir,
    data_dir: ctx.dataDir,
    cache_dir: ctx.cacheDir,
    memory_mode: "markdown",
    pending_deferred_edit_count: pending.length,
    pending_deferred_edits: pending,
    tokens: { input: 0, output: 0, cache_read: 0, cache_write: 0 },
    autonomy: ctx.autonomy.status(engine.name()) ?? null,
    activity: activity === undefined
      ? null
      : {
        hour_histogram: activity.stats.hourHistogram,
        hour_classifications: activity.stats.hourClassifications,
        has_sufficient_heatmap: activity.stats.hasSufficientHeatmap,
        engagement_score: activity.stats.engagementScore,
        sessions_per_day: activity.stats.sessionsPerDay,
        message_count: activity.messageCount,
        turn_count: activity.messageCount,
      },
  };
}

export function diagnostics(_ctx: CommandContext, _rawArgs: unknown): Record<string, unknown> {
  return {
    api_calls: { count: 0, recent: [] },
    tool_calls: { count: 0, recent: [] },
    errors: { count: 0, recent: [] },
    key_fallbacks: { count: 0, recent: [] },
  };
}

export function heartbeatLog(
  ctx: CommandContext,
  engine: ConversationEngine,
  rawArgs: unknown,
): Record<string, unknown> {
  const args = asArgs(rawArgs);
  const limit = typeof args["count"] === "number" ? Math.max(0, Math.floor(args["count"])) : 20;
  return {
    events: ctx.autonomy.heartbeatLog(engine.name(), limit).map((event) => ({
      timestamp: event.timestamp,
      kind: event.kind,
      detail: event.detail,
    })),
  };
}

export function heartbeatTickNow(ctx: CommandContext, engine: ConversationEngine): Record<string, unknown> {
  const dormant = ctx.autonomy.heartbeatTickNow(engine.name());
  if (dormant === undefined) {
    throw new CommandError("invalid_request", `No autonomy state for character '${engine.name()}'`);
  }
  return {
    status: "scheduled",
    character: engine.name(),
    ...(dormant
      ? {
        warning:
          "Heartbeat is dormant. The scheduled tick will be suppressed by the abandonment guard. Run `shore debug heartbeat_status_active` first to wake the clock.",
      }
      : {}),
  };
}

export function heartbeatSetDormant(ctx: CommandContext, engine: ConversationEngine): Record<string, unknown> {
  if (!ctx.autonomy.heartbeatSetDormant(engine.name())) {
    throw new CommandError("invalid_request", `No autonomy state for character '${engine.name()}'`);
  }
  return { status: "dormant", character: engine.name() };
}

export function heartbeatSetActive(ctx: CommandContext, engine: ConversationEngine): Record<string, unknown> {
  if (!ctx.autonomy.heartbeatSetActive(engine.name())) {
    throw new CommandError("invalid_request", `No autonomy state for character '${engine.name()}'`);
  }
  return { status: "active", character: engine.name() };
}

export function listModels(ctx: CommandContext, rawArgs: unknown): Record<string, unknown> {
  const args = asArgs(rawArgs);
  const includeHidden = args["include_hidden"] === true;
  const entries = listEffectiveModels(ctx, includeHidden);
  const active = activeModelName(ctx, entries);
  const hiddenCount = includeHidden
    ? entries.filter((entry) => entry.hidden).length
    : listEffectiveModels(ctx, true).filter((entry) => entry.hidden).length;
  return {
    models: entries.map((entry) => ({
      name: entry.resolved.name,
      qualified_name: entry.resolved.qualifiedName,
      sdk: entry.resolved.sdk,
      provider: entry.resolved.providerKey,
      model_id: entry.resolved.modelId,
      source: entry.source,
      hidden: entry.hidden,
    })),
    active,
    include_hidden: includeHidden,
    hidden_count: hiddenCount,
  };
}

export function modelInfo(ctx: CommandContext, rawArgs: unknown): Record<string, unknown> {
  const args = asArgs(rawArgs);
  const name = typeof args["name"] === "string" && args["name"].length > 0 ? args["name"] : undefined;
  const resolved = name === undefined ? resolveActiveModelOrThrow(ctx) : findEffectiveModel(ctx, name, true);
  const data = toSnakeModel(resolved);
  addSamplerInfo(ctx, resolved, data);
  return data;
}

export function switchModel(ctx: CommandContext, rawArgs: unknown): Record<string, unknown> {
  const args = asArgs(rawArgs);
  const name = typeof args["name"] === "string" ? args["name"] : undefined;
  if (name === undefined || name.length === 0) {
    return { active: ctx.activeModel ?? resolveActiveModel(ctx, true)?.qualifiedName ?? null };
  }
  if (ctx.characterName === undefined) {
    throw new CommandError("invalid_request", "this command requires an attached character");
  }
  const includeHidden = args["include_hidden"] === true;
  const resolved = findEffectiveModel(ctx, name, includeHidden);
  switchModelSelection(ctx.dataDir, ctx.characterName, resolved.providerKey, resolved.modelId);
  ctx.activeModel = name;
  return {
    active: name,
    qualified_name: resolved.qualifiedName,
    provider: resolved.providerKey,
    model_id: resolved.modelId,
    changed: true,
  };
}

export function resetModel(ctx: CommandContext): Record<string, unknown> {
  if (ctx.characterName === undefined) {
    throw new CommandError("invalid_request", "this command requires an attached character");
  }
  const previousActive = ctx.activeModel ?? resolveActiveModel(ctx, true)?.qualifiedName ?? null;
  const { previous } = resetModelSelection(ctx.dataDir, ctx.characterName);
  delete ctx.activeModel;
  return {
    previous: previousActive,
    previous_provider: previous.provider ?? null,
    previous_model_id: previous.model_id ?? null,
    active: null,
    reset_to: "config default",
  };
}

export function setModelSetting(ctx: CommandContext, rawArgs: unknown): Record<string, unknown> {
  const args = asArgs(rawArgs);
  const key = typeof args["key"] === "string" ? args["key"].trim() : "";
  if (!SAMPLER_KEYS.includes(key as SamplerKey)) {
    throw new CommandError(
      "invalid_request",
      `unknown setting key: ${key}; supported: ${SAMPLER_KEYS.join(", ")}`,
    );
  }
  const scope = typeof args["scope"] === "string" ? args["scope"] : "character";
  if (scope !== "character" && scope !== "global") {
    throw new CommandError("invalid_request", `scope must be "character" or "global", got ${JSON.stringify(scope)}`);
  }
  if (scope === "character" && ctx.characterName === undefined) {
    throw new CommandError("invalid_request", "this command requires an attached character");
  }
  const active = resolveActiveModelOrThrow(ctx);
  const value = parseSamplerValue(key as SamplerKey, args["value"] as SamplerSettingValue);
  try {
    persistModelSetting(
      ctx.dataDir,
      scope,
      ctx.characterName,
      active.providerKey,
      active.modelId,
      key as SamplerKey,
      value,
    );
  } catch (e) {
    mapUnknownError(e);
  }
  return {
    changed: true,
    scope,
    model: active.qualifiedName,
    provider: active.providerKey,
    model_id: active.modelId,
    key,
    value: value ?? null,
  };
}

export function modelSettings(ctx: CommandContext, rawArgs: unknown): Record<string, unknown> {
  const args = asArgs(rawArgs);
  const active = typeof args["name"] === "string" && args["name"].length > 0
    ? findEffectiveModel(ctx, args["name"], true)
    : resolveActiveModelOrThrow(ctx);
  const [global, character] = ctx.characterName === undefined
    ? [defaultModelPreferences(), defaultModelPreferences()]
    : loadForCharacter(ctx.dataDir, ctx.characterName);
  const charPrefs = ctx.characterName === undefined ? undefined : character;
  const sampler = resolveSamplerSettings(global, charPrefs, active.providerKey, active.modelId, active);
  const scopes = resolveSamplerScopes(global, charPrefs, active.providerKey, active.modelId, active);
  return {
    model: active.qualifiedName,
    provider: active.providerKey,
    model_id: active.modelId,
    effective_sampler: samplerPayload(sampler),
    saved_global: optionalSamplerPayload(modelPreference(global, active.providerKey, active.modelId)?.sampler),
    saved_character: optionalSamplerPayload(modelPreference(character, active.providerKey, active.modelId)?.sampler),
    scopes: scopesPayload(scopes),
  };
}

export function memoryChangelog(
  ctx: CommandContext,
  engine: ConversationEngine,
  rawArgs: unknown,
): Record<string, unknown> {
  const args = asArgs(rawArgs);
  const limit = typeof args["limit"] === "number" ? Math.max(0, Math.floor(args["limit"])) : 20;
  const dreamsPath = dreamsLogPath(ctx.dataDir, engine.name());
  if (!fs.existsSync(dreamsPath)) return { changelog: [], character: engine.name() };
  const content = fs.readFileSync(dreamsPath, "utf8");
  const sections = parseDreamSections(content)
    .map((section) => {
      const [first, ...rest] = section.split("\n");
      const heading = (first ?? "").replace(/^##\s*/u, "").trim();
      const description = rest.join("\n").trim();
      const dreamPrefix = "Dream Cycle - ";
      let timestamp = "";
      let operation = heading;
      if (heading.startsWith(dreamPrefix)) {
        timestamp = heading.slice(dreamPrefix.length);
        operation = "dream_cycle";
      } else {
        const split = heading.indexOf(" - ");
        if (split >= 0) {
          timestamp = heading.slice(0, split);
          operation = heading.slice(split + 3);
        }
      }
      return { timestamp, operation, description };
    })
    .reverse()
    .slice(0, limit);
  return { changelog: sections, character: engine.name() };
}

export async function memoryDreams(
  ctx: CommandContext,
  engine: ConversationEngine,
  rawArgs: unknown,
): Promise<Record<string, unknown>> {
  const args = asArgs(rawArgs);
  const limit = typeof args["limit"] === "number" ? Math.max(0, Math.floor(args["limit"])) : 10;
  const p = dreamsLogPath(ctx.dataDir, engine.name());
  const entries = await recentDreamEntries(ctx.dataDir, engine.name(), limit);
  return {
    character: engine.name(),
    entries,
    path: p,
    exists: fs.existsSync(p),
  };
}

export async function memoryDream(
  ctx: CommandContext,
  engine: ConversationEngine,
  rawArgs: unknown,
): Promise<Record<string, unknown>> {
  const args = asArgs(rawArgs);
  const statusOnly = args["status"] === true;
  const dryRun = args["dry_run"] === true;
  const force = args["force"] === true;
  if (statusOnly) return dreamStatus(ctx, engine.name());

  const resolved = resolveBackgroundModel(preferenceConfig(ctx), "dreaming", engine.name());
  if (resolved === undefined) {
    throw new CommandError("internal_error", "No model configured");
  }
  let apiKey: string;
  try {
    apiKey = resolveApiKey(resolved);
  } catch (e) {
    throw new CommandError("provider_error", (e as Error).message);
  }
  const result = await runLibrarianSweep({
    configDir: ctx.configSource.configDir,
    dataDir: ctx.dataDir,
    cacheDir: ctx.cacheDir,
    character: engine.name(),
    displayName: resolveDisplayName(ctx.runtime.config),
    resolved,
    apiKey,
    provider: buildProvider(resolved.sdk),
    engine,
    dreamingConfig: ctx.runtime.config.memory.dreaming,
    dryRun,
    force,
    ledger: ctx.ledger,
    retrievalConfig: ctx.runtime.config.memory.retrieval,
  });
  if (result !== undefined) return result as unknown as Record<string, unknown>;
  return {
    character: engine.name(),
    status: "not_due",
    enabled: ctx.runtime.config.memory.dreaming.enabled,
    frequency: ctx.runtime.config.memory.dreaming.frequency,
  };
}

export async function memory(
  ctx: CommandContext,
  engine: ConversationEngine,
  rawArgs: unknown,
): Promise<Record<string, unknown>> {
  const args = asArgs(rawArgs);
  const query = typeof args["query"] === "string" && args["query"].length > 0
    ? args["query"]
    : undefined;
  const store = MarkdownMemoryStore.open(characterMemoryDir(ctx.configSource.configDir, engine.name()));
  if (query === undefined) {
    const entries = store.listAll();
    return {
      character: engine.name(),
      entries: entries.length,
      curated_files: entries.filter((e) => !e.path.startsWith("daily/") && !e.path.startsWith("images/")).length,
      daily_files: entries.filter((e) => e.path.startsWith("daily/")).length,
      image_files: entries.filter((e) => e.path.startsWith("images/")).length,
    };
  }
  const hits = store.searchText(query);
  return {
    character: engine.name(),
    query,
    result: formatDirectMemoryResponse(query, hits),
  };
}

export async function compact(
  ctx: CommandContext,
  engine: ConversationEngine,
  rawArgs: unknown,
): Promise<Record<string, unknown>> {
  const args = asArgs(rawArgs);
  if (engine.messages().length === 0) {
    throw new CommandError("invalid_request", "No messages to compact");
  }
  const dryRun = args["dry_run"] === true;
  const keepTurnsOverride = typeof args["keep_turns"] === "number"
    ? Math.max(0, Math.floor(args["keep_turns"]))
    : undefined;
  const resolved = resolveBackgroundModel(preferenceConfig(ctx), "compaction", engine.name());
  if (resolved === undefined) {
    throw new CommandError("internal_error", "No model configured");
  }
  let apiKey: string;
  try {
    apiKey = resolveApiKey(resolved);
  } catch (e) {
    throw new CommandError("provider_error", (e as Error).message);
  }
  const llm = new RealCompactionLlm({
    resolved,
    apiKey,
    ledger: ctx.ledger,
    character: engine.name(),
  });

  const cachedRequest = ctx.autonomy.cachedLastRequest(engine.name());
  let result: RunCompactionResult;
  try {
    result = await runCompaction({
      character: engine.name(),
      dataDir: ctx.dataDir,
      configDir: ctx.configSource.configDir,
      config: {
        ...ctx.runtime.config.memory.compaction,
        ...(keepTurnsOverride !== undefined ? { keepRecentTurns: keepTurnsOverride } : {}),
      },
      displayName: resolveDisplayName(ctx.runtime.config),
      llm,
      ...(cachedRequest !== undefined ? { cachedRequest } : {}),
    });
  } catch (e) {
    ctx.autonomy.notifyCompactionFailed(engine.name());
    throw e;
  }
  await engine.reload();
  ctx.autonomy.notifyCompactionComplete(engine.name(), result.retainedTurns);
  const outcome = result.outcome;
  if (outcome?.kind === "dryRun" || dryRun) {
    const dry = outcome?.kind === "dryRun" ? outcome.result : undefined;
    return {
      status: "dry_run",
      character: engine.name(),
      would_write_files: dry?.wouldWriteFiles ?? 0,
      file_ops_preview: (dry?.fileOpsPreview ?? []).map((op) => ({
        path: op.path,
        content_preview: op.content.slice(0, 200),
      })),
      message_count: dry?.compactedTurns ?? 0,
      turn_count: dry?.compactedTurns ?? 0,
      compacted_turns: dry?.compactedTurns ?? 0,
      retained_count: dry?.retainedCount ?? 0,
      retained_turns: dry?.retainedTurns ?? result.retainedTurns,
    };
  }
  if (outcome?.kind === "compacted") {
    const compacted = outcome.result;
    return {
      status: "compacted",
      character: engine.name(),
      memory_files_written: compacted.memoryFilesWritten,
      message_count: compacted.compactedTurns,
      turn_count: compacted.compactedTurns,
      compacted_turns: compacted.compactedTurns,
      retained_count: compacted.retainedCount,
      retained_turns: compacted.retainedTurns,
      new_conversation_id: compacted.newConversationId,
    };
  }
  throw new CommandError("invalid_request", "No messages to compact");
}

export function config(ctx: CommandContext, rawArgs: unknown): Record<string, unknown> {
  const args = asArgs(rawArgs);
  const key = typeof args["key"] === "string" ? args["key"] : undefined;
  const value = typeof args["value"] === "string" ? args["value"] : undefined;
  if (key !== undefined && value !== undefined) return configSet(ctx, key, value);
  const app = rustAppConfig(ctx);
  if (key === undefined || key.length === 0) return { config: app };
  const section = app[key];
  if (section === undefined) {
    throw new CommandError("not_found", `Config section not found: ${key}`);
  }
  return { key, config: section };
}

export function configCheck(ctx: CommandContext): Record<string, unknown> {
  const warnings: string[] = [];
  const info: string[] = [];
  const chatModels = [...ctx.runtime.catalog.values()].filter((m) => m.category === "chat").length;
  const toolModels = [...ctx.runtime.catalog.values()].filter((m) => m.category === "tools").length;
  if (chatModels === 0) warnings.push("No chat models configured. Add [chat.*] sections to config.");
  else info.push(`${chatModels} chat model(s) configured`);
  const defaultModel = ctx.runtime.config.app.defaults.model;
  if (defaultModel !== undefined) {
    try {
      findEffectiveModel(ctx, defaultModel, true);
      info.push(`Default model: ${defaultModel}`);
    } catch {
      warnings.push(`Default model "${defaultModel}" not found in catalog`);
    }
  } else if (chatModels > 0) {
    warnings.push("No default model set. First chat model will be used.");
  }
  if (toolModels === 0) info.push("No tool models configured (chat models will be used for tools)");
  else info.push(`${toolModels} tool model(s) configured`);
  warnings.push("No LLM service configured. Set [services.llm].command or [services.llm].socket.");
  for (const model of ctx.runtime.catalog.values()) {
    if (model.category !== "chat" || model.apiKeyEnv === undefined) continue;
    if (process.env[model.apiKeyEnv] === undefined) {
      warnings.push(`API key env var $${model.apiKeyEnv} not set (needed by model ${model.qualifiedName})`);
    }
  }
  return {
    valid: warnings.length === 0,
    warnings,
    info,
    config_dir: ctx.configSource.configDir,
    data_dir: ctx.dataDir,
    cache_dir: ctx.cacheDir,
    chat_models: chatModels,
    tool_models: toolModels,
    memory_mode: "markdown",
  };
}

export function configReset(ctx: CommandContext): Record<string, unknown> {
  try {
    const next: RuntimeConfigState = {
      config: loadConfig(ctx.configSource),
      catalog: loadCatalog(ctx.configSource),
      providers: loadProviderRegistry(ctx.configSource),
    };
    delete ctx.activeModel;
    ctx.autonomy.reloadRuntimeConfig(next.config);
    ctx.reloadRuntimeConfig(next);
    return {
      reset: true,
      message: "Configuration reloaded from disk",
      config_path: ctx.configSource.configFile ?? path.join(ctx.configSource.configDir, "config.toml"),
      invalidated: {
        runtime_overrides: true,
      },
    };
  } catch (e) {
    throw new CommandError("internal_error", `Failed to reload config: ${(e as Error).message}`);
  }
}

function configSet(ctx: CommandContext, key: string, value: string): Record<string, unknown> {
  switch (key) {
    case "defaults.model":
    case "model": {
      findEffectiveModel(ctx, value, true);
      ctx.activeModel = value;
      return { set: key, value };
    }
    case "defaults.stream":
    case "stream": {
      const parsed = parseBool(value);
      (ctx.runtime.config.app.defaults as Record<string, unknown>)["stream"] = parsed;
      return { set: key, value: parsed };
    }
    case "autonomy.enabled":
    case "behavior.autonomy.enabled": {
      const parsed = parseBool(value);
      ctx.runtime.config.app.behavior.autonomy.enabled = parsed;
      ctx.autonomy.reloadRuntimeConfig(ctx.runtime.config);
      return { set: "autonomy.enabled", value: parsed };
    }
    default:
      throw new CommandError(
        "invalid_request",
        `Config key not settable at runtime: ${key}. Supported: defaults.model, defaults.stream, autonomy.enabled`,
      );
  }
}

function rustAppConfig(ctx: CommandContext): Record<string, unknown> {
  const cfg = ctx.runtime.config;
  const heartbeat = cfg.app.behavior.autonomy.heartbeat;
  const compaction = cfg.memory.compaction;
  const notifications = cfg.app.notifications;
  const daemon = cfg.app.daemon;
  return {
    daemon: {
      addr: daemon.addr,
      unsafe_allow_remote_access: daemon.unsafe_allow_remote_access,
      allowed_hosts: [...daemon.allowed_hosts],
    },
    defaults: {
      model: cfg.app.defaults.model ?? null,
      background: {
        model: null,
        heartbeat: null,
        compaction: null,
        dreaming: null,
      },
      heartbeat: null,
      dreaming: null,
      embedding: cfg.app.defaults.embedding ?? null,
      image_generation: null,
      display_name: cfg.app.defaults.display_name ?? null,
      stream: true,
    },
    behavior: {
      autonomy: {
        enabled: cfg.app.behavior.autonomy.enabled,
        heartbeat: {
          enabled: heartbeat.enabled,
          fallback_heartbeat_interval: formatDurationSecs(heartbeat.fallbackHeartbeatIntervalSecs),
          dormant_after_heartbeat_turns: heartbeat.dormantAfterHeartbeatTurns,
          dormant_after_idle_time: formatDurationSecs(heartbeat.dormantAfterIdleTimeSecs),
          minimum_heartbeat_latency: formatDurationSecs(heartbeat.minimumHeartbeatLatencySecs),
          max_tool_rounds: heartbeat.maxToolRounds,
          wrap_up_grace_rounds: heartbeat.wrapUpGraceRounds,
        },
      },
      tool_use: {
        enabled: true,
        max_iterations: 10,
        tools: {},
        search: {
          api_key_env: "TAVILY_API_KEY",
          max_results: 5,
          search_depth: "basic",
          include_answer: true,
        },
      },
    },
    memory: {
      compaction: {
        enabled: compaction.enabled,
        idle_trigger: formatDurationSecs(compaction.idleTriggerSecs),
        min_turns: compaction.minTurns,
        max_turns: compaction.maxTurns,
        max_context_tokens: compaction.maxContextTokens,
        keep_recent_turns: compaction.keepRecentTurns,
      },
      dreaming: {
        enabled: cfg.memory.dreaming.enabled,
        frequency: cfg.memory.dreaming.frequency,
        max_tool_rounds: cfg.memory.dreaming.max_tool_rounds,
      },
      thinking: {
        preserve_prior_turns: true,
      },
      retrieval: cfg.memory.retrieval,
    },
    connections: {
      matrix: null,
      telegram: null,
      discord: null,
    },
    services: {
      llm: {
        command: null,
        socket: null,
      },
    },
    notifications: {
      enabled: notifications.enabled,
      backend: "notify_send",
      ntfy: {
        url: "https://ntfy.sh",
        topic: "",
        token: "",
      },
      command: {
        template: "",
      },
      generation_threshold: formatDurationSecs(notifications.generation_threshold_ms / 1000),
      events: {
        autonomous_message: notifications.events.autonomous_message,
        cache_warning: true,
        compaction_complete: notifications.events.compaction_complete,
        error: notifications.events.error,
        message_complete: notifications.events.message_complete,
        usage_warning: notifications.events.usage_warning,
      },
    },
    usage: cfg.app.usage,
    advanced: {
      api_payload_logging: false,
      cache_forensics: cfg.app.advanced.cache_forensics,
      editor: null,
      max_retries: null,
      retry_backoff: null,
      max_image_size: 2_000_000,
    },
  };
}

function formatDurationSecs(seconds: number): string {
  if (seconds === 0) return "0s";
  if (Number.isInteger(seconds) && seconds % 86_400 === 0) return `${seconds / 86_400}d`;
  if (Number.isInteger(seconds) && seconds % 3_600 === 0) return `${seconds / 3_600}h`;
  if (Number.isInteger(seconds) && seconds % 60 === 0) return `${seconds / 60}m`;
  return `${seconds}s`;
}

function resolveActiveModel(ctx: CommandContext, includeHidden: boolean): ResolvedModel | undefined {
  if (ctx.characterName !== undefined) {
    const resolved = resolveChatModelForCharacter(preferenceConfig(ctx), ctx.characterName);
    if (resolved !== undefined) return resolved;
  }
  if (ctx.activeModel !== undefined) {
    try {
      return findEffectiveModel(ctx, ctx.activeModel, includeHidden);
    } catch {
      return undefined;
    }
  }
  const configured = ctx.runtime.config.app.defaults.model;
  if (configured !== undefined) {
    try {
      return findEffectiveModel(ctx, configured, includeHidden);
    } catch {
      return undefined;
    }
  }
  return listEffectiveModels(ctx, includeHidden)[0]?.resolved;
}

function resolveActiveModelOrThrow(ctx: CommandContext): ResolvedModel {
  const active = resolveActiveModel(ctx, true);
  if (active === undefined) {
    throw new CommandError("invalid_request", "No model specified and no active model set");
  }
  return active;
}

function activeModelName(ctx: CommandContext, entries: EffectiveModel[]): string | null {
  return resolveActiveModel(ctx, true)?.qualifiedName ?? entries[0]?.resolved.qualifiedName ?? null;
}

function preferenceConfig(ctx: CommandContext) {
  return {
    catalog: ctx.runtime.catalog,
    dataDir: ctx.dataDir,
    cacheDir: ctx.cacheDir,
    ...(ctx.runtime.config.app.defaults.model !== undefined
      ? { appDefaultModel: ctx.runtime.config.app.defaults.model }
      : {}),
    providers: providersForPreferences(ctx.runtime.providers),
  };
}

function addSamplerInfo(ctx: CommandContext, resolved: ResolvedModel, data: Record<string, unknown>): void {
  if (ctx.characterName === undefined) return;
  const [global, character] = loadForCharacter(ctx.dataDir, ctx.characterName);
  data["effective_sampler"] = samplerPayload(resolveSamplerSettings(
    global,
    character,
    resolved.providerKey,
    resolved.modelId,
    resolved,
  ));
  data["scopes"] = scopesPayload(resolveSamplerScopes(
    global,
    character,
    resolved.providerKey,
    resolved.modelId,
    resolved,
  ));
}

function samplerPayload(sampler: SamplerSettings): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const key of SAMPLER_KEYS) out[key] = sampler[key] ?? null;
  return out;
}

function optionalSamplerPayload(sampler: SamplerSettings | undefined): Record<string, unknown> | null {
  return sampler === undefined ? null : samplerPayload(sampler);
}

function scopesPayload(scopes: SamplerScopes): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const key of SAMPLER_KEYS) out[key] = scopes[key] ?? null;
  return out;
}

function parseSamplerValue(key: SamplerKey, value: SamplerSettingValue): SamplerSettingValue {
  if (value === null || value === undefined) return null;
  switch (key) {
    case "temperature":
    case "top_p":
      if (typeof value !== "number" || !Number.isFinite(value)) {
        throw new CommandError("invalid_request", `${key} must be a number, got ${JSON.stringify(value)}`);
      }
      return value;
    case "reasoning_effort":
    case "cache_ttl":
      if (typeof value !== "string") {
        throw new CommandError("invalid_request", `${key} must be a string, got ${JSON.stringify(value)}`);
      }
      return value;
    case "thinking_enabled":
      if (typeof value !== "boolean") {
        throw new CommandError("invalid_request", `${key} must be a boolean, got ${JSON.stringify(value)}`);
      }
      return value;
    case "budget_tokens":
    case "max_tokens":
      if (typeof value !== "number" || !Number.isInteger(value) || value < 0 || value > 0xffff_ffff) {
        throw new CommandError(
          "invalid_request",
          `${key} must be a non-negative integer fitting in u32, got ${JSON.stringify(value)}`,
        );
      }
      return value;
  }
}

function characterMemoryDir(configDir: string, character: string): string {
  return path.join(configDir, "characters", character, "workspace", "memory");
}

function pendingDeferredEditPaths(characterDataDir: string): string[] {
  const queue = path.join(characterDataDir, "deferred_edits.jsonl");
  let content: string;
  try {
    content = fs.readFileSync(queue, "utf8");
  } catch {
    return [];
  }
  const paths = new Set<string>();
  for (const line of content.split("\n")) {
    if (line.trim().length === 0) continue;
    try {
      const parsed = JSON.parse(line) as { path?: unknown };
      if (typeof parsed.path === "string" && parsed.path.length > 0 && !parsed.path.includes("..")) {
        paths.add(parsed.path);
      }
    } catch {
      // Best-effort, like Rust.
    }
  }
  return [...paths].sort();
}

function dreamStatus(ctx: CommandContext, character: string): Record<string, unknown> {
  const statePath = path.join(ctx.dataDir, character, "dreams", "state.json");
  const state = readDreamState(statePath);
  const cfg = ctx.runtime.config.memory.dreaming;
  return {
    character,
    enabled: cfg.enabled,
    frequency: cfg.frequency,
    last_run_at: state.last_run_at ?? null,
    due: cfg.enabled && isDueNow(cfg.frequency, state.last_run_at),
    state_path: statePath,
    dreams_path: dreamsLogPath(ctx.dataDir, character),
  };
}

function readDreamState(statePath: string): { last_run_at?: string; runs: number } {
  try {
    const parsed = JSON.parse(fs.readFileSync(statePath, "utf8")) as Record<string, unknown>;
    return {
      ...(typeof parsed["last_run_at"] === "string" ? { last_run_at: parsed["last_run_at"] } : {}),
      runs: typeof parsed["runs"] === "number" ? parsed["runs"] : 0,
    };
  } catch {
    return { runs: 0 };
  }
}

function parseDreamSections(content: string): string[] {
  return content
    .split("\n## ")
    .map((section) => section.trim())
    .filter((section) => section.length > 0 && !section.startsWith("# Dreams"))
    .map((section) => section.startsWith("## ") ? section : `## ${section}`);
}

function formatDirectMemoryResponse(query: string, hits: MarkdownEntry[]): string {
  if (hits.length === 0) return `No memory files matched '${query}'.`;
  const lines = [`Top memory matches for '${query}':`];
  for (const entry of hits.slice(0, 10)) {
    lines.push(`- ${entry.path}\n  ${excerptForQuery(entry.content, query, 220)}`);
  }
  return lines.join("\n");
}

function excerptForQuery(text: string, query: string, limit: number): string {
  const normalized = query.trim().toLowerCase();
  const terms = normalized
    .split(/[^a-z0-9_-]+/i)
    .filter((term) => term.length >= 2);
  const lines = text.split("\n");
  for (let idx = 0; idx < lines.length; idx++) {
    const line = (lines[idx] ?? "").trim();
    if (line.length === 0) continue;
    const lower = line.toLowerCase();
    if (lower.includes(normalized) || terms.some((term) => lower.includes(term))) {
      const window = lines
        .slice(Math.max(0, idx - 1), Math.min(lines.length, idx + 2))
        .map((l) => l.trim())
        .filter((l) => l.length > 0)
        .join(" ");
      return excerpt(window, limit);
    }
  }
  return excerpt(text, limit);
}

function excerpt(text: string, limit: number): string {
  const normalized = text.split("\n").map((line) => line.trim()).join(" ");
  return normalized.length > limit ? `${normalized.slice(0, limit)}...` : normalized;
}

function parseBool(value: string): boolean {
  if (value === "true") return true;
  if (value === "false") return false;
  throw new CommandError("invalid_request", "expected true or false");
}
