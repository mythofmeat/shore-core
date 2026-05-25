import fs from "node:fs";
import path from "node:path";

import {
  defaultSdkForOpenRouterModel,
  type ResolvedModel,
  type Sdk,
} from "./catalog.ts";
import { CommandError } from "../commands/types.ts";

export interface EffectiveProviderDiscovery {
  enabled?: boolean;
  ignore?: string[];
}

export interface EffectiveProviderEntry {
  enabled?: boolean;
  sdk?: string;
  base_url?: string;
  api_key_env?: string;
  keys?: Array<{ env?: string; enabled?: boolean }>;
  discovery?: EffectiveProviderDiscovery;
}

export type EffectiveProviderRegistry = Record<string, EffectiveProviderEntry>;

export interface EffectiveCatalogConfig {
  catalog: Map<string, ResolvedModel>;
  providers?: EffectiveProviderRegistry;
  cacheDir?: string;
}

export interface EffectiveModel {
  resolved: ResolvedModel;
  source: "static" | "discovered";
  hidden: boolean;
}

type EffectiveCatalogSource = EffectiveCatalogConfig | {
  runtime: {
    catalog: Map<string, ResolvedModel>;
    providers?: EffectiveProviderRegistry;
  };
  cacheDir?: string;
};

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

export function listEffectiveModels(
  source: EffectiveCatalogSource,
  includeHidden: boolean,
): EffectiveModel[] {
  const config = normalizeSource(source);
  const out: EffectiveModel[] = [];

  for (const model of config.catalog.values()) {
    if (model.category !== "chat") continue;
    out.push({ resolved: model, source: "static", hidden: false });
  }

  for (const [provider, entry] of Object.entries(config.providers)) {
    if (!providerDiscoveryReadable(entry)) continue;
    const cache = readProviderCache(config.cacheDir, provider);
    for (const discovered of cache?.models ?? []) {
      if (findStaticByUpstream(config.catalog, provider, discovered.model_id) !== undefined) {
        continue;
      }
      const hidden = !isVisible(entry.discovery, discovered.model_id);
      if (hidden && !includeHidden) continue;
      out.push({
        resolved: buildResolvedFromDiscovered(provider, entry, discovered),
        source: "discovered",
        hidden,
      });
    }
  }

  return out;
}

export function findEffectiveModel(
  source: EffectiveCatalogSource,
  name: string,
  includeHidden: boolean,
): ResolvedModel {
  const config = normalizeSource(source);

  const staticExact = config.catalog.get(name);
  if (staticExact !== undefined && staticExact.category === "chat") return staticExact;

  const staticMatches = [...config.catalog.values()].filter(
    (m) => m.category === "chat" && m.name === name,
  );
  if (staticMatches.length === 1) return staticMatches[0]!;
  if (staticMatches.length > 1) {
    throw new CommandError(
      "invalid_request",
      `ambiguous model name "${name}" — matches: ${staticMatches.map((m) => m.qualifiedName).join(", ")}`,
    );
  }

  const providerSplit = name.indexOf(":");
  if (providerSplit > 0 && providerSplit < name.length - 1) {
    const provider = name.slice(0, providerSplit);
    const modelId = name.slice(providerSplit + 1);
    const entry = config.providers[provider];
    if (entry !== undefined) {
      const staticMatch = findStaticByUpstream(config.catalog, provider, modelId);
      if (staticMatch !== undefined) return staticMatch;

      if (providerDiscoveryReadable(entry)) {
        const discovered = readProviderDiscovery(config.cacheDir, provider, modelId);
        if (discovered !== undefined) {
          const hidden = !isVisible(entry.discovery, discovered.model_id);
          if (hidden && !includeHidden) {
            throw hiddenModel(name, provider);
          }
          return buildResolvedFromDiscovered(provider, entry, discovered);
        }
      }
    }
  }

  const hits: Array<{ provider: string; resolved: ResolvedModel; hidden: boolean }> = [];
  for (const [provider, entry] of Object.entries(config.providers)) {
    const staticMatch = findStaticByUpstream(config.catalog, provider, name);
    if (staticMatch !== undefined) {
      hits.push({ provider, resolved: staticMatch, hidden: false });
      continue;
    }

    if (!providerDiscoveryReadable(entry)) continue;
    const discovered = readProviderDiscovery(config.cacheDir, provider, name);
    if (discovered === undefined) continue;
    hits.push({
      provider,
      resolved: buildResolvedFromDiscovered(provider, entry, discovered),
      hidden: !isVisible(entry.discovery, discovered.model_id),
    });
  }

  const visibleHits = includeHidden ? hits : hits.filter((hit) => !hit.hidden);
  if (visibleHits.length === 1) return visibleHits[0]!.resolved;
  if (visibleHits.length > 1) {
    throw new CommandError(
      "invalid_request",
      `ambiguous model name "${name}" — matches: ${visibleHits.map((hit) => `${hit.provider}:${hit.resolved.modelId}`).join(", ")}`,
    );
  }

  if (!includeHidden) {
    const hidden = hits.find((hit) => hit.hidden);
    if (hidden !== undefined) throw hiddenModel(name, hidden.provider);
  }

  throw new CommandError(
    "not_found",
    `model ${JSON.stringify(name)} not found in static catalog or discovered models`,
  );
}

