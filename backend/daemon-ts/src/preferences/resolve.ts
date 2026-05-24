import fs from "node:fs";
import path from "node:path";

import {
  resolveModel,
  type ResolvedModel,
  type Sdk,
} from "../llm/catalog.ts";
import { applySamplerOverlay } from "./overlay.ts";
import {
  SAMPLER_KEYS,
  applySamplerSettingsOverlay,
  defaultModelPreferences,
  samplerSettingsFromResolvedModel,
  selectedModelPair,
  type BackgroundTask,
  type ModelPreferences,
  type PreferenceScope,
  type SamplerScopes,
  type SamplerSettings,
} from "./types.ts";
import {
  loadForCharacter,
  modelPreference,
} from "./store.ts";

export interface BackgroundDefaults {
  model?: string;
  heartbeat?: string;
  compaction?: string;
  dreaming?: string;
}

export interface ProviderDiscoveryConfig {
  enabled?: boolean;
  ignore?: string[];
}

export interface ProviderConfigEntry {
  enabled?: boolean;
  sdk?: string;
  base_url?: string;
  api_key_env?: string;
  keys?: Array<{ env?: string; enabled?: boolean }>;
  discovery?: ProviderDiscoveryConfig;
}

export interface PreferenceResolutionConfig {
  catalog: Map<string, ResolvedModel>;
  dataDir?: string;
  cacheDir?: string;
  dirs?: {
    data?: string;
    cache?: string;
  };
  appDefaultModel?: string;
  backgroundDefaults?: BackgroundDefaults;
  app?: {
    defaults?: {
      model?: string;
      background?: BackgroundDefaults;
    };
  };
  providers?: Record<string, ProviderConfigEntry>;
}

interface DiscoveredModel {
  provider_key: string;
  model_id: string;
  display_name?: string;
  sdk?: string;
  base_url?: string;
  context_length?: number;
  max_output_tokens?: number;
}

interface ProviderModelsCache {
  version: number;
  provider_key: string;
  base_url?: string;
  models: DiscoveredModel[];
}

export function resolveSelectedModel(
  global: ModelPreferences,
  character: ModelPreferences | undefined,
): [string, string] | undefined {
  const charPair = character === undefined ? undefined : selectedModelPair(character.selected);
  if (charPair !== undefined) return charPair;
  return selectedModelPair(global.selected);
}

export function resolveSamplerSettings(
  global: ModelPreferences,
  character: ModelPreferences | undefined,
  provider: string,
  modelId: string,
  staticDefault?: ResolvedModel,
): SamplerSettings {
  let effective = staticDefault === undefined
    ? {}
    : samplerSettingsFromResolvedModel(staticDefault);
  effective = applySamplerSettingsOverlay(effective, global.defaults.sampler);
  if (character !== undefined) {
    effective = applySamplerSettingsOverlay(effective, character.defaults.sampler);
  }
  const globalModel = modelPreference(global, provider, modelId);
  if (globalModel !== undefined) {
    effective = applySamplerSettingsOverlay(effective, globalModel.sampler);
  }
  const characterModel = character === undefined
    ? undefined
    : modelPreference(character, provider, modelId);
  if (characterModel !== undefined) {
    effective = applySamplerSettingsOverlay(effective, characterModel.sampler);
  }
  return effective;
}

export function resolveSamplerScopes(
  global: ModelPreferences,
  character: ModelPreferences | undefined,
  provider: string,
  modelId: string,
  staticDefault?: ResolvedModel,
): SamplerScopes {
  const scopes: SamplerScopes = {};
  const update = (layer: SamplerSettings, scope: PreferenceScope): void => {
    for (const key of SAMPLER_KEYS) {
      if (layer[key] !== undefined) {
        (scopes as Record<string, PreferenceScope | undefined>)[key] = scope;
      }
    }
  };

  if (staticDefault !== undefined) {
    update(samplerSettingsFromResolvedModel(staticDefault), "static_default");
  }
  update(global.defaults.sampler, "global_default");
  if (character !== undefined) update(character.defaults.sampler, "character_default");
  const globalModel = modelPreference(global, provider, modelId);
  if (globalModel !== undefined) update(globalModel.sampler, "global_model");
  const characterModel = character === undefined
    ? undefined
    : modelPreference(character, provider, modelId);
  if (characterModel !== undefined) update(characterModel.sampler, "character_model");
  return scopes;
}

