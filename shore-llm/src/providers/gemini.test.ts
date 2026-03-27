import { describe, it, expect, vi } from "vitest";
import type { GoogleGenerativeAI, GenerativeModel } from "@google/generative-ai";
import {
  translateMessages,
  translateTools,
  generate,
  stream,
  createClient,
} from "./gemini.js";
import type { ProviderRequest } from "./types.js";
import type { ServerResponse } from "node:http";

// ── Helpers ────────────────────────────────────────────────────────────

function baseRequest(overrides?: Partial<ProviderRequest>): ProviderRequest {
  return {
    provider: "gemini",
    model: "gemini-2.0-flash",
    api_key: "test-key",
    messages: [{ role: "user", content: "Hello" }],
    max_tokens: 1024,
    ...overrides,
  };
}

function mockModel(
  overrides?: Partial<GenerativeModel>,
): GenerativeModel {
  return {
    generateContent: vi.fn(),
    generateContentStream: vi.fn(),
    ...overrides,
  } as unknown as GenerativeModel;
}

function mockClient(model: GenerativeModel): GoogleGenerativeAI {
  return {
    getGenerativeModel: vi.fn().mockReturnValue(model),
  } as unknown as GoogleGenerativeAI;
}

function mockResponse(): ServerResponse & {
  written: string[];
  headStatus: number;
  headHeaders: Record<string, string>;
} {
  const written: string[] = [];
  let headStatus = 0;
  const headHeaders: Record<string, string> = {};
  return {
    written,
    headStatus,
    headHeaders,
    writeHead(status: number, headers: Record<string, string>) {
      headStatus = status;
      Object.assign(headHeaders, headers);
      // @ts-expect-error - update bound ref
      this.headStatus = status;
    },
    write(chunk: string) {
      written.push(chunk);
      return true;
    },
    end() {},
  } as unknown as ServerResponse & {
    written: string[];
    headStatus: number;
    headHeaders: Record<string, string>;
  };
}

function parseNdjson(written: string[]): Record<string, unknown>[] {
  return written
    .join("")
    .split("\n")
    .filter((l) => l.length > 0)
    .map((l) => JSON.parse(l));
}

// ── Tests: createClient ───────────────────────────────────────────────

describe("createClient", () => {
  it("creates a GoogleGenerativeAI instance", () => {
    const client = createClient("test-key");
    expect(client).toBeDefined();
    expect(client.apiKey).toBe("test-key");
  });
});

// ── Tests: translateMessages ──────────────────────────────────────────

describe("translateMessages", () => {
  it("translates simple string messages", () => {
    const req = baseRequest({
      messages: [
        { role: "user", content: "Hello" },
        { role: "assistant", content: "Hi there" },
      ],
    });
    const contents = translateMessages(req);
    expect(contents).toEqual([
      { role: "user", parts: [{ text: "Hello" }] },
      { role: "model", parts: [{ text: "Hi there" }] },
    ]);
  });

  it("maps assistant role to model", () => {
    const req = baseRequest({
      messages: [{ role: "assistant", content: "I am a model" }],
    });
    const contents = translateMessages(req);
    expect(contents[0].role).toBe("model");
  });

  it("translates text content blocks", () => {
    const req = baseRequest({
      messages: [
        {
          role: "user",
          content: [
            { type: "text", text: "First" },
            { type: "text", text: "Second" },
          ],
        },
      ],
    });
    const contents = translateMessages(req);
    expect(contents[0].parts).toEqual([
      { text: "First" },
      { text: "Second" },
    ]);
  });

  it("translates tool_use blocks to functionCall parts", () => {
    const req = baseRequest({
      messages: [
        {
          role: "assistant",
          content: [
            {
              type: "tool_use",
              name: "get_weather",
              input: { city: "NYC" },
            },
          ],
        },
      ],
    });
    const contents = translateMessages(req);
    expect(contents[0].parts).toEqual([
      {
        functionCall: {
          name: "get_weather",
          args: { city: "NYC" },
        },
      },
    ]);
  });

  it("translates tool_result blocks to functionResponse parts", () => {
    const req = baseRequest({
      messages: [
        {
          role: "user",
          content: [
            {
              type: "tool_result",
              name: "get_weather",
              tool_use_id: "tool_01",
              content: "Sunny, 72°F",
            },
          ],
        },
      ],
    });
    const contents = translateMessages(req);
    expect(contents[0].parts).toEqual([
      {
        functionResponse: {
          name: "get_weather",
          response: { result: "Sunny, 72°F" },
        },
      },
    ]);
  });

  it("skips messages with empty content arrays", () => {
    const req = baseRequest({
      messages: [{ role: "user", content: [] }],
    });
    const contents = translateMessages(req);
    expect(contents).toEqual([]);
  });
});

