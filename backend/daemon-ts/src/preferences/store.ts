import fs from "node:fs";
import path from "node:path";

import { parse as parseToml, stringify as stringifyToml } from "smol-toml";

import {
  SAMPLER_KEYS,
  cloneModelPreferences,
  cloneSamplerSettings,
  defaultModelPreferences,
  isSamplerSettingsEmpty,
  type ModelPreference,
  type ModelPreferences,
  type SamplerKey,
  type SamplerSettingValue,
  type SamplerSettings,
  type SelectedModel,
} from "./types.ts";

const PREFERENCES_DIR = "preferences";
const PREFERENCES_FILE = "models.toml";
const U32_MAX = 0xffff_ffff;

export type PreferenceErrorKind = "read" | "write" | "parse" | "serialize";

export class PreferenceError extends Error {
  constructor(
    readonly kind: PreferenceErrorKind,
    readonly file: string | undefined,
    message: string,
    readonly source: unknown,
  ) {
    super(formatPreferenceError(kind, file, message));
    this.name = "PreferenceError";
  }
}

export function preferenceKey(provider: string, modelId: string): string {
  return `${provider}:${modelId}`;
}

export function globalPreferencesPath(dataDir: string): string {
  return path.join(dataDir, PREFERENCES_DIR, PREFERENCES_FILE);
}

export function characterPreferencesPath(dataDir: string, character: string): string {
  return path.join(dataDir, character, PREFERENCES_DIR, PREFERENCES_FILE);
}

export function loadPreferences(file: string): ModelPreferences {
  let content: string;
  try {
    content = fs.readFileSync(file, "utf8");
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") {
      return defaultModelPreferences();
    }
    throw new PreferenceError("read", file, (e as Error).message, e);
  }

  let parsed: unknown;
  try {
    parsed = parseToml(content);
  } catch (e) {
    throw new PreferenceError("parse", file, (e as Error).message, e);
  }

  try {
    return parseModelPreferences(parsed);
  } catch (e) {
    if (e instanceof PreferenceError) throw e;
    throw new PreferenceError("parse", file, (e as Error).message, e);
  }
}

export function savePreferences(file: string, prefs: ModelPreferences): void {
  let body: string;
  try {
    body = stringifyToml(toTomlShape(prefs));
  } catch (e) {
    throw new PreferenceError("serialize", undefined, (e as Error).message, e);
  }
  if (!body.endsWith("\n")) body += "\n";

  try {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, body);
  } catch (e) {
    throw new PreferenceError("write", file, (e as Error).message, e);
  }
}

export function loadForCharacter(
  dataDir: string,
  character: string,
): [ModelPreferences, ModelPreferences] {
  const global = loadPreferences(globalPreferencesPath(dataDir));
  const characterPrefs = loadPreferences(characterPreferencesPath(dataDir, character));
  return [global, characterPrefs];
}

export function saveCharacterPreferences(
  dataDir: string,
  character: string,
  prefs: ModelPreferences,
): void {
  savePreferences(characterPreferencesPath(dataDir, character), prefs);
}

export function saveGlobalPreferences(dataDir: string, prefs: ModelPreferences): void {
  savePreferences(globalPreferencesPath(dataDir), prefs);
}

export function modelPreference(
  prefs: ModelPreferences,
  provider: string,
  modelId: string,
): ModelPreference | undefined {
  return prefs.models[preferenceKey(provider, modelId)];
}

export function setModelPreference(
  prefs: ModelPreferences,
  provider: string,
  modelId: string,
  pref: ModelPreference,
): void {
  prefs.models[preferenceKey(provider, modelId)] = {
    sampler: cloneSamplerSettings(pref.sampler),
  };
}

export function clearModelPreference(
  prefs: ModelPreferences,
  provider: string,
  modelId: string,
): ModelPreference | undefined {
  const key = preferenceKey(provider, modelId);
  const previous = prefs.models[key];
  delete prefs.models[key];
  return previous;
}

