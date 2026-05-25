/**
 * Config loader — minimal subset for Phase 2 handshake parity.
 *
 * Loads `config.toml` and `conf.d/*.toml` from `$SHORE_CONFIG_DIR` and
 * deep-merges the parsed objects. Exposes the slices the TS daemon
 * currently needs: default chat/display/embedding selectors, raw
 * `[embedding.*]` profiles, and `[memory.*]` config slices.
 */

import fs from "node:fs";
import path from "node:path";
import { parse as parseToml } from "smol-toml";

import {
  DEFAULT_HEARTBEAT_CONFIG,
  type HeartbeatConfig,
} from "../autonomy/heartbeat.ts";
import {
  defaultSpikeWarningsConfig,
  defaultUsageBudgetConfig,
  type BudgetWeekday,
  type UsageBudgetAction,
  type UsageBudgetConfig,
  type UsageBudgetPeriod,
  type UsageConfig,
  type UsageSpikeWarningsConfig,
} from "../ledger/budget.ts";
import {
  defaultRetrievalConfig,
  type RetrievalBinaryMode,
  type RetrievalConfig,
  type RetrievalMode,
} from "../tools/registry.ts";
import {
  DEFAULT_COMPACTION_CONFIG,
  type CompactionConfig,
} from "../memory/compaction/types.ts";
import {
  DEFAULT_NOTIFICATIONS_CONFIG,
  DEFAULT_NOTIFICATION_EVENTS,
  type NotificationEventsConfig,
  type NotificationsConfig,
} from "../notifications/types.ts";

export interface LoadedConfig {
  app: {
    defaults: {
      model: string | undefined;
      embedding: string | undefined;
      display_name: string | undefined;
    };
    behavior: {
      autonomy: AutonomyConfig;
    };
    advanced: {
      cache_forensics: boolean;
    };
    usage: UsageConfig;
    notifications: NotificationsConfig;
  };
  embedding: Record<string, Record<string, unknown>>;
  memory: {
    compaction: CompactionConfig;
    dreaming: DreamingConfig;
    retrieval: RetrievalConfig;
  };
}

export interface DreamingConfig {
  enabled: boolean;
  frequency: string;
  max_tool_rounds: number;
}

export interface ConfigInput {
  configDir: string;
  configFile?: string;
}

export interface AutonomyConfig {
  enabled: boolean;
  heartbeat: LoadedHeartbeatConfig;
}

export interface LoadedHeartbeatConfig extends HeartbeatConfig {
  enabled: boolean;
  maxToolRounds: number;
  wrapUpGraceRounds: number;
}

/** Load config from a Shore config directory. Missing files are tolerated. */
export function loadConfig(input: string | ConfigInput): LoadedConfig {
  const source = normalizeConfigInput(input);
  const merged = mergeAll(readAllConfigTables(source));

  const defaultsTable = pickTable(merged, "defaults") ?? {};

  return {
    app: {
      defaults: {
        model: typeof defaultsTable["model"] === "string" ? defaultsTable["model"] : undefined,
        embedding:
          typeof defaultsTable["embedding"] === "string"
            ? defaultsTable["embedding"]
            : undefined,
        display_name:
          typeof defaultsTable["display_name"] === "string"
            ? defaultsTable["display_name"]
            : undefined,
      },
      behavior: {
        autonomy: parseAutonomyConfig(pickAutonomyTable(merged)),
      },
      advanced: parseAdvancedConfig(pickTable(merged, "advanced")),
      usage: parseUsageConfig(pickTable(merged, "usage")),
      notifications: parseNotificationsConfig(pickTable(merged, "notifications")),
    },
    embedding: parseEmbeddingProfiles(pickTable(merged, "embedding")),
    memory: {
      compaction: parseCompactionConfig(pickCompactionTable(merged)),
      dreaming: parseDreamingConfig(pickDreamingTable(merged)),
      retrieval: parseRetrievalConfig(pickRetrievalTable(merged)),
    },
  };
}