// ── Tests: translateTools ─────────────────────────────────────────────

describe("translateTools", () => {
  it("returns undefined for empty tools", () => {
    expect(translateTools(undefined)).toBeUndefined();
    expect(translateTools([])).toBeUndefined();
  });

  it("wraps tools in functionDeclarations", () => {
    const tools = [
      {
        name: "get_weather",
        description: "Get weather info",
        input_schema: {
          type: "object",
          properties: { city: { type: "string" } },
        },
      },
    ];
    const result = translateTools(tools);
    expect(result).toEqual([
      {
        functionDeclarations: [
          {
            name: "get_weather",
            description: "Get weather info",
            parameters: {
              type: "object",
              properties: { city: { type: "string" } },
            },
          },
        ],
      },
    ]);
  });
});

// ── Tests: generate ───────────────────────────────────────────────────

describe("generate", () => {
  it("returns normalized response for text completion", async () => {
    const model = mockModel({
      generateContent: vi.fn().mockResolvedValue({
        response: {
          candidates: [
            {
              content: { parts: [{ text: "Hello there!" }] },
              finishReason: "STOP",
            },
          ],
          usageMetadata: {
            promptTokenCount: 10,
            candidatesTokenCount: 20,
            totalTokenCount: 30,
          },
        },
      }),
    });
    const client = mockClient(model);
    const result = await generate(client, baseRequest());

    expect(result.content).toBe("Hello there!");
    expect(result.content_blocks).toEqual([
      { type: "text", text: "Hello there!" },
    ]);
    expect(result.finish_reason).toBe("end_turn");
    expect(result.model).toBe("gemini-2.0-flash");
    expect(result.provider).toBe("gemini");
  });

  it("normalizes usage metadata", async () => {
    const model = mockModel({
      generateContent: vi.fn().mockResolvedValue({
        response: {
          candidates: [
            {
              content: { parts: [{ text: "Hi" }] },
              finishReason: "STOP",
            },
          ],
          usageMetadata: {
            promptTokenCount: 100,
            candidatesTokenCount: 50,
            totalTokenCount: 150,
            cachedContentTokenCount: 40,
          },
        },
      }),
    });
    const client = mockClient(model);
    const result = await generate(client, baseRequest());

    expect(result.usage).toEqual({
      input_tokens: 100,
      output_tokens: 50,
      cache_read_tokens: 40,
      cache_creation_tokens: 0,
    });
  });

  it("handles missing usage metadata", async () => {
    const model = mockModel({
      generateContent: vi.fn().mockResolvedValue({
        response: {
          candidates: [
            {
              content: { parts: [{ text: "Hi" }] },
              finishReason: "STOP",
            },
          ],
        },
      }),
    });
    const client = mockClient(model);
    const result = await generate(client, baseRequest());

    expect(result.usage).toEqual({
      input_tokens: 0,
      output_tokens: 0,
      cache_read_tokens: 0,
      cache_creation_tokens: 0,
    });
  });

  it("normalizes function call responses", async () => {
    const model = mockModel({
      generateContent: vi.fn().mockResolvedValue({
        response: {
          candidates: [
            {
              content: {
                parts: [
                  { text: "Let me check." },
                  {
                    functionCall: {
                      name: "get_weather",
                      args: { city: "NYC" },
                    },
                  },
                ],
              },
              finishReason: "STOP",
            },
          ],
          usageMetadata: {
            promptTokenCount: 10,
            candidatesTokenCount: 20,
            totalTokenCount: 30,
          },
        },
      }),
    });
    const client = mockClient(model);
    const result = await generate(client, baseRequest());

    expect(result.content).toBe("Let me check.");
    expect(result.content_blocks).toEqual([
      { type: "text", text: "Let me check." },
      {
        type: "tool_use",
        id: "gemini_get_weather",
        name: "get_weather",
        input: { city: "NYC" },
      },
    ]);
  });

  it("includes timing information", async () => {
    const model = mockModel({
      generateContent: vi.fn().mockResolvedValue({
        response: {
          candidates: [
            {
              content: { parts: [{ text: "Hi" }] },
              finishReason: "STOP",
            },
          ],
        },
      }),
    });
    const client = mockClient(model);
    const result = await generate(client, baseRequest());

    expect(result.timing.total_ms).toBeGreaterThanOrEqual(0);
    expect(result.timing.time_to_first_token_ms).toBeGreaterThanOrEqual(0);
  });

  it("normalizes MAX_TOKENS finish reason", async () => {
    const model = mockModel({
      generateContent: vi.fn().mockResolvedValue({
        response: {
          candidates: [
            {
              content: { parts: [{ text: "truncated" }] },
              finishReason: "MAX_TOKENS",
            },
          ],
        },
      }),
    });
    const client = mockClient(model);
    const result = await generate(client, baseRequest());

    expect(result.finish_reason).toBe("max_tokens");
  });

  it("passes system instruction to model", async () => {
    const model = mockModel({
      generateContent: vi.fn().mockResolvedValue({
        response: {
          candidates: [
            {
              content: { parts: [{ text: "ok" }] },
              finishReason: "STOP",
            },
          ],
        },
      }),
    });
    const client = mockClient(model);
    await generate(client, baseRequest({ system: "Be concise" }));

    expect(
      (client.getGenerativeModel as ReturnType<typeof vi.fn>),
    ).toHaveBeenCalledWith(
      expect.objectContaining({ systemInstruction: "Be concise" }),
    );
  });

  it("passes reasoning_effort as thinkingConfig", async () => {
    const model = mockModel({
      generateContent: vi.fn().mockResolvedValue({
        response: {
          candidates: [
            {
              content: { parts: [{ text: "ok" }] },
              finishReason: "STOP",
            },
          ],
        },
      }),
    });
    const client = mockClient(model);
    await generate(
      client,
      baseRequest({ provider_options: { reasoning_effort: 2048 } }),
    );

    expect(
      (client.getGenerativeModel as ReturnType<typeof vi.fn>),
    ).toHaveBeenCalledWith(
      expect.objectContaining({
        generationConfig: expect.objectContaining({
          thinkingConfig: { thinkingBudget: 2048 },
        }),
      }),
    );
  });

  it("passes tools to model", async () => {
    const model = mockModel({
      generateContent: vi.fn().mockResolvedValue({
        response: {
          candidates: [
            {
              content: { parts: [{ text: "ok" }] },
              finishReason: "STOP",
            },
          ],
        },
      }),
    });
    const client = mockClient(model);
    await generate(
      client,
      baseRequest({
        tools: [
          {
            name: "get_weather",
            description: "Get weather",
            input_schema: { type: "object", properties: {} },
          },
        ],
      }),
    );

    expect(
      (client.getGenerativeModel as ReturnType<typeof vi.fn>),
    ).toHaveBeenCalledWith(
      expect.objectContaining({
        tools: [
          {
            functionDeclarations: [
              {
                name: "get_weather",
                description: "Get weather",
                parameters: { type: "object", properties: {} },
              },
            ],
          },
        ],
      }),
    );
  });

  it("passes temperature and topP in generationConfig", async () => {
    const model = mockModel({
      generateContent: vi.fn().mockResolvedValue({
        response: {
          candidates: [
            {
              content: { parts: [{ text: "ok" }] },
              finishReason: "STOP",
            },
          ],
        },
      }),
    });
    const client = mockClient(model);
    await generate(client, baseRequest({ temperature: 0.7, top_p: 0.9 }));

    expect(
      (client.getGenerativeModel as ReturnType<typeof vi.fn>),
    ).toHaveBeenCalledWith(
      expect.objectContaining({
        generationConfig: expect.objectContaining({
          temperature: 0.7,
          topP: 0.9,
        }),
      }),
    );
  });

  it("extracts thinking parts with thought flag", async () => {
    const mockModel = {
      generateContent: vi.fn().mockResolvedValue({
        response: {
          candidates: [
            {
              content: {
                parts: [
                  { text: "Let me reason...", thought: true },
                  { text: "The answer is 42." },
                ],
              },
              finishReason: "STOP",
            },
          ],
          usageMetadata: { promptTokenCount: 10, candidatesTokenCount: 20 },
        },
      }),
    };
    const client = {
      getGenerativeModel: vi.fn().mockReturnValue(mockModel),
    } as unknown as GoogleGenerativeAI;

    const result = await generate(client, baseRequest());

    expect(result.content).toBe("The answer is 42.");
    expect(result.content_blocks).toEqual([
      { type: "thinking", thinking: "Let me reason..." },
      { type: "text", text: "The answer is 42." },
    ]);
  });
});

