/**
 * Wire-shape regressions for the Anthropic adapter's thinking +
 * output_config emission.
 *
 * Pins the split between `thinking` and `output_config.effort` that
 * the latest API expects:
 *
 *   - named effort (max/xhigh/high/medium/low) → adaptive thinking +
 *     output_config.effort carries the value
 *   - literal "adaptive" → adaptive thinking, no output_config
 *   - explicit budgetTokens (no effort) → manual {type: "enabled"}
 *
 * This is the production prod-prompt configuration: Sonnet 4.6 /
 * Opus 4.7 expect adaptive thinking, with the depth selected via
 * `output_config.effort`. The pre-fix daemon translated
 * `reasoning_effort=high` to the deprecated manual mode, which is
 * outright rejected on Opus 4.7.
 */
import { describe, expect, it } from "bun:test";

import { AnthropicProvider } from "../src/llm/providers/anthropic.ts";
import type { ChatEvent, ChatRequest, TurnMessage } from "../src/llm/types.ts";
import { type CannedResponse, startFakeAnthropic } from "./_fake_anthropic.ts";

async function drain(stream: AsyncIterable<ChatEvent>): Promise<void> {
  for await (const _ of stream) {
    // exhaust
  }
}

function baseRequest(thinking: ChatRequest["thinking"]): Omit<ChatRequest, "baseUrl"> {
  const messages: TurnMessage[] = [
    { role: "user", content: [{ type: "text", text: "Hi." }] },
  ];
  return {
    system: "You are Casey.",
    messages,
    tools: [],
    thinking,
    cacheTtl: "",
    modelId: "claude-sonnet-4-6",
    apiKey: "test-key",
    maxTokens: 4096,
  };
}

async function captureBody(req: ChatRequest): Promise<Record<string, unknown>> {
  const server = await startFakeAnthropic([
    {
      blocks: [{ type: "text", text: "ok" }],
      stopReason: "end_turn",
    } satisfies CannedResponse,
  ]);
  try {
    await drain(new AnthropicProvider().stream({ ...req, baseUrl: server.baseUrl }));
    return server.bodies[0] as Record<string, unknown>;
  } finally {
    await server.close();
  }
}

describe("anthropic thinking + output_config wire shape", () => {
  it("named effort 'high' → adaptive thinking + output_config.effort", async () => {
    const body = await captureBody(baseRequest({ enabled: true, effort: "high" }));
    expect(body["thinking"]).toEqual({ type: "adaptive" });
    expect(body["output_config"]).toEqual({ effort: "high" });
  });

  it("named effort 'low' → adaptive thinking + output_config.effort=low", async () => {
    const body = await captureBody(baseRequest({ enabled: true, effort: "low" }));
    expect(body["thinking"]).toEqual({ type: "adaptive" });
    expect(body["output_config"]).toEqual({ effort: "low" });
  });

  it("named effort 'max' → adaptive thinking + output_config.effort=max", async () => {
    const body = await captureBody(baseRequest({ enabled: true, effort: "max" }));
    expect(body["thinking"]).toEqual({ type: "adaptive" });
    expect(body["output_config"]).toEqual({ effort: "max" });
  });

  it("literal 'adaptive' → adaptive thinking, NO output_config", async () => {
    const body = await captureBody(baseRequest({ enabled: true, effort: "adaptive" }));
    expect(body["thinking"]).toEqual({ type: "adaptive" });
    expect(body).not.toHaveProperty("output_config");
  });

  it("explicit budgetTokens (no effort) → manual {type: 'enabled'}", async () => {
    const body = await captureBody(
      baseRequest({ enabled: true, budgetTokens: 2048 }),
    );
    expect(body["thinking"]).toEqual({ type: "enabled", budget_tokens: 2048 });
    expect(body).not.toHaveProperty("output_config");
  });

  it("disabled thinking → no thinking, no output_config", async () => {
    const body = await captureBody(baseRequest({ enabled: false }));
    expect(body).not.toHaveProperty("thinking");
    expect(body).not.toHaveProperty("output_config");
  });
});
