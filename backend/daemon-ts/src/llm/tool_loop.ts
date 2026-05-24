/**
 * Provider-agnostic tool-use loop.
 *
 * Calls the provider; if the response contains `tool_use` blocks,
 * dispatches them via the ToolRegistry, appends a synthetic user turn
 * with `tool_result` blocks (one per tool_use, in order), and re-calls.
 * Stops when the response carries no tool_use blocks (or we hit the
 * iteration cap).
 *
 * **The block ordering is what kills the cache regression.** When we
 * append tool_results we MUST preserve the assistant turn that produced
 * them verbatim — including the upstream `thinking` block that
 * preceded the `tool_use`. Anthropic signs thinking blocks and re-hashes
 * the prefix; if we drop or rewrite thinking on the way back in, cache
 * invalidates and (worse) the next turn rejects with a signature error.
 *
 * Caller responsibilities:
 *   - Provide the initial messages array (user-led).
 *   - Provide a registry that knows how to execute each tool by name.
 *   - Consume `events` for the streamed UI/SWP path (text deltas,
 *     thinking deltas, tool_use start/done).
 *   - After the loop ends, `result.messages` is the full message list
 *     including all assistant + tool_result turns to be persisted.
 */

import type { ContentBlock } from "../engine/types.ts";
import type { ToolContext, ToolRegistry } from "../tools/registry.ts";
import type { ChatEvent, ChatRequest, ProviderClient, TurnMessage, UsageStats } from "./types.ts";

export interface ToolLoopOptions {
  provider: ProviderClient;
  request: Omit<ChatRequest, "messages"> & { messages: TurnMessage[] };
  registry: ToolRegistry;
  /** Dependency-injection blob passed to every tool handler. */
  toolContext: ToolContext;
  /** Max round-trips through the provider. Default 10. */
  maxIterations?: number;
  /** Called for every streamed event (text/thinking deltas, tool starts/ends). */
  onEvent?: (event: ChatEvent) => void;
  /** Called when a tool finishes executing — useful for SWP ToolResult frames. */
  onToolResult?: (id: string, name: string, result: string, isError: boolean) => void;
  /** Optional heartbeat-only wrap-up nudge before grace iterations begin. */
  wrapUp?: {
    afterIterations: number;
    text: string;
    onNudge?: () => void;
  };
}

export interface ToolLoopResult {
  /** Final assistant content blocks (the last response's content). */
  finalContent: ContentBlock[];
  /** All assistant + synthetic-user turns appended during the loop. */
  newTurns: TurnMessage[];
  /** Per-call usage stats, in order. */
  usagePerCall: UsageStats[];
  /** Per-call telemetry rows, in provider call order. */
  calls: ToolLoopCall[];
  /** Last call's stop reason. */
  stopReason: string;
}

export interface ToolLoopCall {
  usage: UsageStats;
  stopReason: string;
  totalMs: number;
  ttftMs: number;
}

export async function runToolLoop(opts: ToolLoopOptions): Promise<ToolLoopResult> {
  const maxIter = opts.maxIterations ?? 10;
  const newTurns: TurnMessage[] = [];
  const usagePerCall: UsageStats[] = [];
  const calls: ToolLoopCall[] = [];

  let messages = opts.request.messages;
  let lastContent: ContentBlock[] = [];
  let lastStopReason = "end_turn";
  let wrapUpNudged = false;

  for (let iter = 0; iter < maxIter; iter++) {
    if (
      opts.wrapUp !== undefined
      && !wrapUpNudged
      && iter >= opts.wrapUp.afterIterations
    ) {
      messages = appendWrapUpNudge(messages, opts.wrapUp.text);
      wrapUpNudged = true;
      opts.wrapUp.onNudge?.();
    }
    const req: ChatRequest = { ...opts.request, messages };
    const { content, stopReason, usage, totalMs, ttftMs } = await consumeStream(
      opts.provider.stream(req),
      opts.onEvent,
    );
    usagePerCall.push(usage);
    calls.push({ usage, stopReason, totalMs, ttftMs });
    lastContent = content;
    lastStopReason = stopReason;

    // Append the assistant turn — verbatim. We hand the same content
    // array back next iteration so signatures and IDs are preserved.
    const assistantTurn: TurnMessage = { role: "assistant", content };
    messages = [...messages, assistantTurn];
    newTurns.push(assistantTurn);

    const toolUses = content.filter(
      (b): b is Extract<ContentBlock, { type: "tool_use" }> => b.type === "tool_use",
    );
    if (stopReason !== "tool_use" || toolUses.length === 0) break;

    // Execute each tool, collect results in the same order.
    const resultBlocks: ContentBlock[] = [];
    for (const tu of toolUses) {
      const handler = opts.registry.get(tu.name);
      let result: string;
      let isError = false;
      if (!handler) {
        result = `error: unknown tool "${tu.name}"`;
        isError = true;
      } else {
        try {
          result = await handler.execute(tu.input, opts.toolContext);
        } catch (e) {
          result = `error: ${(e as Error).message}`;
          isError = true;
        }
      }
      opts.onToolResult?.(tu.id, tu.name, result, isError);
      const block: ContentBlock = {
        type: "tool_result",
        tool_use_id: tu.id,
        content: result,
      };
      if (isError) block.is_error = true;
      resultBlocks.push(block);
    }

    const toolUserTurn: TurnMessage = { role: "user", content: resultBlocks };
    messages = [...messages, toolUserTurn];
    newTurns.push(toolUserTurn);
  }

  return { finalContent: lastContent, newTurns, usagePerCall, calls, stopReason: lastStopReason };
}

function appendWrapUpNudge(messages: TurnMessage[], text: string): TurnMessage[] {
  const block: ContentBlock = { type: "text", text };
  const last = messages[messages.length - 1];
  if (last?.role === "user") {
    return [
      ...messages.slice(0, -1),
      { ...last, content: [...last.content, block] },
    ];
  }
  return [...messages, { role: "user", content: [block] }];
}

async function consumeStream(
  events: AsyncIterable<ChatEvent>,
  onEvent: ((e: ChatEvent) => void) | undefined,
): Promise<{
  content: ContentBlock[];
  stopReason: string;
  usage: UsageStats;
  totalMs: number;
  ttftMs: number;
}> {
  const start = Date.now();
  let firstOutputAt: number | undefined;
  for await (const event of events) {
    if (event.kind !== "done" && firstOutputAt === undefined) {
      firstOutputAt = Date.now();
    }
    onEvent?.(event);
    if (event.kind === "done") {
      const totalMs = Date.now() - start;
      return {
        content: event.content,
        stopReason: event.stopReason,
        usage: event.usage,
        totalMs,
        ttftMs: firstOutputAt === undefined ? totalMs : firstOutputAt - start,
      };
    }
  }
  throw new Error("provider stream ended without a 'done' event");
}
