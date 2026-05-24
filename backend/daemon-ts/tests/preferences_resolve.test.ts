import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import type { ResolvedModel } from "../src/llm/catalog.ts";
import {
  defaultModelPreferences,
  resolveActiveForCharacter,
  resolveBackgroundModel,
  resolveChatModelForCharacter,
  resolveSamplerScopes,
  resolveSamplerSettings,
  resolveSelectedModel,
  saveCharacterPreferences,
  saveGlobalPreferences,
  setModelPreference,
  setSelectedModel,
  type PreferenceResolutionConfig,
} from "../src/preferences/index.ts";

function tempDir(): string {
  return mkdtempSync(path.join(tmpdir(), "shore-preferences-resolve-test-"));
}

function model(name: string, partial: Partial<ResolvedModel> = {}): ResolvedModel {
  const providerKey = partial.providerKey ?? "anthropic";
  const modelId = partial.modelId ?? `claude-${name}-4-6`;
  const sdk = partial.sdk ?? (providerKey === "anthropic" ? "anthropic" : "openai");
  return {
    name,
    qualifiedName: partial.qualifiedName ?? `chat.${providerKey}.${name}`,
    category: partial.category ?? "chat",
    providerKey,
    sdk,
    modelId,
    apiKeyEnv: partial.apiKeyEnv ?? `${providerKey.toUpperCase()}_API_KEY`,
    baseUrl: partial.baseUrl,
    maxTokens: partial.maxTokens ?? 8192,
    maxContextTokens: partial.maxContextTokens ?? 200_000,
    temperature: partial.temperature ?? 1.0,
    topP: partial.topP,
    reasoningEffort: partial.reasoningEffort,
    budgetTokens: partial.budgetTokens,
    cacheTtl: partial.cacheTtl ?? (sdk === "anthropic" ? "1h" : undefined),
    openrouterProvider: partial.openrouterProvider,
  };
}

function makeCatalog(): Map<string, ResolvedModel> {
  const entries = [
    model("opus"),
    model("sonnet", { modelId: "claude-sonnet-4-6" }),
    model("haiku", { modelId: "claude-haiku-4-5", maxTokens: 4096 }),
    model("kimi", {
      providerKey: "openrouter",
      modelId: "kimi-k2",
      baseUrl: "https://openrouter.ai/api/v1",
      cacheTtl: undefined,
    }),
    model("bg", { modelId: "claude-bg-4-6", maxTokens: 32768 }),
  ];
  return new Map(entries.map((entry) => [entry.qualifiedName, entry]));
}

function config(dataDir: string, extra: Partial<PreferenceResolutionConfig> = {}): PreferenceResolutionConfig {
  return {
    catalog: makeCatalog(),
    dataDir,
    cacheDir: path.join(dataDir, "cache"),
    appDefaultModel: "kimi",
    ...extra,
  };
}