export function setSelectedModel(
  prefs: ModelPreferences,
  provider: string,
  modelId: string,
): void {
  prefs.selected = { provider, model_id: modelId };
}

export function clearSelectedModel(prefs: ModelPreferences): SelectedModel {
  const previous = { ...prefs.selected };
  prefs.selected = {};
  return previous;
}

export function setSamplerOverride(
  prefs: ModelPreferences,
  provider: string,
  modelId: string,
  key: SamplerKey,
  value: SamplerSettingValue,
): void {
  if (!SAMPLER_KEYS.includes(key)) {
    throw new Error(`unknown setting key: ${key}; supported: ${SAMPLER_KEYS.join(", ")}`);
  }

  const prefKey = preferenceKey(provider, modelId);
  const existing = prefs.models[prefKey] ?? { sampler: {} };
  applySamplerValue(existing.sampler, key, value);
  if (isSamplerSettingsEmpty(existing.sampler)) {
    delete prefs.models[prefKey];
  } else {
    prefs.models[prefKey] = existing;
  }
}

export function clearOverrides(
  prefs: ModelPreferences,
  provider: string,
  modelId: string,
): ModelPreference | undefined {
  return clearModelPreference(prefs, provider, modelId);
}

export function switchModelSelection(
  dataDir: string,
  character: string,
  provider: string,
  modelId: string,
): ModelPreferences {
  const prefs = loadPreferences(characterPreferencesPath(dataDir, character));
  setSelectedModel(prefs, provider, modelId);
  saveCharacterPreferences(dataDir, character, prefs);
  return cloneModelPreferences(prefs);
}

export function resetModelSelection(
  dataDir: string,
  character: string,
): { prefs: ModelPreferences; previous: SelectedModel } {
  const prefs = loadPreferences(characterPreferencesPath(dataDir, character));
  const previous = clearSelectedModel(prefs);
  saveCharacterPreferences(dataDir, character, prefs);
  return { prefs: cloneModelPreferences(prefs), previous };
}

export function setModelSetting(
  dataDir: string,
  scope: "character" | "global",
  character: string | undefined,
  provider: string,
  modelId: string,
  key: SamplerKey,
  value: SamplerSettingValue,
): ModelPreferences {
  const file = scope === "global"
    ? globalPreferencesPath(dataDir)
    : characterPreferencesPath(dataDir, requireCharacter(character));
  const prefs = loadPreferences(file);
  setSamplerOverride(prefs, provider, modelId, key, value);
  savePreferences(file, prefs);
  return cloneModelPreferences(prefs);
}

function parseModelPreferences(raw: unknown): ModelPreferences {
  assertPlainObject(raw, "preferences");
  rejectUnknownKeys(raw, ["selected", "defaults", "models"], "preferences");

  const selected = parseSelectedModel(optionalTable(raw, "selected"), "selected");
  const defaults = optionalTable(raw, "defaults");
  rejectUnknownKeys(defaults, ["sampler"], "defaults");
  const defaultsSampler = parseSamplerSettings(
    optionalTable(defaults, "sampler"),
    "defaults.sampler",
  );

  const modelsTable = optionalTable(raw, "models");
  const models: Record<string, ModelPreference> = {};
  for (const key of Object.keys(modelsTable).sort()) {
    const value = modelsTable[key];
    assertPlainObject(value, `models.${JSON.stringify(key)}`);
    models[key] = {
      sampler: parseSamplerSettings(value, `models.${JSON.stringify(key)}`),
    };
  }

  return { selected, defaults: { sampler: defaultsSampler }, models };
}

function parseSelectedModel(raw: Record<string, unknown>, ctx: string): SelectedModel {
  rejectUnknownKeys(raw, ["provider", "model_id"], ctx);
  const out: SelectedModel = {};
  const provider = raw["provider"];
  const modelId = raw["model_id"];
  if (provider !== undefined) {
    if (typeof provider !== "string") throw new Error(`${ctx}.provider must be a string`);
    out.provider = provider;
  }
  if (modelId !== undefined) {
    if (typeof modelId !== "string") throw new Error(`${ctx}.model_id must be a string`);
    out.model_id = modelId;
  }
  return out;
}

