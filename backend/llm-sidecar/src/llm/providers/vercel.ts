/**
 * Vercel AI SDK adapter — the path for DIRECT (non-OpenRouter) DeepSeek and
 * Moonshot (Kimi) access (issue #164). Both are OpenAI-compatible wires, but
 * their reasoning controls are vendor-specific; the first-party Vercel providers
 * (`@ai-sdk/deepseek`, `@ai-sdk/moonshotai`) model them as typed `providerOptions`
 * and handle `reasoning_content` round-tripping (the area that caused the old
 * Rust deepseek/kimi tool-loop bug), so we don't hand-code each vendor's quirks.
 *
 * One adapter serves both: the AI SDK exposes a unified `LanguageModel` /
 * `streamText` interface, so only the provider factory differs by `req.sdk`.
 *
 * Reasoning controls (from `provider_options`, set by the Rust daemon):
 *   - `thinking_enabled === false` (from `reasoning_effort = "off"`) → thinking
 *     `{ type: "disabled" }` — a real off-switch for always-on reasoning models.
 *   - DeepSeek `reasoning_effort` → `reasoningEffort` (low|medium|high|xhigh|max).
 *   - Moonshot `budget_tokens` → `thinking.budgetTokens`.
 *
 * Prior-turn reasoning is replayed as an assistant `reasoning` content part
 * (DeepSeek/Kimi hard-require it during a tool loop); there is no opaque
 * signature carrier as on the OpenRouter path — the text round-trips directly.
 */

import { createDeepSeek } from "@ai-sdk/deepseek";
import { createMoonshotAI } from "@ai-sdk/moonshotai";
import {
  generateText,
  jsonSchema,
  streamText,
  tool,
  type LanguageModel,
  type LanguageModelUsage,
  type ModelMessage,
  type Tool,
  type ToolSet,
} from "ai";

import type { ContentBlock, ImageRef } from "../../engine/types.ts";
import { resolveImage } from "../images.ts";
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

export class VercelProvider implements SidecarProvider {
  async *stream(req: SidecarRequest, signal?: AbortSignal): AsyncIterable<StreamEvent> {
    const startedAt = Date.now();
    let firstTokenAt = 0;
    const markFirst = () => {
      if (firstTokenAt === 0) firstTokenAt = Date.now();
    };

    const result = streamText(buildCall(req, signal));
    yield { type: "start", model: req.model };

    let textAccum = "";
    let finishReason = "stop";
    let usage: Usage = emptyUsage();

    for await (const part of result.fullStream) {
      switch (part.type) {
        case "reasoning-delta":
          if (part.text.length > 0) {
            markFirst();
            yield { type: "thinking", text: part.text };
          }
          break;
        case "text-delta":
          if (part.text.length > 0) {
            markFirst();
            textAccum += part.text;
            yield { type: "text", text: part.text };
          }
          break;
        case "tool-call":
          markFirst();
          yield { type: "tool_use", id: part.toolCallId, name: part.toolName, input: part.input };
          break;
        case "finish":
          finishReason = part.finishReason;
          usage = toUsage(part.totalUsage);
          break;
        case "error":
          throw part.error;
        default:
          break;
      }
    }

    const total = Date.now() - startedAt;
    yield {
      type: "done",
      content: textAccum,
      finish_reason: mapFinishReason(finishReason),
      usage,
      timing: {
        total_ms: total,
        time_to_first_token_ms: firstTokenAt === 0 ? total : firstTokenAt - startedAt,
      },
    };
  }

  async generate(req: SidecarRequest, signal?: AbortSignal): Promise<GenerateResponse> {
    const startedAt = Date.now();
    // `buildCall` is typed against `streamText`'s params; `generateText` takes a
    // structurally-identical (but nominally distinct) settings object, so cast.
    const result = await generateText(buildCall(req, signal) as Parameters<typeof generateText>[0]);

    const content_blocks: ContentBlock[] = [];
    if (typeof result.reasoningText === "string" && result.reasoningText.length > 0) {
      content_blocks.push({ type: "thinking", thinking: result.reasoningText });
    }
    if (result.text) content_blocks.push({ type: "text", text: result.text });
    for (const tc of result.toolCalls) {
      content_blocks.push({ type: "tool_use", id: tc.toolCallId, name: tc.toolName, input: tc.input });
    }

    const total = Date.now() - startedAt;
    return {
      content: result.text,
      content_blocks,
      finish_reason: mapFinishReason(result.finishReason),
      usage: toUsage(result.usage),
      timing: { total_ms: total, time_to_first_token_ms: total },
      model: req.model,
    };
  }
}

