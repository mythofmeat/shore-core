import OpenAI from "openai";
import type { ServerResponse } from "node:http";
import type {
  ProviderRequest,
  NormalizedResponse,
  NormalizedContentBlock,
  NormalizedUsage,
  StreamEvent,
  EmbedRequest,
  EmbedResponse,
  ImageGenerateRequest,
  ImageGenerateResponse,
} from "./types.js";

// ── Client creation ──────────────────────────────────────────────────

export function createClient(
  apiKey: string,
  baseURL?: string | null,
  defaultHeaders?: Record<string, string>,
): OpenAI {
  return new OpenAI({
    apiKey,
    ...(baseURL ? { baseURL } : {}),
    ...(defaultHeaders ? { defaultHeaders } : {}),
  });
}

// ── Message translation ──────────────────────────────────────────────

interface ContentBlock {
  type: string;
  text?: string;
  id?: string;
  name?: string;
  input?: unknown;
  tool_use_id?: string;
  content?: unknown;
}

export function translateMessages(
  req: ProviderRequest,
): OpenAI.ChatCompletionMessageParam[] {
  const messages: OpenAI.ChatCompletionMessageParam[] = [];

  if (req.system) {
    messages.push({ role: "system", content: req.system });
  }

  for (const msg of req.messages) {
    if (typeof msg.content === "string") {
      if (msg.role === "system") {
        messages.push({ role: "system", content: msg.content });
      } else if (msg.role === "user") {
        messages.push({ role: "user", content: msg.content });
      } else if (msg.role === "assistant") {
        messages.push({ role: "assistant", content: msg.content });
      }
    } else if (Array.isArray(msg.content)) {
      const blocks = msg.content as ContentBlock[];

      if (msg.role === "assistant") {
        const textParts = blocks.filter((b) => b.type === "text");
        const toolParts = blocks.filter((b) => b.type === "tool_use");

        const content = textParts.map((b) => b.text ?? "").join("") || null;
        const toolCalls = toolParts.map((b) => ({
          id: b.id!,
          type: "function" as const,
          function: {
            name: b.name!,
            arguments: JSON.stringify(b.input ?? {}),
          },
        }));

        messages.push({
          role: "assistant",
          content,
          ...(toolCalls.length > 0 ? { tool_calls: toolCalls } : {}),
        });
      } else if (msg.role === "user") {
        const toolResults = blocks.filter((b) => b.type === "tool_result");
        const textBlocks = blocks.filter((b) => b.type === "text");

        for (const tr of toolResults) {
          messages.push({
            role: "tool",
            tool_call_id: tr.tool_use_id ?? "",
            content:
              typeof tr.content === "string"
                ? tr.content
                : JSON.stringify(tr.content ?? ""),
          });
        }

        if (textBlocks.length > 0) {
          const parts = textBlocks.map((b) => ({
            type: "text" as const,
            text: b.text ?? "",
          }));
          messages.push({ role: "user", content: parts });
        }
      }
    }
  }

  return messages;
}

export function translateTools(
  tools?: ProviderRequest["tools"],
): OpenAI.ChatCompletionTool[] | undefined {
  if (!tools || tools.length === 0) return undefined;
  return tools.map((t) => ({
    type: "function" as const,
    function: {
      name: t.name,
      description: t.description,
      parameters: t.input_schema,
    },
  }));
}

// ── Response normalization ───────────────────────────────────────────

function normalizeFinishReason(reason: string | null): string {
  switch (reason) {
    case "stop":
      return "end_turn";
    case "tool_calls":
      return "tool_use";
    case "length":
      return "max_tokens";
    default:
      return reason ?? "end_turn";
  }
}

// ── Main API ───────────────────────────────────────────────────────────

export async function generate(
  client: OpenAI,
  req: ProviderRequest,
  providerName = "openai",
  reasoningField = "reasoning",
): Promise<NormalizedResponse> {
  const messages = translateMessages(req);
  const tools = translateTools(req.tools);

  const params: Record<string, unknown> = {
    model: req.model,
    messages,
    max_tokens: req.max_tokens,
    stream: false,
  };

  if (tools) params.tools = tools;
  if (req.temperature != null) params.temperature = req.temperature;
  if (req.top_p != null) params.top_p = req.top_p;
  if (req.provider_options?.reasoning_effort != null) {
    params.reasoning_effort = req.provider_options.reasoning_effort;
  }

  const start = performance.now();
  const completion = await client.chat.completions.create(
    params as unknown as OpenAI.ChatCompletionCreateParamsNonStreaming,
  );
  const totalMs = performance.now() - start;

  const choice = completion.choices[0];
  const message = choice?.message;

  // Build content blocks
  const contentBlocks: NormalizedContentBlock[] = [];

  const msgExt = message as unknown as Record<string, unknown>;
  const reasoning = reasoningField ? (typeof msgExt?.[reasoningField] === "string" ? msgExt[reasoningField] as string : null) : null;
  if (typeof reasoning === "string" && reasoning.length > 0) {
    contentBlocks.push({ type: "thinking", thinking: reasoning });
  }

  if (message?.content) {
    contentBlocks.push({ type: "text", text: message.content });
  }
  if (message?.tool_calls) {
    for (const tc of message.tool_calls) {
      if (tc.type !== "function") continue;
      let input: unknown = {};
      try {
        input = JSON.parse(tc.function.arguments);
      } catch {
        input = {};
      }
      contentBlocks.push({
        type: "tool_use",
        id: tc.id,
        name: tc.function.name,
        input,
      });
    }
  }

  // Normalize usage
  const usage: NormalizedUsage = {
    input_tokens: completion.usage?.prompt_tokens ?? 0,
    output_tokens: completion.usage?.completion_tokens ?? 0,
    cache_read_tokens: 0,
    cache_creation_tokens: 0,
  };

  return {
    content: message?.content ?? "",
    content_blocks: contentBlocks,
    finish_reason: normalizeFinishReason(choice?.finish_reason ?? null),
    usage,
    timing: {
      total_ms: Math.round(totalMs),
      time_to_first_token_ms: Math.round(totalMs),
    },
    model: completion.model,
    provider: providerName,
  };
}

