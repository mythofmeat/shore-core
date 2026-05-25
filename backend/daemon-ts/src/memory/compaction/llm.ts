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
import type { CacheForensics } from "../../ledger/cache_forensics.ts";
import type { Ledger } from "../../ledger/ledger.ts";
import type {
  ChatEvent,
  ChatRequest,
  ProviderClient,
  TurnMessage,
  UsageStats,
} from "../../llm/types.ts";

import { CompactionError, type CompactionLlm } from "./types.ts";

export interface RealCompactionLlmOptions {
  resolved: ResolvedModel;
  apiKey: string;
  /** Optional override; defaults to the resolved model's base URL. */
  baseUrl?: string;
  /** Cache TTL applied to the compaction call — defaults to "1h" for Anthropic. */
  cacheTtl?: string;
  /** Optional test/diagnostic provider override. */
  provider?: ProviderClient;
  /** Optional ledger sinks for Phase 7 accounting. */
  ledger?: Ledger;
  cacheForensics?: CacheForensics;
  character?: string;
}

export class RealCompactionLlm implements CompactionLlm {
  constructor(private readonly opts: RealCompactionLlmOptions) {}

  async summarize(
    system: string,
    messages: Array<{ role: "user" | "assistant"; content: string }>,
    cachedRequest: ChatRequest | undefined,
  ): Promise<string> {
    const provider = this.opts.provider ?? buildProvider(this.opts.resolved.sdk);
    const req =
      cachedRequest !== undefined
        ? this.buildCachedPrefixRequest(cachedRequest, messages, system)
        : this.buildFreshRequest(system, messages);

    const started = Date.now();
    let result;
    try {
      // Compaction is always non-streaming: no client is watching token-
      // by-token output, and the non-streaming wire shape (no `stream:
      // true` in the body) is what Rust sends, so it's also what the
      // Anthropic prompt cache will see when comparing against a future
      // compaction's prefix.
      result = await provider.generate(req);
    } catch (e) {
      throw new CompactionError("llm", (e as Error).message);
    }
    const totalMs = Date.now() - started;

    let text = "";
    for (const block of result.content) {
      if (block.type === "text") text += block.text;
    }

    this.recordLedger({
      usage: result.usage,
      stopReason: result.stopReason,
      totalMs,
      // Non-streaming: no first-token observation point. Report
      // ttft = totalMs so the metric exists and downstream code that
      // reads ttftMs doesn't get garbage.
      ttftMs: totalMs,
    });
    return text;
  }

  /**
   * Cache-preserving compaction request: reuse the chat call's cached
   * prefix (system + tools + history bytes) so Anthropic's prompt
   * cache hits. Appends the compaction prompt as a single user turn,
   * then a trailing `role:"system"` message that the Anthropic adapter
   * wraps as `<system_instruction>` and folds into the preceding user
   * turn (see `convertInlineSystemMessages`). Mirrors Rust
   * `build_compaction_request` cached branch at
   * `backend/daemon/src/memory/compaction_impls.rs:217-263`.
   */
  private buildCachedPrefixRequest(
    cached: ChatRequest,
    messages: Array<{ role: "user" | "assistant"; content: string }>,
    compactionSystem: string,
  ): ChatRequest {
    // `manager.ts` always passes a single user message in the cached
    // path (the compaction prompt rendered via `buildFinalMessage`).
    // Mirrors Rust's COMPACTION_TAIL_USER_PROMPT_COUNT == 1 invariant;
    // see compaction_impls.rs `append_compaction_tail`.
    if (messages.length !== 1 || messages[0]!.role !== "user") {
      throw new CompactionError(
        "llm",
        `cached-prefix compaction expects exactly 1 trailing user message, got ${messages.length}`,
      );
    }
    const compactionUser: TurnMessage = {
      role: "user",
      content: [{ type: "text", text: messages[0]!.content }],
    };
    const compactionSystemTurn: TurnMessage = {
      role: "system",
      content: [{ type: "text", text: compactionSystem }],
    };

    // Drop signal/forensicRid from the cached request — this is a
    // separate LLM call with its own observability needs; the chat
    // call's AbortController must not cancel a background compaction.
    const { signal: _sig, forensicRid: _rid, ...inherited } = cached;
    void _sig;
    void _rid;
    return {
      ...inherited,
      messages: [...cached.messages, compactionUser, compactionSystemTurn],
      // Override sampling for the compaction model; everything else
      // (system, tools, modelId, apiKey, baseUrl, cacheTtl) stays from
      // the cached request so the prefix bytes match.
      maxTokens: this.opts.resolved.maxTokens ?? cached.maxTokens,
      ...(this.opts.resolved.temperature !== undefined
        ? { temperature: this.opts.resolved.temperature }
        : {}),
    };
  }

  /**
   * Fresh-prefix compaction request — used when there's no cached chat
   * request to inherit. The compacted slice is sent as the messages
   * array; the compaction system rides as top-level `system` (matches
   * the existing pre-2026-05-25 wire shape).
   */
  private buildFreshRequest(
    system: string,
    messages: Array<{ role: "user" | "assistant"; content: string }>,
  ): ChatRequest {
    const turns: TurnMessage[] = messages.map((m) => ({
      role: m.role,
      content: [{ type: "text", text: m.content }],
    }));
    return {
      system,
      messages: turns,
      tools: [],
      thinking: { enabled: false },
      cacheTtl: this.opts.cacheTtl ?? this.opts.resolved.cacheTtl ?? "",
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
  }

  private recordLedger(call: {
    usage: UsageStats;
    stopReason: string;
    totalMs: number;
    ttftMs: number;
  }): void {
    if (this.opts.ledger === undefined || this.opts.character === undefined) return;
    try {
      this.opts.ledger.recordCall(
        {
          provider: this.opts.resolved.providerKey,
          model: this.opts.resolved.modelId,
          callType: "compaction",
          character: this.opts.character,
          inputTokens: call.usage.inputTokens,
          outputTokens: call.usage.outputTokens,
          cacheReadTokens: call.usage.cacheReadInputTokens,
          cacheWriteTokens: call.usage.cacheCreationInputTokens,
          totalMs: call.totalMs,
          ttftMs: call.ttftMs,
          finishReason: call.stopReason,
          thinkingEnabled: false,
          ...(this.opts.cacheTtl !== undefined ? { cacheTtl: this.opts.cacheTtl } : {}),
        },
        this.opts.cacheForensics,
      );
    } catch (e) {
      console.error(`[compaction] ledger record failed: ${(e as Error).message}`);
    }
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