/**
 * Resolve the display name like the Rust impl
 * (`config.app.defaults.resolve_display_name()`): explicit config wins,
 * else fall back to `$USER`, else "user".
 */
export function resolveDisplayName(config: LoadedConfig): string {
  return config.app.defaults.display_name ?? process.env["USER"] ?? "user";
}

/**
 * First chat-kind model in catalog order. The Rust loader builds a sorted
 * catalog from `[chat.<provider>.<model>]` tables; until that catalog port
 * lands, this always returns undefined and the handshake falls back to
 * `defaults.model` (or null).
 */
export function firstChatModelQualifiedName(_config: LoadedConfig): string | undefined {
  return undefined;
}

// ── internals ──────────────────────────────────────────────────────────

function readAllConfigTables(source: ConfigInput): Record<string, unknown>[] {
  const tables: Record<string, unknown>[] = [];

  const baseFile = source.configFile ?? path.join(source.configDir, "config.toml");
  const baseContent = tryReadText(baseFile);
  if (baseContent !== undefined) tables.push(parseTomlOrFail(baseContent, baseFile));

  const confDir = path.join(source.configDir, "conf.d");
  let extras: string[] = [];
  try {
    extras = fs.readdirSync(confDir).filter((n) => n.endsWith(".toml")).sort();
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code !== "ENOENT") throw e;
  }
  for (const name of extras) {
    const full = path.join(confDir, name);
    const content = tryReadText(full);
    if (content !== undefined) tables.push(parseTomlOrFail(content, full));
  }

  return tables;
}

function tryReadText(file: string): string | undefined {
  try {
    return fs.readFileSync(file, "utf8");
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw e;
  }
}

function normalizeConfigInput(input: string | ConfigInput): ConfigInput {
  if (typeof input === "string") return { configDir: input };
  return input;
}

function parseTomlOrFail(content: string, sourcePath: string): Record<string, unknown> {
  try {
    return parseToml(content) as Record<string, unknown>;
  } catch (e) {
    throw new Error(`failed to parse TOML at ${sourcePath}: ${(e as Error).message}`);
  }
}

/**
 * Deep-merge top-level tables. The Rust loader treats conf.d files as
 * overlays on config.toml: later files override earlier ones for scalar
 * fields, nested tables merge recursively, arrays-of-tables (e.g. multiple
 * `[chat.anthropic.opus]` blocks) extend.
 */
function mergeAll(tables: Record<string, unknown>[]): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const t of tables) deepMerge(out, t);
  return out;
}

function deepMerge(target: Record<string, unknown>, src: Record<string, unknown>): void {
  for (const [key, value] of Object.entries(src)) {
    const prev = target[key];
    if (Array.isArray(prev) && Array.isArray(value)) {
      target[key] = [...prev, ...value];
    } else if (isPlainObject(prev) && isPlainObject(value)) {
      const nested = { ...prev };
      deepMerge(nested, value);
      target[key] = nested;
    } else {
      target[key] = value;
    }
  }
}

function pickTable(obj: Record<string, unknown>, key: string): Record<string, unknown> | undefined {
  const v = obj[key];
  return isPlainObject(v) ? v : undefined;
}

function pickRetrievalTable(
  obj: Record<string, unknown>,
): Record<string, unknown> | undefined {
  const memory = pickTable(obj, "memory");
  if (memory === undefined) return undefined;
  return pickTable(memory, "retrieval");
}

function pickDreamingTable(
  obj: Record<string, unknown>,
): Record<string, unknown> | undefined {
  const memory = pickTable(obj, "memory");
  if (memory === undefined) return undefined;
  return pickTable(memory, "dreaming");
}

function pickCompactionTable(
  obj: Record<string, unknown>,
): Record<string, unknown> | undefined {
  const memory = pickTable(obj, "memory");
  if (memory === undefined) return undefined;
  return pickTable(memory, "compaction");
}

function pickAutonomyTable(
  obj: Record<string, unknown>,
): Record<string, unknown> | undefined {
  const behavior = pickTable(obj, "behavior");
  if (behavior === undefined) return undefined;
  return pickTable(behavior, "autonomy");
}

