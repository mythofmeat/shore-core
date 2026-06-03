/**
 * Vercel AI SDK adapter tests (issue #164) — DeepSeek / Moonshot native path.
 * Pure functions only (no network): provider-option reasoning mapping and
 * canonical-turn → AI SDK `ModelMessage` conversion.
 */

import { describe, expect, test } from "bun:test";

import { buildCall, buildProviderOptions, turnToVercel } from "../src/llm/providers/vercel.ts";
import type { SidecarRequest, TurnMessage } from "../src/llm/types.ts";

function req(sdk: "deepseek" | "moonshot", provider_options?: Record<string, unknown>): SidecarRequest {
  return {
    sdk,
    model: sdk === "deepseek" ? "deepseek-reasoner" : "kimi-k2-thinking",
    api_key: "sk-test",
    messages: [],
    max_tokens: 1024,
    ...(provider_options ? { provider_options } : {}),
  };
}

describe("buildProviderOptions", () => {
  test("thinking_enabled=false → thinking disabled (both vendors)", () => {
    expect(buildProviderOptions(req("deepseek", { thinking_enabled: false }))).toEqual({
      deepseek: { thinking: { type: "disabled" } },
    });
    expect(buildProviderOptions(req("moonshot", { thinking_enabled: false }))).toEqual({
      moonshotai: { thinking: { type: "disabled" } },
    });
  });

  test("disable wins over a present effort/budget", () => {
    expect(
      buildProviderOptions(req("deepseek", { thinking_enabled: false, reasoning_effort: "high" })),
    ).toEqual({ deepseek: { thinking: { type: "disabled" } } });
  });

  test("deepseek reasoning_effort → reasoningEffort", () => {
    expect(buildProviderOptions(req("deepseek", { reasoning_effort: "high" }))).toEqual({
      deepseek: { reasoningEffort: "high" },
    });
  });

  test("moonshot budget_tokens → thinking.budgetTokens", () => {
    expect(buildProviderOptions(req("moonshot", { budget_tokens: 4096 }))).toEqual({
      moonshotai: { thinking: { type: "enabled", budgetTokens: 4096 } },
    });
  });

  test("no reasoning options → undefined", () => {
    expect(buildProviderOptions(req("deepseek"))).toBeUndefined();
    expect(buildProviderOptions(req("moonshot"))).toBeUndefined();
    // deepseek ignores budget; moonshot ignores effort — each → undefined.
    expect(buildProviderOptions(req("deepseek", { budget_tokens: 4096 }))).toBeUndefined();
    expect(buildProviderOptions(req("moonshot", { reasoning_effort: "high" }))).toBeUndefined();
  });
});

describe("turnToVercel", () => {
  const names = new Map<string, string>([["tc_1", "search"]]);
  const conv = (t: TurnMessage) => turnToVercel(t, names);

  test("assistant thinking + text + tool_use → reasoning/text/tool-call parts", () => {
    const [m] = conv({
      role: "assistant",
      content: [
        { type: "thinking", thinking: "ponder" },
        { type: "text", text: "answer" },
        { type: "tool_use", id: "tc_1", name: "search", input: { q: "x" } },
      ],
    });
    expect(m?.role).toBe("assistant");
    expect(m?.content).toEqual([
      { type: "reasoning", text: "ponder" },
      { type: "text", text: "answer" },
      { type: "tool-call", toolCallId: "tc_1", toolName: "search", input: { q: "x" } },
    ]);
  });

  test("user tool_result → role:tool with recovered toolName", () => {
    const out = conv({
      role: "user",
      content: [{ type: "tool_result", tool_use_id: "tc_1", content: "result" }],
    });
    expect(out[0]).toEqual({
      role: "tool",
      content: [
        { type: "tool-result", toolCallId: "tc_1", toolName: "search", output: { type: "text", value: "result" } },
      ],
    });
  });

  test("tool_result with unknown tool_use_id throws (no empty toolName)", () => {
    expect(() =>
      conv({
        role: "user",
        content: [{ type: "tool_result", tool_use_id: "tc_missing", content: "result" }],
      }),
    ).toThrow(/unknown tool_use_id: tc_missing/);
  });

  test("user text → single role:user message", () => {
    const out = conv({ role: "user", content: [{ type: "text", text: "hi" }] });
    expect(out).toEqual([{ role: "user", content: [{ type: "text", text: "hi" }] }]);
  });

  test("empty assistant turn → no message", () => {
    expect(conv({ role: "assistant", content: [] })).toEqual([]);
  });
});

describe("buildMessages tool-name causality", () => {
  const withMessages = (messages: SidecarRequest["messages"]): SidecarRequest => ({
    ...req("deepseek"),
    messages,
  });

  test("tool_result resolves a tool_use from an earlier turn", () => {
    const call = buildCall(
      withMessages([
        { role: "assistant", content: [{ type: "tool_use", id: "tc_1", name: "search", input: {} }] },
        { role: "user", content: [{ type: "tool_result", tool_use_id: "tc_1", content: "ok" }] },
      ]),
    );
    const toolMsg = call.messages?.find((m) => m.role === "tool");
    expect(toolMsg?.content).toEqual([
      { type: "tool-result", toolCallId: "tc_1", toolName: "search", output: { type: "text", value: "ok" } },
    ]);
  });

  test("tool_result pointing at a LATER tool_use throws (causality)", () => {
    expect(() =>
      buildCall(
        withMessages([
          { role: "user", content: [{ type: "tool_result", tool_use_id: "tc_1", content: "ok" }] },
          { role: "assistant", content: [{ type: "tool_use", id: "tc_1", name: "search", input: {} }] },
        ]),
      ),
    ).toThrow(/unknown tool_use_id: tc_1/);
  });
});
