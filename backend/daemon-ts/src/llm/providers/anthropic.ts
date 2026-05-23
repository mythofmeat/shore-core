/**
 * Anthropic SDK adapter.
 *
 * This is the load-bearing piece for the cache regression we set out
 * to kill in this rewrite. The Rust impl had its own SSE parser,
 * its own thinking-signature replay, and its own (complex)
 * cache_control placement logic — and accumulated bugs in each.
 * Here we:
 *
 *   1. Delegate streaming and `thinking`/`tool_use` block consolidation
 *      to the SDK (`client.messages.stream(...)`).
 *   2. Round-trip the SDK's `content` array verbatim across turns. We
 *      never inspect `signature` on thinking blocks or `id` on tool_use
 *      blocks — we just hand the array back unchanged when echoing the
 *      assistant turn.
 *   3. Place exactly 4 cache_control breakpoints on a fixed schedule:
 *        a. Last system block  (cache the system + everything before it)
 *        b. Last tool          (cache the tool definitions)
 *        c. Last "stable" turn (last assistant message before the
 *           current pending turn) — caches the entire frozen history
 *        d. Last message in the current turn (advances as the tool
 *           loop iterates, so each tool_result hop hits and extends
 *           the cache)
 *
 * Why this exact schedule:
 *
 * Anthropic allows up to 4 cache_control breakpoints per request. The
 * cache hash is the message-block prefix up to (and including) the
 * breakpoint. The pathology in the Rust daemon was that the "last
 * message" breakpoint walked over tool_use → tool_result boundaries
 * in ways that invalidated the trailing breakpoint (because the
 * thinking block that introduced the tool_use stayed in the prefix,
 * but its hash neighbors changed shape). The SDK preserves the
 * tool-loop block ordering for us; we only need to anchor the
 * breakpoints in the right slots and the cache reads come back green.
 *
 * cache_control itself does NOT count toward the prefix hash (it's
 * metadata, not content) — see `[[feedback-cache-control-prefix]]`
 * memory. So advancing the last-message breakpoint across iterations
 * doesn't bust the cache for the earlier breakpoints; it just adds new
 * cacheable prefixes layered on top.
 */

import Anthropic from "@anthropic-ai/sdk";
import type {
  ContentBlockParam,
  MessageParam,
  RawMessageStreamEvent,
  TextBlockParam,
  ThinkingConfigParam,
  Tool,
  ToolResultBlockParam,
} from "@anthropic-ai/sdk/resources/messages";

import type { ContentBlock, ImageRef } from "../../engine/types.ts";
import { resolveImage } from "../images.ts";
import type {
  ChatEvent,
  ChatRequest,
  ProviderClient,
  ToolDef,
  TurnMessage,
  UsageStats,
} from "../types.ts";

