import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import type {
  ProviderRequest,
  NormalizedResponse,
  ImageGenerateRequest,
} from "./types.js";

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

import { generate, stream, imageGenerate } from "./openrouter.js";

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

// ── Image generation tests ────────────────────────────────────────────

function baseImageRequest(
  overrides?: Partial<ImageGenerateRequest>,
): ImageGenerateRequest {
  return {
    provider: "openrouter",
    model: "black-forest-labs/flux-2-pro",
    api_key: "sk-or-test",
    prompt: "a cat in space",
    ...overrides,
  };
}

function mockFetchResponse(body: unknown, ok = true, status = 200) {
  return vi.fn().mockResolvedValue({
    ok,
    status,
    json: () => Promise.resolve(body),
    text: () => Promise.resolve(JSON.stringify(body)),
  });
}

const FAKE_B64 = "data:image/png;base64,iVBORw0KGgo=";

function chatResponseWithImage(imageUrl = FAKE_B64, content = "A cat floating in space") {
  return {
    choices: [
      {
        message: {
          role: "assistant",
          content,
          images: [{ type: "image_url", image_url: { url: imageUrl } }],
        },
      },
    ],
  };
}

describe("OpenRouter imageGenerate", () => {
  const originalFetch = globalThis.fetch;

  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(() => {
    globalThis.fetch = originalFetch;
  });

  it("sends correct request shape with modalities", async () => {
    globalThis.fetch = mockFetchResponse(chatResponseWithImage());

    await imageGenerate(baseImageRequest());

    expect(globalThis.fetch).toHaveBeenCalledWith(
      "https://openrouter.ai/api/v1/chat/completions",
      expect.objectContaining({
        method: "POST",
        headers: expect.objectContaining({
          Authorization: "Bearer sk-or-test",
        }),
      }),
    );

    const callBody = JSON.parse(
      (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls[0][1].body,
    );
    expect(callBody.model).toBe("black-forest-labs/flux-2-pro");
    expect(callBody.modalities).toEqual(["image", "text"]);
    expect(callBody.messages).toEqual([
      { role: "user", content: "a cat in space" },
    ]);
  });

  it("includes image_config when aspect_ratio or image_size set", async () => {
    globalThis.fetch = mockFetchResponse(chatResponseWithImage());

    await imageGenerate(
      baseImageRequest({ aspect_ratio: "16:9", image_size: "2K" }),
    );

    const callBody = JSON.parse(
      (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls[0][1].body,
    );
    expect(callBody.image_config).toEqual({
      aspect_ratio: "16:9",
      image_size: "2K",
    });
  });

  it("omits image_config when no aspect_ratio or image_size", async () => {
    globalThis.fetch = mockFetchResponse(chatResponseWithImage());

    await imageGenerate(baseImageRequest());

    const callBody = JSON.parse(
      (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls[0][1].body,
    );
    expect(callBody.image_config).toBeUndefined();
  });

  it("extracts base64 data URL from response", async () => {
    globalThis.fetch = mockFetchResponse(chatResponseWithImage());

    const result = await imageGenerate(baseImageRequest());

    expect(result.url).toBe(FAKE_B64);
    expect(result.revised_prompt).toBe("A cat floating in space");
    expect(result.timing.total_ms).toBeGreaterThanOrEqual(0);
  });

  it("uses empty string when message has no content", async () => {
    const response = {
      choices: [
        {
          message: {
            role: "assistant",
            images: [{ type: "image_url", image_url: { url: FAKE_B64 } }],
          },
        },
      ],
    };
    globalThis.fetch = mockFetchResponse(response);

    const result = await imageGenerate(baseImageRequest());

    expect(result.revised_prompt).toBe("");
  });

  it("throws when response has no images", async () => {
    const response = {
      choices: [{ message: { role: "assistant", content: "No image" } }],
    };
    globalThis.fetch = mockFetchResponse(response);

    await expect(imageGenerate(baseImageRequest())).rejects.toThrow(
      "no image data",
    );
  });

  it("throws on non-OK response", async () => {
    globalThis.fetch = mockFetchResponse(
      { error: "rate_limited" },
      false,
      429,
    );

    await expect(imageGenerate(baseImageRequest())).rejects.toThrow("429");
  });

  it("falls back to image-only modalities on 404 output modalities error", async () => {
    const modalityError = JSON.stringify({
      error: {
        message: "No endpoints found that support the requested output modalities: image, text",
        code: 404,
      },
    });

    let callCount = 0;
    globalThis.fetch = vi.fn().mockImplementation(() => {
      callCount++;
      if (callCount === 1) {
        // First call with ["image", "text"] → 404
        return Promise.resolve({
          ok: false,
          status: 404,
          json: () => Promise.resolve(JSON.parse(modalityError)),
          text: () => Promise.resolve(modalityError),
        });
      }
      // Second call with ["image"] → success
      return Promise.resolve({
        ok: true,
        status: 200,
        json: () => Promise.resolve(chatResponseWithImage()),
        text: () => Promise.resolve(JSON.stringify(chatResponseWithImage())),
      });
    });

    const result = await imageGenerate(baseImageRequest());

    expect(result.url).toBe(FAKE_B64);
    expect(globalThis.fetch).toHaveBeenCalledTimes(2);

    // Second call should use ["image"] modality only
    const secondCallBody = JSON.parse(
      (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls[1][1].body,
    );
    expect(secondCallBody.modalities).toEqual(["image"]);
  });

  it("throws when both modality modes fail", async () => {
    const modalityError = JSON.stringify({
      error: {
        message: "No endpoints found that support the requested output modalities: image, text",
        code: 404,
      },
    });
    const imageOnlyError = JSON.stringify({
      error: {
        message: "No endpoints found that support the requested output modalities: image",
        code: 404,
      },
    });

    let callCount = 0;
    globalThis.fetch = vi.fn().mockImplementation(() => {
      callCount++;
      const err = callCount === 1 ? modalityError : imageOnlyError;
      return Promise.resolve({
        ok: false,
        status: 404,
        json: () => Promise.resolve(JSON.parse(err)),
        text: () => Promise.resolve(err),
      });
    });

    await expect(imageGenerate(baseImageRequest())).rejects.toThrow(
      "both modality modes",
    );
  });
});
