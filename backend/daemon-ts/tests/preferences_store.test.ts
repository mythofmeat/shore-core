import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import {
  PreferenceError,
  characterPreferencesPath,
  clearOverrides,
  clearSelectedModel,
  defaultModelPreferences,
  globalPreferencesPath,
  isModelPreferencesEmpty,
  loadForCharacter,
  loadPreferences,
  modelPreference,
  saveCharacterPreferences,
  saveGlobalPreferences,
  savePreferences,
  setModelPreference,
  setModelSetting,
  setSamplerOverride,
  setSelectedModel,
  type ModelPreference,
  type ModelPreferences,
} from "../src/preferences/index.ts";

function tempDir(): string {
  return mkdtempSync(path.join(tmpdir(), "shore-preferences-store-test-"));
}

function writePrefs(dir: string, body: string): string {
  const file = path.join(dir, "models.toml");
  fs.writeFileSync(file, body);
  return file;
}

describe("preferences store", () => {
  it("builds Rust-compatible preference paths", () => {
    expect(globalPreferencesPath("/tmp/shore")).toBe("/tmp/shore/preferences/models.toml");
    expect(characterPreferencesPath("/tmp/shore", "alice")).toBe(
      "/tmp/shore/alice/preferences/models.toml",
    );
  });

  it("missing and empty files yield default preferences", () => {
    const dir = tempDir();
    expect(isModelPreferencesEmpty(loadPreferences(path.join(dir, "missing.toml")))).toBe(true);
    expect(isModelPreferencesEmpty(loadPreferences(writePrefs(dir, "")))).toBe(true);
  });

  it("loads the flattened Rust TOML schema", () => {
    const dir = tempDir();
    const file = writePrefs(dir, `
[selected]
provider = "openrouter"
model_id = "anthropic/claude-sonnet-4.5"

[defaults.sampler]
temperature = 1.0

[models."openrouter:anthropic/claude-sonnet-4.5"]
temperature = 0.8
top_p = 0.95
reasoning_effort = "medium"

[models."openrouter:google/gemini-2.5-flash"]
temperature = 1.2
top_p = 0.9
reasoning_effort = "off"
`);

    const prefs = loadPreferences(file);
    expect(prefs.selected).toEqual({
      provider: "openrouter",
      model_id: "anthropic/claude-sonnet-4.5",
    });
    expect(prefs.defaults.sampler.temperature).toBe(1);
    expect(Object.keys(prefs.models).length).toBe(2);
    expect(
      modelPreference(prefs, "openrouter", "anthropic/claude-sonnet-4.5")?.sampler,
    ).toMatchObject({
      temperature: 0.8,
      top_p: 0.95,
      reasoning_effort: "medium",
    });
    expect(
      modelPreference(prefs, "openrouter", "google/gemini-2.5-flash")?.sampler.reasoning_effort,
    ).toBe("off");
  });

  it("save then load round-trips all sampler fields", () => {
    const dir = tempDir();
    const file = path.join(dir, "models.toml");
    const prefs = defaultModelPreferences();
    setSelectedModel(prefs, "anthropic", "claude-sonnet-4-5");
    prefs.defaults.sampler.temperature = 1.0;
    setModelPreference(prefs, "anthropic", "claude-sonnet-4-5", {
      sampler: {
        temperature: 0.7,
        top_p: 0.95,
        reasoning_effort: "high",
        thinking_enabled: true,
        budget_tokens: 8192,
        max_tokens: 4096,
        cache_ttl: "5m",
      },
    });

    savePreferences(file, prefs);
    const reloaded = loadPreferences(file);
    expect(reloaded).toEqual(prefs);
    expect(fs.readFileSync(file, "utf8")).toContain(
      '[models."anthropic:claude-sonnet-4-5"]',
    );
    expect(fs.readFileSync(file, "utf8")).not.toContain("sampler =");
  });

  it("global and per-character helpers round-trip independently", () => {
    const dir = tempDir();
    const global = defaultModelPreferences();
    global.defaults.sampler.temperature = 1.0;
    const character = defaultModelPreferences();
    setSelectedModel(character, "anthropic", "claude-opus-4-6");

    saveGlobalPreferences(dir, global);
    saveCharacterPreferences(dir, "alice", character);
    const [loadedGlobal, loadedCharacter] = loadForCharacter(dir, "alice");
    expect(loadedGlobal.defaults.sampler.temperature).toBe(1);
    expect(loadedCharacter.selected.model_id).toBe("claude-opus-4-6");
  });

  it("malformed TOML and unknown fields are parse errors", () => {
    const dir = tempDir();
    expect(() => loadPreferences(writePrefs(dir, "this is not valid toml \n="))).toThrow(
      PreferenceError,
    );
    expect(() => loadPreferences(writePrefs(dir, "typo_field = true\n"))).toThrow(
      /unknown field/,
    );
    expect(() =>
      loadPreferences(writePrefs(dir, `
[models."openrouter:foo/bar"]
temperature = 0.5
typo_setting = "x"
`)),
    ).toThrow(/unknown field/);
  });

  it("setters preserve sticky per-model settings and clear empty overrides", () => {
    const prefs = defaultModelPreferences();
    const opus: ModelPreference = { sampler: { temperature: 0.7 } };
    const sonnet: ModelPreference = { sampler: { temperature: 1.2 } };
    setModelPreference(prefs, "anthropic", "opus", opus);
    setModelPreference(prefs, "anthropic", "sonnet", sonnet);
    setSelectedModel(prefs, "anthropic", "sonnet");
    setSelectedModel(prefs, "anthropic", "opus");

    expect(modelPreference(prefs, "anthropic", "opus")?.sampler.temperature).toBe(0.7);
    expect(modelPreference(prefs, "anthropic", "sonnet")?.sampler.temperature).toBe(1.2);

    setSamplerOverride(prefs, "anthropic", "opus", "temperature", null);
    expect(modelPreference(prefs, "anthropic", "opus")).toBeUndefined();
    expect(clearOverrides(prefs, "anthropic", "sonnet")?.sampler.temperature).toBe(1.2);
    expect(modelPreference(prefs, "anthropic", "sonnet")).toBeUndefined();
    expect(clearSelectedModel(prefs)).toEqual({ provider: "anthropic", model_id: "opus" });
  });

  it("persistent setModelSetting writes global and character scopes", () => {
    const dir = tempDir();
    setModelSetting(dir, "global", undefined, "openrouter", "gpt-4o", "top_p", 0.9);
    setModelSetting(dir, "character", "alice", "openrouter", "gpt-4o", "temperature", 0.8);

    const [global, character] = loadForCharacter(dir, "alice");
    expect(modelPreference(global, "openrouter", "gpt-4o")?.sampler.top_p).toBe(0.9);
    expect(modelPreference(character, "openrouter", "gpt-4o")?.sampler.temperature).toBe(0.8);
  });

  it("setModelSetting validates keys and value types", () => {
    const prefs: ModelPreferences = defaultModelPreferences();
    expect(() =>
      setSamplerOverride(prefs, "anthropic", "opus", "temperature", "hot"),
    ).toThrow(/temperature must be a number/);
    expect(() =>
      setSamplerOverride(prefs, "anthropic", "opus", "max_tokens", 1.5),
    ).toThrow(/non-negative integer/);
  });
});
