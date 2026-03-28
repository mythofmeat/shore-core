import Anthropic from "@anthropic-ai/sdk";
import type {
  MessageCreateParamsNonStreaming,
  MessageCreateParamsStreaming,
  MessageParam,
  TextBlockParam,
  RawMessageStreamEvent,
} from "@anthropic-ai/sdk/resources/messages/messages.js";
import type { ServerResponse } from "node:http";
import type {
  NormalizedResponse,
  NormalizedContentBlock,
  NormalizedUsage,
  NormalizedTiming,
  StreamEvent,
} from "./types.js";

// ── Request types ──────────────────────────────────────────────────────

export interface GenerateRequest {
  provider: string;
  model: string;
  api_key: string;
  base_url?: string | null;
  messages: MessageParam[];
  system?: string | TextBlockParam[];
  tools?: Anthropic.Messages.Tool[];
  max_tokens: number;
  temperature?: number | null;
  top_p?: number | null;
  provider_options?: AnthropicProviderOptions;
}

export interface AnthropicProviderOptions {
  cache_control_depth?: number;
  thinking?: boolean;
  budget_tokens?: number;
  /** "adaptive" lets the model decide thinking budget. */
  reasoning_effort?: string;
}

// ── Response and stream types (from shared types) ─────────────────────
// Types imported from ./types.js above

// ── Helpers ────────────────────────────────────────────────────────────

/** Effort values that map to adaptive thinking + output_config on Anthropic. */
const ANTHROPIC_EFFORT_VALUES = new Set(["max", "high", "medium", "low"]);

function applyCacheControl(
  messages: MessageParam[],
  depth: number,
): MessageParam[] {
  if (depth <= 0 || messages.length === 0) return messages;

  const result = messages.map((m) => ({ ...m }));
  const start = Math.max(0, result.length - depth);

  for (let i = start; i < result.length; i++) {
    const msg = result[i];
    if (typeof msg.content === "string") {
      // Convert string content to block form so we can attach cache_control
      result[i] = {
        ...msg,
        content: [
          {
            type: "text" as const,
            text: msg.content,
            cache_control: { type: "ephemeral" as const },
          },
        ],
      };
    } else if (Array.isArray(msg.content) && msg.content.length > 0) {
      const blocks = [...msg.content];
      const last = { ...blocks[blocks.length - 1] } as TextBlockParam & {
        cache_control?: { type: "ephemeral" };
      };
      last.cache_control = { type: "ephemeral" };
      blocks[blocks.length - 1] = last;
      result[i] = { ...msg, content: blocks };
    }
  }

  return result;
}

function buildThinkingConfig(
  opts: AnthropicProviderOptions | undefined,
): Anthropic.Messages.ThinkingConfigParam | undefined {
  // "adaptive" mode: model decides how much to think.
  // Named effort values ("high", "medium", "low", "max") also use adaptive
  // mode; the effort level is passed separately via output_config.
  if (
    opts?.reasoning_effort === "adaptive" ||
    (opts?.reasoning_effort != null &&
      ANTHROPIC_EFFORT_VALUES.has(opts.reasoning_effort))
  ) {
    return { type: "adaptive" };
  }
  // Explicit budget: enable thinking with the given token budget.
  // Presence of budget_tokens implies thinking should be enabled,
  // even without an explicit `thinking: true` flag.
  if (opts?.thinking || opts?.budget_tokens != null) {
    return {
      type: "enabled",
      budget_tokens: opts.budget_tokens ?? 1024,
    };
  }
  return undefined;
}

function buildOutputConfig(
  opts: AnthropicProviderOptions | undefined,
): { effort: string } | undefined {
  if (
    opts?.reasoning_effort != null &&
    ANTHROPIC_EFFORT_VALUES.has(opts.reasoning_effort)
  ) {
    return { effort: opts.reasoning_effort };
  }
  return undefined;
}

function normalizeUsage(usage: Anthropic.Messages.Usage): NormalizedUsage {
  return {
    input_tokens: usage.input_tokens,
    output_tokens: usage.output_tokens,
    cache_read_tokens: usage.cache_read_input_tokens ?? 0,
    cache_creation_tokens: usage.cache_creation_input_tokens ?? 0,
  };
}