describe("selected-model resolution", () => {
  it("uses character selection before global and ignores partial selections", () => {
    const global = defaultModelPreferences();
    setSelectedModel(global, "anthropic", "claude-opus-4-6");
    const character = defaultModelPreferences();
    character.selected.provider = "openrouter";
    expect(resolveSelectedModel(global, character)).toEqual(["anthropic", "claude-opus-4-6"]);

    character.selected.model_id = "kimi-k2";
    expect(resolveSelectedModel(global, character)).toEqual(["openrouter", "kimi-k2"]);
  });

  it("resolveActiveForCharacter follows character, global, legacy, default, first-chat order", () => {
    const dataDir = tempDir();
    const cfg = config(dataDir);

    const global = defaultModelPreferences();
    const character = defaultModelPreferences();
    setSelectedModel(character, "anthropic", "claude-sonnet-4-6");
    expect(
      resolveActiveForCharacter(cfg, dataDir, global, character)?.qualifiedName,
    ).toBe("chat.anthropic.sonnet");

    const globalOnly = defaultModelPreferences();
    setSelectedModel(globalOnly, "openrouter", "kimi-k2");
    expect(
      resolveActiveForCharacter(cfg, dataDir, globalOnly, defaultModelPreferences())?.qualifiedName,
    ).toBe("chat.openrouter.kimi");

    expect(
      resolveActiveForCharacter(
        cfg,
        dataDir,
        defaultModelPreferences(),
        defaultModelPreferences(),
        "opus",
        undefined,
      )?.qualifiedName,
    ).toBe("chat.anthropic.opus");

    expect(
      resolveActiveForCharacter(
        cfg,
        dataDir,
        defaultModelPreferences(),
        defaultModelPreferences(),
        undefined,
        "sonnet",
      )?.qualifiedName,
    ).toBe("chat.anthropic.sonnet");

    expect(
      resolveActiveForCharacter(
        { catalog: new Map([["chat.anthropic.opus", model("opus")]]) },
        dataDir,
        defaultModelPreferences(),
        defaultModelPreferences(),
      )?.qualifiedName,
    ).toBe("chat.anthropic.opus");
  });

  it("restores selected discovered models from cache or provider registry", () => {
    const dataDir = tempDir();
    const cacheDir = path.join(dataDir, "cache");
    fs.mkdirSync(path.join(cacheDir, "providers", "openrouter"), { recursive: true });
    fs.writeFileSync(path.join(cacheDir, "providers", "openrouter", "models.json"), JSON.stringify({
      version: 1,
      provider_key: "openrouter",
      fetched_at: "2026-04-29T00:00:00Z",
      base_url: "https://openrouter.ai/api/v1",
      models: [{
        provider_key: "openrouter",
        model_id: "anthropic/claude-sonnet-4.5",
        sdk: "openai",
        base_url: "https://openrouter.ai/api/v1",
        context_length: 200000,
        max_output_tokens: 8192,
        raw_provider_metadata: null,
        discovered_at: "2026-04-29T00:00:00Z",
      }],
    }));
    const cfg = config(dataDir, {
      cacheDir,
      providers: {
        openrouter: {
          api_key_env: "OR_KEY",
          base_url: "https://openrouter.ai/api/v1",
          discovery: { enabled: true },
        },
      },
    });
    const character = defaultModelPreferences();
    setSelectedModel(character, "openrouter", "anthropic/claude-sonnet-4.5");

    const fromCache = resolveActiveForCharacter(
      cfg,
      dataDir,
      defaultModelPreferences(),
      character,
    );
    expect(fromCache?.providerKey).toBe("openrouter");
    expect(fromCache?.modelId).toBe("anthropic/claude-sonnet-4.5");
    expect(fromCache?.qualifiedName).not.toBe("chat.anthropic.opus");

    fs.rmSync(path.join(cacheDir, "providers", "openrouter", "models.json"));
    const synthesized = resolveActiveForCharacter(
      cfg,
      dataDir,
      defaultModelPreferences(),
      character,
    );
    expect(synthesized?.providerKey).toBe("openrouter");
    expect(synthesized?.modelId).toBe("anthropic/claude-sonnet-4.5");
    expect(synthesized?.baseUrl).toBe("https://openrouter.ai/api/v1");
  });
});

describe("sampler resolver and scopes", () => {
  it("merges static default, defaults, and per-model overrides in Rust precedence order", () => {
    const staticModel = model("opus", { cacheTtl: "1h", maxTokens: 8192 });
    const global = defaultModelPreferences();
    global.defaults.sampler.temperature = 0.1;
    global.defaults.sampler.top_p = 0.10;
    global.defaults.sampler.max_tokens = 100;
    global.defaults.sampler.budget_tokens = 1000;
    setModelPreference(global, "anthropic", "claude-opus-4-6", {
      sampler: { temperature: 0.2, top_p: 0.20 },
    });

    const character = defaultModelPreferences();
    character.defaults.sampler.top_p = 0.30;
    character.defaults.sampler.max_tokens = 200;
    setModelPreference(character, "anthropic", "claude-opus-4-6", {
      sampler: { temperature: 0.4 },
    });

    const sampler = resolveSamplerSettings(
      global,
      character,
      "anthropic",
      "claude-opus-4-6",
      staticModel,
    );
    expect(sampler.temperature).toBe(0.4);
    expect(sampler.top_p).toBe(0.20);
    expect(sampler.max_tokens).toBe(200);
    expect(sampler.budget_tokens).toBe(1000);
    expect(sampler.cache_ttl).toBe("1h");

    const scopes = resolveSamplerScopes(
      global,
      character,
      "anthropic",
      "claude-opus-4-6",
      staticModel,
    );
    expect(scopes.temperature).toBe("character_model");
    expect(scopes.top_p).toBe("global_model");
    expect(scopes.max_tokens).toBe("character_default");
    expect(scopes.budget_tokens).toBe("global_default");
    expect(scopes.cache_ttl).toBe("static_default");
  });

  it("returns empty sampler and empty scopes when no layer has the requested model", () => {
    const sampler = resolveSamplerSettings(
      defaultModelPreferences(),
      defaultModelPreferences(),
      "anthropic",
      "missing",
    );
    const scopes = resolveSamplerScopes(
      defaultModelPreferences(),
      defaultModelPreferences(),
      "anthropic",
      "missing",
    );
    expect(sampler).toEqual({});
    expect(scopes).toEqual({});
  });
});

