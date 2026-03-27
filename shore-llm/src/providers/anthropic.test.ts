import { describe, it, expect, vi, beforeEach } from "vitest";
import type Anthropic from "@anthropic-ai/sdk";
import type {
  Message,
  Usage,
  RawMessageStartEvent,
  RawContentBlockStartEvent,
  RawContentBlockDeltaEvent,
  RawContentBlockStopEvent,
  RawMessageDeltaEvent,
  RawMessageStopEvent,
  RawMessageStreamEvent,
} from "@anthropic-ai/sdk/resources/messages/messages.js";
import {
  buildCreateParams,
  generate,
  stream,
  type GenerateRequest,
} from "./anthropic.js";
import type { ServerResponse } from "node:http";

// ── Helpers ────────────────────────────────────────────────────────────

function baseRequest(overrides?: Partial<GenerateRequest>): GenerateRequest {
  return {
    provider: "anthropic",
    model: "claude-sonnet-4-6",
    api_key: "sk-test",
    messages: [{ role: "user", content: "Hello" }],
    max_tokens: 1024,
    ...overrides,
  };
}

function makeUsage(overrides?: Partial<Usage>): Usage {
  return {
    input_tokens: 10,
    output_tokens: 20,
    cache_creation_input_tokens: 0,
    cache_read_input_tokens: 0,
    cache_creation: null,
    inference_geo: null,
    server_tool_use: null,
    service_tier: null,
    ...overrides,
  };
}

function makeMessage(overrides?: Partial<Message>): Message {
  return {
    id: "msg_01",
    type: "message",
    role: "assistant",
    model: "claude-sonnet-4-6",
    content: [{ type: "text", text: "Hello there!", citations: null }],
    stop_reason: "end_turn",
    stop_sequence: null,
    usage: makeUsage(),
    container: null,
    ...overrides,
  };
}

function mockClient(message: Message): Anthropic {
  return {
    messages: {
      create: vi.fn().mockResolvedValue(message),
    },
  } as unknown as Anthropic;
}

/** Create async iterable from array of stream events */
async function* streamEvents(
  events: RawMessageStreamEvent[],
): AsyncIterable<RawMessageStreamEvent> {
  for (const e of events) yield e;
}

