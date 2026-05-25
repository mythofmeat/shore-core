/**
 * OpenAI-compatible adapter.
 *
 * Used for OpenAI itself + every OpenAI-compatible gateway (DeepSeek,
 * xAI, NanoGPT, etc.). Phase 4a only needs the OpenAI target for the
 * `openai/gpt-5.4-mini` test; the deepseek/xai/etc. variants slot in
 * later without changes to the adapter (they're all base-URL swaps).
 *
 * We translate Anthropic-style content blocks ↔ OpenAI messages here.
 * For phase 4a we don't try to make caching work on this side — OpenAI
 * does its own automatic prefix caching server-side, no client knobs.
 * The cache regression we're killing is Anthropic-specific.
 */

import OpenAI from "openai";
import type {
  ChatCompletionAssistantMessageParam,
  ChatCompletionChunk,
  ChatCompletionMessageParam,
  ChatCompletionTool,
  ChatCompletionToolMessageParam,
} from "openai/resources/chat/completions";

import type { ContentBlock, ImageRef } from "../../engine/types.ts";
import { resolveImage } from "../images.ts";
import type {
  ChatEvent,
  ChatRequest,
  GenerateResult,
  ProviderClient,
  ToolDef,
  TurnMessage,
  UsageStats,
} from "../types.ts";

export class OpenAIProvider implements ProviderClient {
  async *stream(req: ChatRequest): AsyncIterable<ChatEvent> {
    const { client, params } = buildOpenAICall(req, /*streaming*/ true);
    const stream = (await client.chat.completions.create(
      params,
      req.signal ? { signal: req.signal } : undefined,
    )) as AsyncIterable<ChatCompletionChunk>;

    // Per-tool-call accumulators, keyed by tool_call index (the SDK
    // streams a parallel-call array — chunks carry the index slot).
    const toolCalls = new Map<
      number,
      { id: string; name: string; argsJson: string; announced: boolean }
    >();
    let textAccum = "";
    let stopReason = "end_turn";
    const usage: UsageStats = {
      inputTokens: 0,
      outputTokens: 0,
      cacheReadInputTokens: 0,
      cacheCreationInputTokens: 0,
    };

    for await (const chunk of stream) {
      const choice = chunk.choices[0];
      if (choice) {
        const delta = choice.delta;
        if (typeof delta.content === "string" && delta.content.length > 0) {
          textAccum += delta.content;
          yield { kind: "text_delta", text: delta.content };
        }
        if (delta.tool_calls) {
          for (const tc of delta.tool_calls) {
            const idx = tc.index;
            let state = toolCalls.get(idx);
            if (!state) {
              state = {
                id: tc.id ?? `tc_${idx}`,
                name: tc.function?.name ?? "",
                argsJson: "",
                announced: false,
              };
              toolCalls.set(idx, state);
            }
            if (tc.id && tc.id !== state.id) state.id = tc.id;
            if (tc.function?.name) state.name = tc.function.name;
            if (!state.announced && state.name && state.id) {
              state.announced = true;
              yield { kind: "tool_use_start", id: state.id, name: state.name };
            }
            const argDelta = tc.function?.arguments;
            if (argDelta) {
              state.argsJson += argDelta;
              if (state.announced) {
                yield {
                  kind: "tool_use_input_delta",
                  id: state.id,
                  partial_json: argDelta,
                };
              }
            }
          }
        }
        if (choice.finish_reason) {
          stopReason = mapStopReason(choice.finish_reason);
        }
      }
      if (chunk.usage) {
        usage.inputTokens = chunk.usage.prompt_tokens ?? 0;
        usage.outputTokens = chunk.usage.completion_tokens ?? 0;
        const cached = chunk.usage.prompt_tokens_details?.cached_tokens ?? 0;
        usage.cacheReadInputTokens = cached;
      }
    }

    for (const tc of toolCalls.values()) {
      if (tc.announced) yield { kind: "tool_use_done", id: tc.id };
    }

    const content: ContentBlock[] = [];
    if (textAccum) content.push({ type: "text", text: textAccum });
    for (const tc of toolCalls.values()) {
      let input: unknown;
      try {
        input = tc.argsJson.trim() === "" ? {} : JSON.parse(tc.argsJson);
      } catch {
        input = {};
      }
      content.push({ type: "tool_use", id: tc.id, name: tc.name, input });
    }

    yield { kind: "done", content, stopReason, usage };
  }

