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

test("gemini-3.1 Pro drops `minimal` via model_override (Flash keeps it)", () => {
  // Issue #166, grounded in https://ai.google.dev/gemini-api/docs/gemini-3 —
  // Pro exposes thinkingLevel low|medium|high; `minimal` is a Flash/Flash-Lite/
  // Flash-Image level (their default). The override is Pro-specific on purpose.
  const pro = "google/gemini-3.1-pro-preview";
  expect(reasoningDomain("gemini", pro)).toEqual(["low", "medium", "high"]);
  expect(foldEffort("gemini", "minimal", pro)).toBeUndefined();
  expect(foldEffort("gemini", "low", pro)).toBe("low");
  // The Pro-specific match must NOT catch Flash 3.1 ids — they keep `minimal`.
  for (const flash of [
    "google/gemini-3.1-flash-image-preview",
    "google/gemini-3.1-flash-lite",
    "gemini-3.5-flash",
  ]) {
    expect(reasoningDomain("gemini", flash)).toContain("minimal");
  }
});
