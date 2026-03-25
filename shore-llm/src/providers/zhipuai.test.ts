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

import { generate, stream } from "./zhipuai.js";

function baseRequest(overrides?: Partial<ProviderRequest>): ProviderRequest {
  return {
    provider: "zhipuai",
    model: "glm-4",
    api_key: "zhipu-test",
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
  model: "glm-4",
  provider: "zhipuai",
};

describe("ZhipuAI provider", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockCreateClient.mockReturnValue({});
    mockGenerate.mockResolvedValue(mockNormalizedResponse);
    mockStream.mockResolvedValue(undefined);
  });

  it("creates client with ZhipuAI base URL", async () => {
    await generate(baseRequest());
    expect(mockCreateClient).toHaveBeenCalledWith(
      "zhipu-test",
      "https://open.bigmodel.cn/api/paas/v4",
    );
  });

  it("allows base_url override", async () => {
    await generate(baseRequest({ base_url: "https://custom.zhipuai.cn/v4" }));
    expect(mockCreateClient).toHaveBeenCalledWith(
      "zhipu-test",
      "https://custom.zhipuai.cn/v4",
    );
  });

  it("calls generate with zhipuai provider name", async () => {
    await generate(baseRequest());
    expect(mockGenerate).toHaveBeenCalledWith(
      expect.anything(),
      expect.objectContaining({ provider: "zhipuai" }),
      "zhipuai",
    );
  });

  it("calls stream with zhipuai provider name", async () => {
    const mockRes = {} as unknown as import("node:http").ServerResponse;
    await stream(baseRequest(), mockRes);
    expect(mockStream).toHaveBeenCalledWith(
      expect.anything(),
      expect.objectContaining({ provider: "zhipuai" }),
      mockRes,
      "zhipuai",
    );
  });
});
