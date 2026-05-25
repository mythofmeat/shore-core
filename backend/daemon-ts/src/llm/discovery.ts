/**
 * Provider model-catalog discovery.
 *
 * Port of `backend/llm/src/discovery.rs`. Surfaces:
 *   - `discoverOpenAICompatible(baseUrl, apiKey, providerKey)`
 *   - `discoverAnthropic(baseUrl, apiKey, providerKey)`
 *   - `writeProviderCache(cachePath, cache)` (atomic write through .tmp + rename)
 *
 * The 24h auto-refresh loop (Rust `auto_discovery.rs`) is intentionally not
 * ported — see REWRITE.md Phase 9b parity gap #8. Manual refresh via the
 * `refresh_provider_models` / `refresh_all_provider_models` commands is the
 * only entry point.
 */

import fs from "node:fs";
import path from "node:path";

export const PROVIDER_CACHE_VERSION = 1;
const ANTHROPIC_VERSION = "2023-06-01";

export interface DiscoveredModel {
  provider_key: string;
  model_id: string;
  display_name?: string | null;
  sdk: string;
  base_url?: string | null;
  created_at?: number | null;
  owned_by?: string | null;
  description?: string | null;
  context_length?: number | null;
  max_output_tokens?: number | null;
  supports_tools?: boolean | null;
  supports_images?: boolean | null;
  supports_reasoning?: boolean | null;
  supports_prompt_cache?: boolean | null;
  raw_provider_metadata?: unknown;
  discovered_at: string;
}

export interface ProviderModelsCache {
  version: number;
  provider_key: string;
  fetched_at: string;
  base_url?: string | null;
  models: DiscoveredModel[];
}

export class DiscoveryError extends Error {
  constructor(
    readonly kind: "http_status" | "network" | "parse",
    readonly providerKey: string,
    message: string,
    readonly status?: number,
  ) {
    super(message);
    this.name = "DiscoveryError";
  }
}

export type DiscoveryFetcher = (
  url: string,
  init: { headers: Record<string, string> },
) => Promise<{ ok: boolean; status: number; text: () => Promise<string> }>;

const defaultFetcher: DiscoveryFetcher = (url, init) => fetch(url, init);

export async function discoverOpenAICompatible(
  providerKey: string,
  baseUrl: string,
  apiKey: string,
  fetcher: DiscoveryFetcher = defaultFetcher,
): Promise<DiscoveredModel[]> {
  const url = buildModelsUrl(baseUrl);
  return await fetchAndParse(providerKey, baseUrl, url, "openai", {
    accept: "application/json",
    authorization: `Bearer ${apiKey}`,
  }, fetcher);
}

export async function discoverAnthropic(
  providerKey: string,
  baseUrl: string,
  apiKey: string,
  fetcher: DiscoveryFetcher = defaultFetcher,
): Promise<DiscoveredModel[]> {
  const url = buildAnthropicModelsUrl(baseUrl);
  return await fetchAndParse(providerKey, baseUrl, url, "anthropic", {
    accept: "application/json",
    "anthropic-version": ANTHROPIC_VERSION,
    "x-api-key": apiKey,
  }, fetcher);
}

async function fetchAndParse(
  providerKey: string,
  baseUrl: string,
  url: string,
  sdk: "openai" | "anthropic",
  headers: Record<string, string>,
  fetcher: DiscoveryFetcher,
): Promise<DiscoveredModel[]> {
  let resp;
  try {
    resp = await fetcher(url, { headers });
  } catch (e) {
    throw new DiscoveryError("network", providerKey, (e as Error).message);
  }
  const body = await resp.text();
  if (!resp.ok) {
    throw new DiscoveryError(
      "http_status",
      providerKey,
      `discovery HTTP ${resp.status}: ${truncateForLog(body)}`,
      resp.status,
    );
  }
  let envelope: { data?: unknown };
  try {
    envelope = JSON.parse(body) as { data?: unknown };
  } catch (e) {
    throw new DiscoveryError("parse", providerKey, (e as Error).message);
  }
  const data = Array.isArray(envelope.data) ? envelope.data : [];
  const now = new Date().toISOString();
  const out: DiscoveredModel[] = [];
  for (const raw of data) {
    const mapped = mapEntry(providerKey, baseUrl, sdk, raw, now);
    if (mapped !== undefined) out.push(mapped);
  }
  return out;
}

