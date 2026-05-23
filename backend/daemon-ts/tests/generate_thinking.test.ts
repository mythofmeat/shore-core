/**
 * Tests for `buildThinkingConfig` — verifies ResolvedModel → ThinkingConfig
 * derivation + per-call overrides priority. Documented order:
 *   1. overrides.thinking_budget (0 → disabled, >0 → enabled with budget)
 *   2. ResolvedModel.reasoningEffort
 *   3. ResolvedModel.budgetTokens
 *   4. off
 */
import { describe, expect, it } from "bun:test";

import type { ResolvedModel } from "../src/llm/catalog.ts";
import { buildThinkingConfig } from "../src/llm/generate.ts";

function model(partial: Partial<ResolvedModel>): ResolvedModel {
  return {
    name: "test",
    qualifiedName: "chat.test.test",
    category: "chat",
    providerKey: "test",
    sdk: "anthropic",
    modelId: "test/test",
    apiKeyEnv: undefined,
    baseUrl: undefined,
    maxTokens: undefined,
    maxContextTokens: undefined,
    temperature: undefined,
    topP: undefined,
    reasoningEffort: undefined,
    budgetTokens: undefined,
    cacheTtl: undefined,
    openrouterProvider: undefined,
    ...partial,
  };
}

describe("buildThinkingConfig", () => {
  it("returns disabled when nothing is set", () => {
    expect(buildThinkingConfig(model({}), undefined)).toEqual({ enabled: false });
  });

  it("override thinking_budget > 0 enables thinking with that budget", () => {
    const got = buildThinkingConfig(model({}), { thinking_budget: 4096 });
    expect(got).toEqual({ enabled: true, budgetTokens: 4096 });
  });

  it("override thinking_budget = 0 disables thinking even if catalog enables it", () => {
    const got = buildThinkingConfig(
      model({ reasoningEffort: "high" }),
      { thinking_budget: 0 },
    );
    expect(got).toEqual({ enabled: false });
  });

  it("catalog reasoningEffort enables thinking with that effort", () => {
    expect(buildThinkingConfig(model({ reasoningEffort: "low" }), undefined)).toEqual({
      enabled: true,
      effort: "low",
    });
  });

  it("catalog reasoningEffort + budgetTokens passes both through", () => {
    expect(
      buildThinkingConfig(
        model({ reasoningEffort: "adaptive", budgetTokens: 8192 }),
        undefined,
      ),
    ).toEqual({ enabled: true, effort: "adaptive", budgetTokens: 8192 });
  });

  it("catalog budgetTokens alone enables thinking", () => {
    expect(buildThinkingConfig(model({ budgetTokens: 2048 }), undefined)).toEqual({
      enabled: true,
      budgetTokens: 2048,
    });
  });

  it("override budget wins over catalog effort", () => {
    expect(
      buildThinkingConfig(
        model({ reasoningEffort: "high", budgetTokens: 8192 }),
        { thinking_budget: 1024 },
      ),
    ).toEqual({ enabled: true, budgetTokens: 1024 });
  });
});