function parseEmbeddingProfiles(
  table: Record<string, unknown> | undefined,
): Record<string, Record<string, unknown>> {
  if (table === undefined) return {};
  const out: Record<string, Record<string, unknown>> = {};
  for (const [name, value] of Object.entries(table)) {
    if (isPlainObject(value)) out[name] = value;
  }
  return out;
}

/**
 * Mirrors `core/config/src/app.rs::CompactionConfig` defaults. Reads from
 * `[memory.compaction]` with the same field names as Rust (snake_case TOML
 * keys → camelCase TS struct).
 *
 * `idle_trigger` is a duration string ("30m", "1800s", or seconds as a
 * number); Rust uses `ConfigDuration`, here we accept both numeric seconds
 * and the standard `1h` / `30m` / `45s` shorthand parsed by `parseDurationSecs`.
 */
function parseCompactionConfig(
  table: Record<string, unknown> | undefined,
): CompactionConfig {
  if (table === undefined) return { ...DEFAULT_COMPACTION_CONFIG };
  return {
    enabled:
      typeof table["enabled"] === "boolean"
        ? table["enabled"]
        : DEFAULT_COMPACTION_CONFIG.enabled,
    idleTriggerSecs:
      parseDurationSecs(table["idle_trigger"]) ??
      DEFAULT_COMPACTION_CONFIG.idleTriggerSecs,
    minTurns:
      asNumber(table["min_turns"]) ?? DEFAULT_COMPACTION_CONFIG.minTurns,
    maxTurns:
      asNumber(table["max_turns"]) ?? DEFAULT_COMPACTION_CONFIG.maxTurns,
    maxContextTokens:
      asNumber(table["max_context_tokens"]) ??
      DEFAULT_COMPACTION_CONFIG.maxContextTokens,
    keepRecentTurns:
      asNumber(table["keep_recent_turns"]) ??
      DEFAULT_COMPACTION_CONFIG.keepRecentTurns,
  };
}

function parseDreamingConfig(
  table: Record<string, unknown> | undefined,
): DreamingConfig {
  const defaults: DreamingConfig = {
    enabled: false,
    frequency: "0 3 * * *",
    max_tool_rounds: 12,
  };
  if (table === undefined) return defaults;
  return {
    enabled:
      typeof table["enabled"] === "boolean"
        ? table["enabled"]
        : defaults.enabled,
    frequency:
      typeof table["frequency"] === "string"
        ? table["frequency"]
        : defaults.frequency,
    max_tool_rounds:
      asNumber(table["max_tool_rounds"]) ?? defaults.max_tool_rounds,
  };
}

function parseAutonomyConfig(
  table: Record<string, unknown> | undefined,
): AutonomyConfig {
  const heartbeat = table === undefined ? undefined : pickTable(table, "heartbeat");
  return {
    enabled:
      table !== undefined && typeof table["enabled"] === "boolean"
        ? table["enabled"]
        : false,
    heartbeat: parseHeartbeatConfig(heartbeat),
  };
}

function parseHeartbeatConfig(
  table: Record<string, unknown> | undefined,
): LoadedHeartbeatConfig {
  return {
    enabled:
      table !== undefined && typeof table["enabled"] === "boolean"
        ? table["enabled"]
        : true,
    fallbackHeartbeatIntervalSecs:
      parseDurationSecs(table?.["fallback_heartbeat_interval"]) ??
      DEFAULT_HEARTBEAT_CONFIG.fallbackHeartbeatIntervalSecs,
    dormantAfterHeartbeatTurns:
      asNumber(table?.["dormant_after_heartbeat_turns"]) ??
      DEFAULT_HEARTBEAT_CONFIG.dormantAfterHeartbeatTurns,
    dormantAfterIdleTimeSecs:
      parseDurationSecs(table?.["dormant_after_idle_time"]) ??
      DEFAULT_HEARTBEAT_CONFIG.dormantAfterIdleTimeSecs,
    minimumHeartbeatLatencySecs:
      parseDurationSecs(table?.["minimum_heartbeat_latency"]) ??
      DEFAULT_HEARTBEAT_CONFIG.minimumHeartbeatLatencySecs,
    maxToolRounds: asNumber(table?.["max_tool_rounds"]) ?? 12,
    wrapUpGraceRounds: asNumber(table?.["wrap_up_grace_rounds"]) ?? 3,
  };
}

