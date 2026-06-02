/**
 * Z.ai adapter (sidecar contract shape).
 *
 * Z.ai speaks OpenAI chat completions for messages/tools, but has provider
 * specific base URLs and thinking controls. Keep it separate from the generic
 * OpenAI adapter so the Z.ai-only body fields and finish reasons stay explicit.
 *
 * The sidecar contract intentionally does not replay prior `thinking` blocks as
 * outbound `reasoning_content`; `zai_clear_thinking` only controls the documented
 * request flag.
 */

import OpenAI from "openai";
import type {
  ChatCompletionChunk,
  ChatCompletionCreateParams,
  ChatCompletionMessageParam,
  ChatCompletionTool,
} from "openai/resources/chat/completions";

import type { ContentBlock } from "../../engine/types.ts";
import type {
  GenerateResponse,
  SidecarProvider,
  SidecarRequest,
  StreamEvent,
  SystemContent,
  TurnMessage,
  Usage,
  WireMessage,
} from "../types.ts";
import { turnToOpenAI } from "./openai.ts";

export const ZAI_BASE_URL = "https://api.z.ai/api/paas/v4";
export const ZAI_CODING_BASE_URL = "https://api.z.ai/api/coding/paas/v4";

export type ZaiChatCompletionCreateParams = Omit<ChatCompletionCreateParams, "stream"> & {
  stream: boolean;
  thinking: { type: "enabled"; clear_thinking?: boolean };
};

export class ZaiProvider implements SidecarProvider {
  async *stream(req: SidecarRequest, signal?: AbortSignal): AsyncIterable<StreamEvent> {
    const { client, params } = buildZaiCall(req, /*streaming*/ true);
    const stream = (await client.chat.completions.create(
      params as ChatCompletionCreateParams,
      signal ? { signal } : undefined,
    )) as AsyncIterable<ChatCompletionChunk>;
    yield* zaiStreamEvents(req.model, stream);
  }

  async generate(req: SidecarRequest, signal?: AbortSignal): Promise<GenerateResponse> {
    const startedAt = Date.now();
    const { client, params } = buildZaiCall(req, /*streaming*/ false);
    const completion = await client.chat.completions.create(
      params as ChatCompletionCreateParams,
      signal ? { signal } : undefined,
    );
    return zaiGenerateResponse(req.model, completion, Date.now() - startedAt);
  }
}

export function resolveZaiBaseUrl(req: SidecarRequest): string {
  if (req.base_url) return trimTrailingSlash(req.base_url);
  return providerBool(req, "zai_subscription") ? ZAI_CODING_BASE_URL : ZAI_BASE_URL;
}

export function buildZaiCall(
  req: SidecarRequest,
  streaming: boolean,
): { client: OpenAI; params: ZaiChatCompletionCreateParams } {
  const client = new OpenAI({
    apiKey: req.api_key,
    baseURL: resolveZaiBaseUrl(req),
    maxRetries: 0,
  });
  return { client, params: buildZaiParams(req, streaming) };
}

export function buildZaiParams(
  req: SidecarRequest,
  streaming: boolean,
): ZaiChatCompletionCreateParams {
  // Z.ai's `thinking` object documents `clear_thinking` (default true) nested
  // here, NOT as a top-level field. It controls clearing prior-turn
  // `reasoning_content`; since the sidecar never replays reasoning_content into
  // history, it's inert for us — so we only send it when the operator
  // explicitly sets it, otherwise we let Z.ai apply its own default.
  const thinking: ZaiChatCompletionCreateParams["thinking"] = { type: "enabled" };
  const clearThinking = req.provider_options?.["zai_clear_thinking"];
  if (typeof clearThinking === "boolean") thinking.clear_thinking = clearThinking;

  const params: ZaiChatCompletionCreateParams = {
    model: req.model,
    messages: buildZaiMessages(req),
    max_tokens: req.max_tokens,
    stream: streaming,
    thinking,
  };

  if (streaming) params.stream_options = { include_usage: true };

  const tools = toZaiTools(req.tools);
  if (tools.length > 0) params.tools = tools;
  if (req.temperature !== undefined) params.temperature = req.temperature;
  if (req.top_p !== undefined) params.top_p = req.top_p;

  return params;
}