function mapEntry(
  providerKey: string,
  baseUrl: string,
  sdk: string,
  raw: unknown,
  now: string,
): DiscoveredModel | undefined {
  if (!isPlainObject(raw)) return undefined;
  const id = typeof raw["id"] === "string" ? raw["id"] : undefined;
  if (id === undefined || id.length === 0) return undefined;

  const displayName = stringOrUndefined(raw["name"]) ?? stringOrUndefined(raw["display_name"]);
  const createdAt = numberOrUndefined(raw["created"])
    ?? parseRfc3339Seconds(stringOrUndefined(raw["created_at"]));
  const ownedBy = stringOrUndefined(raw["owned_by"]);
  const description = stringOrUndefined(raw["description"]);
  const contextLength = numberOrUndefined(raw["context_length"]);
  const topProvider = isPlainObject(raw["top_provider"]) ? raw["top_provider"] : undefined;
  const maxOutputTokens = numberOrUndefined(topProvider?.["max_completion_tokens"])
    ?? numberOrUndefined(raw["max_completion_tokens"]);

  const supportsTools = supportedParam(raw, ["tools", "tool_use", "function_calling"]);
  const supportsReasoning = supportedParam(raw, ["reasoning", "include_reasoning"]);
  const supportsImages = modalityIncludes(raw, "input", "image");
  const supportsPromptCache = supportedParam(raw, ["prompt_cache", "cache_control"]);

  return {
    provider_key: providerKey,
    model_id: id,
    ...(displayName !== undefined ? { display_name: displayName } : {}),
    sdk,
    base_url: baseUrl,
    ...(createdAt !== undefined ? { created_at: createdAt } : {}),
    ...(ownedBy !== undefined ? { owned_by: ownedBy } : {}),
    ...(description !== undefined ? { description } : {}),
    ...(contextLength !== undefined ? { context_length: contextLength } : {}),
    ...(maxOutputTokens !== undefined ? { max_output_tokens: maxOutputTokens } : {}),
    ...(supportsTools !== undefined ? { supports_tools: supportsTools } : {}),
    ...(supportsImages !== undefined ? { supports_images: supportsImages } : {}),
    ...(supportsReasoning !== undefined ? { supports_reasoning: supportsReasoning } : {}),
    ...(supportsPromptCache !== undefined ? { supports_prompt_cache: supportsPromptCache } : {}),
    raw_provider_metadata: raw,
    discovered_at: now,
  };
}

function supportedParam(raw: Record<string, unknown>, candidates: string[]): boolean | undefined {
  const arr = raw["supported_parameters"];
  if (!Array.isArray(arr)) return undefined;
  const values = arr.filter((v): v is string => typeof v === "string");
  return candidates.some((c) => values.includes(c));
}

function modalityIncludes(
  raw: Record<string, unknown>,
  side: "input" | "output",
  modality: string,
): boolean | undefined {
  const arch = raw["architecture"];
  if (!isPlainObject(arch)) return undefined;
  const arr = arch[`${side}_modalities`];
  if (!Array.isArray(arr)) return undefined;
  return arr.some((v) => v === modality);
}

function buildModelsUrl(baseUrl: string): string {
  return `${baseUrl.replace(/\/+$/, "")}/models`;
}

function buildAnthropicModelsUrl(baseUrl: string): string {
  const trimmed = baseUrl.replace(/\/+$/, "");
  return trimmed.endsWith("/v1") ? `${trimmed}/models` : `${trimmed}/v1/models`;
}

function truncateForLog(body: string): string {
  const MAX = 512;
  if (body.length <= MAX) return body;
  return `${body.slice(0, MAX)}…`;
}

/**
 * Write a provider cache atomically: serialize → write to .tmp sibling →
 * rename in. A previous good cache survives a serialization or I/O failure.
 */
export function writeProviderCache(cachePath: string, cache: ProviderModelsCache): void {
  fs.mkdirSync(path.dirname(cachePath), { recursive: true });
  const bytes = JSON.stringify(cache, null, 2);
  const tmp = `${cachePath}.tmp`;
  fs.writeFileSync(tmp, bytes);
  fs.renameSync(tmp, cachePath);
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stringOrUndefined(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function numberOrUndefined(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function parseRfc3339Seconds(value: string | undefined): number | undefined {
  if (value === undefined) return undefined;
  const ms = Date.parse(value);
  return Number.isFinite(ms) ? Math.floor(ms / 1000) : undefined;
}