export class AnthropicProvider implements ProviderClient {
  async *stream(req: ChatRequest): AsyncIterable<ChatEvent> {
    const client = new Anthropic({
      apiKey: req.apiKey,
      ...(req.baseUrl ? { baseURL: stripTrailingV1(req.baseUrl) } : {}),
    });

    const cacheControl = req.cacheTtl
      ? makeCacheControl(req.cacheTtl)
      : undefined;
    const system = buildSystem(req.system, cacheControl);
    const tools = buildTools(req.tools, cacheControl);
    // Anthropic rejects role:"system" in the messages array — wrap any
    // mid-history system turns into <system_instruction> blocks first.
    const messages = buildMessages(
      convertInlineSystemMessages(req.messages),
      cacheControl,
    );
    const thinking = buildThinking(req.thinking, req.maxTokens);

    const params: Parameters<typeof client.messages.stream>[0] = {
      model: req.modelId,
      max_tokens: req.maxTokens,
      messages,
      ...(system.length > 0 ? { system } : {}),
      ...(tools.length > 0 ? { tools } : {}),
      ...(thinking ? { thinking } : {}),
    };

    // OpenRouter-specific: pin provider routing to Anthropic so the
    // request actually hits Anthropic's API (which honors cache_control
    // on system + tools + messages). Without this, OpenRouter may route
    // to Bedrock or Vertex, where `cache_control` on system blocks is
    // silently ignored — manifesting as turn-0 cache_creation=0 despite
    // breakpoints being present. See OpenRouter docs:
    // https://openrouter.ai/docs/features/prompt-caching (Anthropic
    // section: "Automatic caching is only supported when requests are
    // routed to the Anthropic provider directly").
    if (req.baseUrl && req.baseUrl.includes("openrouter.ai")) {
      (params as { provider?: unknown }).provider = {
        order: ["anthropic"],
        allow_fallbacks: false,
      };
    }

    // Anthropic rejects temperature/top_p when thinking is on (must use
    // defaults). Only forward sampling knobs when thinking is disabled.
    if (!thinking) {
      if (req.temperature !== undefined) params.temperature = req.temperature;
      if (req.topP !== undefined) params.top_p = req.topP;
    }

    const stream = client.messages.stream(
      params,
      req.signal ? { signal: req.signal } : undefined,
    );

    // Per-block accumulators — keyed by block index from the SSE stream.
    const accum = new Map<number, AccumState>();
    let stopReason = "end_turn";
    const usage: UsageStats = {
      inputTokens: 0,
      outputTokens: 0,
      cacheReadInputTokens: 0,
      cacheCreationInputTokens: 0,
    };

    for await (const event of stream as AsyncIterable<RawMessageStreamEvent>) {
      switch (event.type) {
        case "message_start": {
          const u = event.message.usage;
          usage.inputTokens = u.input_tokens ?? 0;
          usage.outputTokens = u.output_tokens ?? 0;
          usage.cacheReadInputTokens = u.cache_read_input_tokens ?? 0;
          usage.cacheCreationInputTokens = u.cache_creation_input_tokens ?? 0;
          break;
        }
        case "content_block_start": {
          const blk = event.content_block;
          const idx = event.index;
          if (blk.type === "text") {
            accum.set(idx, { kind: "text", text: blk.text ?? "" });
          } else if (blk.type === "thinking") {
            accum.set(idx, {
              kind: "thinking",
              text: blk.thinking ?? "",
              signature: blk.signature ?? "",
            });
          } else if (blk.type === "tool_use") {
            accum.set(idx, {
              kind: "tool_use",
              id: blk.id,
              name: blk.name,
              partialJson: "",
            });
            yield { kind: "tool_use_start", id: blk.id, name: blk.name };
          } else if (blk.type === "redacted_thinking") {
            // Keep every redacted_thinking block verbatim — including
            // OpenRouter's `openrouter.reasoning:`-prefixed ones, which
            // are how OpenRouter relays signed thinking content back.
            // The Rust impl filtered them; we don't, because filtering
            // strips reasoning data the next-turn cache prefix needs
            // (and the model may need on subsequent turns).
            accum.set(idx, { kind: "redacted_thinking", data: blk.data });
          }
          break;
        }
        case "content_block_delta": {
          const state = accum.get(event.index);
          if (!state) break;
          const d = event.delta;
          if (d.type === "text_delta" && state.kind === "text") {
            state.text += d.text;
            yield { kind: "text_delta", text: d.text };
          } else if (d.type === "thinking_delta" && state.kind === "thinking") {
            state.text += d.thinking;
            yield { kind: "thinking_delta", text: d.thinking };
          } else if (d.type === "signature_delta" && state.kind === "thinking") {
            state.signature += d.signature;
          } else if (d.type === "input_json_delta" && state.kind === "tool_use") {
            state.partialJson += d.partial_json;
            yield {
              kind: "tool_use_input_delta",
              id: state.id,
              partial_json: d.partial_json,
            };
          }
          break;
        }
        case "content_block_stop": {
          const state = accum.get(event.index);
          if (state?.kind === "tool_use") {
            yield { kind: "tool_use_done", id: state.id };
          }
          break;
        }
        case "message_delta": {
          if (event.delta.stop_reason) stopReason = event.delta.stop_reason;
          const u = event.usage;
          if (u.output_tokens !== undefined && u.output_tokens !== null) {
            usage.outputTokens = u.output_tokens;
          }
          if (u.input_tokens !== undefined && u.input_tokens !== null) {
            usage.inputTokens = u.input_tokens;
          }
          if (u.cache_read_input_tokens !== undefined && u.cache_read_input_tokens !== null) {
            usage.cacheReadInputTokens = u.cache_read_input_tokens;
          }
          if (
            u.cache_creation_input_tokens !== undefined &&
            u.cache_creation_input_tokens !== null
          ) {
            usage.cacheCreationInputTokens = u.cache_creation_input_tokens;
          }
          break;
        }
        case "message_stop":
          // Nothing to do — accumulation closes naturally.
          break;
      }
    }

    yield {
      kind: "done",
      content: accumToContentBlocks(accum),
      stopReason,
      usage,
    };
  }
}

