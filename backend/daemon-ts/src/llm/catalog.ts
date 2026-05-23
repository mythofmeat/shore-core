/**
 * Minimal model catalog parsing for phase 4a.
 *
 * The full Rust catalog (`core/config/src/models.rs`, ~700 lines) cascades:
 *   hardcoded provider defaults
 *     → `[providers.<name>]` registry overlay (Phase 4b — full prompt port)
 *     → `[chat.<provider>]` scalar keys
 *     → `[chat.<provider>.<model>]` overrides
 *
 * Phase 4a needs just enough to resolve the two test targets:
 *   - `[chat.openrouter.<m>]` → sdk=anthropic against openrouter base, model_id="anthropic/..."
 *   - `[chat.openai.<m>]`     → sdk=openai against api.openai.com
 *
 * We deliberately don't port `[providers.<name>]` registry, discovery
 * cache, sampler overlays, or the per-character preferences resolver —
 * those bring along a few thousand lines that aren't on the cache test's
 * critical path. Phase 4b absorbs them.
 *
 * Key semantic preserved from the Rust impl: `cache_ttl` defaults to
 * "1h" for the Anthropic SDK so the test config doesn't need to set it
 * (see ResolvedModel::from_parts in models.rs:293).
 */

import fs from "node:fs";
import path from "node:path";

import { parse as parseToml } from "smol-toml";

export type Sdk = "anthropic" | "openai" | "gemini" | "zai";

export interface ResolvedModel {
  name: string;
  qualifiedName: string;
  category: "chat" | "tools";
  providerKey: string;
  sdk: Sdk;
  modelId: string;
  apiKeyEnv: string | undefined;
  baseUrl: string | undefined;
  maxTokens: number | undefined;
  maxContextTokens: number | undefined;
  temperature: number | undefined;
  topP: number | undefined;
  reasoningEffort: string | undefined;
  budgetTokens: number | undefined;
  cacheTtl: string | undefined;
  openrouterProvider: Record<string, unknown> | undefined;
}

/**
 * Hardcoded provider defaults (mirrors hardcoded_defaults() in models.rs).
 * Only the subset of providers phase 4a needs.
 */
function hardcodedProviderDefaults(providerKey: string): Partial<ResolvedModel> {
  const base = { maxContextTokens: 200_000, maxTokens: 8192, temperature: 1.0 };
  switch (providerKey) {
    case "anthropic":
      return {
        ...base,
        sdk: "anthropic",
        apiKeyEnv: "ANTHROPIC_API_KEY",
      };
    case "openrouter":
      return {
        ...base,
        // OpenRouter's "default" SDK is openai-compat, but the catalog
        // overrides this per-model based on `model_id` prefix (see
        // defaultSdkForOpenRouterModel). Explicit per-model `sdk = "..."`
        // in TOML always wins over the prefix default.
        sdk: "openai",
        apiKeyEnv: "OPENROUTER_API_KEY",
        baseUrl: "https://openrouter.ai/api/v1",
      };
    case "openai":
      return {
        ...base,
        sdk: "openai",
        apiKeyEnv: "OPENAI_API_KEY",
      };
    case "deepseek":
      return {
        ...base,
        sdk: "openai",
        apiKeyEnv: "DEEPSEEK_API_KEY",
        baseUrl: "https://api.deepseek.com/v1",
      };
    case "xai":
      return {
        ...base,
        sdk: "openai",
        apiKeyEnv: "XAI_API_KEY",
        baseUrl: "https://api.x.ai/v1",
      };
    default:
      return { ...base };
  }
}

interface CatalogTables {
  chat?: Record<string, unknown>;
  tools?: Record<string, unknown>;
}

/**
 * Parse `[chat.<provider>.<model>]` and `[tools.<provider>.<model>]`
 * sections of the (already-merged) raw config table.
 */
export function parseCatalog(rawConfig: Record<string, unknown>): Map<string, ResolvedModel> {
  const out = new Map<string, ResolvedModel>();
  const tables = rawConfig as CatalogTables;
  if (tables.chat) parseCategory("chat", tables.chat as Record<string, unknown>, out);
  if (tables.tools) parseCategory("tools", tables.tools as Record<string, unknown>, out);
  return out;
}