function normalizeContentBlocks(
  blocks: Anthropic.Messages.ContentBlock[],
): NormalizedContentBlock[] {
  return blocks
    .map((block): NormalizedContentBlock | null => {
      if (block.type === "text") {
        return { type: "text", text: block.text };
      }
      if (block.type === "thinking") {
        return {
          type: "thinking",
          thinking: block.thinking,
          signature: (block as unknown as { signature?: string }).signature,
        };
      }
      if (block.type === "redacted_thinking") {
        return {
          type: "redacted_thinking",
          data: (block as unknown as { data: string }).data,
        };
      }
      if (block.type === "tool_use") {
        return {
          type: "tool_use",
          id: block.id,
          name: block.name,
          input: block.input,
        };
      }
      return null;
    })
    .filter((b): b is NormalizedContentBlock => b !== null);
}

function extractTextContent(blocks: Anthropic.Messages.ContentBlock[]): string {
  return blocks
    .filter((b) => b.type === "text")
    .map((b) => (b as Anthropic.Messages.TextBlock).text)
    .join("");
}

// ── Main API ───────────────────────────────────────────────────────────

/** Create an Anthropic SDK client from per-request credentials. */
export function createClient(
  apiKey: string,
  baseURL?: string | null,
): Anthropic {
  return new Anthropic({
    apiKey,
    ...(baseURL ? { baseURL } : {}),
  });
}

/** Build the SDK params from a normalized GenerateRequest. */
export function buildCreateParams(
  req: GenerateRequest,
  stream: boolean,
): MessageCreateParamsNonStreaming | MessageCreateParamsStreaming {
  const messages = req.provider_options?.cache_control_depth
    ? applyCacheControl(req.messages, req.provider_options.cache_control_depth)
    : req.messages;

  const thinking = buildThinkingConfig(req.provider_options);
  const outputConfig = buildOutputConfig(req.provider_options);

  const params: Record<string, unknown> = {
    model: req.model,
    max_tokens: req.max_tokens,
    messages,
    stream,
  };

  if (req.system != null) params.system = req.system;
  if (req.tools != null && req.tools.length > 0) params.tools = req.tools;
  if (req.temperature != null) params.temperature = req.temperature;
  if (req.top_p != null) params.top_p = req.top_p;
  if (thinking) params.thinking = thinking;
  if (outputConfig) params.output_config = outputConfig;

  return params as unknown as MessageCreateParamsNonStreaming | MessageCreateParamsStreaming;
}

/** Non-streaming generate: call the API and return a normalized response. */
export async function generate(
  client: Anthropic,
  req: GenerateRequest,
): Promise<NormalizedResponse> {
  const params = buildCreateParams(req, false) as MessageCreateParamsNonStreaming;
  const start = performance.now();
  const msg = await client.messages.create(params);
  const totalMs = performance.now() - start;

  return {
    content: extractTextContent(msg.content),
    content_blocks: normalizeContentBlocks(msg.content),
    finish_reason: msg.stop_reason ?? "end_turn",
    usage: normalizeUsage(msg.usage),
    timing: {
      total_ms: Math.round(totalMs),
      time_to_first_token_ms: Math.round(totalMs),
    },
    model: msg.model,
    provider: "anthropic",
  };
}

