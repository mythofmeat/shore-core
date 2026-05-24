/**
 * Mirror of `backend/ledger/src/pricing.rs::tests` — sync calculate_cost
 * paths, the Anthropic 1h multiplier, openrouter id mapping, DB-fallback
 * memory rehydration, and the OpenRouter catalog parser.
 */

import { describe, expect, it } from "bun:test";

import { Ledger } from "../src/ledger/ledger.ts";
import {
  isAnthropicPricing,
  PricingEngine,
  toOpenRouterId,
  type ModelPricing,
} from "../src/ledger/pricing.ts";

function anthropicPricing(): ModelPricing {
  return {
    input_per_token: 0.000015,
    output_per_token: 0.000075,
    cache_read_per_token: 0.0000015,
    cache_write_per_token: 0.00001875,
  };
}

function engineWith(pricing?: ModelPricing): { ledger: Ledger; engine: PricingEngine } {
  const ledger = Ledger.openInMemory();
  const engine = new PricingEngine(ledger, async () => ({ data: [] }));
  if (pricing !== undefined) {
    engine.storePricing("anthropic/claude-opus-4.6", pricing);
  }
  return { ledger, engine };
}

describe("toOpenRouterId", () => {
  it("dots Anthropic minor versions", () => {
    expect(toOpenRouterId("anthropic", "claude-opus-4-6")).toBe("anthropic/claude-opus-4.6");
    expect(toOpenRouterId("anthropic", "claude-sonnet-4")).toBe("anthropic/claude-sonnet-4");
  });

  it("preserves openrouter and slash-prefixed ids", () => {
    expect(toOpenRouterId("openai", "gpt-4o")).toBe("openai/gpt-4o");
    expect(
      toOpenRouterId("openrouter", "google/gemini-3.1-flash-lite-preview"),
    ).toBe("google/gemini-3.1-flash-lite-preview");
    expect(
      toOpenRouterId("openrouter-anthropic", "anthropic/claude-opus-4.6"),
    ).toBe("anthropic/claude-opus-4.6");
  });
});

describe("isAnthropicPricing", () => {
  it("recognizes routed Anthropic rows", () => {
    expect(isAnthropicPricing("anthropic", "claude-opus-4-6")).toBe(true);
    expect(isAnthropicPricing("openrouter-anthropic", "anthropic/claude-opus-4.6")).toBe(true);
    expect(isAnthropicPricing("openrouter", "anthropic/claude-opus-4.6")).toBe(true);
    expect(isAnthropicPricing("openai", "gpt-4o")).toBe(false);
    expect(isAnthropicPricing("openrouter", "openai/gpt-4o")).toBe(false);
  });
});

describe("PricingEngine.calculateCost", () => {
  it("multiplies tokens by per-token prices", () => {
    const { engine, ledger } = engineWith(anthropicPricing());
    const cost = engine.calculateCost({
      provider: "anthropic",
      model: "claude-opus-4-6",
      inputTokens: 100,
      outputTokens: 50,
      cacheReadTokens: 80,
      cacheWriteTokens: 20,
      cacheTtl: "5m",
    });
    expect(cost).toBeDefined();
    expect(cost!.input).toBeCloseTo(0.0015, 10);
    expect(cost!.output).toBeCloseTo(0.00375, 10);
    expect(cost!.cache_read).toBeCloseTo(0.00012, 10);
    expect(cost!.cache_write).toBeCloseTo(0.000375, 10);
    ledger.close();
  });

  it("applies the Anthropic 1h cache_write multiplier (1.6x)", () => {
    const { engine, ledger } = engineWith(anthropicPricing());
    const cost = engine.calculateCost({
      provider: "anthropic",
      model: "claude-opus-4-6",
      inputTokens: 100,
      outputTokens: 50,
      cacheReadTokens: 80,
      cacheWriteTokens: 20,
      cacheTtl: "1h",
    });
    expect(cost!.cache_write).toBeCloseTo(0.0006, 10);
    expect(cost!.total).toBeCloseTo(0.0015 + 0.00375 + 0.00012 + 0.0006, 10);
    ledger.close();
  });

  it("defaults TTL to 1h when omitted", () => {
    const { engine, ledger } = engineWith(anthropicPricing());
    const cost = engine.calculateCost({
      provider: "anthropic",
      model: "claude-opus-4-6",
      inputTokens: 100,
      outputTokens: 50,
      cacheReadTokens: 80,
      cacheWriteTokens: 20,
    });
    expect(cost!.cache_write).toBeCloseTo(0.0006, 10);
    ledger.close();
  });

  it("does NOT apply the Anthropic multiplier for OpenRouter-routed rows", () => {
    const { engine, ledger } = engineWith(anthropicPricing());
    const cost = engine.calculateCost({
      provider: "openrouter-anthropic",
      model: "anthropic/claude-opus-4.6",
      inputTokens: 100,
      outputTokens: 50,
      cacheReadTokens: 80,
      cacheWriteTokens: 20,
      cacheTtl: "1h",
    });
    expect(cost!.cache_write).toBeCloseTo(0.000375, 10);
    ledger.close();
  });

  it("returns undefined for unknown models", () => {
    const { engine, ledger } = engineWith();
    const cost = engine.calculateCost({
      provider: "unknown",
      model: "model",
      inputTokens: 100,
      outputTokens: 50,
      cacheReadTokens: 0,
      cacheWriteTokens: 0,
    });
    expect(cost).toBeUndefined();
    ledger.close();
  });
});

