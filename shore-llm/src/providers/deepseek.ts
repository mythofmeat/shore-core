import type { ServerResponse } from "node:http";
import type { ProviderRequest, NormalizedResponse } from "./types.js";
import { createClient, generate as openaiGenerate, stream as openaiStream } from "./openai.js";

export async function generate(req: ProviderRequest): Promise<NormalizedResponse> {
  const client = createClient(req.api_key, req.base_url);
  return openaiGenerate(client, req, "deepseek", "reasoning_content");
}

export async function stream(req: ProviderRequest, res: ServerResponse): Promise<void> {
  const client = createClient(req.api_key, req.base_url);
  return openaiStream(client, req, res, "deepseek", "reasoning_content");
}
