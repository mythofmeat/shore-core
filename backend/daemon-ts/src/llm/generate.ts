/**
 * Generate one assistant response for a character.
 *
 * Glue between SWP, the engine, the catalog, prompt assembly, the
 * provider client, and the tool loop. The orchestrator is intentionally
 * a free function (not a method on `ConversationEngine`) so the engine
 * stays a pure state owner and this layer can be tested in isolation
 * with a mock provider.
 *
 * Frame stream (one generation, one LLM tool-loop iteration is one turn):
 *   stream_start
 *     stream_chunk × N         (text/thinking deltas)
 *     tool_call × M            (per tool_use block)
 *     tool_result × M          (after each tool executes)
 *   stream_end   is_final=false (intermediate turn boundary)
 *   ... repeat per loop iteration ...
 *   stream_end   is_final=true  (terminal — carries final aggregated text)
 *
 * Each new turn (assistant content + synthetic user tool_results) is
 * appended via `engine.appendMessage`, which fans out a History
 * broadcast to all connected clients. The history snapshot is the
 * canonical post-stream view; stream_chunks are best-effort live
 * rendering.
 */

import type { ConversationEngine } from "../engine/engine.ts";
import { buildChatContext } from "../engine/context.ts";
import type { ContentBlock, Message } from "../engine/types.ts";
import type { ServerMessage } from "../swp/types.ts";
import { AnthropicProvider } from "./providers/anthropic.ts";
import { OpenAIProvider } from "./providers/openai.ts";
import type { ResolvedModel } from "./catalog.ts";
import type { ToolRegistry } from "./tools/registry.ts";
import { runToolLoop } from "./tool_loop.ts";
import type {
  ChatEvent,
  ChatRequest,
  ProviderClient,
  TurnMessage,
} from "./types.ts";

export interface GenerationOverrides {
  temperature?: number;
  top_p?: number;
  thinking_budget?: number;
}

export interface GenerateOptions {
  engine: ConversationEngine;
  /** `$CONFIG_DIR/characters/<character>/`. */
  characterConfigDir: string;
  displayName: string;
  resolved: ResolvedModel;
  registry: ToolRegistry;
  /** Where the orchestrator routes SWP frames (stream_start, chunks, end). */
  broadcast: (frame: ServerMessage) => void;
  /** Request id for correlating frames to the originating ClientMessage. */
  rid?: string;
  isPrivate?: boolean;
  /** Per-call sampler overrides from the ClientMessage frame. */
  overrides?: GenerationOverrides;
  /** AbortSignal for cancelling the generation mid-stream. */
  signal?: AbortSignal;
}

export interface GenerateResult {
  finalText: string;
  turnCount: number;
}

/**
 * Drive a single assistant generation end-to-end. Caller has already
 * `appendMessage`'d the user turn; this reads the current history and
 * appends the resulting assistant (+ tool_result) turns.
 */
