import { describe, expect, it } from "bun:test";

import {
  discoverAnthropic,
  discoverOpenAICompatible,
  DiscoveryError,
  type DiscoveryFetcher,
} from "../src/llm/discovery.ts";

function captureFetcher(body: unknown): {
  fetcher: DiscoveryFetcher;
  calls: Array<{ url: string; headers: Record<string, string> }>;
} {
  const calls: Array<{ url: string; headers: Record<string, string> }> = [];
  const fetcher: DiscoveryFetcher = async (url, init) => {
    calls.push({ url, headers: init.headers });
    return {
      ok: true,
      status: 200,
      text: async () => JSON.stringify(body),
    };
  };
  return { fetcher, calls };
}

describe("llm/discovery", () => {
  it("openai-compatible appends /models, sends bearer auth, maps fields", async () => {
    const { fetcher, calls } = captureFetcher({
      data: [
        {
          id: "openai/gpt-4o",
          display_name: "GPT-4o",
          context_length: 128_000,
          owned_by: "openai",
          top_provider: { max_completion_tokens: 16_384 },
          supported_parameters: ["tools", "reasoning"],
          architecture: { input_modalities: ["text", "image"] },
        },
      ],
    });
    const models = await discoverOpenAICompatible(
      "upstream",
      "https://example.test/v1",
      "sk-test",
      fetcher,
    );
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe("https://example.test/v1/models");
    expect(calls[0].headers.authorization).toBe("Bearer sk-test");
    expect(models).toHaveLength(1);
    expect(models[0]).toMatchObject({
      provider_key: "upstream",
      model_id: "openai/gpt-4o",
      display_name: "GPT-4o",
      sdk: "openai",
      base_url: "https://example.test/v1",
      context_length: 128_000,
      max_output_tokens: 16_384,
      supports_tools: true,
      supports_reasoning: true,
      supports_images: true,
      owned_by: "openai",
    });
  });

  it("openai-compatible normalizes trailing slash on base URL", async () => {
    const { fetcher, calls } = captureFetcher({ data: [] });
    await discoverOpenAICompatible("upstream", "https://example.test/v1/", "k", fetcher);
    expect(calls[0].url).toBe("https://example.test/v1/models");
  });

  it("anthropic uses x-api-key + anthropic-version + /v1/models", async () => {
    const { fetcher, calls } = captureFetcher({
      data: [{ id: "claude-sonnet-4-20250514", display_name: "Claude Sonnet 4" }],
    });
    const models = await discoverAnthropic(
      "anthropic",
      "https://api.anthropic.com",
      "sk-ant-test",
      fetcher,
    );
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe("https://api.anthropic.com/v1/models");
    expect(calls[0].headers["x-api-key"]).toBe("sk-ant-test");
    expect(calls[0].headers["anthropic-version"]).toBe("2023-06-01");
    expect(calls[0].headers.authorization).toBeUndefined();
    expect(models[0]).toMatchObject({
      model_id: "claude-sonnet-4-20250514",
      display_name: "Claude Sonnet 4",
      sdk: "anthropic",
    });
  });

  it("anthropic skips re-adding /v1 when base URL already includes it", async () => {
    const { fetcher, calls } = captureFetcher({ data: [] });
    await discoverAnthropic("anthropic", "https://gateway.test/v1", "k", fetcher);
    expect(calls[0].url).toBe("https://gateway.test/v1/models");
  });

  it("propagates HTTP failures as DiscoveryError with status", async () => {
    const fetcher: DiscoveryFetcher = async () => ({
      ok: false,
      status: 401,
      text: async () => "{\"error\":\"unauthorized\"}",
    });
    await expect(discoverOpenAICompatible("upstream", "https://example.test/v1", "bad", fetcher))
      .rejects.toBeInstanceOf(DiscoveryError);
  });

  it("propagates network errors as DiscoveryError", async () => {
    const fetcher: DiscoveryFetcher = async () => {
      throw new Error("ENOTFOUND example.test");
    };
    await expect(discoverOpenAICompatible("upstream", "https://example.test/v1", "k", fetcher))
      .rejects.toMatchObject({ kind: "network" });
  });

  it("rejects parseable-but-bad JSON as parse error", async () => {
    const fetcher: DiscoveryFetcher = async () => ({
      ok: true,
      status: 200,
      text: async () => "not json",
    });
    await expect(discoverOpenAICompatible("upstream", "https://example.test/v1", "k", fetcher))
      .rejects.toMatchObject({ kind: "parse" });
  });

  it("treats absent `data` array as empty model list", async () => {
    const { fetcher } = captureFetcher({});
    const models = await discoverOpenAICompatible("upstream", "https://example.test/v1", "k", fetcher);
    expect(models).toEqual([]);
  });

  it("skips entries missing `id` rather than failing the batch", async () => {
    const { fetcher } = captureFetcher({
      data: [{ id: "good/m1" }, { name: "no-id" }, { id: "good/m2" }],
    });
    const models = await discoverOpenAICompatible("upstream", "https://example.test/v1", "k", fetcher);
    expect(models.map((m) => m.model_id)).toEqual(["good/m1", "good/m2"]);
  });
});
