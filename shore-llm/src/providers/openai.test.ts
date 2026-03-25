import { describe, it, expect, vi } from "vitest";
import type OpenAI from "openai";
import {
  translateMessages,
  translateTools,
  generate,
  stream,
} from "./openai.js";
import type { ProviderRequest } from "./types.js";
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
});
