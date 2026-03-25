import type { ServerResponse } from "node:http";
import type { ProviderRequest, NormalizedResponse } from "./types.js";
import {
  createClient,
  generate as openaiGenerate,
  stream as openaiStream,
} from "./openai.js";

const ZHIPUAI_BASE_URL = "https://open.bigmodel.cn/api/paas/v4";

// ── Main API ────────────────────────────────────────────────────────

export async function generate(
  req: ProviderRequest,
): Promise<NormalizedResponse> {
  const client = createClient(req.api_key, req.base_url ?? ZHIPUAI_BASE_URL);
  return openaiGenerate(client, req, "zhipuai");
}

export async function stream(
  req: ProviderRequest,
  res: ServerResponse,
): Promise<void> {
  const client = createClient(req.api_key, req.base_url ?? ZHIPUAI_BASE_URL);
  return openaiStream(client, req, res, "zhipuai");
}