function parseSamplerSettings(raw: Record<string, unknown>, ctx: string): SamplerSettings {
  rejectUnknownKeys(raw, [...SAMPLER_KEYS], ctx);
  const out: SamplerSettings = {};
  for (const key of SAMPLER_KEYS) {
    const value = raw[key];
    if (value !== undefined) applySamplerValue(out, key, value);
  }
  return out;
}

function applySamplerValue(
  sampler: SamplerSettings,
  key: SamplerKey,
  value: unknown,
): void {
  if (value === null || value === undefined) {
    delete sampler[key];
    return;
  }

  switch (key) {
    case "temperature":
    case "top_p":
      sampler[key] = requireFiniteNumber(value, key);
      return;
    case "reasoning_effort":
    case "cache_ttl":
      if (typeof value !== "string") throw new Error(`${key} must be a string`);
      sampler[key] = value;
      return;
    case "thinking_enabled":
      if (typeof value !== "boolean") throw new Error(`${key} must be a boolean`);
      sampler.thinking_enabled = value;
      return;
    case "budget_tokens":
    case "max_tokens":
      sampler[key] = requireU32(value, key);
      return;
  }
}

function requireFiniteNumber(value: unknown, key: string): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new Error(`${key} must be a number`);
  }
  return value;
}

function requireU32(value: unknown, key: string): number {
  if (
    typeof value !== "number"
    || !Number.isInteger(value)
    || value < 0
    || value > U32_MAX
  ) {
    throw new Error(`${key} must be a non-negative integer fitting in u32`);
  }
  return value;
}

function optionalTable(raw: Record<string, unknown>, key: string): Record<string, unknown> {
  const value = raw[key];
  if (value === undefined) return {};
  assertPlainObject(value, key);
  return value;
}

function assertPlainObject(value: unknown, ctx: string): asserts value is Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`${ctx} must be a TOML table`);
  }
}

function rejectUnknownKeys(
  raw: Record<string, unknown>,
  allowed: readonly string[],
  ctx: string,
): void {
  const allowedSet = new Set(allowed);
  for (const key of Object.keys(raw)) {
    if (!allowedSet.has(key)) {
      throw new Error(`unknown field ${JSON.stringify(key)} in ${ctx}`);
    }
  }
}

function toTomlShape(prefs: ModelPreferences): Record<string, unknown> {
  const clean = cloneModelPreferences(prefs);
  const models: Record<string, unknown> = {};
  for (const key of Object.keys(clean.models).sort()) {
    const entry = clean.models[key];
    if (entry !== undefined) models[key] = samplerToToml(entry.sampler);
  }
  return {
    selected: selectedToToml(clean.selected),
    defaults: { sampler: samplerToToml(clean.defaults.sampler) },
    models,
  };
}

function selectedToToml(selected: SelectedModel): Record<string, unknown> {
  return {
    ...(selected.provider !== undefined ? { provider: selected.provider } : {}),
    ...(selected.model_id !== undefined ? { model_id: selected.model_id } : {}),
  };
}

function samplerToToml(sampler: SamplerSettings): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const key of SAMPLER_KEYS) {
    const value = sampler[key];
    if (value !== undefined) out[key] = value;
  }
  return out;
}

function requireCharacter(character: string | undefined): string {
  if (character === undefined || character.length === 0) {
    throw new Error("this operation requires an attached character");
  }
  return character;
}

function formatPreferenceError(
  kind: PreferenceErrorKind,
  file: string | undefined,
  message: string,
): string {
  switch (kind) {
    case "read":
      return `failed to read ${file ?? "<unknown>"}: ${message}`;
    case "write":
      return `failed to write ${file ?? "<unknown>"}: ${message}`;
    case "parse":
      return `failed to parse ${file ?? "<unknown>"}: ${message}`;
    case "serialize":
      return `failed to serialize preferences: ${message}`;
  }
}