export function buildZaiMessages(req: SidecarRequest): ChatCompletionMessageParam[] {
  const messages: ChatCompletionMessageParam[] = [];
  const systemText = systemToText(req.system);
  if (systemText) messages.push({ role: "system", content: systemText });
  for (const turn of req.messages) messages.push(...turnToOpenAI(normalizeTurn(turn)));
  return messages;
}

export async function* zaiStreamEvents(
  requestModel: string,
  chunks: AsyncIterable<ChatCompletionChunk>,
  now: () => number = Date.now,
): AsyncIterable<StreamEvent> {
  const startedAt = now();
  let firstTokenAt = 0;
  const markFirst = () => {
    if (firstTokenAt === 0) firstTokenAt = now();
  };

  let startSent = false;
  let model = requestModel;
  const sendStart = function* (): Iterable<StreamEvent> {
    if (startSent) return;
    startSent = true;
    yield { type: "start", model };
  };

  const toolCalls = new Map<number, { id: string; name: string; argsJson: string }>();
  let textAccum = "";
  let finishReason: string | undefined;
  let usage: Usage = emptyUsage();

  for await (const chunk of chunks) {
    if (!startSent && typeof chunk.model === "string" && chunk.model.length > 0) {
      model = chunk.model;
    }
    yield* sendStart();

    const choice = chunk.choices[0];
    if (choice) {
      const delta = choice.delta as ZaiDelta;
      if (typeof delta.reasoning_content === "string" && delta.reasoning_content.length > 0) {
        markFirst();
        yield { type: "thinking", text: delta.reasoning_content };
      }

      if (typeof delta.content === "string" && delta.content.length > 0) {
        markFirst();
        textAccum += delta.content;
        yield { type: "text", text: delta.content };
      }

      if (Array.isArray(delta.tool_calls)) {
        for (const tc of delta.tool_calls) {
          const idx = typeof tc.index === "number" ? tc.index : 0;
          let state = toolCalls.get(idx);
          if (!state) {
            state = { id: tc.id ?? `tc_${idx}`, name: tc.function?.name ?? "", argsJson: "" };
            toolCalls.set(idx, state);
          }
          if (tc.id) state.id = tc.id;
          if (tc.function?.name) state.name = tc.function.name;
          if (tc.function?.arguments !== undefined) {
            state.argsJson += stringifyArguments(tc.function.arguments);
          }
        }
      }

      if (choice.finish_reason) finishReason = choice.finish_reason;
    }
    if (chunk.usage) usage = extractUsage(chunk.usage as RawUsage);
  }

  yield* sendStart();

  for (const [, tc] of [...toolCalls.entries()].sort((a, b) => a[0] - b[0])) {
    markFirst();
    yield { type: "tool_use", id: tc.id, name: tc.name, input: parseArgs(tc.argsJson) };
  }

  const total = now() - startedAt;
  yield {
    type: "done",
    content: textAccum,
    finish_reason: normalizeZaiFinishReason(finishReason),
    usage,
    timing: {
      total_ms: total,
      time_to_first_token_ms: firstTokenAt === 0 ? total : firstTokenAt - startedAt,
    },
  };
}

export function zaiGenerateResponse(
  requestModel: string,
  completion: unknown,
  totalMs: number,
): GenerateResponse {
  const c = completion as ZaiCompletion;
  const choice = c.choices[0];
  const message = choice?.message;
  const contentBlocks: ContentBlock[] = [];

  if (typeof message?.reasoning_content === "string" && message.reasoning_content.length > 0) {
    contentBlocks.push({ type: "thinking", thinking: message.reasoning_content });
  }

  const text = typeof message?.content === "string" ? message.content : "";
  if (text) contentBlocks.push({ type: "text", text });

  if (Array.isArray(message?.tool_calls)) {
    for (const tc of message.tool_calls) {
      if (tc.type !== undefined && tc.type !== "function") continue;
      const rawArgs = tc.function?.arguments;
      contentBlocks.push({
        type: "tool_use",
        id: tc.id ?? "tc_0",
        name: tc.function?.name ?? "",
        input:
          typeof rawArgs === "string"
            ? parseArgs(rawArgs)
            : rawArgs && typeof rawArgs === "object"
              ? rawArgs
              : {},
      });
    }
  }

  return {
    content: text,
    content_blocks: contentBlocks,
    finish_reason: normalizeZaiFinishReason(choice?.finish_reason),
    usage: extractUsage(c.usage),
    timing: { total_ms: totalMs, time_to_first_token_ms: totalMs },
    model: typeof c.model === "string" && c.model.length > 0 ? c.model : requestModel,
  };
}

