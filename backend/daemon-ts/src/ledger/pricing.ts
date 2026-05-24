/**
 * Model pricing via OpenRouter API with local DB cache.
 *
 * Mirrors `backend/ledger/src/pricing.rs`: memory-cached `ModelPricing`
 * keyed by OpenRouter model id, with a write-through `pricing` table in
 * the same SQLite database. `to_openrouter_id` and `is_anthropic_pricing`
 * are exposed because `record_call` (TS port) needs them.
 */

import type { Database } from "bun:sqlite";

import { ledgerDatabase, type Ledger } from "./ledger.ts";

/**
 * Anthropic 1h cache TTL write price is 2× input (5min is 1.25×). The
 * catalog stores the 5-minute price, so we scale by 2.0/1.25 = 1.6 when
 * the TTL is "1h" for a native Anthropic call.
 */
export const ANTHROPIC_1H_CACHE_WRITE_MULTIPLIER = 1.6;

export interface ModelPricing {
  input_per_token: number;
  output_per_token: number;
  cache_read_per_token: number;
  cache_write_per_token: number;
}

export interface CostBreakdown {
  input: number;
  output: number;
  cache_read: number;
  cache_write: number;
  total: number;
}

export interface CostRequest {
  provider: string;
  model: string;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
  cacheTtl?: string | undefined;
}

interface PricingRow {
  input_per_token: number;
  output_per_token: number;
  cache_read_per_token: number;
  cache_write_per_token: number;
}

/** Reusable fetch hook so tests can avoid the live OpenRouter endpoint. */
export type OpenRouterCatalogFetch = () => Promise<unknown>;

const DEFAULT_FETCH: OpenRouterCatalogFetch = async () => {
  const resp = await fetch("https://openrouter.ai/api/v1/models");
  if (!resp.ok) {
    throw new Error(`OpenRouter catalog HTTP ${resp.status}`);
  }
  return await resp.json();
};

export class PricingEngine {
  private readonly memoryCache = new Map<string, ModelPricing>();
  private readonly db: Database;

  constructor(
    ledger: Ledger,
    private readonly fetcher: OpenRouterCatalogFetch = DEFAULT_FETCH,
  ) {
    this.db = ledgerDatabase(ledger);
  }

  storePricing(modelId: string, pricing: ModelPricing): void {
    this.db
      .query(
        `INSERT OR REPLACE INTO pricing
            (model_id, input_per_token, output_per_token,
             cache_read_per_token, cache_write_per_token, fetched_at)
           VALUES ($model_id, $input, $output, $cache_read, $cache_write, $fetched_at)`,
      )
      .run({
        $model_id: modelId,
        $input: pricing.input_per_token,
        $output: pricing.output_per_token,
        $cache_read: pricing.cache_read_per_token,
        $cache_write: pricing.cache_write_per_token,
        $fetched_at: new Date().toISOString(),
      });
    this.memoryCache.set(modelId, { ...pricing });
  }

  getCachedPricing(modelId: string): ModelPricing | undefined {
    const cached = this.memoryCache.get(modelId);
    if (cached !== undefined) return { ...cached };

    const row = this.db
      .query<PricingRow, { $model_id: string }>(
        `SELECT input_per_token, output_per_token,
                cache_read_per_token, cache_write_per_token
           FROM pricing WHERE model_id = $model_id`,
      )
      .get({ $model_id: modelId });
    if (row === null) return undefined;

    const pricing: ModelPricing = {
      input_per_token: row.input_per_token,
      output_per_token: row.output_per_token,
      cache_read_per_token: row.cache_read_per_token,
      cache_write_per_token: row.cache_write_per_token,
    };
    this.memoryCache.set(modelId, pricing);
    return { ...pricing };
  }

  /**
   * Fetch the full OpenRouter catalog and write every row to both DB and
   * memory caches. Returns the entry for `targetModelId` if present.
   */
  async fetchPricing(
    provider: string,
    model: string,
  ): Promise<ModelPricing | undefined> {
    const target = toOpenRouterId(provider, model);
    let body: unknown;
    try {
      body = await this.fetcher();
    } catch (e) {
      console.warn(
        `[pricing] OpenRouter catalog fetch failed: ${(e as Error).message}`,
      );
      return undefined;
    }
    const models = extractCatalog(body);
    if (models === undefined) {
      console.warn("[pricing] OpenRouter catalog response missing data array");
      return undefined;
    }

    let result: ModelPricing | undefined;
    for (const m of models) {
      const id = stringProp(m, "id");
      if (id === undefined) continue;
      const pricingObj = recordProp(m, "pricing");
      if (pricingObj === undefined) continue;

      const pricing: ModelPricing = {
        input_per_token: parsePrice(pricingObj["prompt"]),
        output_per_token: parsePrice(pricingObj["completion"]),
        cache_read_per_token: parsePrice(
          pricingObj["input_cache_read"] ?? pricingObj["cache_read"],
        ),
        cache_write_per_token: parsePrice(
          pricingObj["input_cache_write"] ?? pricingObj["cache_write"],
        ),
      };
      if (id === target) result = { ...pricing };
      try {
        this.storePricing(id, pricing);
      } catch (e) {
        console.warn(
          `[pricing] failed to cache pricing for ${id}: ${(e as Error).message}`,
        );
      }
    }
    return result;
  }