/** Streaming generate: emit newline-delimited JSON events to res. */
export async function stream(
  client: Anthropic,
  req: GenerateRequest,
  res: ServerResponse,
): Promise<void> {
  const params = buildCreateParams(req, true) as MessageCreateParamsStreaming;
  const start = performance.now();
  let firstTokenMs: number | null = null;

  const sdkStream = await client.messages.create(params);

  res.chunkedEncoding = false;
  res.writeHead(200, {
    "Content-Type": "application/x-ndjson",
  });

  // Accumulated state
  let textContent = "";
  let finishReason = "end_turn";
  let usage: NormalizedUsage = {
    input_tokens: 0,
    output_tokens: 0,
    cache_read_tokens: 0,
    cache_creation_tokens: 0,
  };
  let model = req.model;

  // Track active tool_use blocks by index
  const toolBlocks = new Map<
    number,
    { id: string; name: string; jsonChunks: string[] }
  >();

  // Track thinking block indices and their accumulated signatures
  const thinkingBlocks = new Set<number>();
  const thinkingSignatures = new Map<number, string>();

  // Track redacted_thinking blocks (arrive complete at content_block_start)
  const redactedThinkingBlocks = new Map<number, string>();

  function writeLine(event: StreamEvent): void {
    res.write(JSON.stringify(event) + "\n");
  }

  for await (const event of sdkStream as AsyncIterable<RawMessageStreamEvent>) {
    switch (event.type) {
      case "message_start": {
        model = event.message.model;
        usage = normalizeUsage(event.message.usage);
        writeLine({ type: "start", model });
        break;
      }

      case "content_block_start": {
        const block = event.content_block;
        if (block.type === "tool_use") {
          toolBlocks.set(event.index, {
            id: block.id,
            name: block.name,
            jsonChunks: [],
          });
        } else if (block.type === "thinking") {
          thinkingBlocks.add(event.index);
        } else if (block.type === "redacted_thinking") {
          redactedThinkingBlocks.set(
            event.index,
            (block as unknown as { data: string }).data,
          );
        }
        break;
      }

      case "content_block_delta": {
        const delta = event.delta;
        if (delta.type === "text_delta") {
          if (firstTokenMs === null) {
            firstTokenMs = performance.now() - start;
          }
          textContent += delta.text;
          writeLine({ type: "text", text: delta.text });
        } else if (delta.type === "thinking_delta") {
          if (firstTokenMs === null) {
            firstTokenMs = performance.now() - start;
          }
          writeLine({ type: "thinking", text: delta.thinking });
        } else if (
          delta.type === "signature_delta" &&
          thinkingBlocks.has(event.index)
        ) {
          const existing = thinkingSignatures.get(event.index) ?? "";
          thinkingSignatures.set(
            event.index,
            existing + (delta as unknown as { signature: string }).signature,
          );
        } else if (delta.type === "input_json_delta") {
          const tool = toolBlocks.get(event.index);
          if (tool) {
            tool.jsonChunks.push(delta.partial_json);
          }
        }
        break;
      }

      case "content_block_stop": {
        const tool = toolBlocks.get(event.index);
        if (tool) {
          let input: unknown = {};
          const raw = tool.jsonChunks.join("");
          if (raw.length > 0) {
            try {
              input = JSON.parse(raw);
            } catch {
              input = {};
            }
          }
          writeLine({
            type: "tool_use",
            id: tool.id,
            name: tool.name,
            input,
          });
          toolBlocks.delete(event.index);
        }

        // Emit thinking signature when a thinking block finishes.
        if (thinkingBlocks.has(event.index)) {
          const sig = thinkingSignatures.get(event.index);
          if (sig) {
            writeLine({ type: "thinking_signature", signature: sig });
            thinkingSignatures.delete(event.index);
          }
          thinkingBlocks.delete(event.index);
        }

        // Emit redacted thinking data when the block finishes.
        const redactedData = redactedThinkingBlocks.get(event.index);
        if (redactedData != null) {
          writeLine({ type: "redacted_thinking", data: redactedData });
          redactedThinkingBlocks.delete(event.index);
        }
        break;
      }

      case "message_delta": {
        finishReason = event.delta.stop_reason ?? finishReason;
        usage = {
          ...usage,
          output_tokens: event.usage.output_tokens,
          cache_read_tokens: event.usage.cache_read_input_tokens ?? usage.cache_read_tokens,
          cache_creation_tokens: event.usage.cache_creation_input_tokens ?? usage.cache_creation_tokens,
        };
        break;
      }

      case "message_stop": {
        const totalMs = performance.now() - start;
        writeLine({
          type: "done",
          content: textContent,
          finish_reason: finishReason,
          usage,
          timing: {
            total_ms: Math.round(totalMs),
            time_to_first_token_ms: Math.round(firstTokenMs ?? totalMs),
          },
        });
        break;
      }
    }
  }

  res.end();
}
