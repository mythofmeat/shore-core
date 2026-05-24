import fs from "node:fs";
import path from "node:path";
import { parse as parseToml } from "smol-toml";

import { defaultSdkForOpenRouterModel, type ResolvedModel, type Sdk } from "../llm/catalog.ts";
import {
  asArgs,
  CommandError,
  type CommandContext,
  type ConfigSource,
} from "./types.ts";

export interface ProviderKeyEntry {
  name: string;
  env: string;
  enabled: boolean;
  warn_on_fallback: boolean;
}

export interface ProviderDiscovery {
  enabled: boolean;
  ignore: string[];
}

export interface ProviderEntry {
  enabled: boolean;
  sdk?: Sdk;
  base_url?: string;
  keys: ProviderKeyEntry[];
  discovery: ProviderDiscovery;
}

export type ProviderRegistry = Record<string, ProviderEntry>;

interface DiscoveredModel {
  provider_key?: string;
  model_id: string;
  display_name?: string | null;
  sdk?: string | null;
  base_url?: string | null;
  created_at?: string | number | null;
  owned_by?: string | null;
  description?: string | null;
  context_length?: number | null;
  max_output_tokens?: number | null;
  supports_tools?: boolean | null;
  supports_images?: boolean | null;
  supports_reasoning?: boolean | null;
  supports_prompt_cache?: boolean | null;
  raw_provider_metadata?: unknown;
  discovered_at?: string | null;
}

interface ProviderModelsCache {
  version?: number;
  provider_key?: string;
  fetched_at?: string;
  base_url?: string | null;
  models?: DiscoveredModel[];
}

export function loadProviderRegistry(source: ConfigSource): ProviderRegistry {
  const raw = readMergedConfig(source);
  const providers = raw["providers"];
  if (!isPlainObject(providers)) return {};

  const out: ProviderRegistry = {};
  for (const [name, value] of Object.entries(providers).sort(([a], [b]) => a.localeCompare(b))) {
    if (name === "claude_code") {
      throw new Error(
        "[providers.claude_code] is no longer supported; drop this section from your config",
      );
    }
    if (!isPlainObject(value)) continue;
    out[name] = parseProviderEntry(value);
  }
  return out;
}

export type PreferenceProviderRegistry = Record<string, {
  enabled?: boolean;
  sdk?: string;
  base_url?: string;
  api_key_env?: string;
  keys?: Array<{ env?: string; enabled?: boolean }>;
  discovery?: { enabled?: boolean; ignore?: string[] };
}>;

export function providersForPreferences(registry: ProviderRegistry): PreferenceProviderRegistry {
  const out: PreferenceProviderRegistry = {};
  for (const [name, entry] of Object.entries(registry)) {
    out[name] = {
      enabled: entry.enabled,
      ...(entry.sdk !== undefined ? { sdk: entry.sdk } : {}),
      ...(entry.base_url !== undefined ? { base_url: entry.base_url } : {}),
      ...(entry.keys[0]?.env !== undefined ? { api_key_env: entry.keys[0].env } : {}),
      keys: entry.keys.map((key) => ({ env: key.env, enabled: key.enabled })),
      discovery: {
        enabled: entry.discovery.enabled,
        ignore: [...entry.discovery.ignore],
      },
    };
  }
  return out;
}

export function listProviders(ctx: CommandContext): Record<string, unknown> {
  const providers = Object.entries(ctx.runtime.providers)
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([name, entry]) => {
      const keys = entry.keys.map((key) => ({
        name: key.name,
        enabled: key.enabled,
        warn_on_fallback: key.warn_on_fallback,
        env_set: envSet(key.env),
      }));
      const cache = readProviderCache(ctx.cacheDir, name);
      const hidden = (cache?.models ?? []).filter(
        (m) => !isVisible(entry.discovery, m.model_id),
      ).length;
      return {
        name,
        enabled: entry.enabled,
        sdk: entry.sdk ?? null,
        base_url: entry.base_url ?? null,
        discovery_enabled: entry.discovery.enabled,
        keys,
        cache: cache === undefined
          ? {
            present: false,
            models: 0,
            visible: 0,
            hidden: 0,
            fetched_at: null,
          }
          : {
            present: true,
            models: cache.models?.length ?? 0,
            visible: Math.max(0, (cache.models?.length ?? 0) - hidden),
            hidden,
            fetched_at: cache.fetched_at ?? null,
          },
      };
    });
  return { providers };
}

