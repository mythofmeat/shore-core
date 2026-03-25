import {
  GoogleGenerativeAI,
  type Content,
  type Part,
  type FunctionDeclaration,
  type FunctionDeclarationsTool,
  type UsageMetadata,
  type GenerativeModel,
  type EnhancedGenerateContentResponse,
} from "@google/generative-ai";
import type { ServerResponse } from "node:http";
import type {
  ProviderRequest,
  NormalizedResponse,
  NormalizedContentBlock,
  NormalizedUsage,
  StreamEvent,
} from "./types.js";

// ── Provider options ──────────────────────────────────────────────────

export interface GeminiProviderOptions {
  reasoning_effort?: number;
}

// ── Client creation ──────────────────────────────────────────────────

export function createClient(apiKey: string): GoogleGenerativeAI {
  return new GoogleGenerativeAI(apiKey);
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

export function translateMessages(req: ProviderRequest): Content[] {
  const contents: Content[] = [];

  for (const msg of req.messages) {
    const role = msg.role === "assistant" ? "model" : msg.role;

    if (typeof msg.content === "string") {
      contents.push({ role, parts: [{ text: msg.content }] });
    } else if (Array.isArray(msg.content)) {
      const blocks = msg.content as ContentBlock[];
      const parts: Part[] = [];

      for (const block of blocks) {
        if (block.type === "text") {
          parts.push({ text: block.text ?? "" });
        } else if (block.type === "tool_use") {
          parts.push({
            functionCall: {
              name: block.name!,
              args: (block.input ?? {}) as object,
            },
          });
        } else if (block.type === "tool_result") {
          parts.push({
            functionResponse: {
              name: block.name ?? block.tool_use_id ?? "",
              response:
                typeof block.content === "string"
                  ? { result: block.content }
                  : ((block.content ?? {}) as object),
            },
          });
        }
      }

      if (parts.length > 0) {
        contents.push({ role, parts });
      }
    }
  }

  return contents;
}

export function translateTools(
  tools?: ProviderRequest["tools"],
): FunctionDeclarationsTool[] | undefined {
  if (!tools || tools.length === 0) return undefined;
  return [
    {
      functionDeclarations: tools.map(
        (t): FunctionDeclaration => ({
          name: t.name,
          description: t.description,
          parameters: t.input_schema as unknown as FunctionDeclaration["parameters"],
        }),
      ),
    },
  ];
}

// ── Response normalization ──────────────────────────────────────────

function normalizeFinishReason(reason: string | undefined): string {
  switch (reason) {
    case "STOP":
      return "end_turn";
    case "MAX_TOKENS":
      return "max_tokens";
    case "SAFETY":
      return "safety";
    case "RECITATION":
      return "recitation";
    case "MALFORMED_FUNCTION_CALL":
      return "tool_use";
    default:
      return reason?.toLowerCase() ?? "end_turn";
  }
}

function normalizeUsage(metadata?: UsageMetadata): NormalizedUsage {
  return {
    input_tokens: metadata?.promptTokenCount ?? 0,
    output_tokens: metadata?.candidatesTokenCount ?? 0,
    cache_read_tokens: metadata?.cachedContentTokenCount ?? 0,
    cache_creation_tokens: 0,
  };
}

function extractParts(parts: Part[]): {
  text: string;
  blocks: NormalizedContentBlock[];
} {
  let text = "";
  const blocks: NormalizedContentBlock[] = [];

  for (const part of parts) {
    if ("text" in part && part.text != null) {
      text += part.text;
      blocks.push({ type: "text", text: part.text });
    } else if ("functionCall" in part && part.functionCall) {
      blocks.push({
        type: "tool_use",
        id: `gemini_${part.functionCall.name}`,
        name: part.functionCall.name,
        input: part.functionCall.args,
      });
    }
  }

  return { text, blocks };
}

// ── Model creation helper ───────────────────────────────────────────

function getModel(
  client: GoogleGenerativeAI,
  req: ProviderRequest,
): GenerativeModel {
  const opts = req.provider_options as GeminiProviderOptions | undefined;

  const generationConfig: Record<string, unknown> = {
    maxOutputTokens: req.max_tokens,
  };
  if (req.temperature != null) generationConfig.temperature = req.temperature;
  if (req.top_p != null) generationConfig.topP = req.top_p;
  if (opts?.reasoning_effort != null) {
    generationConfig.thinkingConfig = {
      thinkingBudget: opts.reasoning_effort,
    };
  }

  const tools = translateTools(req.tools);

  return client.getGenerativeModel({
    model: req.model,
    generationConfig: generationConfig as Parameters<
      GoogleGenerativeAI["getGenerativeModel"]
    >[0]["generationConfig"],
    ...(tools ? { tools } : {}),
    ...(req.system ? { systemInstruction: req.system } : {}),
  });
}

// ── Main API ─────────────────────────────────────────────────────────

export async function generate(
  client: GoogleGenerativeAI,
  req: ProviderRequest,
): Promise<NormalizedResponse> {
  const model = getModel(client, req);
  const contents = translateMessages(req);

  const start = performance.now();
  const result = await model.generateContent({ contents });
  const totalMs = performance.now() - start;

  const response = result.response;
  const candidate = response.candidates?.[0];
  const parts = candidate?.content?.parts ?? [];
  const { text, blocks } = extractParts(parts);

  return {
    content: text,
    content_blocks: blocks,
    finish_reason: normalizeFinishReason(
      candidate?.finishReason as string | undefined,
    ),
    usage: normalizeUsage(response.usageMetadata),
    timing: {
      total_ms: Math.round(totalMs),
      time_to_first_token_ms: Math.round(totalMs),
    },
    model: req.model,
    provider: "gemini",
  };
}

export async function stream(
  client: GoogleGenerativeAI,
  req: ProviderRequest,
  res: ServerResponse,
): Promise<void> {
  const model = getModel(client, req);
  const contents = translateMessages(req);

  const start = performance.now();
  let firstTokenMs: number | null = null;
  const result = await model.generateContentStream({ contents });

  res.writeHead(200, {
    "Content-Type": "application/x-ndjson",
    "Transfer-Encoding": "chunked",
  });

  let textContent = "";
  let finishReason = "end_turn";
  let usage: NormalizedUsage = {
    input_tokens: 0,
    output_tokens: 0,
    cache_read_tokens: 0,
    cache_creation_tokens: 0,
  };

  function writeLine(event: StreamEvent): void {
    res.write(JSON.stringify(event) + "\n");
  }

  writeLine({ type: "start", model: req.model });

  const functionCalls: Array<{ name: string; args: object }> = [];

  for await (const chunk of result.stream) {
    const candidate = chunk.candidates?.[0];
    const parts = candidate?.content?.parts ?? [];

    for (const part of parts) {
      if ("text" in part && part.text != null) {
        if (firstTokenMs === null) {
          firstTokenMs = performance.now() - start;
        }
        textContent += part.text;
        writeLine({ type: "text", text: part.text });
      } else if ("functionCall" in part && part.functionCall) {
        functionCalls.push({
          name: part.functionCall.name,
          args: part.functionCall.args,
        });
      }
    }

    if (candidate?.finishReason) {
      finishReason = normalizeFinishReason(candidate.finishReason as string);
    }

    if (chunk.usageMetadata) {
      usage = normalizeUsage(chunk.usageMetadata);
    }
  }

  for (const fc of functionCalls) {
    writeLine({
      type: "tool_use",
      id: `gemini_${fc.name}`,
      name: fc.name,
      input: fc.args,
    });
  }

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
