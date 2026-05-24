import { describe, expect, it } from "bun:test";

import {
  parseEmbeddingResponse,
  resolveEmbedder,
} from "../src/llm/embed.ts";

describe("embedding provider", () => {
  it("parses OpenAI-compatible embedding responses", () => {
    const vectors = parseEmbeddingResponse(
      {
        data: [
          { embedding: [1, 0, 0.5] },
          { embedding: [0, 1, 0.25] },
        ],
      },
      2,
    );

    expect(vectors).toEqual([
      [1, 0, 0.5],
      [0, 1, 0.25],
    ]);
  });

  it("errors clearly when no embedding profile is configured", () => {
    expect(() => resolveEmbedder(undefined, {})).toThrow(
      /no embedding profile configured/,
    );
  });

  it("errors clearly for unsupported local profiles", () => {
    expect(() =>
      resolveEmbedder("local", {
        local: { provider: "local", model_id: "bge-large-en-v1.5" },
      }),
    ).toThrow(/no longer supported/);
  });

  it("live OpenAI-compatible endpoint smoke test when explicitly enabled", async () => {
    if (process.env["SHORE_EMBED_LIVE"] !== "1") return;

    const apiKeyEnv = process.env["SHORE_EMBED_API_KEY_ENV"] ?? "OPENAI_API_KEY";
    const modelId =
      process.env["SHORE_EMBED_MODEL"] ?? "text-embedding-3-small";
    const baseUrl = process.env["SHORE_EMBED_BASE_URL"];
    const dimensions =
      Number.parseInt(process.env["SHORE_EMBED_DIMENSIONS"] ?? "", 10) ||
      undefined;

    const profile: Record<string, unknown> = {
      model_id: modelId,
      api_key_env: apiKeyEnv,
    };
    if (baseUrl !== undefined) profile["base_url"] = baseUrl;
    if (dimensions !== undefined) profile["dimensions"] = dimensions;

    const embedder = resolveEmbedder("live", { live: profile });
    const vectors = await embedder.embed(["shore semantic search smoke"]);
    expect(vectors.length).toBe(1);
    expect(vectors[0]!.length).toBeGreaterThan(0);
  });
});