export function listProviderModels(ctx: CommandContext, rawArgs: unknown): Record<string, unknown> {
  const args = asArgs(rawArgs);
  const provider = requireProvider(args);
  const includeHidden = args["include_hidden"] === true;
  const registryEntry = ctx.runtime.providers[provider];
  const knownInStatic = [...ctx.runtime.catalog.values()].some(
    (m) => m.category === "chat" && m.providerKey === provider,
  );
  if (registryEntry === undefined && !knownInStatic) {
    throw new CommandError("not_found", `provider ${JSON.stringify(provider)} is not configured`);
  }

  const cache = readProviderCache(ctx.cacheDir, provider);
  const discovered: unknown[] = [];
  const hidden: unknown[] = [];
  for (const model of cache?.models ?? []) {
    const row = discoveredToJson(model);
    const visible = registryEntry === undefined
      ? true
      : isVisible(registryEntry.discovery, model.model_id);
    if (visible || includeHidden) discovered.push(row);
    else hidden.push(row);
  }

  const staticModels = [...ctx.runtime.catalog.values()]
    .filter((m) => m.category === "chat" && m.providerKey === provider)
    .sort((a, b) => a.qualifiedName.localeCompare(b.qualifiedName))
    .map((m) => ({
      source: "static",
      name: m.name,
      qualified_name: m.qualifiedName,
      model_id: m.modelId,
      sdk: m.sdk,
      max_tokens: m.maxTokens ?? null,
    }));

  return {
    provider,
    discovered,
    hidden,
    static: staticModels,
    include_hidden: includeHidden,
    cache: cache === undefined
      ? { fetched_at: null, model_count: 0 }
      : { fetched_at: cache.fetched_at ?? null, model_count: cache.models?.length ?? 0 },
  };
}

export function refreshProviderModels(ctx: CommandContext, rawArgs: unknown): Record<string, unknown> {
  const args = asArgs(rawArgs);
  const provider = requireProvider(args);
  const entry = ctx.runtime.providers[provider];
  if (entry === undefined) {
    throw new CommandError("not_found", `provider ${JSON.stringify(provider)} is not configured`);
  }
  if (!entry.enabled) {
    throw new CommandError("invalid_request", `provider ${JSON.stringify(provider)} is disabled`);
  }
  if (!entry.discovery.enabled) {
    throw new CommandError(
      "invalid_request",
      `provider ${JSON.stringify(provider)} has discovery disabled`,
    );
  }
  return {
    provider,
    model_count: 0,
    fetched_at: null,
    cache_path: providerCachePath(ctx.cacheDir, provider),
    status: "not_implemented",
    message: "provider model refresh not implemented in TS daemon",
  };
}

export function refreshAllProviderModels(ctx: CommandContext): Record<string, unknown> {
  const results: unknown[] = [];
  const skipped: unknown[] = [];
  for (const [provider, entry] of Object.entries(ctx.runtime.providers).sort(([a], [b]) => a.localeCompare(b))) {
    if (!entry.enabled) {
      skipped.push({ provider, reason: "disabled" });
      continue;
    }
    if (!entry.discovery.enabled) {
      skipped.push({ provider, reason: "discovery disabled" });
      continue;
    }
    results.push({
      provider,
      ok: false,
      error: "provider model refresh not implemented in TS daemon",
    });
  }
  return { results, skipped };
}