export function findStaticModel(
  catalog: Map<string, ResolvedModel>,
  provider: string,
  modelId: string,
): ResolvedModel | undefined {
  for (const model of catalog.values()) {
    if (model.providerKey === provider && model.modelId === modelId) return model;
  }
  return undefined;
}

export function resolveActiveForCharacter(
  config: PreferenceResolutionConfig,
  dataDir: string,
  global: ModelPreferences,
  character: ModelPreferences,
  legacyActiveModel?: string,
  appDefaultModel?: string,
): ResolvedModel | undefined {
  const charPair = selectedModelPair(character.selected);
  if (charPair !== undefined) {
    const resolved = resolveProviderModel(config, dataDir, charPair[0], charPair[1]);
    if (resolved !== undefined) return resolved;
  }

  const globalPair = selectedModelPair(global.selected);
  if (globalPair !== undefined) {
    const resolved = resolveProviderModel(config, dataDir, globalPair[0], globalPair[1]);
    if (resolved !== undefined) return resolved;
  }

  if (legacyActiveModel !== undefined && legacyActiveModel.length > 0) {
    const resolved = tryResolveModel(config.catalog, legacyActiveModel);
    if (resolved !== undefined) return resolved;
  }

  const defaultModel = appDefaultModel ?? appDefaultModelOf(config);
  if (defaultModel !== undefined && defaultModel.length > 0) {
    const resolved = tryResolveModel(config.catalog, defaultModel);
    if (resolved !== undefined) return resolved;
  }

  return firstChatModel(config.catalog);
}

export function overlayForCharacter(
  dataDir: string,
  character: string,
  base: ResolvedModel,
  op: string,
): ResolvedModel {
  try {
    const [globalPrefs, characterPrefs] = loadForCharacter(dataDir, character);
    const overlay = resolveSamplerSettings(
      globalPrefs,
      characterPrefs,
      base.providerKey,
      base.modelId,
      base,
    );
    return applySamplerOverlay(base, overlay);
  } catch (e) {
    console.warn(
      `[shore-daemon-ts] preferences load failed for ${character} during ${op}; using raw model settings: ${(e as Error).message}`,
    );
    return base;
  }
}

export function resolveChatModelForCharacter(
  config: PreferenceResolutionConfig,
  character: string,
): ResolvedModel | undefined {
  const dataDir = requireDataDir(config);
  let globalPrefs: ModelPreferences;
  let characterPrefs: ModelPreferences;
  try {
    [globalPrefs, characterPrefs] = loadForCharacter(dataDir, character);
  } catch (e) {
    console.warn(
      `[shore-daemon-ts] preferences load failed for ${character}; using empty defaults: ${(e as Error).message}`,
    );
    globalPrefs = defaultModelPreferences();
    characterPrefs = defaultModelPreferences();
  }
  const legacy = loadLegacyActiveModel(path.join(dataDir, character));
  const resolved = resolveActiveForCharacter(
    config,
    dataDir,
    globalPrefs,
    characterPrefs,
    legacy,
    appDefaultModelOf(config),
  );
  if (resolved === undefined) return undefined;
  const overlay = resolveSamplerSettings(
    globalPrefs,
    characterPrefs,
    resolved.providerKey,
    resolved.modelId,
    resolved,
  );
  return applySamplerOverlay(resolved, overlay);
}

export function resolveBackgroundModel(
  config: PreferenceResolutionConfig,
  task: BackgroundTask,
  character: string,
): ResolvedModel | undefined {
  const dataDir = requireDataDir(config);
  const backgroundName = resolveBackgroundModelName(config, task);
  if (backgroundName !== undefined) {
    const base = tryResolveModel(config.catalog, backgroundName);
    if (base !== undefined) {
      return overlayForCharacter(dataDir, character, base, task);
    }
    console.warn(
      `[shore-daemon-ts] configured ${task} model ${JSON.stringify(backgroundName)} not found; falling back to active chat model`,
    );
    return resolveChatModelForCharacter(config, character);
  }
  return resolveChatModelForCharacter(config, character);
}