  async generate(req: ChatRequest): Promise<GenerateResult> {
    const { client, params } = buildOpenAICall(req, /*streaming*/ false);
    const completion = await client.chat.completions.create(
      params,
      req.signal ? { signal: req.signal } : undefined,
    );
    // `params.stream` is false; the SDK returns a single ChatCompletion
    // (the streaming branch returns AsyncIterable). The SDK's discriminated
    // union doesn't narrow on a runtime boolean, so the cast is safe but
    // necessary.
    const c = completion as Awaited<
      ReturnType<typeof client.chat.completions.create>
    > & { choices: Array<{ message: { content?: string; tool_calls?: unknown[] }; finish_reason: string | null }>; usage?: { prompt_tokens?: number; completion_tokens?: number; prompt_tokens_details?: { cached_tokens?: number } } };

    const choice = c.choices[0];
    const content: ContentBlock[] = [];
    if (choice?.message.content) {
      content.push({ type: "text", text: choice.message.content });
    }
    if (Array.isArray(choice?.message.tool_calls)) {
      for (const tc of choice.message.tool_calls) {
        const tool = tc as { id?: string; function?: { name?: string; arguments?: string } };
        let input: unknown;
        try {
          const raw = tool.function?.arguments ?? "";
          input = raw.trim() === "" ? {} : JSON.parse(raw);
        } catch {
          input = {};
        }
        content.push({
          type: "tool_use",
          id: tool.id ?? "tc_0",
          name: tool.function?.name ?? "",
          input,
        });
      }
    }

    const usage: UsageStats = {
      inputTokens: c.usage?.prompt_tokens ?? 0,
      outputTokens: c.usage?.completion_tokens ?? 0,
      cacheReadInputTokens: c.usage?.prompt_tokens_details?.cached_tokens ?? 0,
      cacheCreationInputTokens: 0,
    };

    return {
      content,
      stopReason: choice?.finish_reason
        ? mapStopReason(choice.finish_reason)
        : "end_turn",
      usage,
    };
  }
}

/**
 * Shared per-call setup for streaming + non-streaming. Returns the
 * configured client and the request params; the caller picks the
 * SDK method and consumes the response shape.
 */
function buildOpenAICall(
  req: ChatRequest,
  streaming: boolean,
): {
  client: OpenAI;
  params: Parameters<typeof OpenAI.prototype.chat.completions.create>[0];
} {
  const client = new OpenAI({
    apiKey: req.apiKey,
    ...(req.baseUrl ? { baseURL: req.baseUrl } : {}),
  });

  const messages: ChatCompletionMessageParam[] = [];
  const systemText = systemToText(req.system);
  if (systemText) {
    messages.push({ role: "system", content: systemText });
  }
  for (const turn of req.messages) {
    messages.push(...turnToOpenAI(turn));
  }

  const tools: ChatCompletionTool[] = req.tools.map((t) => ({
    type: "function",
    function: {
      name: t.name,
      description: t.description,
      parameters: t.inputSchema as Record<string, unknown>,
    },
  }));

  const params: Parameters<typeof client.chat.completions.create>[0] = {
    model: req.modelId,
    messages,
    max_tokens: req.maxTokens,
    ...(streaming
      ? { stream: true, stream_options: { include_usage: true } }
      : {}),
  };
  if (tools.length > 0) params.tools = tools;
  if (req.temperature !== undefined) params.temperature = req.temperature;
  if (req.topP !== undefined) params.top_p = req.topP;
  if (req.thinking.enabled && req.thinking.effort) {
    const effort = mapReasoningEffort(req.thinking.effort);
    if (effort) {
      (params as { reasoning_effort?: typeof effort }).reasoning_effort = effort;
    }
  }

  return { client, params };
}