export function readProviderCache(cacheDir: string, provider: string): ProviderModelsCache | undefined {
  let raw: string;
  try {
    raw = fs.readFileSync(providerCachePath(cacheDir, provider), "utf8");
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw e;
  }
  try {
    const parsed = JSON.parse(raw) as ProviderModelsCache;
    if (!Array.isArray(parsed.models)) return undefined;
    return parsed;
  } catch {
    return undefined;
  }
}

export function providerCachePath(cacheDir: string, provider: string): string {
  return path.join(cacheDir, "providers", provider, "models.json");
}

export function isVisible(discovery: ProviderDiscovery | undefined, modelId: string): boolean {
  let visible = true;
  for (const pattern of discovery?.ignore ?? []) {
    const negate = pattern.startsWith("!");
    const body = negate ? pattern.slice(1) : pattern;
    if (globMatches(body, modelId)) visible = negate;
  }
  return visible;
}

export function discoveredModelToResolved(
  provider: string,
  entry: ProviderEntry | undefined,
  model: DiscoveredModel,
): ResolvedModel {
  const sdk = parseSdk(entry?.sdk ?? (typeof model.sdk === "string" ? model.sdk : undefined))
    ?? (provider === "openrouter" ? defaultSdkForOpenRouterModel(model.model_id) : defaultSdk(provider));
  return {
    name: model.model_id,
    qualifiedName: `chat.${provider}.${model.model_id}`,
    category: "chat",
    providerKey: provider,
    sdk,
    modelId: model.model_id,
    apiKeyEnv: entry?.keys.find((key) => key.enabled)?.env ?? defaultApiKeyEnv(provider),
    baseUrl: entry?.base_url ?? (typeof model.base_url === "string" ? model.base_url : defaultBaseUrl(provider)),
    maxTokens: numberOrUndefined(model.max_output_tokens) ?? 8192,
    maxContextTokens: numberOrUndefined(model.context_length) ?? 200_000,
    temperature: 1.0,
    topP: undefined,
    reasoningEffort: undefined,
    budgetTokens: undefined,
    cacheTtl: sdk === "anthropic" ? "1h" : undefined,
    openrouterProvider: undefined,
  };
}

function parseProviderEntry(value: Record<string, unknown>): ProviderEntry {
  const compactKey = typeof value["api_key_env"] === "string" ? value["api_key_env"] : undefined;
  const rawKeys = Array.isArray(value["keys"]) ? value["keys"] : [];
  if (compactKey !== undefined && rawKeys.length > 0) {
    throw new Error("providers entry declares both api_key_env and keys");
  }
  const keys: ProviderKeyEntry[] = [];
  if (compactKey !== undefined) {
    keys.push({
      name: "default",
      env: compactKey,
      enabled: true,
      warn_on_fallback: false,
    });
  }
  for (const raw of rawKeys) {
    if (!isPlainObject(raw)) continue;
    const name = typeof raw["name"] === "string" ? raw["name"] : "";
    const env = typeof raw["env"] === "string" ? raw["env"] : "";
    if (name.length === 0 || env.length === 0) continue;
    keys.push({
      name,
      env,
      enabled: typeof raw["enabled"] === "boolean" ? raw["enabled"] : true,
      warn_on_fallback:
        typeof raw["warn_on_fallback"] === "boolean" ? raw["warn_on_fallback"] : false,
    });
  }
  const discovery = isPlainObject(value["discovery"]) ? value["discovery"] : {};
  const ignore = Array.isArray(discovery["ignore"])
    ? discovery["ignore"].filter((v): v is string => typeof v === "string")
    : [];
  const sdk = parseSdk(typeof value["sdk"] === "string" ? value["sdk"] : undefined);
  return {
    enabled: typeof value["enabled"] === "boolean" ? value["enabled"] : true,
    ...(sdk !== undefined ? { sdk } : {}),
    ...(typeof value["base_url"] === "string" ? { base_url: value["base_url"] } : {}),
    keys,
    discovery: {
      enabled: typeof discovery["enabled"] === "boolean" ? discovery["enabled"] : false,
      ignore,
    },
  };
}