/**
 * The Anthropic SDK always appends `/v1/messages` to the configured
 * `baseURL`. Shore users (and config docs) write OpenRouter's base as
 * `https://openrouter.ai/api/v1`, the same form OpenRouter uses for the
 * OpenAI-compatible endpoint — so we strip the trailing `/v1` before
 * handing it to the SDK to avoid `/v1/v1/messages`. The Rust impl made
 * the same adjustment in build_http_request().
 */
function stripTrailingV1(baseUrl: string): string {
  return baseUrl.replace(/\/v1\/?$/, "");
}

// ── conversion helpers ──────────────────────────────────────────────────

type AccumState =
  | { kind: "text"; text: string }
  | { kind: "thinking"; text: string; signature: string }
  | { kind: "redacted_thinking"; data: string }
  | { kind: "tool_use"; id: string; name: string; partialJson: string };

function accumToContentBlocks(accum: Map<number, AccumState>): ContentBlock[] {
  const indexed = [...accum.entries()].sort((a, b) => a[0] - b[0]);
  const out: ContentBlock[] = [];
  for (const [, s] of indexed) {
    if (s.kind === "text") {
      out.push({ type: "text", text: s.text });
    } else if (s.kind === "thinking") {
      out.push({ type: "thinking", thinking: s.text, signature: s.signature });
    } else if (s.kind === "redacted_thinking") {
      out.push({ type: "redacted_thinking", data: s.data });
    } else if (s.kind === "tool_use") {
      let input: unknown;
      try {
        input = s.partialJson.trim() === "" ? {} : JSON.parse(s.partialJson);
      } catch {
        input = {};
      }
      out.push({ type: "tool_use", id: s.id, name: s.name, input });
    }
  }
  return out;
}

type CacheControl = { type: "ephemeral" } | { type: "ephemeral"; ttl: "5m" | "1h" };

function makeCacheControl(ttl: string): CacheControl {
  if (ttl === "" || ttl === "5m") return { type: "ephemeral" };
  if (ttl === "1h") return { type: "ephemeral", ttl };
  // Unknown TTLs (e.g. user typo) fall back to default. We don't throw
  // because cache_ttl can come from third-party config and we'd rather
  // miss the cache than fail the call.
  return { type: "ephemeral" };
}

function buildSystem(
  system: string,
  cacheControl: CacheControl | undefined,
): TextBlockParam[] {
  if (!system) return [];
  const block: TextBlockParam = { type: "text", text: system };
  if (cacheControl) block.cache_control = cacheControl;
  return [block];
}

function buildTools(
  tools: ToolDef[],
  cacheControl: CacheControl | undefined,
): Tool[] {
  if (tools.length === 0) return [];
  return tools.map((t, i) => {
    const tool: Tool = {
      name: t.name,
      description: t.description,
      input_schema: t.inputSchema as Tool["input_schema"],
    };
    // Cache breakpoint on the final tool covers all tool definitions.
    if (cacheControl && i === tools.length - 1) tool.cache_control = cacheControl;
    return tool;
  });
}

/** Wrap text in the canonical inline-system sentinel. Single source of
 * truth for the tag spelling — matches Rust `stream_helpers.rs:209`. */
export function wrapInlineSystemInstruction(text: string): string {
  return `<system_instruction>${text}</system_instruction>`;
}

/** Post-conversion turn — system role has been wrapped away. */
type WireTurn = {
  role: "user" | "assistant";
  content: ContentBlock[];
  images?: ImageRef[];
};

/**
 * Convert `role:"system"` turns into wrapped `role:"user"` turns. Anthropic
 * rejects `role:"system"` in the `messages` array, so heartbeat recaps and
 * compaction prompts ride as `<system_instruction>` text blocks. If the
 * previous emitted turn is already a user message, append to it rather
 * than emitting consecutive user turns (the API rejects those too).
 *
 * Mirrors Rust `convert_inline_system_messages` (`providers/anthropic.rs:391`).
 */
export function convertInlineSystemMessages(turns: TurnMessage[]): WireTurn[] {
  const out: WireTurn[] = [];
  for (const turn of turns) {
    if (turn.role !== "system") {
      const w: WireTurn = { role: turn.role, content: turn.content.slice() };
      if (turn.images && turn.images.length > 0) w.images = turn.images.slice();
      out.push(w);
      continue;
    }
    const text = turn.content
      .filter((b): b is Extract<ContentBlock, { type: "text" }> => b.type === "text")
      .map((b) => b.text)
      .join("");
    const wrapped = wrapInlineSystemInstruction(text);

    const prev = out[out.length - 1];
    if (prev && prev.role === "user") {
      prev.content.push({ type: "text", text: wrapped });
      continue;
    }
    out.push({
      role: "user",
      content: [{ type: "text", text: wrapped }],
    });
  }
  return out;
}

