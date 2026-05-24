import type { AutonomyRegistry } from "../autonomy/registry.ts";
import type { LoadedConfig } from "../config/loader.ts";
import type { ConversationEngine, EngineRegistry } from "../engine/engine.ts";
import type { Ledger } from "../ledger/ledger.ts";
import type { PricingEngine } from "../ledger/pricing.ts";
import type { ResolvedModel } from "../llm/catalog.ts";
import type { ProviderRegistry } from "./providers.ts";

export type ErrorCode =
  | "invalid_request"
  | "not_found"
  | "internal_error"
  | "busy"
  | "provider_error";

export class CommandError extends Error {
  constructor(
    readonly code: ErrorCode,
    message: string,
  ) {
    super(message);
    this.name = "CommandError";
  }
}

export interface ConfigSource {
  configDir: string;
  configFile?: string;
}

export interface RuntimeConfigState {
  config: LoadedConfig;
  catalog: Map<string, ResolvedModel>;
  providers: ProviderRegistry;
}

export interface CommandContext {
  configSource: ConfigSource;
  runtime: RuntimeConfigState;
  dataDir: string;
  cacheDir: string;
  engines: EngineRegistry;
  autonomy: AutonomyRegistry;
  ledger: Ledger;
  pricing: PricingEngine;
  characterName?: string;
  activeModel?: string;
  reloadRuntimeConfig: (next: RuntimeConfigState) => void;
}

export interface DispatchInput {
  ctx: CommandContext;
  engine?: ConversationEngine;
  name: string;
  args: unknown;
}

export function asArgs(args: unknown): Record<string, unknown> {
  if (typeof args === "object" && args !== null && !Array.isArray(args)) {
    return args as Record<string, unknown>;
  }
  return {};
}

export function requiredString(
  args: Record<string, unknown>,
  key: string,
  message = `Missing required argument: ${key}`,
): string {
  const value = args[key];
  if (typeof value !== "string" || value.length === 0) {
    throw new CommandError("invalid_request", message);
  }
  return value;
}

export function engineRequired(
  engine: ConversationEngine | undefined,
  command: string,
): ConversationEngine {
  if (engine === undefined) {
    throw new CommandError(
      "invalid_request",
      `Command '${command}' requires a character`,
    );
  }
  return engine;
}

export function mapUnknownError(e: unknown): never {
  if (e instanceof CommandError) throw e;
  const message = (e as Error).message ?? String(e);
  if (/not found/i.test(message)) {
    throw new CommandError("not_found", message);
  }
  if (/compaction already running/i.test(message)) {
    throw new CommandError("busy", message);
  }
  if (
    /out of range|must be|missing|required|unknown|no alternate|no assistant|no messages|insufficient messages|private conversation/i
      .test(message)
  ) {
    throw new CommandError("invalid_request", message);
  }
  throw new CommandError("internal_error", message);
}

export function toSnakeModel(model: ResolvedModel): Record<string, unknown> {
  return {
    name: model.name,
    qualified_name: model.qualifiedName,
    category: model.category,
    provider_key: model.providerKey,
    sdk: model.sdk,
    model_id: model.modelId,
    api_key_env: model.apiKeyEnv ?? null,
    base_url: model.baseUrl ?? null,
    max_tokens: model.maxTokens ?? null,
    max_context_tokens: model.maxContextTokens ?? null,
    temperature: model.temperature ?? null,
    top_p: model.topP ?? null,
    reasoning_effort: model.reasoningEffort ?? null,
    budget_tokens: model.budgetTokens ?? null,
    cache_ttl: model.cacheTtl ?? null,
    openrouter_provider: model.openrouterProvider ?? null,
  };
}