function requireProvider(args: Record<string, unknown>): string {
  const provider = args["provider"];
  if (typeof provider !== "string" || provider.length === 0) {
    throw new CommandError("invalid_request", "missing required argument: provider");
  }
  return provider;
}

function discoveredToJson(model: DiscoveredModel): Record<string, unknown> {
  return {
    source: "discovered",
    model_id: model.model_id,
    display_name: model.display_name ?? null,
    sdk: model.sdk ?? null,
    owned_by: model.owned_by ?? null,
    context_length: model.context_length ?? null,
    max_output_tokens: model.max_output_tokens ?? null,
    supports_tools: model.supports_tools ?? null,
    supports_images: model.supports_images ?? null,
    supports_reasoning: model.supports_reasoning ?? null,
    supports_prompt_cache: model.supports_prompt_cache ?? null,
    discovered_at: model.discovered_at ?? null,
  };
}

function envSet(name: string): boolean {
  const value = process.env[name];
  return value !== undefined && value.trim().length > 0;
}

function readMergedConfig(source: ConfigSource): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  const baseFile = source.configFile ?? path.join(source.configDir, "config.toml");
  if (fs.existsSync(baseFile)) {
    deepMerge(out, parseToml(fs.readFileSync(baseFile, "utf8")) as Record<string, unknown>);
  }
  const confDir = path.join(source.configDir, "conf.d");
  if (fs.existsSync(confDir)) {
    for (const name of fs.readdirSync(confDir).filter((n) => n.endsWith(".toml")).sort()) {
      deepMerge(
        out,
        parseToml(fs.readFileSync(path.join(confDir, name), "utf8")) as Record<string, unknown>,
      );
    }
  }
  return out;
}

function deepMerge(target: Record<string, unknown>, src: Record<string, unknown>): void {
  for (const [key, value] of Object.entries(src)) {
    const prev = target[key];
    if (isPlainObject(prev) && isPlainObject(value)) {
      const nested = { ...prev };
      deepMerge(nested, value);
      target[key] = nested;
    } else if (Array.isArray(prev) && Array.isArray(value)) {
      target[key] = [...prev, ...value];
    } else {
      target[key] = value;
    }
  }
}

function globMatches(pattern: string, text: string): boolean {
  const escaped = pattern
    .split("*")
    .map((part) => part.replace(/[.+?^${}()|[\]\\]/g, "\\$&"))
    .join(".*");
  return new RegExp(`^${escaped}$`).test(text);
}

function parseSdk(raw: string | undefined): Sdk | undefined {
  if (raw === "anthropic" || raw === "openai" || raw === "gemini" || raw === "zai") return raw;
  if (raw === "deepseek" || raw === "zhipuai") return "openai";
  return undefined;
}

function defaultSdk(provider: string): Sdk {
  if (provider === "anthropic") return "anthropic";
  return "openai";
}

function defaultApiKeyEnv(provider: string): string | undefined {
  switch (provider) {
    case "anthropic":
      return "ANTHROPIC_API_KEY";
    case "openai":
      return "OPENAI_API_KEY";
    case "openrouter":
      return "OPENROUTER_API_KEY";
    case "deepseek":
      return "DEEPSEEK_API_KEY";
    case "xai":
      return "XAI_API_KEY";
    default:
      return undefined;
  }
}

function defaultBaseUrl(provider: string): string | undefined {
  switch (provider) {
    case "openrouter":
      return "https://openrouter.ai/api/v1";
    case "deepseek":
      return "https://api.deepseek.com/v1";
    case "xai":
      return "https://api.x.ai/v1";
    default:
      return undefined;
  }
}

function numberOrUndefined(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