// ── request construction ────────────────────────────────────────────────────

type VercelCall = Parameters<typeof streamText>[0];

export function buildCall(req: SidecarRequest, signal?: AbortSignal): VercelCall {
  const call: VercelCall = {
    model: buildModel(req),
    messages: buildMessages(req),
    maxOutputTokens: req.max_tokens,
  };
  const tools = buildTools(req.tools);
  if (tools) call.tools = tools;
  if (req.temperature !== undefined) call.temperature = req.temperature;
  if (req.top_p !== undefined) call.topP = req.top_p;
  if (signal) call.abortSignal = signal;
  const providerOptions = buildProviderOptions(req);
  if (providerOptions) call.providerOptions = providerOptions;
  return call;
}

function buildModel(req: SidecarRequest): LanguageModel {
  const settings = {
    apiKey: req.api_key,
    ...(req.base_url ? { baseURL: req.base_url } : {}),
  };
  return req.sdk === "deepseek"
    ? createDeepSeek(settings)(req.model)
    : createMoonshotAI(settings)(req.model);
}

/**
 * Map Shore's `provider_options` onto the AI SDK provider's typed reasoning
 * options. The key is the provider id (`deepseek` / `moonshotai`). Returns
 * `undefined` when nothing applies.
 */
type ProviderOptionsValue = NonNullable<VercelCall["providerOptions"]>;

export function buildProviderOptions(req: SidecarRequest): ProviderOptionsValue | undefined {
  const opts = req.provider_options ?? {};
  const inner: Record<string, unknown> = {};

  // `thinking_enabled === false` (from `reasoning_effort = "off"`) is a hard
  // disable and wins over any effort/budget.
  if (opts["thinking_enabled"] === false) {
    inner["thinking"] = { type: "disabled" };
  } else if (req.sdk === "deepseek") {
    const effort = opts["reasoning_effort"];
    if (typeof effort === "string" && effort !== "off") inner["reasoningEffort"] = effort;
  } else {
    // moonshot: a positive budget enables thinking with that budget.
    const budget = opts["budget_tokens"];
    if (typeof budget === "number" && budget > 0) {
      inner["thinking"] = { type: "enabled", budgetTokens: budget };
    }
  }

  if (Object.keys(inner).length === 0) return undefined;
  const key = req.sdk === "deepseek" ? "deepseek" : "moonshotai";
  // The values are plain JSON; the SDK's JSONObject index type is satisfied at
  // runtime — assert it to stay on the typed providerOptions path.
  return { [key]: inner } as ProviderOptionsValue;
}

function buildTools(tools: unknown[] | undefined): ToolSet | undefined {
  if (!tools || tools.length === 0) return undefined;
  const out: Record<string, Tool> = {};
  for (const raw of tools) {
    const t = raw as { name?: string; description?: string; input_schema?: unknown };
    // No `execute`: the Rust daemon owns the tool loop, so the SDK surfaces the
    // tool-call and stops (finishReason "tool-calls").
    out[t.name ?? ""] = tool({
      description: t.description ?? "",
      inputSchema: jsonSchema((t.input_schema ?? {}) as Record<string, unknown>),
    });
  }
  return out;
}

// ── message conversion ────────────────────────────────────────────────────────

function buildMessages(req: SidecarRequest): ModelMessage[] {
  const messages: ModelMessage[] = [];
  const systemText = systemToText(req.system);
  if (systemText) messages.push({ role: "system", content: systemText });
  // AI SDK tool-result parts require the tool NAME, but Shore's tool_result
  // block carries only the id — recover names from prior assistant tool_use.
  const toolNames = collectToolNames(req.messages);
  for (const turn of req.messages) messages.push(...turnToVercel(normalizeTurn(turn), toolNames));
  return messages;
}