export async function generateResponse(
  opts: GenerateOptions,
): Promise<GenerateResult> {
  const apiKey = resolveApiKey(opts.resolved);
  const provider = buildProvider(opts.resolved.sdk);

  const snapshot = opts.engine.historySnapshot();
  const ctx = buildChatContext({
    characterName: opts.engine.name(),
    characterConfigDir: opts.characterConfigDir,
    displayName: opts.displayName,
    isPrivate: opts.isPrivate ?? false,
    // 4c.1 minimum: no compaction yet, so the in-prompt history is the
    // full retained history; nothing was archived.
    hasPriorContext: false,
    messages: snapshot.messages,
    ...(opts.resolved.maxContextTokens !== undefined
      ? { maxContextTokens: opts.resolved.maxContextTokens }
      : {}),
    ...(opts.resolved.maxTokens !== undefined
      ? { maxOutputTokens: opts.resolved.maxTokens }
      : {}),
  });

  const systemString = ctx.prompt.system.map((b) => b.content).join("\n\n");
  const messages = promptMessagesToTurns(ctx.prompt.messages);

  const tools = opts.registry.list().map((t) => ({
    name: t.name,
    description: t.description,
    inputSchema: t.inputSchema,
  }));

  const thinking = buildThinkingConfig(opts.resolved, opts.overrides);
  const temperature = opts.overrides?.temperature ?? opts.resolved.temperature;
  const topP = opts.overrides?.top_p ?? opts.resolved.topP;

  const request: ChatRequest = {
    system: systemString,
    messages,
    tools,
    thinking,
    cacheTtl: opts.resolved.cacheTtl ?? "",
    modelId: opts.resolved.modelId,
    apiKey,
    maxTokens: opts.resolved.maxTokens ?? 4096,
    ...(opts.resolved.baseUrl !== undefined ? { baseUrl: opts.resolved.baseUrl } : {}),
    ...(temperature !== undefined ? { temperature } : {}),
    ...(topP !== undefined ? { topP } : {}),
    ...(opts.signal !== undefined ? { signal: opts.signal } : {}),
  };

  // Stream-frame emitter — fired per ChatEvent during the loop. Aggregates
  // text per turn so we can emit a stream_end carrying the final
  // assembled string when the turn closes.
  let turnText = "";
  let turnCount = 0;
  const startTs = Date.now();
  let firstTokenTs: number | null = null;
  const rid = opts.rid;

  const emit = (m: ServerMessage): void => {
    if (rid !== undefined) (m as { rid?: string }).rid = rid;
    opts.broadcast(m);
  };

  emit({ type: "stream_start", regen: false });

  const onEvent = (event: ChatEvent): void => {
    switch (event.kind) {
      case "text_delta":
        if (firstTokenTs === null) firstTokenTs = Date.now();
        turnText += event.text;
        emit({ type: "stream_chunk", text: event.text, content_type: "text" });
        break;
      case "thinking_delta":
        if (firstTokenTs === null) firstTokenTs = Date.now();
        emit({ type: "stream_chunk", text: event.text, content_type: "thinking" });
        break;
      case "tool_use_start":
        emit({
          type: "tool_call",
          tool_id: event.id,
          tool_name: event.name,
          input: {},
        });
        break;
      case "done": {
        turnCount++;
        const isFinal = event.stopReason !== "tool_use";
        const elapsed = Date.now() - startTs;
        const ttft = firstTokenTs !== null ? firstTokenTs - startTs : elapsed;
        emit({
          type: "stream_end",
          content: turnText,
          metadata: {
            tokens: {
              input: event.usage.inputTokens,
              output: event.usage.outputTokens,
              cache_read: event.usage.cacheReadInputTokens,
              cache_write: event.usage.cacheCreationInputTokens,
            },
            timing: { total_ms: elapsed, ttft_ms: ttft },
            model: opts.resolved.modelId,
          },
          finish_reason: event.stopReason,
          is_final: isFinal,
        });
        // Reset for the next loop iteration's stream_end.
        turnText = "";
        break;
      }
      default:
        break;
    }
  };

  const onToolResult = (
    id: string,
    name: string,
    output: string,
    isError: boolean,
  ): void => {
    emit({
      type: "tool_result",
      tool_id: id,
      tool_name: name,
      output,
      ...(isError ? { is_error: true } : {}),
    });
  };

  const result = await runToolLoop({
    provider,
    request,
    registry: opts.registry,
    onEvent,
    onToolResult,
  });

  // Persist the new turns. Each appendMessage triggers a History
  // broadcast — clients use that as the canonical post-stream view.
  for (const turn of result.newTurns) {
    const msg = turnToMessage(turn);
    await opts.engine.appendMessage(msg);
  }

  const finalText = result.finalContent
    .filter((b): b is Extract<ContentBlock, { type: "text" }> => b.type === "text")
    .map((b) => b.text)
    .join("");

  return { finalText, turnCount };
}