// ── Tests: stream ─────────────────────────────────────────────────────

describe("stream", () => {
  it("emits start, text, done events for simple text response", async () => {
    async function* fakeStream() {
      yield {
        candidates: [
          {
            content: { parts: [{ text: "Hello" }] },
            finishReason: undefined,
          },
        ],
      };
      yield {
        candidates: [
          {
            content: { parts: [{ text: " world" }] },
            finishReason: "STOP",
          },
        ],
        usageMetadata: {
          promptTokenCount: 5,
          candidatesTokenCount: 10,
          totalTokenCount: 15,
        },
      };
    }

    const model = mockModel({
      generateContentStream: vi.fn().mockResolvedValue({
        stream: fakeStream(),
      }),
    });
    const client = mockClient(model);
    const res = mockResponse();
    await stream(client, baseRequest(), res);

    const lines = parseNdjson(res.written);
    expect(lines[0]).toMatchObject({ type: "start", model: "gemini-2.0-flash" });
    expect(lines[1]).toEqual({ type: "text", text: "Hello" });
    expect(lines[2]).toEqual({ type: "text", text: " world" });
    expect(lines[3]).toMatchObject({
      type: "done",
      content: "Hello world",
      finish_reason: "end_turn",
    });
    expect(lines[3].usage).toBeDefined();
    expect(lines[3].timing).toBeDefined();
  });

  it("emits tool_use events for function calls", async () => {
    async function* fakeStream() {
      yield {
        candidates: [
          {
            content: {
              parts: [
                {
                  functionCall: {
                    name: "get_weather",
                    args: { city: "NYC" },
                  },
                },
              ],
            },
            finishReason: "STOP",
          },
        ],
        usageMetadata: {
          promptTokenCount: 5,
          candidatesTokenCount: 10,
          totalTokenCount: 15,
        },
      };
    }

    const model = mockModel({
      generateContentStream: vi.fn().mockResolvedValue({
        stream: fakeStream(),
      }),
    });
    const client = mockClient(model);
    const res = mockResponse();
    await stream(client, baseRequest(), res);

    const lines = parseNdjson(res.written);
    expect(lines[0]).toMatchObject({ type: "start" });
    expect(lines[1]).toEqual({
      type: "tool_use",
      id: "gemini_get_weather",
      name: "get_weather",
      input: { city: "NYC" },
    });
    expect(lines[2]).toMatchObject({
      type: "done",
      content: "",
      finish_reason: "end_turn",
    });
  });

  it("sets correct content-type header for ndjson", async () => {
    async function* fakeStream() {
      yield {
        candidates: [
          {
            content: { parts: [{ text: "Hi" }] },
            finishReason: "STOP",
          },
        ],
      };
    }

    const model = mockModel({
      generateContentStream: vi.fn().mockResolvedValue({
        stream: fakeStream(),
      }),
    });
    const client = mockClient(model);
    const res = mockResponse();
    await stream(client, baseRequest(), res);

    expect(res.headHeaders["Content-Type"]).toBe("application/x-ndjson");
  });

  it("includes usage in done event", async () => {
    async function* fakeStream() {
      yield {
        candidates: [
          {
            content: { parts: [{ text: "Hi" }] },
            finishReason: "STOP",
          },
        ],
        usageMetadata: {
          promptTokenCount: 10,
          candidatesTokenCount: 5,
          totalTokenCount: 15,
          cachedContentTokenCount: 3,
        },
      };
    }

    const model = mockModel({
      generateContentStream: vi.fn().mockResolvedValue({
        stream: fakeStream(),
      }),
    });
    const client = mockClient(model);
    const res = mockResponse();
    await stream(client, baseRequest(), res);

    const lines = parseNdjson(res.written);
    const done = lines.find((l) => l.type === "done") as Record<string, unknown>;
    const usage = done.usage as Record<string, number>;
    expect(usage.input_tokens).toBe(10);
    expect(usage.output_tokens).toBe(5);
    expect(usage.cache_read_tokens).toBe(3);
    expect(usage.cache_creation_tokens).toBe(0);
  });

  it("tracks timing with time_to_first_token_ms", async () => {
    async function* fakeStream() {
      yield {
        candidates: [
          {
            content: { parts: [{ text: "Hi" }] },
            finishReason: "STOP",
          },
        ],
      };
    }

    const model = mockModel({
      generateContentStream: vi.fn().mockResolvedValue({
        stream: fakeStream(),
      }),
    });
    const client = mockClient(model);
    const res = mockResponse();
    await stream(client, baseRequest(), res);

    const lines = parseNdjson(res.written);
    const done = lines.find((l) => l.type === "done") as Record<string, unknown>;
    const timing = done.timing as Record<string, number>;
    expect(timing.total_ms).toBeGreaterThanOrEqual(0);
    expect(timing.time_to_first_token_ms).toBeGreaterThanOrEqual(0);
  });

  it("emits thinking events for thought-flagged parts", async () => {
    async function* fakeStream() {
      yield {
        candidates: [
          {
            content: { parts: [{ text: "Reasoning...", thought: true }] },
            finishReason: undefined,
          },
        ],
        usageMetadata: undefined,
      };
      yield {
        candidates: [
          {
            content: { parts: [{ text: "Answer" }] },
            finishReason: "STOP",
          },
        ],
        usageMetadata: { promptTokenCount: 10, candidatesTokenCount: 5 },
      };
    }

    const mockModel = {
      generateContentStream: vi.fn().mockResolvedValue({
        stream: fakeStream(),
      }),
    };
    const client = {
      getGenerativeModel: vi.fn().mockReturnValue(mockModel),
    } as unknown as GoogleGenerativeAI;

    const res = mockResponse();
    await stream(client, baseRequest(), res);

    const lines = parseNdjson(res.written);
    expect(lines[0]).toMatchObject({ type: "start" });
    expect(lines[1]).toEqual({ type: "thinking", text: "Reasoning..." });
    expect(lines[2]).toEqual({ type: "text", text: "Answer" });
    expect(lines[3]).toMatchObject({ type: "done", content: "Answer" });
  });
});