function parseCategory(
  category: "chat" | "tools",
  providers: Record<string, unknown>,
  out: Map<string, ResolvedModel>,
): void {
  for (const [providerKey, providerVal] of Object.entries(providers)) {
    if (!isObject(providerVal)) continue;

    const hardcoded = hardcodedProviderDefaults(providerKey);
    const providerScalars = pickScalars(providerVal);

    for (const [modelName, modelVal] of Object.entries(providerVal)) {
      if (!isObject(modelVal)) continue; // scalar (provider default), not a model entry

      const modelFields = pickScalars(modelVal);
      const modelId =
        typeof modelFields["model_id"] === "string" ? modelFields["model_id"] : undefined;
      if (!modelId) {
        throw new Error(
          `[${category}.${providerKey}.${modelName}] is missing required field model_id`,
        );
      }

      const merged: Record<string, unknown> = {
        ...hardcoded,
        ...providerScalars,
        ...modelFields,
      };

      // SDK resolution priority:
      //   1. Explicit `sdk = "..."` in per-model or per-provider TOML
      //   2. For OpenRouter: derive from model_id prefix (anthropic/* →
      //      anthropic SDK, etc.) so users get cache-correct routing out
      //      of the box.
      //   3. Hardcoded provider default.
      const userExplicitSdk =
        "sdk" in modelFields
          ? modelFields["sdk"]
          : "sdk" in providerScalars
            ? providerScalars["sdk"]
            : undefined;
      let sdk: Sdk;
      if (userExplicitSdk !== undefined) {
        sdk = parseSdk(userExplicitSdk, hardcoded.sdk ?? "openai");
      } else if (providerKey === "openrouter") {
        sdk = defaultSdkForOpenRouterModel(modelId);
      } else {
        sdk = hardcoded.sdk ?? "openai";
      }
      const qualifiedName = `${category}.${providerKey}.${modelName}`;

      // cache_ttl default = "1h" for Anthropic SDK unless explicitly set.
      // Empty string disables caching.
      let cacheTtl: string | undefined;
      if ("cache_ttl" in modelFields) {
        cacheTtl = String(modelFields["cache_ttl"]);
      } else if ("cache_ttl" in providerScalars) {
        cacheTtl = String(providerScalars["cache_ttl"]);
      } else if (sdk === "anthropic") {
        cacheTtl = "1h";
      }

      out.set(qualifiedName, {
        name: modelName,
        qualifiedName,
        category,
        providerKey,
        sdk,
        modelId,
        apiKeyEnv:
          (merged["api_key_env"] as string | undefined) ?? hardcoded.apiKeyEnv,
        baseUrl: (merged["base_url"] as string | undefined) ?? hardcoded.baseUrl,
        maxTokens:
          asNumber(merged["max_tokens"]) ?? hardcoded.maxTokens,
        maxContextTokens:
          asNumber(merged["max_context_tokens"]) ?? hardcoded.maxContextTokens,
        temperature:
          asNumber(merged["temperature"]) ?? hardcoded.temperature,
        topP: asNumber(merged["top_p"]) ?? hardcoded.topP,
        reasoningEffort:
          typeof merged["reasoning_effort"] === "string"
            ? (merged["reasoning_effort"] as string)
            : undefined,
        budgetTokens: asNumber(merged["budget_tokens"]),
        cacheTtl,
        openrouterProvider: isObject(merged["openrouter_provider"])
          ? (merged["openrouter_provider"] as Record<string, unknown>)
          : undefined,
      });
    }
  }
}

function pickScalars(obj: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(obj)) {
    // `openrouter_provider` is a dict-valued config field, not a model
    // sub-table — preserve it (matches RESERVED_DICT_KEYS in Rust).
    if (k === "openrouter_provider") {
      out[k] = v;
      continue;
    }
    if (!isObject(v)) out[k] = v;
  }
  return out;
}

function parseSdk(v: unknown, fallback: Sdk): Sdk {
  if (v === "anthropic" || v === "openai" || v === "gemini" || v === "zai") return v;
  if (v === "deepseek" || v === "zhipuai") return "openai"; // deprecated aliases
  return fallback;
}

/**
 * Map an OpenRouter model id to the SDK that should front it. First-match
 * wins, so put exceptions above their parent prefix. Per-model TOML
 * `sdk = "..."` always overrides this.
 *
 * The `gemini` and `zai` entries are deliberately speculative — adapter
 * implementations land in later phases. Until then, requests for those
 * models error at provider-construction time (not catalog resolution),
 * which is the right failure surface. Users hitting it before adapters
 * ship can pin `sdk = "openai"` per-model in TOML as the escape hatch.
 */
export function defaultSdkForOpenRouterModel(modelId: string): Sdk {
  if (modelId.startsWith("anthropic/")) return "anthropic";
  if (modelId.startsWith("google/")) return "gemini";
  if (modelId.startsWith("z-ai/")) return "zai";
  return "openai";
}

function asNumber(v: unknown): number | undefined {
  return typeof v === "number" ? v : undefined;
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/**
 * Load and merge config.toml + conf.d/*.toml from `configDir` and return
 * the catalog. Mirrors the merging done in config/loader.ts but exposes
 * the raw catalog so callers can resolve by qualified name.
 */
export function loadCatalog(configDir: string): Map<string, ResolvedModel> {
  const raw = readMergedConfig(configDir);
  return parseCatalog(raw);
}

function readMergedConfig(configDir: string): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  const baseFile = path.join(configDir, "config.toml");
  if (fs.existsSync(baseFile)) {
    deepMerge(out, parseToml(fs.readFileSync(baseFile, "utf8")) as Record<string, unknown>);
  }
  const confDir = path.join(configDir, "conf.d");
  if (fs.existsSync(confDir)) {
    for (const name of fs.readdirSync(confDir).filter((n) => n.endsWith(".toml")).sort()) {
      const content = fs.readFileSync(path.join(confDir, name), "utf8");
      deepMerge(out, parseToml(content) as Record<string, unknown>);
    }
  }
  return out;
}

function deepMerge(target: Record<string, unknown>, src: Record<string, unknown>): void {
  for (const [k, v] of Object.entries(src)) {
    const prev = target[k];
    if (isObject(prev) && isObject(v)) {
      const nested = { ...prev };
      deepMerge(nested, v);
      target[k] = nested;
    } else if (Array.isArray(prev) && Array.isArray(v)) {
      target[k] = [...prev, ...v];
    } else {
      target[k] = v;
    }
  }
}

/**
 * Resolve a qualified name (`chat.openrouter.haiku45`) or a short alias
 * (`haiku45`) against the catalog. For short names, errors on ambiguity.
 */
export function resolveModel(
  catalog: Map<string, ResolvedModel>,
  name: string,
): ResolvedModel {
  const exact = catalog.get(name);
  if (exact) return exact;

  const matches: ResolvedModel[] = [];
  for (const m of catalog.values()) {
    if (m.name === name) matches.push(m);
  }
  if (matches.length === 1) return matches[0]!;
  if (matches.length === 0) throw new Error(`model not found: ${name}`);
  throw new Error(
    `ambiguous model name "${name}" — matches: ${matches.map((m) => m.qualifiedName).join(", ")}`,
  );
}
