import { describe, it, expect, vi } from "vitest";
import type OpenAI from "openai";
import {
  translateMessages,
  translateTools,
  generate,
  stream,
  embed,
  imageGenerate,
} from "./openai.js";
import type { ProviderRequest, EmbedRequest, ImageGenerateRequest } from "./types.js";
import type { ServerResponse } from "node:http";

// ── Helpers ────────────────────────────────────────────────────────────

function baseRequest(overrides?: Partial<ProviderRequest>): ProviderRequest {
  return {
    provider: "openai",
    model: "gpt-4",
    api_key: "sk-test",
    messages: [{ role: "user", content: "Hello" }],
    max_tokens: 1024,
    ...overrides,
  };
}

function makeCompletion(
  overrides?: Record<string, unknown>,
): OpenAI.ChatCompletion {
  return {
    id: "chatcmpl-01",
    object: "chat.completion",
    created: 1234567890,
    model: "gpt-4",
    choices: [
      {
        index: 0,
        message: {
          role: "assistant",
          content: "Hello there!",
          refusal: null,
        },
        finish_reason: "stop",
        logprobs: null,
      },
    ],
    usage: {
      prompt_tokens: 10,
      completion_tokens: 20,
      total_tokens: 30,
    },
    ...overrides,
  } as unknown as OpenAI.ChatCompletion;
}

function mockClient(completion: OpenAI.ChatCompletion): OpenAI {
  return {
    chat: {
      completions: {
        create: vi.fn().mockResolvedValue(completion),
      },
    },
  } as unknown as OpenAI;
}

async function* chunkEvents(
  chunks: Record<string, unknown>[],
): AsyncIterable<OpenAI.ChatCompletionChunk> {
  for (const c of chunks) yield c as unknown as OpenAI.ChatCompletionChunk;
}

