import type { ServerResponse } from "node:http";

// ── Normalized response types ─────────────────────────────────────────

export interface NormalizedResponse {
  content: string;
  content_blocks: NormalizedContentBlock[];
  finish_reason: string;
  usage: NormalizedUsage;
  timing: NormalizedTiming;
  model: string;
  provider: string;
}

export interface NormalizedContentBlock {
  type: string;
  text?: string;
  id?: string;
  name?: string;
  input?: unknown;
  thinking?: string;
  signature?: string;
}

export interface NormalizedUsage {
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_creation_tokens: number;
}

export interface NormalizedTiming {
  total_ms: number;
  time_to_first_token_ms: number;
}

// ── Stream event types ─────────────────────────────────────────────────

export type StreamEvent =
  | { type: "start"; model: string }
  | { type: "text"; text: string }
  | { type: "thinking"; text: string }
  | { type: "thinking_signature"; signature: string }
  | { type: "tool_use"; id: string; name: string; input: unknown }
  | {
      type: "done";
      content: string;
      finish_reason: string;
      usage: NormalizedUsage;
      timing: NormalizedTiming;
    };

// ── Generic request type ──────────────────────────────────────────────

export interface ProviderRequest {
  provider: string;
  model: string;
  api_key: string;
  base_url?: string | null;
  messages: Array<{ role: string; content: unknown }>;
  system?: string | null;
  tools?: Array<{
    name: string;
    description: string;
    input_schema: Record<string, unknown>;
  }>;
  max_tokens: number;
  temperature?: number | null;
  top_p?: number | null;
  provider_options?: Record<string, unknown>;
}

// ── Embed types ─────────────────────────────────────────────────────────

export interface EmbedRequest {
  provider: string;
  model: string;
  api_key: string;
  base_url?: string | null;
  input: string[];
}

export interface EmbedResponse {
  embeddings: number[][];
  usage: { total_tokens: number };
  timing: { total_ms: number };
}

// ── Image generation types ──────────────────────────────────────────────

export interface ImageGenerateRequest {
  provider: string;
  model: string;
  api_key: string;
  base_url?: string | null;
  prompt: string;
  size?: string;
  quality?: string;
  /** OpenRouter aspect ratio (e.g. "1:1", "16:9"). */
  aspect_ratio?: string;
  /** OpenRouter image size (e.g. "1K", "2K", "4K"). */
  image_size?: string;
}

export interface ImageGenerateResponse {
  url: string;
  revised_prompt: string;
  timing: { total_ms: number };
}

// ── Provider interface ─────────────────────────────────────────────────

export interface Provider {
  generate(req: ProviderRequest): Promise<NormalizedResponse>;
  stream(req: ProviderRequest, res: ServerResponse): Promise<void>;
}
