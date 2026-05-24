import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { ConversationEngine } from "../src/engine/engine.ts";
import type { Message } from "../src/engine/types.ts";
import type { ResolvedModel } from "../src/llm/catalog.ts";
import { generateResponse } from "../src/llm/generate.ts";
import type { ChatEvent, ChatRequest, ProviderClient } from "../src/llm/types.ts";
import { Ledger } from "../src/ledger/ledger.ts";
import { ToolRegistry } from "../src/tools/registry.ts";

class QueuedProvider implements ProviderClient {
  readonly requests: ChatRequest[] = [];

  constructor(private readonly responses: ChatEvent[][]) {}

  async *stream(req: ChatRequest): AsyncIterable<ChatEvent> {
    this.requests.push(req);
    const next = this.responses.shift();
    if (next === undefined) throw new Error("no queued response");
    for (const event of next) yield event;
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

async function setup(): Promise<{
  root: string;
  engine: ConversationEngine;
  characterConfigDir: string;
}> {
  const root = mkdtempSync(path.join(tmpdir(), "shore-generate-ledger-test-"));
  const characterConfigDir = path.join(root, "config", "characters", "aria");
  fs.mkdirSync(path.join(characterConfigDir, "workspace"), { recursive: true });
  const engine = new ConversationEngine("aria", path.join(root, "data", "aria"));
  const user: Message = {
    msg_id: "m_user",
    role: "user",
    content: "hello",
    images: [],
    content_blocks: [{ type: "text", text: "hello" }],
    timestamp: "2026-04-05T12:00:00Z",
  };
  await engine.appendMessage(user);
  return { root, engine, characterConfigDir };
}

describe("generateResponse ledger recording", () => {
  it("records message and tool-loop provider calls", async () => {
    const { root, engine, characterConfigDir } = await setup();
    const ledger = Ledger.openInMemory();
    const provider = new QueuedProvider([
      [
        {
          kind: "done",
          content: [{ type: "tool_use", id: "toolu_1", name: "lookup", input: {} }],
          stopReason: "tool_use",
          usage: {
            inputTokens: 100,
            outputTokens: 10,
            cacheReadInputTokens: 0,
            cacheCreationInputTokens: 50,
          },
        },
      ],
      [
        { kind: "text_delta", text: "done" },
        {
          kind: "done",
          content: [{ type: "text", text: "done" }],
          stopReason: "end_turn",
          usage: {
            inputTokens: 120,
            outputTokens: 12,
            cacheReadInputTokens: 50,
            cacheCreationInputTokens: 10,
          },
        },
      ],
    ]);

    const registry = new ToolRegistry();
    registry.register({
      name: "lookup",
      description: "lookup",
      inputSchema: { type: "object" },
      execute: async () => "tool result",
    });

    const frames: unknown[] = [];
    const result = await generateResponse({
      engine,
      characterConfigDir,
      configDir: path.join(root, "config"),
      displayName: "Ren",
      resolved: resolvedModel(),
      registry,
      broadcast: (frame) => frames.push(frame),
      provider,
      ledger,
    });

    expect(result.finalText).toBe("done");
    expect(result.turnCount).toBe(2);
    expect(provider.requests).toHaveLength(2);

    const rows = ledger.recent(2);
    expect(rows[0]?.call_type).toBe("tool_loop");
    expect(rows[0]?.finish_reason).toBe("end_turn");
    expect(rows[0]?.cache_read_tokens).toBe(50);
    expect(rows[1]?.call_type).toBe("message");
    expect(rows[1]?.finish_reason).toBe("tool_use");
    expect(rows[1]?.cache_ttl).toBe("1h");
    expect(rows[1]?.character).toBe("aria");

    const streamEnds = frames.filter(
      (frame): frame is { type: string } =>
        typeof frame === "object" &&
        frame !== null &&
        (frame as { type?: unknown }).type === "stream_end",
    );
    expect(streamEnds).toHaveLength(2);
    ledger.close();
  });
});
