import type { ServerResponse } from "node:http";
import type { ProviderRequest, NormalizedResponse } from "./types.js";
import {
  createClient,
  generate as openaiGenerate,
  stream as openaiStream,
} from "./openai.js";

// ── Types ────────────────────────────────────────────────────────────

export interface OpenRouterProviderOptions {
  http_referer?: string;
  x_title?: string;
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

// ── Main API ────────────────────────────────────────────────────────

export async function generate(
  req: ProviderRequest,
): Promise<NormalizedResponse> {
  const client = createOpenRouterClient(req.api_key, req.provider_options);
  return openaiGenerate(client, req, "openrouter");
}

export async function stream(
  req: ProviderRequest,
  res: ServerResponse,
): Promise<void> {
  const client = createOpenRouterClient(req.api_key, req.provider_options);
  return openaiStream(client, req, res, "openrouter");
}