describe("chat and background resolvers with preference overlays", () => {
  it("resolveChatModelForCharacter handles no overrides, global-only, character-only, and both set", () => {
    const dataDir = tempDir();
    const cfg = config(dataDir, { appDefaultModel: "opus" });

    expect(resolveChatModelForCharacter(cfg, "alice")?.temperature).toBe(1.0);

    const global = defaultModelPreferences();
    setModelPreference(global, "anthropic", "claude-opus-4-6", {
      sampler: { temperature: 0.7, top_p: 0.9 },
    });
    saveGlobalPreferences(dataDir, global);
    expect(resolveChatModelForCharacter(cfg, "alice")).toMatchObject({
      qualifiedName: "chat.anthropic.opus",
      temperature: 0.7,
      topP: 0.9,
    });

    const character = defaultModelPreferences();
    setModelPreference(character, "anthropic", "claude-opus-4-6", {
      sampler: { temperature: 0.4, max_tokens: 12345 },
    });
    saveCharacterPreferences(dataDir, "alice", character);
    expect(resolveChatModelForCharacter(cfg, "alice")).toMatchObject({
      temperature: 0.4,
      topP: 0.9,
      maxTokens: 12345,
    });

    setSelectedModel(character, "anthropic", "claude-sonnet-4-6");
    setModelPreference(character, "anthropic", "claude-sonnet-4-6", {
      sampler: { temperature: 0.2 },
    });
    saveCharacterPreferences(dataDir, "alice", character);
    expect(resolveChatModelForCharacter(cfg, "alice")).toMatchObject({
      qualifiedName: "chat.anthropic.sonnet",
      temperature: 0.2,
    });
  });

  it("resolveBackgroundModel uses task pin, blanket pin, then active chat with overlays", () => {
    const dataDir = tempDir();
    const character = defaultModelPreferences();
    setSelectedModel(character, "anthropic", "claude-sonnet-4-6");
    setModelPreference(character, "anthropic", "claude-sonnet-4-6", {
      sampler: { max_tokens: 12000 },
    });
    setModelPreference(character, "anthropic", "claude-bg-4-6", {
      sampler: { max_tokens: 64000 },
    });
    saveCharacterPreferences(dataDir, "alice", character);

    const followsChat = resolveBackgroundModel(config(dataDir, { appDefaultModel: "opus" }), "compaction", "alice");
    expect(followsChat).toMatchObject({
      qualifiedName: "chat.anthropic.sonnet",
      maxTokens: 12000,
    });

    const blanket = resolveBackgroundModel(
      config(dataDir, { backgroundDefaults: { model: "bg" } }),
      "dreaming",
      "alice",
    );
    expect(blanket).toMatchObject({
      qualifiedName: "chat.anthropic.bg",
      maxTokens: 64000,
    });

    const perTask = resolveBackgroundModel(
      config(dataDir, { backgroundDefaults: { model: "opus", heartbeat: "bg" } }),
      "heartbeat",
      "alice",
    );
    expect(perTask?.qualifiedName).toBe("chat.anthropic.bg");
  });
});