function mockStreamClient(
  events: RawMessageStreamEvent[],
): Anthropic {
  return {
    messages: {
      create: vi.fn().mockResolvedValue(streamEvents(events)),
    },
  } as unknown as Anthropic;
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

// ── Tests: buildCreateParams ───────────────────────────────────────────

describe("buildCreateParams", () => {
  it("builds basic params", () => {
    const req = baseRequest();
    const params = buildCreateParams(req, false);
    expect(params).toMatchObject({
      model: "claude-sonnet-4-6",
      max_tokens: 1024,
      messages: [{ role: "user", content: "Hello" }],
      stream: false,
    });
  });

  it("includes system when provided", () => {
    const req = baseRequest({ system: "You are helpful" });
    const params = buildCreateParams(req, false);
    expect((params as Record<string, unknown>).system).toBe("You are helpful");
  });

  it("includes tools when provided", () => {
    const tools = [
      {
        name: "get_weather",
        description: "Get weather",
        input_schema: {
          type: "object" as const,
          properties: { city: { type: "string" } },
        },
      },
    ];
    const req = baseRequest({ tools });
    const params = buildCreateParams(req, false);
    expect((params as Record<string, unknown>).tools).toBe(tools);
  });

  it("includes temperature and top_p when provided", () => {
    const req = baseRequest({ temperature: 0.7, top_p: 0.9 });
    const params = buildCreateParams(req, false) as Record<string, unknown>;
    expect(params.temperature).toBe(0.7);
    expect(params.top_p).toBe(0.9);
  });

  it("omits temperature and top_p when null", () => {
    const req = baseRequest({ temperature: null, top_p: null });
    const params = buildCreateParams(req, false) as Record<string, unknown>;
    expect(params).not.toHaveProperty("temperature");
    expect(params).not.toHaveProperty("top_p");
  });

  it("enables thinking when provider_options.thinking is true", () => {
    const req = baseRequest({
      provider_options: { thinking: true, budget_tokens: 2048 },
    });
    const params = buildCreateParams(req, false) as Record<string, unknown>;
    expect(params.thinking).toEqual({
      type: "enabled",
      budget_tokens: 2048,
    });
  });

  it("defaults budget_tokens to 1024 when not specified", () => {
    const req = baseRequest({
      provider_options: { thinking: true },
    });
    const params = buildCreateParams(req, false) as Record<string, unknown>;
    expect(params.thinking).toEqual({
      type: "enabled",
      budget_tokens: 1024,
    });
  });

  it("omits thinking when provider_options.thinking is false", () => {
    const req = baseRequest({
      provider_options: { thinking: false },
    });
    const params = buildCreateParams(req, false) as Record<string, unknown>;
    expect(params).not.toHaveProperty("thinking");
  });

  it("enables thinking when budget_tokens is set without explicit thinking flag", () => {
    const req = baseRequest({
      provider_options: { budget_tokens: 4096 },
    });
    const params = buildCreateParams(req, false) as Record<string, unknown>;
    expect(params.thinking).toEqual({
      type: "enabled",
      budget_tokens: 4096,
    });
  });

  it("enables adaptive thinking when reasoning_effort is 'adaptive'", () => {
    const req = baseRequest({
      provider_options: { reasoning_effort: "adaptive" },
    });
    const params = buildCreateParams(req, false) as Record<string, unknown>;
    expect(params.thinking).toEqual({
      type: "adaptive",
    });
  });

  it("applies cache_control to last N messages", () => {
    const req = baseRequest({
      messages: [
        { role: "user", content: "First" },
        { role: "assistant", content: "Response" },
        { role: "user", content: "Second" },
      ],
      provider_options: { cache_control_depth: 2 },
    });
    const params = buildCreateParams(req, false);
    const msgs = params.messages;

    // First message: no cache_control
    expect(msgs[0].content).toBe("First");

    // Second message (depth=2, index 1): converted to blocks with cache_control
    expect(Array.isArray(msgs[1].content)).toBe(true);
    const block1 = (msgs[1].content as Array<Record<string, unknown>>)[0];
    expect(block1.cache_control).toEqual({ type: "ephemeral" });

    // Third message: converted to blocks with cache_control
    expect(Array.isArray(msgs[2].content)).toBe(true);
    const block2 = (msgs[2].content as Array<Record<string, unknown>>)[0];
    expect(block2.cache_control).toEqual({ type: "ephemeral" });
  });

  it("sets stream flag correctly", () => {
    const req = baseRequest();
    expect(buildCreateParams(req, true).stream).toBe(true);
    expect(buildCreateParams(req, false).stream).toBe(false);
  });
});

// ── Tests: generate ────────────────────────────────────────────────────

describe("generate", () => {
  it("returns normalized response for text completion", async () => {
    const msg = makeMessage();
    const client = mockClient(msg);
    const req = baseRequest();

    const result = await generate(client, req);

    expect(result.content).toBe("Hello there!");
    expect(result.content_blocks).toEqual([
      { type: "text", text: "Hello there!" },
    ]);
    expect(result.finish_reason).toBe("end_turn");
    expect(result.model).toBe("claude-sonnet-4-6");
    expect(result.provider).toBe("anthropic");
  });

  it("normalizes usage including cache tokens", async () => {
    const msg = makeMessage({
      usage: makeUsage({
        input_tokens: 100,
        output_tokens: 50,
        cache_read_input_tokens: 80,
        cache_creation_input_tokens: 20,
      }),
    });
    const client = mockClient(msg);
    const result = await generate(client, baseRequest());

    expect(result.usage).toEqual({
      input_tokens: 100,
      output_tokens: 50,
      cache_read_tokens: 80,
      cache_creation_tokens: 20,
    });
  });

  it("handles null cache tokens as zero", async () => {
    const msg = makeMessage({
      usage: makeUsage({
        cache_read_input_tokens: null,
        cache_creation_input_tokens: null,
      }),
    });
    const client = mockClient(msg);
    const result = await generate(client, baseRequest());

    expect(result.usage.cache_read_tokens).toBe(0);
    expect(result.usage.cache_creation_tokens).toBe(0);
  });

  it("includes timing with total_ms and time_to_first_token_ms", async () => {
    const msg = makeMessage();
    const client = mockClient(msg);
    const result = await generate(client, baseRequest());

    expect(result.timing).toHaveProperty("total_ms");
    expect(result.timing).toHaveProperty("time_to_first_token_ms");
    expect(typeof result.timing.total_ms).toBe("number");
    expect(result.timing.total_ms).toBeGreaterThanOrEqual(0);
  });

  it("normalizes tool_use content blocks", async () => {
    const msg = makeMessage({
      content: [
        { type: "text", text: "I'll check the weather.", citations: null },
        {
          type: "tool_use",
          id: "toolu_01",
          name: "get_weather",
          input: { city: "NYC" },
          caller: { type: "client" },
        },
      ],
      stop_reason: "tool_use",
    });
    const client = mockClient(msg);
    const result = await generate(client, baseRequest());

    expect(result.content).toBe("I'll check the weather.");
    expect(result.finish_reason).toBe("tool_use");
    expect(result.content_blocks).toEqual([
      { type: "text", text: "I'll check the weather." },
      { type: "tool_use", id: "toolu_01", name: "get_weather", input: { city: "NYC" } },
    ]);
  });

  it("normalizes thinking content blocks", async () => {
    const msg = makeMessage({
      content: [
        {
          type: "thinking",
          thinking: "Let me think...",
          signature: "sig_abc",
        },
        { type: "text", text: "The answer is 42.", citations: null },
      ],
    });
    const client = mockClient(msg);
    const result = await generate(client, baseRequest());

    expect(result.content).toBe("The answer is 42.");
    expect(result.content_blocks).toEqual([
      { type: "thinking", thinking: "Let me think...", signature: "sig_abc" },
      { type: "text", text: "The answer is 42." },
    ]);
  });

  it("normalizes redacted_thinking content blocks", async () => {
    const msg = makeMessage({
      content: [
        {
          type: "thinking",
          thinking: "Visible thinking",
          signature: "sig_1",
        },
        {
          type: "redacted_thinking",
          data: "opaque_encrypted_data",
        },
        { type: "text", text: "Answer.", citations: null },
      ],
    });
    const client = mockClient(msg);
    const result = await generate(client, baseRequest());

    expect(result.content).toBe("Answer.");
    expect(result.content_blocks).toEqual([
      { type: "thinking", thinking: "Visible thinking", signature: "sig_1" },
      { type: "redacted_thinking", data: "opaque_encrypted_data" },
      { type: "text", text: "Answer." },
    ]);
  });

  it("passes correct params to SDK", async () => {
    const msg = makeMessage();
    const client = mockClient(msg);
    const req = baseRequest({
      system: "Be helpful",
      temperature: 0.5,
    });

    await generate(client, req);

    expect(client.messages.create).toHaveBeenCalledWith(
      expect.objectContaining({
        model: "claude-sonnet-4-6",
        max_tokens: 1024,
        system: "Be helpful",
        temperature: 0.5,
        stream: false,
      }),
    );
  });
});

// ── Tests: stream ──────────────────────────────────────────────────────

describe("stream", () => {
  it("emits start, text, done events for simple text response", async () => {
    const events: RawMessageStreamEvent[] = [
      {
        type: "message_start",
        message: makeMessage({ content: [] }),
      } as RawMessageStartEvent,
      {
        type: "content_block_start",
        index: 0,
        content_block: { type: "text", text: "", citations: null },
      } as RawContentBlockStartEvent,
      {
        type: "content_block_delta",
        index: 0,
        delta: { type: "text_delta", text: "Hello" },
      } as RawContentBlockDeltaEvent,
      {
        type: "content_block_delta",
        index: 0,
        delta: { type: "text_delta", text: " world" },
      } as RawContentBlockDeltaEvent,
      {
        type: "content_block_stop",
        index: 0,
      } as RawContentBlockStopEvent,
      {
        type: "message_delta",
        delta: { stop_reason: "end_turn", stop_sequence: null, container: null },
        usage: {
          output_tokens: 5,
          input_tokens: null,
          cache_read_input_tokens: null,
          cache_creation_input_tokens: null,
          server_tool_use: null,
        },
      } as RawMessageDeltaEvent,
      { type: "message_stop" } as RawMessageStopEvent,
    ];

    const client = mockStreamClient(events);
    const res = mockResponse();
    await stream(client, baseRequest(), res);

    const lines = res.written
      .join("")
      .split("\n")
      .filter((l) => l.length > 0)
      .map((l) => JSON.parse(l));

    expect(lines[0]).toMatchObject({ type: "start", model: "claude-sonnet-4-6" });
    expect(lines[1]).toEqual({ type: "text", text: "Hello" });
    expect(lines[2]).toEqual({ type: "text", text: " world" });
    expect(lines[3]).toMatchObject({
      type: "done",
      content: "Hello world",
      finish_reason: "end_turn",
    });
    expect(lines[3].usage).toBeDefined();
    expect(lines[3].timing).toBeDefined();
    expect(lines[3].timing.total_ms).toBeGreaterThanOrEqual(0);
    expect(lines[3].timing.time_to_first_token_ms).toBeGreaterThanOrEqual(0);
  });

  it("emits thinking events", async () => {
    const events: RawMessageStreamEvent[] = [
      {
        type: "message_start",
        message: makeMessage({ content: [] }),
      } as RawMessageStartEvent,
      {
        type: "content_block_start",
        index: 0,
        content_block: { type: "thinking", thinking: "", signature: "" },
      } as RawContentBlockStartEvent,
      {
        type: "content_block_delta",
        index: 0,
        delta: { type: "thinking_delta", thinking: "Hmm..." },
      } as RawContentBlockDeltaEvent,
      {
        type: "content_block_delta",
        index: 0,
        delta: { type: "signature_delta", signature: "sig_stream_123" },
      } as unknown as RawContentBlockDeltaEvent,
      {
        type: "content_block_stop",
        index: 0,
      } as RawContentBlockStopEvent,
      {
        type: "content_block_start",
        index: 1,
        content_block: { type: "text", text: "", citations: null },
      } as RawContentBlockStartEvent,
      {
        type: "content_block_delta",
        index: 1,
        delta: { type: "text_delta", text: "Answer" },
      } as RawContentBlockDeltaEvent,
      {
        type: "content_block_stop",
        index: 1,
      } as RawContentBlockStopEvent,
      {
        type: "message_delta",
        delta: { stop_reason: "end_turn", stop_sequence: null, container: null },
        usage: {
          output_tokens: 10,
          input_tokens: null,
          cache_read_input_tokens: null,
          cache_creation_input_tokens: null,
          server_tool_use: null,
        },
      } as RawMessageDeltaEvent,
      { type: "message_stop" } as RawMessageStopEvent,
    ];

    const client = mockStreamClient(events);
    const res = mockResponse();
    await stream(client, baseRequest(), res);

    const lines = res.written
      .join("")
      .split("\n")
      .filter((l) => l.length > 0)
      .map((l) => JSON.parse(l));

    expect(lines[0]).toMatchObject({ type: "start" });
    expect(lines[1]).toEqual({ type: "thinking", text: "Hmm..." });
    expect(lines[2]).toEqual({ type: "thinking_signature", signature: "sig_stream_123" });
    expect(lines[3]).toEqual({ type: "text", text: "Answer" });
    expect(lines[4]).toMatchObject({ type: "done", content: "Answer" });
  });

  it("emits redacted_thinking events", async () => {
    const events: RawMessageStreamEvent[] = [
      {
        type: "message_start",
        message: makeMessage({ content: [] }),
      } as RawMessageStartEvent,
      {
        type: "content_block_start",
        index: 0,
        content_block: { type: "redacted_thinking", data: "opaque_data_123" },
      } as unknown as RawContentBlockStartEvent,
      {
        type: "content_block_stop",
        index: 0,
      } as RawContentBlockStopEvent,
      {
        type: "content_block_start",
        index: 1,
        content_block: { type: "text", text: "", citations: null },
      } as RawContentBlockStartEvent,
      {
        type: "content_block_delta",
        index: 1,
        delta: { type: "text_delta", text: "Answer" },
      } as RawContentBlockDeltaEvent,
      {
        type: "content_block_stop",
        index: 1,
      } as RawContentBlockStopEvent,
      {
        type: "message_delta",
        delta: { stop_reason: "end_turn", stop_sequence: null, container: null },
        usage: {
          output_tokens: 10,
          input_tokens: null,
          cache_read_input_tokens: null,
          cache_creation_input_tokens: null,
          server_tool_use: null,
        },
      } as RawMessageDeltaEvent,
      { type: "message_stop" } as RawMessageStopEvent,
    ];

    const client = mockStreamClient(events);
    const res = mockResponse();
    await stream(client, baseRequest(), res);

    const lines = res.written
      .join("")
      .split("\n")
      .filter((l) => l.length > 0)
      .map((l) => JSON.parse(l));

    expect(lines[0]).toMatchObject({ type: "start" });
    expect(lines[1]).toEqual({ type: "redacted_thinking", data: "opaque_data_123" });
    expect(lines[2]).toEqual({ type: "text", text: "Answer" });
    expect(lines[3]).toMatchObject({ type: "done", content: "Answer" });
  });

  it("emits tool_use events with accumulated JSON input", async () => {
    const events: RawMessageStreamEvent[] = [
      {
        type: "message_start",
        message: makeMessage({ content: [] }),
      } as RawMessageStartEvent,
      {
        type: "content_block_start",
        index: 0,
        content_block: {
          type: "tool_use",
          id: "toolu_01",
          name: "get_weather",
          input: {},
          caller: { type: "client" },
        },
      } as unknown as RawContentBlockStartEvent,
      {
        type: "content_block_delta",
        index: 0,
        delta: { type: "input_json_delta", partial_json: '{"ci' },
      } as RawContentBlockDeltaEvent,
      {
        type: "content_block_delta",
        index: 0,
        delta: { type: "input_json_delta", partial_json: 'ty":"NYC"}' },
      } as RawContentBlockDeltaEvent,
      {
        type: "content_block_stop",
        index: 0,
      } as RawContentBlockStopEvent,
      {
        type: "message_delta",
        delta: { stop_reason: "tool_use", stop_sequence: null, container: null },
        usage: {
          output_tokens: 15,
          input_tokens: null,
          cache_read_input_tokens: null,
          cache_creation_input_tokens: null,
          server_tool_use: null,
        },
      } as RawMessageDeltaEvent,
      { type: "message_stop" } as RawMessageStopEvent,
    ];

    const client = mockStreamClient(events);
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
      id: "toolu_01",
      name: "get_weather",
      input: { city: "NYC" },
    });
    expect(lines[2]).toMatchObject({
      type: "done",
      finish_reason: "tool_use",
    });
  });

  it("sets correct content-type header for ndjson", async () => {
    const events: RawMessageStreamEvent[] = [
      {
        type: "message_start",
        message: makeMessage({ content: [] }),
      } as RawMessageStartEvent,
      {
        type: "message_delta",
        delta: { stop_reason: "end_turn", stop_sequence: null, container: null },
        usage: {
          output_tokens: 0,
          input_tokens: null,
          cache_read_input_tokens: null,
          cache_creation_input_tokens: null,
          server_tool_use: null,
        },
      } as RawMessageDeltaEvent,
      { type: "message_stop" } as RawMessageStopEvent,
    ];

    const client = mockStreamClient(events);
    const res = mockResponse();
    await stream(client, baseRequest(), res);

    expect(res.headHeaders["Content-Type"]).toBe("application/x-ndjson");
  });

  it("includes cache tokens in done event usage", async () => {
    const events: RawMessageStreamEvent[] = [
      {
        type: "message_start",
        message: makeMessage({
          content: [],
          usage: makeUsage({
            cache_read_input_tokens: 50,
            cache_creation_input_tokens: 30,
          }),
        }),
      } as RawMessageStartEvent,
      {
        type: "message_delta",
        delta: { stop_reason: "end_turn", stop_sequence: null, container: null },
        usage: {
          output_tokens: 5,
          input_tokens: null,
          cache_read_input_tokens: 50,
          cache_creation_input_tokens: 30,
          server_tool_use: null,
        },
      } as RawMessageDeltaEvent,
      { type: "message_stop" } as RawMessageStopEvent,
    ];

    const client = mockStreamClient(events);
    const res = mockResponse();
    await stream(client, baseRequest(), res);

    const lines = res.written
      .join("")
      .split("\n")
      .filter((l) => l.length > 0)
      .map((l) => JSON.parse(l));

    const doneEvent = lines.find((l: Record<string, unknown>) => l.type === "done");
    expect(doneEvent.usage.cache_read_tokens).toBe(50);
    expect(doneEvent.usage.cache_creation_tokens).toBe(30);
  });
});