function normalizeSource(source: EffectiveCatalogSource): Required<EffectiveCatalogConfig> {
  if ("runtime" in source) {
    return {
      catalog: source.runtime.catalog,
      providers: source.runtime.providers ?? {},
      cacheDir: source.cacheDir ?? "",
    };
  }
  return {
    catalog: source.catalog,
    providers: source.providers ?? {},
    cacheDir: source.cacheDir ?? "",
  };
}

function findStaticByUpstream(
  catalog: Map<string, ResolvedModel>,
  provider: string,
  modelId: string,
): ResolvedModel | undefined {
  for (const model of catalog.values()) {
    if (
      model.category === "chat"
      && model.providerKey === provider
      && model.modelId === modelId
    ) {
      return model;
    }
  }
  return undefined;
}

function providerDiscoveryReadable(entry: EffectiveProviderEntry): boolean {
  return entry.enabled !== false && entry.discovery?.enabled === true;
}

function readProviderDiscovery(
  cacheDir: string,
  provider: string,
  modelId: string,
): DiscoveredModel | undefined {
  return readProviderCache(cacheDir, provider)?.models?.find((m) => m.model_id === modelId);
}

function readProviderCache(cacheDir: string, provider: string): ProviderModelsCache | undefined {
  if (cacheDir.length === 0) return undefined;
  let raw: string;
  try {
    raw = fs.readFileSync(path.join(cacheDir, "providers", provider, "models.json"), "utf8");
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

function isVisible(discovery: EffectiveProviderDiscovery | undefined, modelId: string): boolean {
  let visible = true;
  for (const pattern of discovery?.ignore ?? []) {
    const negate = pattern.startsWith("!");
    const body = negate ? pattern.slice(1) : pattern;
    if (globMatches(body, modelId)) visible = negate;
  }
  return visible;
}

function buildResolvedFromDiscovered(
  provider: string,
  entry: EffectiveProviderEntry,
  model: DiscoveredModel,
): ResolvedModel {
  const sdk = parseSdk(entry.sdk)
    ?? parseSdk(typeof model.sdk === "string" ? model.sdk : undefined)
    ?? (provider === "openrouter" ? defaultSdkForOpenRouterModel(model.model_id) : defaultSdk(provider));
  return {
    name: model.model_id,
    qualifiedName: `chat.${provider}.${model.model_id}`,
    category: "chat",
    providerKey: provider,
    sdk,
    modelId: model.model_id,
    apiKeyEnv: entry.api_key_env ?? firstEnabledKeyEnv(entry) ?? defaultApiKeyEnv(provider),
    baseUrl: entry.base_url ?? (typeof model.base_url === "string" ? model.base_url : defaultBaseUrl(provider)),
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

function firstEnabledKeyEnv(entry: EffectiveProviderEntry): string | undefined {
  return entry.keys?.find((key) => key.enabled !== false && typeof key.env === "string")?.env;
}

function hiddenModel(name: string, provider: string): CommandError {
  return new CommandError(
    "not_found",
    `model ${JSON.stringify(name)} is hidden by provider ${JSON.stringify(provider)} discovery filters`,
  );
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
