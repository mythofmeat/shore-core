/**
 * Embedding model abstraction and OpenAI-compatible implementation.
 *
 * Mirrors `backend/llm/src/embed/mod.rs` plus
 * `backend/daemon/src/memory/retrieval.rs`: callers resolve one configured
 * `[embedding.<name>]` profile, then pass the resulting provider into
 * workspace hybrid search. No local embedder is bundled.
 */

export interface Embedder {
  embed(inputs: string[]): Promise<number[][]>;
  modelId(): string;
  dimensions(): number;
}

export type EmbeddingCatalog = Record<string, Record<string, unknown>>;

const OPENAI_BASE_URL = "https://api.openai.com/v1";
const embedderCache = new Map<string, Embedder>();

export class OpenAIEmbedder implements Embedder {
  constructor(
    private readonly model: string,
    private readonly apiKey: string,
    private readonly baseUrl: string | undefined,
    private readonly dim: number,
  ) {}

  async embed(inputs: string[]): Promise<number[][]> {
    const base = (this.baseUrl ?? OPENAI_BASE_URL).replace(/\/+$/, "");
    const response = await fetch(`${base}/embeddings`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${this.apiKey}`,
        "content-type": "application/json",
      },
      body: JSON.stringify({
        model: this.model,
        input: inputs,
      }),
    });

    const text = await response.text();
    if (!response.ok) {
      throw new Error(
        `embedding request failed: HTTP ${response.status} ${response.statusText}; body preview: ${bodyPreview(text, 200)}`,
      );
    }

    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch (e) {
      throw new Error(
        `embedding response was not valid JSON: ${(e as Error).message}; body preview: ${bodyPreview(text, 200)}`,
      );
    }

    return parseEmbeddingResponse(parsed, inputs.length);
  }

  modelId(): string {
    return this.model;
  }

  dimensions(): number {
    return this.dim;
  }
}

export function resolveEmbedder(
  defaultName: string | undefined,
  catalog: EmbeddingCatalog,
): Embedder {
  const names = Object.keys(catalog).sort();
  const profileName = defaultName ?? names[0];
  if (profileName === undefined) {
    throw new Error(
      "no embedding profile configured; semantic search disabled. Add an [embedding.<name>] block pointing at an OpenAI-compatible embeddings endpoint (see CONFIGURATION.md).",
    );
  }

  const profile = catalog[profileName];
  if (profile === undefined) {
    throw new Error(
      `embedding profile '${profileName}' is not declared; add an [embedding.${profileName}] block to your config`,
    );
  }

  const provider = stringField(profile, "provider") ?? "openai";
  if (provider === "local") {
    throw new Error(
      `embedding profile '${profileName}' uses provider = "local", which is no longer supported. Run an OpenAI-compatible embeddings server yourself (e.g. text-embedding-inference, llama.cpp server) and point base_url at it.`,
    );
  }

  const modelId = stringField(profile, "model_id");
  if (typeof modelId !== "string" || modelId.length === 0) {
    throw new Error(`embedding profile '${profileName}' is missing model_id`);
  }

  const apiKeyEnv = stringField(profile, "api_key_env") ?? "OPENAI_API_KEY";
  const apiKey = process.env[apiKeyEnv];
  if (!apiKey) {
    throw new Error(`embedding API key env var '${apiKeyEnv}' is not set`);
  }

  const dimensions = numberField(profile, "dimensions") ?? 1536;
  const cacheKey = [
    provider,
    modelId,
    apiKeyEnv,
    stringField(profile, "base_url") ?? "default",
    String(dimensions),
  ].join("::");
  const cached = embedderCache.get(cacheKey);
  if (cached !== undefined) return cached;

  const embedder = new OpenAIEmbedder(
    modelId,
    apiKey,
    stringField(profile, "base_url"),
    dimensions,
  );
  embedderCache.set(cacheKey, embedder);
  return embedder;
}

export function parseEmbeddingResponse(
  response: unknown,
  expectedCount: number,
): number[][] {
  if (!isRecord(response) || !Array.isArray(response["data"])) {
    throw new Error("embedding response missing data array");
  }

  const data = response["data"];
  if (data.length !== expectedCount) {
    throw new Error(
      `embedding response returned ${data.length} vectors for ${expectedCount} inputs`,
    );
  }

  return data.map((item, itemIdx) => {
    if (!isRecord(item) || !Array.isArray(item["embedding"])) {
      throw new Error(
        `embedding response item ${itemIdx} missing embedding array`,
      );
    }
    return item["embedding"].map((n, numIdx) => {
      if (typeof n !== "number" || !Number.isFinite(n)) {
        throw new Error(
          `embedding response item ${itemIdx} has non-numeric value at position ${numIdx}`,
        );
      }
      return n;
    });
  });
}

function bodyPreview(text: string, maxChars: number): string {
  const chars = [...text];
  if (chars.length <= maxChars) return text;
  return `${chars.slice(0, maxChars).join("")}...`;
}

function stringField(obj: Record<string, unknown>, key: string): string | undefined {
  const v = obj[key];
  return typeof v === "string" ? v : undefined;
}

function numberField(obj: Record<string, unknown>, key: string): number | undefined {
  const v = obj[key];
  return typeof v === "number" && Number.isFinite(v) ? v : undefined;
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}