function parseNotificationsConfig(
  table: Record<string, unknown> | undefined,
): NotificationsConfig {
  if (table === undefined) return { ...DEFAULT_NOTIFICATIONS_CONFIG, events: { ...DEFAULT_NOTIFICATION_EVENTS } };

  const eventsTable = pickTable(table, "events");
  const events: NotificationEventsConfig = { ...DEFAULT_NOTIFICATION_EVENTS };
  if (eventsTable !== undefined) {
    for (const key of Object.keys(events) as Array<keyof NotificationEventsConfig>) {
      const raw = eventsTable[key];
      if (typeof raw === "boolean") events[key] = raw;
    }
  }

  return {
    enabled: typeof table["enabled"] === "boolean" ? table["enabled"] : false,
    generation_threshold_ms:
      (parseDurationSecs(table["generation_threshold"]) ?? 0) * 1000,
    events,
  };
}

function parseAdvancedConfig(
  table: Record<string, unknown> | undefined,
): LoadedConfig["app"]["advanced"] {
  return {
    cache_forensics:
      table !== undefined && typeof table["cache_forensics"] === "boolean"
        ? table["cache_forensics"]
        : false,
  };
}

function parseUsageConfig(
  table: Record<string, unknown> | undefined,
): UsageConfig {
  const timezone =
    table !== undefined && table["timezone"] === "utc" ? "utc" : "local";
  const allowCompaction =
    table !== undefined && typeof table["allow_compaction_over_budget"] === "boolean"
      ? table["allow_compaction_over_budget"]
      : true;
  const budgetsRaw = table === undefined ? undefined : table["budgets"];
  const budgets = Array.isArray(budgetsRaw)
    ? budgetsRaw
        .filter(isPlainObject)
        .map((entry) => parseBudget(entry))
        .filter((b): b is UsageBudgetConfig => b !== undefined)
    : [];
  const spike = table === undefined ? undefined : pickTable(table, "spike_warnings");
  return {
    timezone,
    allow_compaction_over_budget: allowCompaction,
    budgets,
    spike_warnings: parseSpikeWarnings(spike),
  };
}

function parseBudget(table: Record<string, unknown>): UsageBudgetConfig | undefined {
  const cost = asNumber(table["cost_usd"]);
  if (cost === undefined) return undefined;

  const defaults = defaultUsageBudgetConfig();
  const warnAtRaw = table["warn_at"];
  const warnAt = Array.isArray(warnAtRaw)
    ? warnAtRaw.filter((v): v is number => typeof v === "number" && Number.isFinite(v))
    : defaults.warn_at;
  const usageKindRaw = table["usage_kind"];
  const usageKind = Array.isArray(usageKindRaw)
    ? usageKindRaw.filter((v): v is string => typeof v === "string")
    : [];

  const out: UsageBudgetConfig = {
    name: typeof table["name"] === "string" ? table["name"] : "",
    period: parsePeriod(table["period"], defaults.period),
    cost_usd: cost,
    warn_at: warnAt.length > 0 ? warnAt : defaults.warn_at,
    limit: parseLimit(table["limit"], defaults.limit),
    usage_kind: usageKind,
  };
  if (typeof table["character"] === "string") out.character = table["character"];
  if (typeof table["provider"] === "string") out.provider = table["provider"];
  if (typeof table["api_key"] === "string") out.api_key = table["api_key"];
  if (typeof table["model"] === "string") out.model = table["model"];
  if (typeof table["call_type"] === "string") out.call_type = table["call_type"];
  if (typeof table["allow_compaction_over_budget"] === "boolean") {
    out.allow_compaction_over_budget = table["allow_compaction_over_budget"];
  }
  const resetHour = asNumber(table["reset_hour"]);
  if (resetHour !== undefined) out.reset_hour = Math.max(0, Math.min(23, Math.floor(resetHour)));
  const weekday = parseWeekday(table["reset_day_of_week"]);
  if (weekday !== undefined) out.reset_day_of_week = weekday;
  const dayOfMonth = asNumber(table["reset_day_of_month"]);
  if (dayOfMonth !== undefined) {
    out.reset_day_of_month = Math.max(1, Math.min(31, Math.floor(dayOfMonth)));
  }
  return out;
}

