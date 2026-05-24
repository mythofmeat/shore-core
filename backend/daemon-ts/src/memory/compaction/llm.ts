/**
 * Production `CompactionLlm` — drives a single provider call for compaction.
 *
 * Port of `backend/daemon/src/memory/compaction_impls.rs::RealCompactionLlm`,
 * adapted for the TS provider boundary.
 *
 * The Rust path uses `LedgerClient` (records call type, handles credential
 * fallback) and a sophisticated cached-prefix replay built on
 * `LlmRequest` + `convert_inline_system_messages`. The TS rewrite hasn't
 * landed the ledger yet (Phase 7) and has no autonomy manager tracking the
 * most-recent chat request to feed in here, so this adapter currently only
 * implements the fresh-prefix path: build a `ChatRequest` from
 * `(system, messages)`, stream it through the appropriate provider, and
 * collect the assistant turn's text.
 *
 * The cached-prefix optimization (preserving Anthropic's chat-cache hash
 * across a compaction call) is a deliberate omission flagged in
 * REWRITE.md — it requires the autonomy manager + an LlmRequest mirror,
 * neither of which exist in TS-land yet.
 */

import { AnthropicProvider } from "../../llm/providers/anthropic.ts";
import { OpenAIProvider } from "../../llm/providers/openai.ts";
import type { ResolvedModel } from "../../llm/catalog.ts";
import type {
  ChatEvent,
  ChatRequest,
  ProviderClient,
  TurnMessage,
} from "../../llm/types.ts";

import { CompactionError, type CompactionLlm } from "./types.ts";

export interface RealCompactionLlmOptions {
  resolved: ResolvedModel;
  apiKey: string;
  /** Optional override; defaults to the resolved model's base URL. */
  baseUrl?: string;
  /** Cache TTL applied to the compaction call — defaults to "1h" for Anthropic. */
  cacheTtl?: string;
}

export class RealCompactionLlm implements CompactionLlm {
  constructor(private readonly opts: RealCompactionLlmOptions) {}

  async summarize(
    system: string,
    messages: Array<{ role: "user" | "assistant"; content: string }>,
    cachedRequest: ChatRequest | undefined,
  ): Promise<string> {
    if (cachedRequest !== undefined) {
      // See module docstring + REWRITE.md: cached-prefix path is not
      // wired up in TS-land yet. Falling back to the fresh path is
      // semantically correct (just suboptimal for cache); callers
      // expecting the cache-preserving wire shape need to wait until
      // the autonomy manager lands.
    }

    const provider = buildProvider(this.opts.resolved.sdk);
    const turns = messages.map<TurnMessage>((m) => ({
      role: m.role,
      content: [{ type: "text", text: m.content }],
    }));

    const req: ChatRequest = {
      system,
      messages: turns,
      tools: [],
      thinking: { enabled: false },
      cacheTtl: this.opts.cacheTtl ?? "1h",
      modelId: this.opts.resolved.modelId,
      apiKey: this.opts.apiKey,
      maxTokens: this.opts.resolved.maxTokens ?? 4096,
      ...(this.opts.baseUrl !== undefined
        ? { baseUrl: this.opts.baseUrl }
        : this.opts.resolved.baseUrl !== undefined
          ? { baseUrl: this.opts.resolved.baseUrl }
          : {}),
      ...(this.opts.resolved.temperature !== undefined
        ? { temperature: this.opts.resolved.temperature }
        : {}),
    };

    let text = "";
    try {
      for await (const ev of provider.stream(req)) {
        if (ev.kind === "done") {
          for (const block of ev.content) {
            if (block.type === "text") text += block.text;
          }
        }
      }
    } catch (e) {
      throw new CompactionError("llm", (e as Error).message);
    }
    return text;
  }
}

function buildProvider(sdk: ResolvedModel["sdk"]): ProviderClient {
  switch (sdk) {
    case "anthropic":
      return new AnthropicProvider();
    case "openai":
      return new OpenAIProvider();
    case "gemini":
    case "zai":
      throw new CompactionError(
        "llm",
        `provider SDK "${sdk}" is not implemented in shore-daemon-ts yet`,
      );
  }
}

// Pin so unused-import lint doesn't strip ChatEvent — it's part of the
// provider stream's typed iterable surface.
type _PinChatEvent = ChatEvent;