  /** Cache hit → memory; cache miss → catalog fetch. Best-effort. */
  async getOrFetch(provider: string, model: string): Promise<ModelPricing | undefined> {
    const modelId = toOpenRouterId(provider, model);
    const cached = this.getCachedPricing(modelId);
    if (cached !== undefined) return cached;
    try {
      return await this.fetchPricing(provider, model);
    } catch (e) {
      console.warn(`[pricing] fetch failed: ${(e as Error).message}`);
      return undefined;
    }
  }

  /**
   * Sync price multiplication against cached pricing only. Returns
   * undefined when the model is unknown to the catalog.
   */
  calculateCost(request: CostRequest): CostBreakdown | undefined {
    const modelId = toOpenRouterId(request.provider, request.model);
    const pricing = this.getCachedPricing(modelId);
    if (pricing === undefined) return undefined;

    const input = pricing.input_per_token * request.inputTokens;
    const output = pricing.output_per_token * request.outputTokens;
    const cacheRead = pricing.cache_read_per_token * request.cacheReadTokens;

    let cacheWrite = pricing.cache_write_per_token * request.cacheWriteTokens;
    const ttl = request.cacheTtl ?? "1h";
    if (request.provider === "anthropic" && ttl === "1h") {
      cacheWrite *= ANTHROPIC_1H_CACHE_WRITE_MULTIPLIER;
    }

    return {
      input,
      output,
      cache_read: cacheRead,
      cache_write: cacheWrite,
      total: input + output + cacheRead + cacheWrite,
    };
  }

  clearCache(): void {
    this.db.exec("DELETE FROM pricing");
    this.memoryCache.clear();
  }
}

/**
 * Map a (provider, model) pair to OpenRouter's model id format.
 *
 * - `openrouter` provider: model is already in OpenRouter format.
 * - Any provider whose model contains "/": pre-formatted, pass through
 *   (custom providers like `openrouter-anthropic` resolve their model id
 *   to `anthropic/<id>` upstream of the ledger).
 * - `anthropic`: OpenRouter uses a dot for minor versions
 *   (`claude-opus-4-6` → `claude-opus-4.6`).
 * - Otherwise `{provider}/{model}`.
 */
export function toOpenRouterId(provider: string, model: string): string {
  if (provider === "openrouter" || model.includes("/")) {
    return model;
  }
  if (provider === "anthropic") {
    return `anthropic/${normalizeAnthropicModel(model)}`;
  }
  return `${provider}/${model}`;
}

/**
 * Recognize Anthropic-family rows. Native Anthropic uses the literal
 * provider key; OpenRouter-routed Anthropic carries an `anthropic/...`
 * model id by the time it reaches the ledger.
 */
export function isAnthropicPricing(provider: string, model: string): boolean {
  return provider === "anthropic" || model.startsWith("anthropic/");
}

function normalizeAnthropicModel(model: string): string {
  const chars = [...model];
  for (let i = chars.length - 1; i >= 1; i--) {
    if (
      chars[i] === "-"
      && i + 1 < chars.length
      && isAsciiDigit(chars[i - 1]!)
      && isAsciiDigit(chars[i + 1]!)
    ) {
      chars[i] = ".";
      break;
    }
  }
  return chars.join("");
}

function isAsciiDigit(ch: string): boolean {
  return ch >= "0" && ch <= "9";
}

function parsePrice(v: unknown): number {
  if (typeof v === "number" && Number.isFinite(v)) return v;
  if (typeof v === "string") {
    const n = Number.parseFloat(v);
    return Number.isFinite(n) ? n : 0;
  }
  return 0;
}

function extractCatalog(body: unknown): Record<string, unknown>[] | undefined {
  if (!isRecord(body)) return undefined;
  const data = body["data"];
  if (!Array.isArray(data)) return undefined;
  return data.filter(isRecord);
}

function stringProp(obj: Record<string, unknown>, key: string): string | undefined {
  const v = obj[key];
  return typeof v === "string" ? v : undefined;
}

function recordProp(
  obj: Record<string, unknown>,
  key: string,
): Record<string, unknown> | undefined {
  const v = obj[key];
  return isRecord(v) ? v : undefined;
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}