function parseSpikeWarnings(
  table: Record<string, unknown> | undefined,
): UsageSpikeWarningsConfig {
  const defaults = defaultSpikeWarningsConfig();
  if (table === undefined) return defaults;
  return {
    enabled: typeof table["enabled"] === "boolean" ? table["enabled"] : defaults.enabled,
    period: parsePeriod(table["period"], defaults.period),
    multiplier: asNumber(table["multiplier"]) ?? defaults.multiplier,
    min_cost_usd: asNumber(table["min_cost_usd"]) ?? defaults.min_cost_usd,
  };
}

function parsePeriod(
  raw: unknown,
  fallback: UsageBudgetPeriod,
): UsageBudgetPeriod {
  if (raw === "hour" || raw === "day" || raw === "week" || raw === "month") {
    return raw;
  }
  return fallback;
}

function parseLimit(raw: unknown, fallback: UsageBudgetAction): UsageBudgetAction {
  if (raw === "warn" || raw === "block" || raw === "pause_background") return raw;
  return fallback;
}

function parseWeekday(raw: unknown): BudgetWeekday | undefined {
  if (
    raw === "monday"
    || raw === "tuesday"
    || raw === "wednesday"
    || raw === "thursday"
    || raw === "friday"
    || raw === "saturday"
    || raw === "sunday"
  ) {
    return raw;
  }
  return undefined;
}

function parseRetrievalConfig(
  table: Record<string, unknown> | undefined,
): RetrievalConfig {
  const defaults = defaultRetrievalConfig();
  if (table === undefined) return defaults;
  return {
    mode: parseRetrievalMode(table["mode"], defaults.mode),
    max_file_bytes:
      asNumber(table["max_file_bytes"]) ?? defaults.max_file_bytes,
    max_indexed_files:
      asNumber(table["max_indexed_files"]) ?? defaults.max_indexed_files,
    max_total_indexed_bytes:
      asNumber(table["max_total_indexed_bytes"]) ??
      defaults.max_total_indexed_bytes,
    max_embed_chars_per_file:
      asNumber(table["max_embed_chars_per_file"]) ??
      defaults.max_embed_chars_per_file,
    binary: parseRetrievalBinary(table["binary"], defaults.binary),
  };
}

function parseRetrievalMode(raw: unknown, fallback: RetrievalMode): RetrievalMode {
  if (raw === "auto" || raw === "lexical" || raw === "hybrid") return raw;
  return fallback;
}

function parseRetrievalBinary(
  raw: unknown,
  fallback: RetrievalBinaryMode,
): RetrievalBinaryMode {
  if (raw === "skip" || raw === "metadata" || raw === "try_embed") return raw;
  return fallback;
}

function asNumber(v: unknown): number | undefined {
  return typeof v === "number" && Number.isFinite(v) ? v : undefined;
}

function parseDurationSecs(raw: unknown): number | undefined {
  if (typeof raw === "number" && Number.isFinite(raw)) return raw;
  if (typeof raw !== "string") return undefined;
  const trimmed = raw.trim();
  const match = /^(\d+(?:\.\d+)?)(ms|s|m|h|d)?$/.exec(trimmed);
  if (match === null) return undefined;
  const value = Number.parseFloat(match[1]!);
  const unit = match[2] ?? "s";
  switch (unit) {
    case "ms":
      return value / 1000;
    case "s":
      return value;
    case "m":
      return value * 60;
    case "h":
      return value * 3600;
    case "d":
      return value * 86400;
  }
  return undefined;
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}