export async function stream(
  client: OpenAI,
  req: ProviderRequest,
  res: ServerResponse,
  providerName = "openai",
  reasoningField = "reasoning",
): Promise<void> {
  const messages = translateMessages(req);
  const tools = translateTools(req.tools);

  const params: Record<string, unknown> = {
    model: req.model,
    messages,
    max_tokens: req.max_tokens,
    stream: true,
    stream_options: { include_usage: true },
  };

  if (tools) params.tools = tools;
  if (req.temperature != null) params.temperature = req.temperature;
  if (req.top_p != null) params.top_p = req.top_p;
  if (req.provider_options?.reasoning_effort != null) {
    params.reasoning_effort = req.provider_options.reasoning_effort;
  }

  const start = performance.now();
  let firstTokenMs: number | null = null;
  const sdkStream = await client.chat.completions.create(
    params as unknown as OpenAI.ChatCompletionCreateParamsStreaming,
  );

  res.chunkedEncoding = false;
  res.writeHead(200, {
    "Content-Type": "application/x-ndjson",
  });

  let textContent = "";
  let finishReason = "end_turn";
  let usage: NormalizedUsage = {
    input_tokens: 0,
    output_tokens: 0,
    cache_read_tokens: 0,
    cache_creation_tokens: 0,
  };
  let model = req.model;
  let startSent = false;

  // Track tool call accumulation
  const toolCalls = new Map<
    number,
    { id: string; name: string; argChunks: string[] }
  >();

  function writeLine(event: StreamEvent): void {
    res.write(JSON.stringify(event) + "\n");
  }

  for await (const chunk of sdkStream as AsyncIterable<OpenAI.ChatCompletionChunk>) {
    if (!startSent) {
      model = chunk.model ?? model;
      writeLine({ type: "start", model });
      startSent = true;
    }

    const choice = chunk.choices?.[0];
    if (choice) {
      const delta = choice.delta;

      const deltaExt = delta as unknown as Record<string, unknown>;
      const reasoningChunk = reasoningField ? (typeof deltaExt?.[reasoningField] === "string" ? deltaExt[reasoningField] as string : null) : null;
      if (typeof reasoningChunk === "string" && reasoningChunk.length > 0) {
        if (firstTokenMs === null) {
          firstTokenMs = performance.now() - start;
        }
        writeLine({ type: "thinking", text: reasoningChunk });
      }

      // Text content
      if (delta?.content) {
        if (firstTokenMs === null) {
          firstTokenMs = performance.now() - start;
        }
        textContent += delta.content;
        writeLine({ type: "text", text: delta.content });
      }

      // Tool calls
      if (delta?.tool_calls) {
        for (const tc of delta.tool_calls) {
          if (!toolCalls.has(tc.index)) {
            toolCalls.set(tc.index, {
              id: tc.id ?? "",
              name: tc.function?.name ?? "",
              argChunks: [],
            });
          }
          const tracked = toolCalls.get(tc.index)!;
          if (tc.id) tracked.id = tc.id;
          if (tc.function?.name) tracked.name = tc.function.name;
          if (tc.function?.arguments) {
            tracked.argChunks.push(tc.function.arguments);
          }
        }
      }

      // Finish reason
      if (choice.finish_reason) {
        finishReason = normalizeFinishReason(choice.finish_reason);
      }
    }

    // Usage (final chunk with stream_options.include_usage)
    if (chunk.usage) {
      usage = {
        input_tokens: chunk.usage.prompt_tokens ?? 0,
        output_tokens: chunk.usage.completion_tokens ?? 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
      };
    }
  }

  // Emit accumulated tool calls
  for (const [, tc] of toolCalls) {
    let input: unknown = {};
    const raw = tc.argChunks.join("");
    if (raw.length > 0) {
      try {
        input = JSON.parse(raw);
      } catch {
        input = {};
      }
    }
    writeLine({
      type: "tool_use",
      id: tc.id,
      name: tc.name,
      input,
    });
  }

  // Done event
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

  res.end();
}

// ── Embed API ───────────────────────────────────────────────────────────

export async function embed(
  client: OpenAI,
  req: EmbedRequest,
): Promise<EmbedResponse> {
  const start = performance.now();
  const response = await client.embeddings.create({
    model: req.model,
    input: req.input,
  });
  const totalMs = performance.now() - start;

  const embeddings = response.data.map((d) => d.embedding);
  const totalTokens = response.usage?.total_tokens ?? 0;

  return {
    embeddings,
    usage: { total_tokens: totalTokens },
    timing: { total_ms: Math.round(totalMs) },
  };
}

// ── Image Generation API ────────────────────────────────────────────────

export async function imageGenerate(
  client: OpenAI,
  req: ImageGenerateRequest,
): Promise<ImageGenerateResponse> {
  const start = performance.now();

  const params: OpenAI.ImageGenerateParams = {
    model: req.model,
    prompt: req.prompt,
  };
  if (req.size) params.size = req.size as OpenAI.ImageGenerateParams["size"];
  if (req.quality)
    params.quality = req.quality as OpenAI.ImageGenerateParams["quality"];

  const response = await client.images.generate(params);
  const totalMs = performance.now() - start;

  const image = response.data?.[0];

  return {
    url: image?.url ?? "",
    revised_prompt: image?.revised_prompt ?? "",
    timing: { total_ms: Math.round(totalMs) },
  };
}