function buildMessages(
  turns: WireTurn[],
  cacheControl: CacheControl | undefined,
): MessageParam[] {
  // Compute the "last stable" position (last assistant turn before the
  // pending tail) and the absolute last message position. The Anthropic
  // recipe is: cache the largest prefix that won't change between
  // iterations (the stable assistant turn), and cache the current
  // iteration too (the last tool_result for a tool loop, or the latest
  // user message for plain chat). Up to two breakpoints in messages,
  // combined with the two in system/tools, = the 4-breakpoint budget.
  const lastIdx = turns.length - 1;
  let stableIdx = -1;
  for (let i = lastIdx - 1; i >= 0; i--) {
    if (turns[i]!.role === "assistant") {
      stableIdx = i;
      break;
    }
  }

  return turns.map((turn, i) => {
    const imageBlocks = imagesToAnthropicBlocks(turn.images);
    const content: ContentBlockParam[] = [
      ...imageBlocks,
      ...turn.content.map((b) => toAnthropicBlockParam(b)),
    ];
    if (cacheControl && (i === stableIdx || i === lastIdx)) {
      applyMessageBreakpoint(content, cacheControl);
    }
    return { role: turn.role, content };
  });
}

function imagesToAnthropicBlocks(
  images: ImageRef[] | undefined,
): ContentBlockParam[] {
  if (!images || images.length === 0) return [];
  const out: ContentBlockParam[] = [];
  for (const img of images) {
    const resolved = resolveImage(img);
    if (!resolved) continue;
    out.push({
      type: "image",
      source: {
        type: "base64",
        media_type: resolved.mediaType as
          | "image/png"
          | "image/jpeg"
          | "image/webp"
          | "image/gif",
        data: resolved.base64,
      },
    });
  }
  return out;
}

function applyMessageBreakpoint(
  content: ContentBlockParam[],
  cacheControl: CacheControl,
): void {
  // Apply the breakpoint to the last block that supports it. tool_use,
  // text, and tool_result all support cache_control; thinking blocks do
  // NOT (Anthropic rejects cache_control on thinking).
  for (let i = content.length - 1; i >= 0; i--) {
    const b = content[i]! as ContentBlockParam & { cache_control?: unknown };
    if (b.type === "text" || b.type === "tool_use" || b.type === "tool_result") {
      b.cache_control = cacheControl;
      return;
    }
  }
}

function toAnthropicBlockParam(b: ContentBlock): ContentBlockParam {
  switch (b.type) {
    case "text":
      return { type: "text", text: b.text };
    case "thinking":
      return {
        type: "thinking",
        thinking: b.thinking,
        signature: b.signature ?? "",
      };
    case "redacted_thinking":
      return { type: "redacted_thinking", data: b.data };
    case "tool_use":
      return {
        type: "tool_use",
        id: b.id,
        name: b.name,
        input: (b.input ?? {}) as Record<string, unknown>,
      };
    case "tool_result": {
      const out: ToolResultBlockParam = {
        type: "tool_result",
        tool_use_id: b.tool_use_id,
        content: b.content,
      };
      if (b.is_error) out.is_error = true;
      return out;
    }
  }
}

function buildThinking(
  cfg: ChatRequest["thinking"],
  maxTokens: number,
): ThinkingConfigParam | undefined {
  if (!cfg.enabled) return undefined;
  if (cfg.effort === "adaptive") return { type: "adaptive" };

  // Map effort → budget per the Rust impl (see anthropic.rs:18). Fall
  // back to an explicit budget if provided. Budget must be ≥1024 and
  // strictly less than max_tokens.
  let budget = cfg.budgetTokens;
  if (budget === undefined) {
    const fraction = effortFraction(cfg.effort);
    if (fraction === undefined) return undefined;
    budget = Math.max(1024, Math.floor(maxTokens * fraction));
  }
  // Anthropic requires budget < max_tokens; clamp to maxTokens - 1.
  if (budget >= maxTokens) budget = Math.max(1024, maxTokens - 1);
  return { type: "enabled", budget_tokens: budget };
}

function effortFraction(effort: string | undefined): number | undefined {
  switch (effort) {
    case "low":
      return 0.25;
    case "medium":
      return 0.5;
    case "high":
      return 0.75;
    case "xhigh":
      return 0.9;
    case "max":
      return 0.95;
    default:
      return undefined;
  }
}