// ── conversion ──────────────────────────────────────────────────────────

function systemToText(system: ChatRequest["system"]): string {
  if (typeof system === "string") return system;
  return system.map((b) => b.text).join("");
}

function turnToOpenAI(turn: TurnMessage): ChatCompletionMessageParam[] {
  // OpenAI splits a tool_result-containing user turn into one `tool`
  // role message per result, and assistant tool_use becomes `tool_calls`
  // on the assistant message. Plain text falls through.
  if (turn.role === "system") {
    // OpenAI accepts mid-history `role:"system"` natively — pass through
    // verbatim. (The Rust impl defensively wrapped these in
    // <system_instruction> in case OpenRouter routed to a non-OpenAI
    // backend that rejects raw system messages. We don't: SDK selection
    // by upstream is a config concern, not an adapter responsibility.
    // Catalog auto-routes `anthropic/*` on OpenRouter to the Anthropic
    // SDK so this adapter only fronts genuine OpenAI-compatible endpoints.)
    const text = turn.content
      .filter((b): b is Extract<ContentBlock, { type: "text" }> => b.type === "text")
      .map((b) => b.text)
      .join("");
    return [{ role: "system", content: text }];
  }

  if (turn.role === "assistant") {
    const text = turn.content
      .filter((b) => b.type === "text")
      .map((b) => (b as { text: string }).text)
      .join("");
    const toolUses = turn.content.filter(
      (b): b is Extract<ContentBlock, { type: "tool_use" }> => b.type === "tool_use",
    );
    const msg: ChatCompletionAssistantMessageParam = { role: "assistant" };
    if (text) msg.content = text;
    if (toolUses.length > 0) {
      msg.tool_calls = toolUses.map((tu) => ({
        id: tu.id,
        type: "function",
        function: {
          name: tu.name,
          arguments: JSON.stringify(tu.input ?? {}),
        },
      }));
    }
    return [msg];
  }

  // User turn: split tool_results into role:tool messages; text/etc.
  // ride on a single user message. Images become image_url parts in the
  // user message's multipart content array, prepended before text.
  const out: ChatCompletionMessageParam[] = [];
  const textParts: Array<{ type: "text"; text: string }> = [];
  for (const b of turn.content) {
    if (b.type === "tool_result") {
      const toolMsg: ChatCompletionToolMessageParam = {
        role: "tool",
        tool_call_id: b.tool_use_id,
        content: b.content,
      };
      out.push(toolMsg);
    } else if (b.type === "text") {
      textParts.push({ type: "text", text: b.text });
    }
  }
  const imageParts = imagesToOpenAIParts(turn.images);
  if (imageParts.length > 0 || textParts.length > 0) {
    const parts: Array<
      { type: "text"; text: string } | { type: "image_url"; image_url: { url: string } }
    > = [...imageParts, ...textParts];
    out.push({ role: "user", content: parts });
  }
  return out;
}

function imagesToOpenAIParts(
  images: ImageRef[] | undefined,
): Array<{ type: "image_url"; image_url: { url: string } }> {
  if (!images || images.length === 0) return [];
  const out: Array<{ type: "image_url"; image_url: { url: string } }> = [];
  for (const img of images) {
    const resolved = resolveImage(img);
    if (!resolved) continue;
    out.push({
      type: "image_url",
      image_url: { url: `data:${resolved.mediaType};base64,${resolved.base64}` },
    });
  }
  return out;
}

function mapReasoningEffort(
  effort: string,
): "low" | "medium" | "high" | "minimal" | undefined {
  switch (effort) {
    case "low":
      return "low";
    case "medium":
      return "medium";
    case "high":
    case "xhigh":
    case "max":
      return "high";
    default:
      return undefined;
  }
}

function mapStopReason(finish: string): string {
  switch (finish) {
    case "stop":
      return "end_turn";
    case "tool_calls":
      return "tool_use";
    case "length":
      return "max_tokens";
    default:
      return finish;
  }
}
