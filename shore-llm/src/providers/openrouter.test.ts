import { describe, it, expect, vi, beforeEach } from "vitest";
import type { ProviderRequest, NormalizedResponse } from "./types.js";

const { mockCreateClient, mockGenerate, mockStream } = vi.hoisted(() => ({
  mockCreateClient: vi.fn(),
  mockGenerate: vi.fn(),
  mockStream: vi.fn(),
}));

vi.mock("./openai.js", () => ({
  createClient: mockCreateClient,
  generate: mockGenerate,
  stream: mockStream,
}));

import { generate, stream } from "./openrouter.js";

function baseRequest(overrides?: Partial<ProviderRequest>): ProviderRequest {
  return {
    provider: "openrouter",
    model: "openai/gpt-4",
    api_key: "sk-or-test",
    messages: [{ role: "user", content: "Hello" }],
    max_tokens: 1024,
    ...overrides,
  };
}

const mockNormalizedResponse: NormalizedResponse = {
  content: "Hello!",
  content_blocks: [{ type: "text", text: "Hello!" }],
  finish_reason: "end_turn",
  usage: {
    input_tokens: 10,
    output_tokens: 20,
    cache_read_tokens: 0,
    cache_creation_tokens: 0,
  },
  timing: { total_ms: 100, time_to_first_token_ms: 50 },
  model: "openai/gpt-4",
  provider: "openrouter",
};

describe("OpenRouter provider", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockCreateClient.mockReturnValue({});
    mockGenerate.mockResolvedValue(mockNormalizedResponse);
    mockStream.mockResolvedValue(undefined);
  });

  it("creates client with OpenRouter base URL", async () => {
    await generate(baseRequest());
    expect(mockCreateClient).toHaveBeenCalledWith(
      "sk-or-test",
      "https://openrouter.ai/api/v1",
      expect.any(Object),
    );
  });

  it("passes custom headers from provider_options", async () => {
    await generate(
      baseRequest({
        provider_options: {
          http_referer: "https://myapp.com",
          x_title: "My App",
        },
      }),
    );
    expect(mockCreateClient).toHaveBeenCalledWith(
      "sk-or-test",
      "https://openrouter.ai/api/v1",
      {
        "HTTP-Referer": "https://myapp.com",
        "X-Title": "My App",
      },
    );
  });

  it("passes empty headers when no provider_options", async () => {
    await generate(baseRequest());
    expect(mockCreateClient).toHaveBeenCalledWith(
      "sk-or-test",
      "https://openrouter.ai/api/v1",
      {},
    );
  });

  it("calls generate with openrouter provider name", async () => {
    await generate(baseRequest());
    expect(mockGenerate).toHaveBeenCalledWith(
      expect.anything(),
      expect.objectContaining({ provider: "openrouter" }),
      "openrouter",
    );
  });

  it("calls stream with openrouter provider name", async () => {
    const mockRes = {} as unknown as import("node:http").ServerResponse;
    await stream(baseRequest(), mockRes);
    expect(mockStream).toHaveBeenCalledWith(
      expect.anything(),
      expect.objectContaining({ provider: "openrouter" }),
      mockRes,
      "openrouter",
    );
  });
});
