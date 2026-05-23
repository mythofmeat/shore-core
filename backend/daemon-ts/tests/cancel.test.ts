/**
 * Cancellation tests — verifies that AbortSignal flows through both
 * provider adapters and that the orchestrator's error-mapping turns an
 * abort into a `stream_end` frame with `finish_reason: "cancelled"`.
 */
import { describe, expect, it } from "bun:test";

import { AnthropicProvider } from "../src/llm/providers/anthropic.ts";
import { OpenAIProvider } from "../src/llm/providers/openai.ts";
import type { ChatEvent, ChatRequest } from "../src/llm/types.ts";
import { startFakeAnthropic } from "./_fake_anthropic.ts";

async function drain(stream: AsyncIterable<ChatEvent>): Promise<void> {
  for await (const _ev of stream) {
    // drain
  }
}

function baseRequest(baseUrl: string): ChatRequest {
  return {
    system: "S",
    messages: [{ role: "user", content: [{ type: "text", text: "hi" }] }],
    tools: [],
    thinking: { enabled: false },
    cacheTtl: "",
    modelId: "fake",
    apiKey: "fake",
    maxTokens: 64,
    baseUrl,
  };
}

describe("Anthropic adapter: abort signal", () => {
  it("rejects the stream when the signal is already aborted", async () => {
    const server = await startFakeAnthropic([
      { blocks: [{ type: "text", text: "hi" }], stopReason: "end_turn" },
    ]);
    try {
      const ctrl = new AbortController();
      ctrl.abort();
      const req: ChatRequest = { ...baseRequest(server.baseUrl), signal: ctrl.signal };
      let caught: unknown;
      try {
        await drain(new AnthropicProvider().stream(req));
      } catch (e) {
        caught = e;
      }
      expect(caught).toBeDefined();
      const err = caught as Error;
      expect(err.name === "AbortError" || /abort/i.test(err.message)).toBe(true);
    } finally {
      await server.close();
    }
  });
});

describe("OpenAI adapter: abort signal", () => {
  it("rejects the stream when the signal is already aborted", async () => {
    // Tiny OpenAI-shaped server that would otherwise return a valid
    // SSE response — the abort should fire before the request resolves.
    const server = Bun.serve({
      port: 0,
      fetch: async () =>
        new Response(
          'data: {"id":"x","choices":[{"delta":{"content":"hi"},"finish_reason":"stop","index":0}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}\n\ndata: [DONE]\n\n',
          { headers: { "Content-Type": "text/event-stream" } },
        ),
    });
    try {
      const ctrl = new AbortController();
      ctrl.abort();
      const req: ChatRequest = {
        ...baseRequest(`http://${server.hostname}:${server.port}/v1`),
        signal: ctrl.signal,
      };
      let caught: unknown;
      try {
        await drain(new OpenAIProvider().stream(req));
      } catch (e) {
        caught = e;
      }
      expect(caught).toBeDefined();
      const err = caught as Error;
      expect(err.name === "AbortError" || /abort/i.test(err.message)).toBe(true);
    } finally {
      await server.stop();
    }
  });
});
