import type { ResolvedModel } from "../llm/catalog.ts";
import type { SamplerSettings } from "./types.ts";

export function applySamplerOverlay(
  model: ResolvedModel,
  overlay: SamplerSettings,
): ResolvedModel {
  const patched: ResolvedModel = { ...model };
  if (overlay.temperature !== undefined) patched.temperature = overlay.temperature;
  if (overlay.top_p !== undefined) patched.topP = overlay.top_p;
  if (overlay.reasoning_effort !== undefined) {
    patched.reasoningEffort = overlay.reasoning_effort === "off"
      ? undefined
      : overlay.reasoning_effort;
  }
  if (overlay.budget_tokens !== undefined) patched.budgetTokens = overlay.budget_tokens;
  if (overlay.max_tokens !== undefined) patched.maxTokens = overlay.max_tokens;
  if (overlay.cache_ttl !== undefined) patched.cacheTtl = overlay.cache_ttl;
  return patched;
}
