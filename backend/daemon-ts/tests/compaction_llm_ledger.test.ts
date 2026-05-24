import { describe, expect, it } from "bun:test";

import type { ResolvedModel } from "../src/llm/catalog.ts";
import type { ChatEvent, ChatRequest, ProviderClient } from "../src/llm/types.ts";
import { Ledger } from "../src/ledger/ledger.ts";
import { RealCompactionLlm } from "../src/memory/compaction/llm.ts";

class OneShotProvider implements ProviderClient {
  readonly requests: ChatRequest[] = [];

  async *stream(req: ChatRequest): AsyncIterable<ChatEvent> {
    this.requests.push(req);
    yield { kind: "text_delta", text: "<memory><write path=\"MEMORY.md\">ok" };
    yield {
      kind: "done",
      content: [{ type: "text", text: "<memory><write path=\"MEMORY.md\">ok</write></memory>" }],
      stopReason: "end_turn",
      usage: {
        inputTokens: 200,
        outputTokens: 40,
        cacheReadInputTokens: 150,
        cacheCreationInputTokens: 20,
      },
    };
  }
}

function resolvedModel(): ResolvedModel {
  return {
    name: "haiku",
    qualifiedName: "chat.anthropic.haiku",
    category: "chat",
    providerKey: "anthropic",
    sdk: "anthropic",
    modelId: "claude-haiku-test",
    apiKeyEnv: undefined,
    baseUrl: undefined,
    maxTokens: 4096,
    maxContextTokens: undefined,
    temperature: undefined,
    topP: undefined,
    reasoningEffort: undefined,
    budgetTokens: undefined,
    cacheTtl: "1h",
    openrouterProvider: undefined,
  };
}

describe("RealCompactionLlm ledger recording", () => {
  it("records compaction provider calls", async () => {
    const ledger = Ledger.openInMemory();
    const provider = new OneShotProvider();
    const llm = new RealCompactionLlm({
      resolved: resolvedModel(),
      apiKey: "test-key",
      provider,
      ledger,
      character: "aria",
      cacheTtl: "1h",
    });

    const text = await llm.summarize(
      "system",
      [{ role: "user", content: "compact this" }],
      undefined,
    );

    expect(text).toContain("MEMORY.md");
    expect(provider.requests).toHaveLength(1);
    const rows = ledger.recent(1);
    expect(rows[0]?.call_type).toBe("compaction");
    expect(rows[0]?.character).toBe("aria");
    expect(rows[0]?.cache_read_tokens).toBe(150);
    expect(rows[0]?.cache_ttl).toBe("1h");
    ledger.close();
  });
});