function resolveProviderModel(
  config: PreferenceResolutionConfig,
  _dataDir: string,
  provider: string,
  modelId: string,
): ResolvedModel | undefined {
  const staticModel = findStaticModel(config.catalog, provider, modelId);
  if (staticModel !== undefined) return { ...staticModel };

  const discovered = resolveDiscoveredProviderModel(config, provider, modelId);
  if (discovered !== undefined) return discovered;

  return synthesizeSelectedProviderModel(config, provider, modelId);
}

function resolveDiscoveredProviderModel(
  config: PreferenceResolutionConfig,
  provider: string,
  modelId: string,
): ResolvedModel | undefined {
  const entry = providerEntry(config, provider);
  if (entry === undefined || entry.discovery?.enabled !== true) return undefined;
  const cacheDir = cacheDirOf(config);
  if (cacheDir === undefined) return undefined;
  const discovered = readProviderDiscovery(cacheDir, provider, modelId);
  if (discovered === undefined) return undefined;
  return buildResolvedFromDiscovered(provider, entry, discovered);
}

function synthesizeSelectedProviderModel(
  config: PreferenceResolutionConfig,
  provider: string,
  modelId: string,
): ResolvedModel | undefined {
  const entry = providerEntry(config, provider);
  if (entry === undefined) return undefined;
  const defaults = hardcodedProviderDefaults(provider, modelId);
  const sdk = parseSdk(entry.sdk) ?? defaults.sdk;
  return {
    name: modelId,
    qualifiedName: `chat.${provider}.${modelId}`,
    category: "chat",
    providerKey: provider,
    sdk,
    modelId,
    apiKeyEnv: entry.api_key_env ?? firstEnabledKeyEnv(entry) ?? defaults.apiKeyEnv,
    baseUrl: entry.base_url ?? defaults.baseUrl,
    maxTokens: defaults.maxTokens,
    maxContextTokens: defaults.maxContextTokens,
    temperature: defaults.temperature,
    topP: undefined,
    reasoningEffort: undefined,
    budgetTokens: undefined,
    cacheTtl: sdk === "anthropic" ? "1h" : undefined,
    openrouterProvider: undefined,
  };
}

function buildResolvedFromDiscovered(
  provider: string,
  entry: ProviderConfigEntry,
  discovered: DiscoveredModel,
): ResolvedModel {
  const defaults = hardcodedProviderDefaults(provider, discovered.model_id);
  const sdk = parseSdk(entry.sdk) ?? parseSdk(discovered.sdk) ?? defaults.sdk;
  return {
    name: discovered.model_id,
    qualifiedName: `chat.${provider}.${discovered.model_id}`,
    category: "chat",
    providerKey: provider,
    sdk,
    modelId: discovered.model_id,
    apiKeyEnv: entry.api_key_env ?? firstEnabledKeyEnv(entry) ?? defaults.apiKeyEnv,
    baseUrl: entry.base_url ?? discovered.base_url ?? defaults.baseUrl,
    maxTokens: asU32(discovered.max_output_tokens) ?? defaults.maxTokens,
    maxContextTokens: asU32(discovered.context_length) ?? defaults.maxContextTokens,
    temperature: defaults.temperature,
    topP: undefined,
    reasoningEffort: undefined,
    budgetTokens: undefined,
    cacheTtl: sdk === "anthropic" ? "1h" : undefined,
    openrouterProvider: undefined,
  };
}

function tryResolveModel(
  catalog: Map<string, ResolvedModel>,
  name: string,
): ResolvedModel | undefined {
  try {
    return { ...resolveModel(catalog, name) };
  } catch {
    return undefined;
  }
}

function firstChatModel(catalog: Map<string, ResolvedModel>): ResolvedModel | undefined {
  const chatModels = [...catalog.values()]
    .filter((m) => m.category === "chat")
    .sort((a, b) => a.qualifiedName.localeCompare(b.qualifiedName));
  const first = chatModels[0];
  return first === undefined ? undefined : { ...first };
}

