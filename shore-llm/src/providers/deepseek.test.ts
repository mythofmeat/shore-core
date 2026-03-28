import { describe, it, expect, vi } from "vitest";
import type OpenAI from "openai";
import { generate, stream } from "./deepseek.js";
import type { ProviderRequest } from "./types.js";
import type { ServerResponse } from "node:http";

// ── Helpers ────────────────────────────────────────────────────────────

function baseRequest(overrides?: Partial<ProviderRequest>): ProviderRequest {
  return {
    provider: "deepseek",
    model: "deepseek-reasoner",
    api_key: "sk-test",
    messages: [{ role: "user", content: "What is 6 times 7?" }],
    max_tokens: 1024,
    ...overrides,
  };
}

function mockCompletion(messageOverrides?: Record<string, unknown>): OpenAI.ChatCompletion {
  return {
    id: "chatcmpl-ds-01",
    object: "chat.completion",
    created: 1234567890,
    model: "deepseek-reasoner",
    choices: [
      {
        index: 0,
        message: {
          role: "assistant",
          content: "The answer is 42.",
          reasoning_content: "Let me think: 6 × 7 = 42.",
          refusal: null,
          ...messageOverrides,
        },
        finish_reason: "stop",
        logprobs: null,
      },
    ],
    usage: { prompt_tokens: 10, completion_tokens: 20, total_tokens: 30 },
  } as unknown as OpenAI.ChatCompletion;
}

// ── Tests: generate ────────────────────────────────────────────────────

describe("deepseek generate", () => {
  it("extracts reasoning_content into a thinking block", async () => {
    const completion = mockCompletion();

    // Patch createClient so we can inject a mock
    const openaiModule = await import("./openai.js");
    vi.spyOn(openaiModule, "createClient").mockReturnValue({
      chat: {
        completions: {
          create: vi.fn().mockResolvedValue(completion),
        },
      },
    } as unknown as OpenAI);

    const result = await generate(baseRequest());

    expect(result.provider).toBe("deepseek");
    expect(result.content_blocks).toEqual([
      { type: "thinking", thinking: "Let me think: 6 × 7 = 42." },
      { type: "text", text: "The answer is 42." },
    ]);

    vi.restoreAllMocks();
  });
});

// ── Tests: stream ──────────────────────────────────────────────────────

async function* chunkEvents(
  chunks: Record<string, unknown>[],
): AsyncIterable<OpenAI.ChatCompletionChunk> {
  for (const c of chunks) yield c as unknown as OpenAI.ChatCompletionChunk;
}

function mockResponse(): ServerResponse & { written: string[] } {
  const written: string[] = [];
  return {
    written,
    chunkedEncoding: false,
    writeHead(_status: number, _headers: Record<string, string>) {},
    write(chunk: string) {
      written.push(chunk);
      return true;
    },
    end() {},
  } as unknown as ServerResponse & { written: string[] };
}

describe("deepseek stream", () => {
  it("emits thinking events from reasoning_content delta field", async () => {
    const chunks = [
      {
        id: "chatcmpl-ds-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "deepseek-reasoner",
        choices: [
          {
            index: 0,
            delta: { role: "assistant", reasoning_content: "Thinking..." },
            finish_reason: null,
            logprobs: null,
          },
        ],
      },
      {
        id: "chatcmpl-ds-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "deepseek-reasoner",
        choices: [
          {
            index: 0,
            delta: { content: "The answer is 42." },
            finish_reason: null,
            logprobs: null,
          },
        ],
      },
      {
        id: "chatcmpl-ds-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "deepseek-reasoner",
        choices: [{ index: 0, delta: {}, finish_reason: "stop", logprobs: null }],
        usage: { prompt_tokens: 10, completion_tokens: 20, total_tokens: 30 },
      },
    ];

    const openaiModule = await import("./openai.js");
    vi.spyOn(openaiModule, "createClient").mockReturnValue({
      chat: {
        completions: {
          create: vi.fn().mockResolvedValue(chunkEvents(chunks)),
        },
      },
    } as unknown as OpenAI);

    const res = mockResponse();
    await stream(baseRequest(), res);

    const lines = res.written
      .join("")
      .split("\n")
      .filter((l) => l.length > 0)
      .map((l) => JSON.parse(l));

    expect(lines[0]).toMatchObject({ type: "start", model: "deepseek-reasoner" });
    expect(lines[1]).toEqual({ type: "thinking", text: "Thinking..." });
    expect(lines[2]).toEqual({ type: "text", text: "The answer is 42." });
    expect(lines[3]).toMatchObject({ type: "done", content: "The answer is 42." });

    vi.restoreAllMocks();
  });
});