function mockStreamClient(chunks: Record<string, unknown>[]): OpenAI {
  return {
    chat: {
      completions: {
        create: vi.fn().mockResolvedValue(chunkEvents(chunks)),
      },
    },
  } as unknown as OpenAI;
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

// ── Tests: translateMessages ───────────────────────────────────────────

describe("translateMessages", () => {
  it("translates simple text messages", () => {
    const req = baseRequest({
      messages: [
        { role: "user", content: "Hello" },
        { role: "assistant", content: "Hi" },
      ],
    });
    const result = translateMessages(req);
    expect(result).toEqual([
      { role: "user", content: "Hello" },
      { role: "assistant", content: "Hi" },
    ]);
  });

  it("prepends system message", () => {
    const req = baseRequest({ system: "Be helpful" });
    const result = translateMessages(req);
    expect(result[0]).toEqual({ role: "system", content: "Be helpful" });
    expect(result[1]).toEqual({ role: "user", content: "Hello" });
  });

  it("converts tool_use blocks to tool_calls", () => {
    const req = baseRequest({
      messages: [
        { role: "user", content: "What's the weather?" },
        {
          role: "assistant",
          content: [
            { type: "text", text: "Let me check." },
            {
              type: "tool_use",
              id: "tc1",
              name: "weather",
              input: { city: "NYC" },
            },
          ],
        },
      ],
    });
    const result = translateMessages(req);
    expect(result[1]).toMatchObject({
      role: "assistant",
      content: "Let me check.",
      tool_calls: [
        {
          id: "tc1",
          type: "function",
          function: {
            name: "weather",
            arguments: '{"city":"NYC"}',
          },
        },
      ],
    });
  });

  it("handles tool_result blocks in user messages", () => {
    const req = baseRequest({
      messages: [
        {
          role: "user",
          content: [
            { type: "tool_result", tool_use_id: "tc1", content: "72°F" },
          ],
        },
      ],
    });
    const result = translateMessages(req);
    expect(result).toContainEqual({
      role: "tool",
      tool_call_id: "tc1",
      content: "72°F",
    });
  });
});

describe("translateTools", () => {
  it("returns undefined for empty tools", () => {
    expect(translateTools(undefined)).toBeUndefined();
    expect(translateTools([])).toBeUndefined();
  });

  it("translates tools to OpenAI format", () => {
    const tools = [
      {
        name: "get_weather",
        description: "Get weather",
        input_schema: {
          type: "object",
          properties: { city: { type: "string" } },
        },
      },
    ];
    const result = translateTools(tools);
    expect(result).toEqual([
      {
        type: "function",
        function: {
          name: "get_weather",
          description: "Get weather",
          parameters: {
            type: "object",
            properties: { city: { type: "string" } },
          },
        },
      },
    ]);
  });
});

// ── Tests: generate ────────────────────────────────────────────────────

describe("generate", () => {
  it("returns normalized response for text completion", async () => {
    const completion = makeCompletion();
    const client = mockClient(completion);
    const result = await generate(client, baseRequest());

    expect(result.content).toBe("Hello there!");
    expect(result.content_blocks).toEqual([
      { type: "text", text: "Hello there!" },
    ]);
    expect(result.finish_reason).toBe("end_turn");
    expect(result.model).toBe("gpt-4");
    expect(result.provider).toBe("openai");
  });

  it("normalizes usage", async () => {
    const completion = makeCompletion();
    const client = mockClient(completion);
    const result = await generate(client, baseRequest());

    expect(result.usage).toEqual({
      input_tokens: 10,
      output_tokens: 20,
      cache_read_tokens: 0,
      cache_creation_tokens: 0,
    });
  });

  it("normalizes stop finish_reason to end_turn", async () => {
    const completion = makeCompletion();
    const client = mockClient(completion);
    const result = await generate(client, baseRequest());
    expect(result.finish_reason).toBe("end_turn");
  });

  it("normalizes tool_calls finish_reason to tool_use", async () => {
    const completion = makeCompletion({
      choices: [
        {
          index: 0,
          message: {
            role: "assistant",
            content: null,
            refusal: null,
            tool_calls: [
              {
                id: "tc1",
                type: "function",
                function: {
                  name: "weather",
                  arguments: '{"city":"NYC"}',
                },
              },
            ],
          },
          finish_reason: "tool_calls",
          logprobs: null,
        },
      ],
    });
    const client = mockClient(completion);
    const result = await generate(client, baseRequest());

    expect(result.finish_reason).toBe("tool_use");
    expect(result.content_blocks).toEqual([
      {
        type: "tool_use",
        id: "tc1",
        name: "weather",
        input: { city: "NYC" },
      },
    ]);
  });

  it("normalizes length finish_reason to max_tokens", async () => {
    const completion = makeCompletion({
      choices: [
        {
          index: 0,
          message: {
            role: "assistant",
            content: "Truncated...",
            refusal: null,
          },
          finish_reason: "length",
          logprobs: null,
        },
      ],
    });
    const client = mockClient(completion);
    const result = await generate(client, baseRequest());
    expect(result.finish_reason).toBe("max_tokens");
  });

  it("includes timing", async () => {
    const completion = makeCompletion();
    const client = mockClient(completion);
    const result = await generate(client, baseRequest());

    expect(result.timing.total_ms).toBeGreaterThanOrEqual(0);
    expect(result.timing.time_to_first_token_ms).toBeGreaterThanOrEqual(0);
  });

  it("uses custom provider name", async () => {
    const completion = makeCompletion();
    const client = mockClient(completion);
    const result = await generate(client, baseRequest(), "deepseek");
    expect(result.provider).toBe("deepseek");
  });

  it("extracts reasoning field for kimi", async () => {
    const completion = makeCompletion({
      choices: [
        {
          index: 0,
          message: {
            role: "assistant" as const,
            content: "Here is my answer.",
            reasoning: "Let me think about this...",
            refusal: null,
          },
          finish_reason: "stop" as const,
          logprobs: null,
        },
      ],
    });
    const client = mockClient(completion);
    const result = await generate(client, baseRequest(), "openrouter");

    expect(result.content_blocks).toEqual([
      { type: "thinking", thinking: "Let me think about this..." },
      { type: "text", text: "Here is my answer." },
    ]);
  });

  it("extracts reasoning_content for deepseek", async () => {
    const completion = makeCompletion({
      choices: [
        {
          index: 0,
          message: {
            role: "assistant" as const,
            content: "The answer is 42.",
            reasoning_content: "Let me reason step by step...",
            refusal: null,
          },
          finish_reason: "stop" as const,
          logprobs: null,
        },
      ],
    });
    const client = mockClient(completion);
    const result = await generate(client, baseRequest(), "deepseek", "reasoning_content");

    expect(result.content_blocks).toEqual([
      { type: "thinking", thinking: "Let me reason step by step..." },
      { type: "text", text: "The answer is 42." },
    ]);
  });

  it("passes correct params to SDK", async () => {
    const completion = makeCompletion();
    const client = mockClient(completion);
    const req = baseRequest({ temperature: 0.7, top_p: 0.9 });
    await generate(client, req);

    expect(client.chat.completions.create).toHaveBeenCalledWith(
      expect.objectContaining({
        model: "gpt-4",
        temperature: 0.7,
        top_p: 0.9,
        stream: false,
      }),
      expect.objectContaining({ timeout: expect.any(Number) }),
    );
  });

  it("passes reasoning_effort from provider_options to SDK", async () => {
    const completion = makeCompletion();
    const client = mockClient(completion);
    const req = baseRequest({
      provider_options: { reasoning_effort: "high" },
    });
    await generate(client, req);

    expect(client.chat.completions.create).toHaveBeenCalledWith(
      expect.objectContaining({
        reasoning_effort: "high",
      }),
      expect.objectContaining({ timeout: expect.any(Number) }),
    );
  });
});

// ── Tests: stream ──────────────────────────────────────────────────────

describe("stream", () => {
  it("emits start, text, done events for simple text response", async () => {
    const chunks = [
      {
        id: "chatcmpl-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "gpt-4",
        choices: [
          {
            index: 0,
            delta: { role: "assistant", content: "" },
            finish_reason: null,
            logprobs: null,
          },
        ],
      },
      {
        id: "chatcmpl-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "gpt-4",
        choices: [
          {
            index: 0,
            delta: { content: "Hello" },
            finish_reason: null,
            logprobs: null,
          },
        ],
      },
      {
        id: "chatcmpl-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "gpt-4",
        choices: [
          {
            index: 0,
            delta: { content: " world" },
            finish_reason: null,
            logprobs: null,
          },
        ],
      },
      {
        id: "chatcmpl-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "gpt-4",
        choices: [
          {
            index: 0,
            delta: {},
            finish_reason: "stop",
            logprobs: null,
          },
        ],
      },
      {
        id: "chatcmpl-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "gpt-4",
        choices: [],
        usage: {
          prompt_tokens: 10,
          completion_tokens: 5,
          total_tokens: 15,
        },
      },
    ];

    const client = mockStreamClient(chunks);
    const res = mockResponse();
    await stream(client, baseRequest(), res);

    const lines = res.written
      .join("")
      .split("\n")
      .filter((l) => l.length > 0)
      .map((l) => JSON.parse(l));

    expect(lines[0]).toMatchObject({ type: "start", model: "gpt-4" });
    expect(lines[1]).toEqual({ type: "text", text: "Hello" });
    expect(lines[2]).toEqual({ type: "text", text: " world" });
    expect(lines[3]).toMatchObject({
      type: "done",
      content: "Hello world",
      finish_reason: "end_turn",
    });
    expect(lines[3].usage).toBeDefined();
    expect(lines[3].usage.input_tokens).toBe(10);
    expect(lines[3].usage.output_tokens).toBe(5);
  });

  it("emits tool_use events with accumulated arguments", async () => {
    const chunks = [
      {
        id: "chatcmpl-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "gpt-4",
        choices: [
          {
            index: 0,
            delta: {
              role: "assistant",
              tool_calls: [
                {
                  index: 0,
                  id: "tc1",
                  type: "function",
                  function: { name: "weather", arguments: "" },
                },
              ],
            },
            finish_reason: null,
            logprobs: null,
          },
        ],
      },
      {
        id: "chatcmpl-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "gpt-4",
        choices: [
          {
            index: 0,
            delta: {
              tool_calls: [
                { index: 0, function: { arguments: '{"ci' } },
              ],
            },
            finish_reason: null,
            logprobs: null,
          },
        ],
      },
      {
        id: "chatcmpl-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "gpt-4",
        choices: [
          {
            index: 0,
            delta: {
              tool_calls: [
                { index: 0, function: { arguments: 'ty":"NYC"}' } },
              ],
            },
            finish_reason: null,
            logprobs: null,
          },
        ],
      },
      {
        id: "chatcmpl-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "gpt-4",
        choices: [
          {
            index: 0,
            delta: {},
            finish_reason: "tool_calls",
            logprobs: null,
          },
        ],
      },
    ];

    const client = mockStreamClient(chunks);
    const res = mockResponse();
    await stream(client, baseRequest(), res);

    const lines = res.written
      .join("")
      .split("\n")
      .filter((l) => l.length > 0)
      .map((l) => JSON.parse(l));

    expect(lines[0]).toMatchObject({ type: "start" });
    expect(lines[1]).toEqual({
      type: "tool_use",
      id: "tc1",
      name: "weather",
      input: { city: "NYC" },
    });
    expect(lines[2]).toMatchObject({
      type: "done",
      finish_reason: "tool_use",
    });
  });

  it("sets correct content-type header for ndjson", async () => {
    const chunks = [
      {
        id: "chatcmpl-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "gpt-4",
        choices: [
          {
            index: 0,
            delta: {},
            finish_reason: "stop",
            logprobs: null,
          },
        ],
      },
    ];

    const client = mockStreamClient(chunks);
    const res = mockResponse();
    await stream(client, baseRequest(), res);

    expect(res.headHeaders["Content-Type"]).toBe("application/x-ndjson");
  });

  it("emits thinking events for kimi delta.reasoning", async () => {
    const chunks = [
      {
        id: "chatcmpl-kimi-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "moonshotai/kimi-k2.5",
        choices: [
          {
            index: 0,
            delta: { role: "assistant", content: "", reasoning: "Thinking step 1..." },
            finish_reason: null,
            logprobs: null,
          },
        ],
      },
      {
        id: "chatcmpl-kimi-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "moonshotai/kimi-k2.5",
        choices: [
          {
            index: 0,
            delta: { content: "", reasoning: " Thinking step 2..." },
            finish_reason: null,
            logprobs: null,
          },
        ],
      },
      {
        id: "chatcmpl-kimi-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "moonshotai/kimi-k2.5",
        choices: [
          {
            index: 0,
            delta: { content: "Here is my answer." },
            finish_reason: null,
            logprobs: null,
          },
        ],
      },
      {
        id: "chatcmpl-kimi-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "moonshotai/kimi-k2.5",
        choices: [
          {
            index: 0,
            delta: {},
            finish_reason: "stop",
            logprobs: null,
          },
        ],
        usage: { prompt_tokens: 10, completion_tokens: 20, total_tokens: 30 },
      },
    ];

    const client = mockStreamClient(chunks);
    const res = mockResponse();
    await stream(client, baseRequest(), res, "openrouter");

    const lines = res.written
      .join("")
      .split("\n")
      .filter((l: string) => l.length > 0)
      .map((l: string) => JSON.parse(l));

    expect(lines[0]).toMatchObject({ type: "start", model: "moonshotai/kimi-k2.5" });
    expect(lines[1]).toEqual({ type: "thinking", text: "Thinking step 1..." });
    expect(lines[2]).toEqual({ type: "thinking", text: " Thinking step 2..." });
    expect(lines[3]).toEqual({ type: "text", text: "Here is my answer." });
    expect(lines[4]).toMatchObject({ type: "done", content: "Here is my answer." });
  });

  it("does not emit text events for empty content chunks during reasoning", async () => {
    // Kimi sends content: "" during the thinking phase — these must not emit text events.
    const chunks = [
      {
        id: "chatcmpl-kimi-02",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "moonshotai/kimi-k2.5",
        choices: [
          {
            index: 0,
            delta: { content: "", reasoning: "Thinking..." },
            finish_reason: null,
            logprobs: null,
          },
        ],
      },
      {
        id: "chatcmpl-kimi-02",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "moonshotai/kimi-k2.5",
        choices: [{ index: 0, delta: { content: "Done." }, finish_reason: "stop", logprobs: null }],
        usage: { prompt_tokens: 5, completion_tokens: 5, total_tokens: 10 },
      },
    ];

    const client = mockStreamClient(chunks);
    const res = mockResponse();
    await stream(client, baseRequest(), res, "openrouter");

    const lines = res.written
      .join("")
      .split("\n")
      .filter((l: string) => l.length > 0)
      .map((l: string) => JSON.parse(l));

    const textEvents = lines.filter((l: { type: string }) => l.type === "text");
    expect(textEvents).toHaveLength(1);
    expect(textEvents[0]).toEqual({ type: "text", text: "Done." });
  });

  it("emits thinking events for deepseek reasoning_content", async () => {
    const chunks = [
      {
        id: "chatcmpl-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "deepseek-reasoner",
        choices: [
          {
            index: 0,
            delta: { role: "assistant", reasoning_content: "Step 1..." },
            finish_reason: null,
            logprobs: null,
          },
        ],
      },
      {
        id: "chatcmpl-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "deepseek-reasoner",
        choices: [
          {
            index: 0,
            delta: { reasoning_content: " Step 2..." },
            finish_reason: null,
            logprobs: null,
          },
        ],
      },
      {
        id: "chatcmpl-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "deepseek-reasoner",
        choices: [
          {
            index: 0,
            delta: { content: "The answer" },
            finish_reason: null,
            logprobs: null,
          },
        ],
      },
      {
        id: "chatcmpl-01",
        object: "chat.completion.chunk",
        created: 1234567890,
        model: "deepseek-reasoner",
        choices: [
          {
            index: 0,
            delta: {},
            finish_reason: "stop",
            logprobs: null,
          },
        ],
        usage: { prompt_tokens: 10, completion_tokens: 20, total_tokens: 30 },
      },
    ];

    const client = mockStreamClient(chunks);
    const res = mockResponse();
    await stream(client, baseRequest(), res, "deepseek", "reasoning_content");

    const lines = res.written
      .join("")
      .split("\n")
      .filter((l: string) => l.length > 0)
      .map((l: string) => JSON.parse(l));

    expect(lines[0]).toMatchObject({ type: "start", model: "deepseek-reasoner" });
    expect(lines[1]).toEqual({ type: "thinking", text: "Step 1..." });
    expect(lines[2]).toEqual({ type: "thinking", text: " Step 2..." });
    expect(lines[3]).toEqual({ type: "text", text: "The answer" });
    expect(lines[4]).toMatchObject({ type: "done", content: "The answer" });
  });
});

// ── Tests: embed ──────────────────────────────────────────────────────

function baseEmbedRequest(overrides?: Partial<EmbedRequest>): EmbedRequest {
  return {
    provider: "openai",
    model: "text-embedding-3-small",
    api_key: "sk-test",
    input: ["hello world"],
    ...overrides,
  };
}

function mockEmbedClient(response: Record<string, unknown>): OpenAI {
  return {
    embeddings: {
      create: vi.fn().mockResolvedValue(response),
    },
  } as unknown as OpenAI;
}

describe("embed", () => {
  it("returns embeddings for single input", async () => {
    const client = mockEmbedClient({
      data: [{ embedding: [0.1, -0.2, 0.3], index: 0 }],
      usage: { total_tokens: 5 },
    });
    const result = await embed(client, baseEmbedRequest());

    expect(result.embeddings).toEqual([[0.1, -0.2, 0.3]]);
    expect(result.usage.total_tokens).toBe(5);
    expect(result.timing.total_ms).toBeGreaterThanOrEqual(0);
  });

  it("returns embeddings for multiple inputs", async () => {
    const client = mockEmbedClient({
      data: [
        { embedding: [0.1, 0.2], index: 0 },
        { embedding: [0.3, 0.4], index: 1 },
      ],
      usage: { total_tokens: 10 },
    });
    const result = await embed(
      client,
      baseEmbedRequest({ input: ["text one", "text two"] }),
    );

    expect(result.embeddings).toEqual([
      [0.1, 0.2],
      [0.3, 0.4],
    ]);
    expect(result.usage.total_tokens).toBe(10);
  });

  it("passes correct params to SDK", async () => {
    const client = mockEmbedClient({
      data: [{ embedding: [0.1], index: 0 }],
      usage: { total_tokens: 3 },
    });
    await embed(client, baseEmbedRequest({ model: "text-embedding-3-large" }));

    expect(client.embeddings.create).toHaveBeenCalledWith({
      model: "text-embedding-3-large",
      input: ["hello world"],
    });
  });

  it("handles missing usage gracefully", async () => {
    const client = mockEmbedClient({
      data: [{ embedding: [0.5], index: 0 }],
      usage: undefined,
    });
    const result = await embed(client, baseEmbedRequest());

    expect(result.usage.total_tokens).toBe(0);
  });
});

// ── Tests: imageGenerate ──────────────────────────────────────────────

function baseImageRequest(
  overrides?: Partial<ImageGenerateRequest>,
): ImageGenerateRequest {
  return {
    provider: "openai",
    model: "dall-e-3",
    api_key: "sk-test",
    prompt: "a cat wearing a top hat",
    ...overrides,
  };
}

function mockImageClient(response: Record<string, unknown>): OpenAI {
  return {
    images: {
      generate: vi.fn().mockResolvedValue(response),
    },
  } as unknown as OpenAI;
}

describe("imageGenerate", () => {
  it("returns url and revised_prompt", async () => {
    const client = mockImageClient({
      data: [
        {
          url: "https://example.com/image.png",
          revised_prompt: "a fluffy cat wearing a tall black top hat",
        },
      ],
    });
    const result = await imageGenerate(client, baseImageRequest());

    expect(result.url).toBe("https://example.com/image.png");
    expect(result.revised_prompt).toBe(
      "a fluffy cat wearing a tall black top hat",
    );
    expect(result.timing.total_ms).toBeGreaterThanOrEqual(0);
  });

  it("passes size and quality to SDK", async () => {
    const client = mockImageClient({
      data: [{ url: "https://example.com/img.png", revised_prompt: "test" }],
    });
    await imageGenerate(
      client,
      baseImageRequest({ size: "1024x1024", quality: "hd" }),
    );

    expect(client.images.generate).toHaveBeenCalledWith(
      expect.objectContaining({
        model: "dall-e-3",
        prompt: "a cat wearing a top hat",
        size: "1024x1024",
        quality: "hd",
      }),
    );
  });

  it("omits size and quality when not provided", async () => {
    const client = mockImageClient({
      data: [{ url: "https://example.com/img.png", revised_prompt: "test" }],
    });
    await imageGenerate(client, baseImageRequest());

    expect(client.images.generate).toHaveBeenCalledWith({
      model: "dall-e-3",
      prompt: "a cat wearing a top hat",
    });
  });

  it("handles missing revised_prompt", async () => {
    const client = mockImageClient({
      data: [{ url: "https://example.com/img.png" }],
    });
    const result = await imageGenerate(client, baseImageRequest());

    expect(result.revised_prompt).toBe("");
  });
});