function readProviderDiscovery(
  cacheDir: string,
  provider: string,
  modelId: string,
): DiscoveredModel | undefined {
  const cachePath = path.join(cacheDir, "providers", provider, "models.json");
  let raw: string;
  try {
    raw = fs.readFileSync(cachePath, "utf8");
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw e;
  }

  let parsed: ProviderModelsCache;
  try {
    parsed = JSON.parse(raw) as ProviderModelsCache;
  } catch {
    return undefined;
  }
  if (parsed.version > 1 || !Array.isArray(parsed.models)) return undefined;
  return parsed.models.find((m) => m.model_id === modelId);
}

function resolveBackgroundModelName(
  config: PreferenceResolutionConfig,
  task: BackgroundTask,
): string | undefined {
  const bg = config.backgroundDefaults ?? config.app?.defaults?.background;
  return bg?.[task] ?? bg?.model;
}

function appDefaultModelOf(config: PreferenceResolutionConfig): string | undefined {
  return config.appDefaultModel ?? config.app?.defaults?.model;
}

function requireDataDir(config: PreferenceResolutionConfig): string {
  const dataDir = config.dataDir ?? config.dirs?.data;
  if (dataDir === undefined) {
    throw new Error("preference resolution requires dataDir");
  }
  return dataDir;
}

function cacheDirOf(config: PreferenceResolutionConfig): string | undefined {
  return config.cacheDir ?? config.dirs?.cache;
}

function providerEntry(
  config: PreferenceResolutionConfig,
  provider: string,
): ProviderConfigEntry | undefined {
  const entry = config.providers?.[provider];
  if (entry === undefined) return undefined;
  if (entry.enabled === false) return undefined;
  return entry;
}

function hardcodedProviderDefaults(
  provider: string,
  modelId: string,
): {
  sdk: Sdk;
  apiKeyEnv: string | undefined;
  baseUrl: string | undefined;
  maxTokens: number;
  maxContextTokens: number;
  temperature: number;
} {
  const base = { maxContextTokens: 200_000, maxTokens: 8192, temperature: 1.0 };
  switch (provider) {
    case "anthropic":
      return {
        ...base,
        sdk: "anthropic",
        apiKeyEnv: "ANTHROPIC_API_KEY",
        baseUrl: undefined,
      };
    case "openrouter":
      return {
        ...base,
        sdk: "openai",
        apiKeyEnv: "OPENROUTER_API_KEY",
        baseUrl: "https://openrouter.ai/api/v1",
      };
    case "openai":
      return {
        ...base,
        sdk: "openai",
        apiKeyEnv: "OPENAI_API_KEY",
        baseUrl: undefined,
      };
    case "deepseek":
      return {
        ...base,
        sdk: "openai",
        apiKeyEnv: "DEEPSEEK_API_KEY",
        baseUrl: "https://api.deepseek.com/v1",
      };
    case "xai":
      return {
        ...base,
        sdk: "openai",
        apiKeyEnv: "XAI_API_KEY",
        baseUrl: "https://api.x.ai/v1",
      };
    default:
      return {
        ...base,
        sdk: parseSdk(provider) ?? "openai",
        apiKeyEnv: undefined,
        baseUrl: undefined,
      };
  }
}

function parseSdk(raw: string | undefined): Sdk | undefined {
  if (raw === "anthropic" || raw === "openai" || raw === "gemini" || raw === "zai") {
    return raw;
  }
  if (raw === "deepseek" || raw === "zhipuai") return "openai";
  return undefined;
}

function asU32(value: unknown): number | undefined {
  if (
    typeof value !== "number"
    || !Number.isInteger(value)
    || value < 0
    || value > 0xffff_ffff
  ) {
    return undefined;
  }
  return value;
}

function firstEnabledKeyEnv(entry: ProviderConfigEntry): string | undefined {
  return entry.keys?.find((key) => key.enabled !== false && typeof key.env === "string")?.env;
}

function loadLegacyActiveModel(characterDataDir: string): string | undefined {
  const file = path.join(characterDataDir, "runtime_state.json");
  try {
    const parsed = JSON.parse(fs.readFileSync(file, "utf8")) as { active_model?: unknown };
    return typeof parsed.active_model === "string" ? parsed.active_model : undefined;
  } catch {
    return undefined;
  }
}