/**
 * Build the `ThinkingConfig` from catalog defaults + per-call overrides.
 *
 * Priority:
 *   1. `overrides.thinking_budget` (set + > 0 enables thinking with that
 *      explicit budget; 0 disables thinking even if catalog enables it).
 *   2. Catalog `reasoning_effort` (low/medium/high/xhigh/max/adaptive) —
 *      enables thinking, adapter maps to a budget at request build.
 *   3. Catalog `budget_tokens` — enables thinking with explicit budget.
 *   4. Otherwise off.
 */
export function buildThinkingConfig(
  resolved: ResolvedModel,
  overrides: GenerationOverrides | undefined,
): import("./types.ts").ThinkingConfig {
  if (overrides?.thinking_budget !== undefined) {
    if (overrides.thinking_budget <= 0) return { enabled: false };
    return { enabled: true, budgetTokens: overrides.thinking_budget };
  }
  if (resolved.reasoningEffort !== undefined) {
    const cfg: import("./types.ts").ThinkingConfig = {
      enabled: true,
      effort: resolved.reasoningEffort,
    };
    if (resolved.budgetTokens !== undefined) cfg.budgetTokens = resolved.budgetTokens;
    return cfg;
  }
  if (resolved.budgetTokens !== undefined) {
    return { enabled: true, budgetTokens: resolved.budgetTokens };
  }
  return { enabled: false };
}

function buildProvider(sdk: ResolvedModel["sdk"]): ProviderClient {
  switch (sdk) {
    case "anthropic":
      return new AnthropicProvider();
    case "openai":
      return new OpenAIProvider();
    case "gemini":
    case "zai":
      throw new Error(
        `provider SDK "${sdk}" is not implemented in shore-daemon-ts yet`,
      );
  }
}

function resolveApiKey(resolved: ResolvedModel): string {
  if (!resolved.apiKeyEnv) {
    throw new Error(
      `model ${resolved.qualifiedName} has no api_key_env set; cannot resolve credentials`,
    );
  }
  const key = process.env[resolved.apiKeyEnv];
  if (!key) {
    throw new Error(
      `env var ${resolved.apiKeyEnv} is unset (required by ${resolved.qualifiedName})`,
    );
  }
  return key;
}

/** Convert assembled PromptMessage[] into the TurnMessage[] the providers want. */
function promptMessagesToTurns(
  prompt: import("../engine/prompt.ts").PromptMessage[],
): TurnMessage[] {
  return prompt.map((pm) => {
    const content =
      pm.content_blocks.length > 0
        ? pm.content_blocks
        : [{ type: "text" as const, text: pm.content }];
    const turn: TurnMessage = { role: pm.role, content };
    if (pm.images.length > 0) turn.images = pm.images;
    return turn;
  });
}

/** Materialize a TurnMessage as a persistable Message for active.jsonl. */
function turnToMessage(turn: TurnMessage): Message {
  const content = turn.content
    .filter((b): b is Extract<ContentBlock, { type: "text" }> => b.type === "text")
    .map((b) => b.text)
    .join("");
  return {
    msg_id: `m_${crypto.randomUUID()}`,
    role: turn.role,
    content,
    images: [],
    content_blocks: turn.content,
    timestamp: rfc3339LocalNow(),
  };
}

function rfc3339LocalNow(): string {
  const now = new Date();
  const tzOffsetMinutes = -now.getTimezoneOffset();
  const sign = tzOffsetMinutes >= 0 ? "+" : "-";
  const abs = Math.abs(tzOffsetMinutes);
  const tzh = String(Math.floor(abs / 60)).padStart(2, "0");
  const tzm = String(abs % 60).padStart(2, "0");
  const pad = (n: number, w = 2): string => String(n).padStart(w, "0");
  const ms = String(now.getMilliseconds()).padStart(3, "0");
  return (
    `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}` +
    `T${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}.${ms}${sign}${tzh}:${tzm}`
  );
}
