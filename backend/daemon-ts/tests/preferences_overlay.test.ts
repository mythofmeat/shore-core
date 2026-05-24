import { describe, expect, it } from "bun:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { ConversationEngine } from "../src/engine/engine.ts";
import type { Message } from "../src/engine/types.ts";
import type { ResolvedModel } from "../src/llm/catalog.ts";
import {
  buildThinkingConfig,
  prepareChatRequest,
} from "../src/llm/generate.ts";
import { ToolRegistry } from "../src/tools/registry.ts";
import {
  applySamplerOverlay,
  defaultModelPreferences,
  saveCharacterPreferences,
  saveGlobalPreferences,
  setModelPreference,
} from "../src/preferences/index.ts";

function tempDir(): string {
  return mkdtempSync(path.join(tmpdir(), "shore-preferences-overlay-test-"));
}

function model(partial: Partial<ResolvedModel> = {}): ResolvedModel {
  const sdk = partial.sdk ?? "anthropic";
  return {
    name: partial.name ?? "opus",
    qualifiedName: partial.qualifiedName ?? "chat.anthropic.opus",
    category: partial.category ?? "chat",
    providerKey: partial.providerKey ?? "anthropic",
    sdk,
    modelId: partial.modelId ?? "claude-opus-4-6",
    apiKeyEnv: partial.apiKeyEnv,
    baseUrl: partial.baseUrl,
    maxTokens: partial.maxTokens ?? 4096,
    maxContextTokens: partial.maxContextTokens ?? 200_000,
    temperature: partial.temperature,
    topP: partial.topP,
    reasoningEffort: partial.reasoningEffort,
    budgetTokens: partial.budgetTokens,
    cacheTtl: partial.cacheTtl ?? (sdk === "anthropic" ? "1h" : undefined),
    openrouterProvider: partial.openrouterProvider,
  };
}

async function requestFixture(resolved: ResolvedModel): Promise<{
  root: string;
  dataDir: string;
  engine: ConversationEngine;
  configDir: string;
  characterConfigDir: string;
}> {
  const root = tempDir();
  const dataDir = path.join(root, "data");
  const configDir = path.join(root, "config");
  const characterConfigDir = path.join(configDir, "characters", "alice");
  const engine = new ConversationEngine("alice", path.join(dataDir, "alice"));
  const user: Message = {
    msg_id: "m_user",
    role: "user",
    content: "hello",
    images: [],
    content_blocks: [{ type: "text", text: "hello" }],
    timestamp: "2026-05-24T12:00:00Z",
  };
  await engine.appendMessage(user);
  void resolved;
  return { root, dataDir, engine, configDir, characterConfigDir };
}

describe("applySamplerOverlay", () => {
  it("patches only set fields and does not mutate the input model", () => {
    const base = model({
      temperature: 1.0,
      topP: 0.9,
      maxTokens: 4096,
      reasoningEffort: "medium",
    });
    const patched = applySamplerOverlay(base, {
      temperature: 0.7,
      budget_tokens: 2048,
    });

    expect(patched).toMatchObject({
      temperature: 0.7,
      topP: 0.9,
      maxTokens: 4096,
      reasoningEffort: "medium",
      budgetTokens: 2048,
    });
    expect(base.temperature).toBe(1.0);
    expect(base.budgetTokens).toBeUndefined();
  });

  it("uses reasoning_effort = off as the Rust clear sentinel", () => {
    const base = model({ reasoningEffort: "high" });
    const patched = applySamplerOverlay(base, { reasoning_effort: "off" });
    expect(patched.reasoningEffort).toBeUndefined();
    expect(buildThinkingConfig(patched, undefined)).toEqual({ enabled: false });
  });

  it("off clears only reasoning_effort and preserves an explicit budget", () => {
    const patched = applySamplerOverlay(
      model({ reasoningEffort: "high", budgetTokens: 4096 }),
      { reasoning_effort: "off" },
    );
    expect(patched.reasoningEffort).toBeUndefined();
    expect(patched.budgetTokens).toBe(4096);
    expect(buildThinkingConfig(patched, undefined)).toEqual({
      enabled: true,
      budgetTokens: 4096,
    });
  });

  it("ignores thinking_enabled because Rust stores it but does not apply it yet", () => {
    const patched = applySamplerOverlay(
      model({ reasoningEffort: "high" }),
      { thinking_enabled: false },
    );
    expect(patched.reasoningEffort).toBe("high");
    expect(buildThinkingConfig(patched, undefined)).toEqual({
      enabled: true,
      effort: "high",
    });
  });
});

describe("request-build sampler precedence", () => {
  it("applies per-call overrides over character, global, and catalog defaults", async () => {
    const resolved = model({
      temperature: 1.0,
      topP: 0.5,
      reasoningEffort: "high",
      budgetTokens: 4096,
      maxTokens: 4096,
      cacheTtl: "1h",
    });
    const { dataDir, engine, configDir, characterConfigDir } = await requestFixture(resolved);

    const global = defaultModelPreferences();
    setModelPreference(global, "anthropic", "claude-opus-4-6", {
      sampler: {
        temperature: 0.7,
        top_p: 0.8,
        reasoning_effort: "medium",
        max_tokens: 8192,
        cache_ttl: "5m",
      },
    });
    saveGlobalPreferences(dataDir, global);

    const character = defaultModelPreferences();
    setModelPreference(character, "anthropic", "claude-opus-4-6", {
      sampler: {
        temperature: 0.3,
        max_tokens: 12000,
      },
    });
    saveCharacterPreferences(dataDir, "alice", character);

    const request = prepareChatRequest({
      engine,
      characterConfigDir,
      configDir,
      displayName: "Ren",
      resolved,
      registry: new ToolRegistry(),
      apiKey: "test-key",
      overrides: {
        temperature: 0.1,
        top_p: 0.2,
        thinking_budget: 0,
      },
    });

    expect(request.temperature).toBe(0.1);
    expect(request.topP).toBe(0.2);
    expect(request.maxTokens).toBe(12000);
    expect(request.cacheTtl).toBe("5m");
    expect(request.thinking).toEqual({ enabled: false });
  });

  it("uses stored character reasoning when no per-call thinking override is present", async () => {
    const resolved = model({ reasoningEffort: "high", budgetTokens: undefined });
    const { dataDir, engine, configDir, characterConfigDir } = await requestFixture(resolved);

    const global = defaultModelPreferences();
    setModelPreference(global, "anthropic", "claude-opus-4-6", {
      sampler: { reasoning_effort: "medium" },
    });
    saveGlobalPreferences(dataDir, global);

    const character = defaultModelPreferences();
    setModelPreference(character, "anthropic", "claude-opus-4-6", {
      sampler: { reasoning_effort: "off" },
    });
    saveCharacterPreferences(dataDir, "alice", character);

    const request = prepareChatRequest({
      engine,
      characterConfigDir,
      configDir,
      displayName: "Ren",
      resolved,
      registry: new ToolRegistry(),
      apiKey: "test-key",
    });

    expect(request.thinking).toEqual({ enabled: false });
  });

  it("missing preference files leave catalog defaults unchanged", async () => {
    const resolved = model({ temperature: 1.0, topP: 0.9, maxTokens: 4096 });
    const { engine, configDir, characterConfigDir } = await requestFixture(resolved);

    const request = prepareChatRequest({
      engine,
      characterConfigDir,
      configDir,
      displayName: "Ren",
      resolved,
      registry: new ToolRegistry(),
      apiKey: "test-key",
    });

    expect(request.temperature).toBe(1.0);
    expect(request.topP).toBe(0.9);
    expect(request.maxTokens).toBe(4096);
  });
});