function collectToolNames(messages: WireMessage[]): Map<string, string> {
  const names = new Map<string, string>();
  for (const turn of messages) {
    if (typeof turn.content === "string") continue;
    for (const b of turn.content) {
      if (b.type === "tool_use") names.set(b.id, b.name);
    }
  }
  return names;
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

function textOf(turn: TurnMessage): string {
  return turn.content
    .filter((b): b is Extract<ContentBlock, { type: "text" }> => b.type === "text")
    .map((b) => b.text)
    .join("");
}

/**
 * One canonical turn → AI SDK `ModelMessage`(s). Assistant turns replay a prior
 * thinking block as a `reasoning` content part (DeepSeek/Kimi require it across
 * a tool loop). User tool_results become a separate `role:"tool"` message.
 */
export function turnToVercel(turn: TurnMessage, toolNames: Map<string, string>): ModelMessage[] {
  if (turn.role === "system") {
    return [{ role: "system", content: textOf(turn) }];
  }

  if (turn.role === "assistant") {
    const parts: Array<
      | { type: "reasoning"; text: string }
      | { type: "text"; text: string }
      | { type: "tool-call"; toolCallId: string; toolName: string; input: unknown }
    > = [];
    const thinking = turn.content.find(
      (b): b is Extract<ContentBlock, { type: "thinking" }> => b.type === "thinking",
    );
    if (thinking && thinking.thinking.length > 0) {
      parts.push({ type: "reasoning", text: thinking.thinking });
    }
    const text = textOf(turn);
    if (text) parts.push({ type: "text", text });
    for (const b of turn.content) {
      if (b.type === "tool_use") {
        parts.push({ type: "tool-call", toolCallId: b.id, toolName: b.name, input: b.input ?? {} });
      }
    }
    return parts.length > 0 ? [{ role: "assistant", content: parts }] : [];
  }

  // User turn: tool_results → one `role:"tool"` message; text + images ride a
  // single user message.
  const out: ModelMessage[] = [];
  const toolParts = turn.content
    .filter((b): b is Extract<ContentBlock, { type: "tool_result" }> => b.type === "tool_result")
    .map((b) => ({
      type: "tool-result" as const,
      toolCallId: b.tool_use_id,
      toolName: toolNames.get(b.tool_use_id) ?? "",
      output: { type: "text" as const, value: b.content },
    }));
  if (toolParts.length > 0) out.push({ role: "tool", content: toolParts });

  const userParts: Array<
    { type: "text"; text: string } | { type: "image"; image: string; mediaType: string }
  > = [];
  for (const img of imageParts(turn.images)) userParts.push(img);
  for (const b of turn.content) {
    if (b.type === "text") userParts.push({ type: "text", text: b.text });
  }
  if (userParts.length > 0) out.push({ role: "user", content: userParts });
  return out;
}

function imageParts(
  images: ImageRef[] | undefined,
): Array<{ type: "image"; image: string; mediaType: string }> {
  if (!images || images.length === 0) return [];
  const out: Array<{ type: "image"; image: string; mediaType: string }> = [];
  for (const img of images) {
    const resolved = resolveImage(img);
    if (!resolved) continue;
    out.push({ type: "image", image: resolved.base64, mediaType: resolved.mediaType });
  }
  return out;
}

// ── helpers ───────────────────────────────────────────────────────────────────

function emptyUsage(): Usage {
  return { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_creation_tokens: 0 };
}

function toUsage(u: LanguageModelUsage | undefined): Usage {
  return {
    input_tokens: u?.inputTokens ?? 0,
    output_tokens: u?.outputTokens ?? 0,
    cache_read_tokens: u?.inputTokenDetails?.cacheReadTokens ?? 0,
    cache_creation_tokens: u?.inputTokenDetails?.cacheWriteTokens ?? 0,
  };
}

function mapFinishReason(finish: string): string {
  switch (finish) {
    case "stop":
      return "end_turn";
    case "tool-calls":
      return "tool_use";
    case "length":
      return "max_tokens";
    default:
      return finish;
  }
}