describe("PricingEngine cache + DB fallback", () => {
  it("populates the memory cache on DB hit", () => {
    const ledger = Ledger.openInMemory();
    const engine = new PricingEngine(ledger, async () => ({ data: [] }));
    engine.storePricing("test/model", {
      input_per_token: 0.000015,
      output_per_token: 0.000075,
      cache_read_per_token: 0.0000015,
      cache_write_per_token: 0.00001875,
    });

    // Re-instantiate so the memory cache is empty but the DB row remains.
    const reborn = new PricingEngine(ledger, async () => ({ data: [] }));
    const cached = reborn.getCachedPricing("test/model");
    expect(cached?.input_per_token).toBeCloseTo(0.000015, 10);
    expect(cached?.cache_write_per_token).toBeCloseTo(0.00001875, 10);
    ledger.close();
  });

  it("clearCache empties both DB and memory", () => {
    const { engine, ledger } = engineWith(anthropicPricing());
    engine.clearCache();
    expect(engine.getCachedPricing("anthropic/claude-opus-4.6")).toBeUndefined();
    ledger.close();
  });
});

describe("PricingEngine.fetchPricing (catalog parsing)", () => {
  it("populates pricing for every entry and returns the target", async () => {
    const ledger = Ledger.openInMemory();
    const engine = new PricingEngine(ledger, async () => ({
      data: [
        {
          id: "anthropic/claude-opus-4.6",
          pricing: {
            prompt: "0.000015",
            completion: "0.000075",
            input_cache_read: 0.0000015,
            input_cache_write: 0.00001875,
          },
        },
        {
          id: "openai/gpt-4o",
          pricing: {
            prompt: 0.000005,
            completion: 0.000015,
            cache_read: 0.00000125,
            cache_write: 0,
          },
        },
      ],
    }));

    const target = await engine.fetchPricing("anthropic", "claude-opus-4-6");
    expect(target?.input_per_token).toBeCloseTo(0.000015, 10);
    expect(target?.cache_write_per_token).toBeCloseTo(0.00001875, 10);

    const gpt = engine.getCachedPricing("openai/gpt-4o");
    expect(gpt?.input_per_token).toBeCloseTo(0.000005, 10);
    expect(gpt?.cache_read_per_token).toBeCloseTo(0.00000125, 10);
    ledger.close();
  });

  it("returns undefined when the model isn't in the catalog", async () => {
    const ledger = Ledger.openInMemory();
    const engine = new PricingEngine(ledger, async () => ({
      data: [{ id: "openai/gpt-4o", pricing: { prompt: "0.0001", completion: "0.0002" } }],
    }));
    expect(await engine.fetchPricing("anthropic", "claude-opus-4-6")).toBeUndefined();
    ledger.close();
  });
});

describe("Ledger.recalculateCosts", () => {
  it("rewrites pricing_catalog rows for the matching model", () => {
    const { engine, ledger } = engineWith(anthropicPricing());
    ledger.insert({
      ts: "2026-05-18T03:00:00Z",
      character: "Alice",
      provider: "anthropic",
      model: "claude-opus-4-6",
      call_type: "message",
      input_tokens: 100,
      output_tokens: 50,
      cache_read_tokens: 0,
      cache_write_tokens: 0,
      cache_ttl: "5m",
      total_ms: 100,
      ttft_ms: 0,
      finish_reason: "end_turn",
      thinking_enabled: false,
      cost_source: "pricing_catalog",
    });
    const result = ledger.recalculateCosts("anthropic/claude-opus-4.6", engine);
    expect(result.updated).toBe(1);
    expect(result.total).toBe(1);
    const row = ledger.recent(1)[0]!;
    expect(row.total_cost).toBeCloseTo(0.0015 + 0.00375, 10);
    ledger.close();
  });

  it("skips rows priced via provider_reported", () => {
    const { engine, ledger } = engineWith(anthropicPricing());
    ledger.insert({
      ts: "2026-05-18T03:00:00Z",
      character: "Alice",
      provider: "anthropic",
      model: "claude-opus-4-6",
      call_type: "message",
      input_tokens: 100,
      output_tokens: 50,
      cache_read_tokens: 0,
      cache_write_tokens: 0,
      cache_ttl: "5m",
      total_ms: 100,
      ttft_ms: 0,
      finish_reason: "end_turn",
      thinking_enabled: false,
      cost_source: "provider_reported",
      total_cost: 0.0042,
    });
    const result = ledger.recalculateCosts("anthropic/claude-opus-4.6", engine);
    expect(result.updated).toBe(0);
    const row = ledger.recent(1)[0]!;
    expect(row.total_cost).toBeCloseTo(0.0042, 10);
    ledger.close();
  });
});
