import type { ServerResponse } from "node:http";
import type {
  ProviderRequest,
  NormalizedResponse,
  ImageGenerateRequest,
  ImageGenerateResponse,
} from "./types.js";
import {
  createClient,
  generate as openaiGenerate,
  stream as openaiStream,
} from "./openai.js";
import {
  createClient as createAnthropicClient,
  generate as anthropicGenerate,
  stream as anthropicStream,
} from "./anthropic.js";
import type { GenerateRequest } from "./anthropic.js";

// ── Types ────────────────────────────────────────────────────────────

export interface OpenRouterProviderOptions {
  http_referer?: string;
  x_title?: string;
  openrouter_provider?: Record<string, unknown>;
}

// ── Client creation ──────────────────────────────────────────────────

export function createOpenRouterClient(
  apiKey: string,
  providerOptions?: Record<string, unknown>,
) {
  const opts = providerOptions as OpenRouterProviderOptions | undefined;
  const headers: Record<string, string> = {};
  if (opts?.http_referer) headers["HTTP-Referer"] = opts.http_referer;
  if (opts?.x_title) headers["X-Title"] = opts.x_title;

  return createClient(apiKey, "https://openrouter.ai/api/v1", headers);
}

// ── Helpers ─────────────────────────────────────────────────────────

/** Build extra body params for the OpenRouter API (e.g. provider routing). */
function extraBodyParams(
  providerOptions?: Record<string, unknown>,
): Record<string, unknown> | undefined {
  const opts = providerOptions as OpenRouterProviderOptions | undefined;
  if (opts?.openrouter_provider) {
    return { provider: opts.openrouter_provider };
  }
  return undefined;
}

// ── Main API ────────────────────────────────────────────────────────

const OPENROUTER_BASE_URL = "https://openrouter.ai/api/v1";

/**
 * Anthropic models on OpenRouter use the Anthropic SDK so that
 * cache_control breakpoints, thinking config, and cache token
 * reporting all work natively.  Non-Anthropic models use the
 * OpenAI-compatible path.
 */
function isAnthropicModel(model: string): boolean {
  return model.startsWith("anthropic/");
}

export async function generate(
  req: ProviderRequest,
): Promise<NormalizedResponse> {
  if (isAnthropicModel(req.model)) {
    const client = createAnthropicClient(req.api_key, OPENROUTER_BASE_URL);
    return anthropicGenerate(
      client,
      req as unknown as GenerateRequest,
      "openrouter",
      extraBodyParams(req.provider_options),
    );
  }
  const client = createOpenRouterClient(req.api_key, req.provider_options);
  return openaiGenerate(client, req, "openrouter", "reasoning", extraBodyParams(req.provider_options));
}

export async function stream(
  req: ProviderRequest,
  res: ServerResponse,
): Promise<void> {
  if (isAnthropicModel(req.model)) {
    const client = createAnthropicClient(req.api_key, OPENROUTER_BASE_URL);
    return anthropicStream(
      client,
      req as unknown as GenerateRequest,
      res,
      extraBodyParams(req.provider_options),
    );
  }
  const client = createOpenRouterClient(req.api_key, req.provider_options);
  return openaiStream(client, req, res, "openrouter", "reasoning", extraBodyParams(req.provider_options));
}

// ── Image generation ────────────────────────────────────────────────

interface OpenRouterImageMessage {
  role: string;
  content?: string;
  images?: Array<{
    type: string;
    image_url: { url: string };
  }>;
}

interface OpenRouterChatResponse {
  choices: Array<{ message: OpenRouterImageMessage }>;
}

/**
 * Generate an image via OpenRouter's chat completions API.
 *
 * OpenRouter routes image generation through `/v1/chat/completions` with
 * a `modalities` parameter — NOT the OpenAI `/v1/images/generations` endpoint.
 * Images are returned as base64 data URLs in `message.images[]`.
 *
 * Text+image models (Gemini, GPT-5 Image) use `modalities: ["image", "text"]`.
 * Image-only models (Flux, Sourceful) use `modalities: ["image"]`.
 * We try text+image first and fall back to image-only on 404.
 */
export async function imageGenerate(
  req: ImageGenerateRequest,
): Promise<ImageGenerateResponse> {
  const start = performance.now();

  // Try text+image modalities first, fall back to image-only.
  const result =
    (await tryImageGenerate(req, ["image", "text"])) ??
    (await tryImageGenerate(req, ["image"]));

  if (!result) {
    throw new Error("OpenRouter image generation failed for both modality modes");
  }

  const totalMs = performance.now() - start;
  return { ...result, timing: { total_ms: Math.round(totalMs) } };
}

async function tryImageGenerate(
  req: ImageGenerateRequest,
  modalities: string[],
): Promise<Omit<ImageGenerateResponse, "timing"> | null> {
  const body: Record<string, unknown> = {
    model: req.model,
    messages: [{ role: "user", content: req.prompt }],
    modalities,
  };

  // Only include image_config when at least one field is set.
  const imageConfig: Record<string, string> = {};
  if (req.aspect_ratio) imageConfig.aspect_ratio = req.aspect_ratio;
  if (req.image_size) imageConfig.image_size = req.image_size;
  if (Object.keys(imageConfig).length > 0) {
    body.image_config = imageConfig;
  }

  const response = await fetch("https://openrouter.ai/api/v1/chat/completions", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${req.api_key}`,
    },
    body: JSON.stringify(body),
  });

  if (!response.ok) {
    const text = await response.text();
    // 404 with "output modalities" → model doesn't support this mode, try next.
    if (response.status === 404 && text.includes("output modalities")) {
      return null;
    }
    throw new Error(
      `OpenRouter image generation failed (${response.status}): ${text}`,
    );
  }

  const data = (await response.json()) as OpenRouterChatResponse;

  const message = data.choices?.[0]?.message;
  const imageUrl = message?.images?.[0]?.image_url?.url ?? "";
  const revisedPrompt = message?.content ?? "";

  if (!imageUrl) {
    throw new Error("OpenRouter response contained no image data");
  }

  return { url: imageUrl, revised_prompt: revisedPrompt };
}
