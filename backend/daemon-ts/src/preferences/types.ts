import type { ResolvedModel } from "../llm/catalog.ts";

export const SAMPLER_KEYS = [
  "temperature",
  "top_p",
  "reasoning_effort",
  "thinking_enabled",
  "budget_tokens",
  "max_tokens",
  "cache_ttl",
] as const;

export type SamplerKey = (typeof SAMPLER_KEYS)[number];

export interface SamplerSettings {
  temperature?: number;
  top_p?: number;
  reasoning_effort?: string;
  thinking_enabled?: boolean;
  budget_tokens?: number;
  max_tokens?: number;
  cache_ttl?: string;
}

export interface SelectedModel {
  provider?: string;
  model_id?: string;
}

export interface ModelPreference {
  sampler: SamplerSettings;
}

export interface PreferenceDefaults {
  sampler: SamplerSettings;
}

export interface ModelPreferences {
  selected: SelectedModel;
  defaults: PreferenceDefaults;
  models: Record<string, ModelPreference>;
}

export type PreferenceScope =
  | "static_default"
  | "global_default"
  | "character_default"
  | "global_model"
  | "character_model";

export interface SamplerScopes {
  temperature?: PreferenceScope;
  top_p?: PreferenceScope;
  reasoning_effort?: PreferenceScope;
  thinking_enabled?: PreferenceScope;
  budget_tokens?: PreferenceScope;
  max_tokens?: PreferenceScope;
  cache_ttl?: PreferenceScope;
}

export type BackgroundTask = "heartbeat" | "compaction" | "dreaming";

export type SamplerSettingValue = number | string | boolean | null | undefined;

export function defaultModelPreferences(): ModelPreferences {
  return {
    selected: {},
    defaults: { sampler: {} },
    models: {},
  };
}

export function cloneSamplerSettings(sampler: SamplerSettings): SamplerSettings {
  return {
    ...(sampler.temperature !== undefined ? { temperature: sampler.temperature } : {}),
    ...(sampler.top_p !== undefined ? { top_p: sampler.top_p } : {}),
    ...(sampler.reasoning_effort !== undefined
      ? { reasoning_effort: sampler.reasoning_effort }
      : {}),
    ...(sampler.thinking_enabled !== undefined
      ? { thinking_enabled: sampler.thinking_enabled }
      : {}),
    ...(sampler.budget_tokens !== undefined ? { budget_tokens: sampler.budget_tokens } : {}),
    ...(sampler.max_tokens !== undefined ? { max_tokens: sampler.max_tokens } : {}),
    ...(sampler.cache_ttl !== undefined ? { cache_ttl: sampler.cache_ttl } : {}),
  };
}

export function cloneModelPreferences(prefs: ModelPreferences): ModelPreferences {
  const models: Record<string, ModelPreference> = {};
  for (const key of Object.keys(prefs.models).sort()) {
    const entry = prefs.models[key];
    if (entry !== undefined) models[key] = { sampler: cloneSamplerSettings(entry.sampler) };
  }
  return {
    selected: {
      ...(prefs.selected.provider !== undefined ? { provider: prefs.selected.provider } : {}),
      ...(prefs.selected.model_id !== undefined ? { model_id: prefs.selected.model_id } : {}),
    },
    defaults: { sampler: cloneSamplerSettings(prefs.defaults.sampler) },
    models,
  };
}

export function isSamplerSettingsEmpty(sampler: SamplerSettings): boolean {
  return SAMPLER_KEYS.every((key) => sampler[key] === undefined);
}

export function isModelPreferencesEmpty(prefs: ModelPreferences): boolean {
  return (
    !isSelectedModelSet(prefs.selected)
    && isSamplerSettingsEmpty(prefs.defaults.sampler)
    && Object.keys(prefs.models).length === 0
  );
}

export function applySamplerSettingsOverlay(
  base: SamplerSettings,
  overlay: SamplerSettings,
): SamplerSettings {
  const next = cloneSamplerSettings(base);
  for (const key of SAMPLER_KEYS) {
    const value = overlay[key];
    if (value !== undefined) {
      (next as Record<SamplerKey, number | string | boolean | undefined>)[key] = value;
    }
  }
  return next;
}

export function samplerSettingsFromResolvedModel(model: ResolvedModel): SamplerSettings {
  return {
    ...(model.temperature !== undefined ? { temperature: model.temperature } : {}),
    ...(model.topP !== undefined ? { top_p: model.topP } : {}),
    ...(model.reasoningEffort !== undefined
      ? { reasoning_effort: model.reasoningEffort }
      : {}),
    ...(model.budgetTokens !== undefined ? { budget_tokens: model.budgetTokens } : {}),
    ...(model.maxTokens !== undefined ? { max_tokens: model.maxTokens } : {}),
    ...(model.cacheTtl !== undefined ? { cache_ttl: model.cacheTtl } : {}),
  };
}

export function isSelectedModelSet(selected: SelectedModel): boolean {
  return selected.provider !== undefined && selected.model_id !== undefined;
}

export function selectedModelPair(selected: SelectedModel): [string, string] | undefined {
  const provider = selected.provider;
  const modelId = selected.model_id;
  if (provider === undefined || modelId === undefined) return undefined;
  return [provider, modelId];
}