function toZaiTools(tools: unknown[] | undefined): ChatCompletionTool[] {
  if (!tools) return [];
  return tools.map((raw) => {
    const t = raw as { name?: string; description?: string; input_schema?: unknown };
    return {
      type: "function",
      function: {
        name: t.name ?? "",
        description: t.description ?? "",
        parameters: (t.input_schema ?? {}) as Record<string, unknown>,
      },
    };
  });
}

function normalizeTurn(turn: WireMessage): TurnMessage {
  if (typeof turn.content === "string") {
    return { role: turn.role, content: [{ type: "text", text: turn.content }] };
  }
  return { role: turn.role, content: turn.content };
}

function systemToText(system: SystemContent | undefined): string {
  if (system === undefined) return "";
  if (typeof system === "string") return system;
  return system.map((b) => b.text).join("\n\n");
}

function providerBool(req: SidecarRequest, key: string): boolean {
  return req.provider_options?.[key] === true;
}

function trimTrailingSlash(url: string): string {
  return url.endsWith("/") ? url.replace(/\/+$/, "") : url;
}

interface RawUsage {
  prompt_tokens?: number;
  completion_tokens?: number;
  prompt_tokens_details?: {
    cached_tokens?: number;
    cache_write_tokens?: number;
  };
  cost?: number;
}

interface ZaiDelta {
  content?: string | null;
  reasoning_content?: string | null;
  tool_calls?: Array<{
    index?: number;
    id?: string;
    function?: { name?: string; arguments?: string | Record<string, unknown> };
  }>;
}

interface ZaiCompletion {
  model?: string;
  choices: Array<{
    message?: {
      content?: string | null;
      reasoning_content?: string | null;
      tool_calls?: Array<{
        id?: string;
        type?: string;
        function?: { name?: string; arguments?: string | Record<string, unknown> };
      }>;
    };
    finish_reason?: string | null;
  }>;
  usage?: RawUsage;
}

function emptyUsage(): Usage {
  return {
    input_tokens: 0,
    output_tokens: 0,
    cache_read_tokens: 0,
    cache_creation_tokens: 0,
  };
}

function extractUsage(u: RawUsage | undefined): Usage {
  const usage: Usage = {
    input_tokens: u?.prompt_tokens ?? 0,
    output_tokens: u?.completion_tokens ?? 0,
    cache_read_tokens: u?.prompt_tokens_details?.cached_tokens ?? 0,
    cache_creation_tokens: u?.prompt_tokens_details?.cache_write_tokens ?? 0,
  };
  if (typeof u?.cost === "number") usage.total_cost_usd = u.cost;
  return usage;
}

function stringifyArguments(args: string | Record<string, unknown>): string {
  return typeof args === "string" ? args : JSON.stringify(args);
}

function parseArgs(argsJson: string): unknown {
  if (argsJson.trim() === "") return {};
  try {
    return JSON.parse(argsJson);
  } catch {
    return {};
  }
}

function normalizeZaiFinishReason(reason: string | null | undefined): string {
  switch (reason) {
    case "stop":
      return "end_turn";
    case "tool_calls":
      return "tool_use";
    case "length":
    case "model_context_window_exceeded":
      return "max_tokens";
    case "content_filter":
    case "sensitive":
      return "content_filter";
    case "network_error":
      return "end_turn";
    case "end_turn":
    case "max_tokens":
    case "tool_use":
    case "refusal":
    case "stop_sequence":
      return reason;
    default:
      return "end_turn";
  }
}
