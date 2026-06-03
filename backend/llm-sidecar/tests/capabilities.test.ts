import { expect, test } from "bun:test";

import { effortBudget, foldEffort, geminiLevelName, reasoningDomain } from "../src/llm/capabilities.ts";

test("openai/openrouter accept xhigh (passthrough) and reject max", () => {
  for (const sdk of ["openai", "openrouter"] as const) {
    expect(reasoningDomain(sdk)).toEqual(["minimal", "low", "medium", "high", "xhigh"]);
    // xhigh is the real ceiling — sent as-is, NOT folded to high.
    expect(foldEffort(sdk, "xhigh")).toBe("xhigh");
    expect(foldEffort(sdk, "high")).toBe("high");
    // max is Anthropic-only — out of domain here, so nothing is sent.
    expect(foldEffort(sdk, "max")).toBeUndefined();
  }
});

test("anthropic keeps max/xhigh; effort→budget table intact", () => {
  expect(reasoningDomain("anthropic")).toContain("max");
  expect(reasoningDomain("anthropic")).toContain("xhigh");
  expect(effortBudget("max")).toBe(24576);
  expect(effortBudget("xhigh")).toBe(16384);
  expect(effortBudget("medium")).toBe(8192);
});

test("gemini thinking levels stop at high", () => {
  expect(geminiLevelName("high")).toBe("high");
  expect(geminiLevelName("max")).toBeUndefined();
  expect(geminiLevelName("xhigh")).toBeUndefined();
});
