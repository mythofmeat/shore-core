/**
 * Model capability matrix — typed accessors over the SINGLE SOURCE OF TRUTH
 * `core/config/capabilities.toml`, which `bun build` inlines into the sidecar
 * bundle (so this is compiled in, not read from disk at runtime).
 *
 * The Rust daemon reads the same file (`core/config/src/capabilities.rs`); the
 * model parser + rule evaluator here mirror the Rust ones, kept in lockstep by
 * the cross-language parity fixtures (this module's tests + the Rust tests).
 *
 * Each adapter calls into here instead of hand-coding effort/thinking tables.
 */

// Bun resolves this `.toml` import at build time and inlines the parsed object.
import rawCaps from "../../../../core/config/capabilities.toml";

export type Sdk = "anthropic" | "openai" | "openrouter" | "gemini" | "zai";
type ClaudeFamily = "opus" | "sonnet" | "haiku";

interface SdkEffort {
  domain: readonly string[];
  fold?: Record<string, string>;
  budget?: Record<string, number>;
}

interface ModelOverride {
  match: string;
  domain: readonly string[];
}

interface ClaudeRule {
  contains?: string;
  family?: string;
  min_major?: number;
  min_minor?: number;
  max_major?: number;
  max_minor?: number;
  adaptive?: boolean;
  enabled?: boolean;
  rejects_sampling?: boolean;
}

interface CapabilitiesDoc {
  reasoning_effort: {
    anthropic: SdkEffort;
    openai: SdkEffort;
    openrouter: SdkEffort;
    gemini: SdkEffort;
    zai: SdkEffort;
    model_override?: ModelOverride[];
  };
  claude: {
    default_adaptive: boolean;
    default_enabled: boolean;
    default_rejects_sampling: boolean;
    thinking_rule?: ClaudeRule[];
    sampler_rule?: ClaudeRule[];
  };
}

const caps = rawCaps as CapabilitiesDoc;

function sdkEffort(sdk: Sdk): SdkEffort {
  switch (sdk) {
    case "anthropic":
      return caps.reasoning_effort.anthropic;
    case "openai":
      return caps.reasoning_effort.openai;
    case "openrouter":
      return caps.reasoning_effort.openrouter;
    case "gemini":
      return caps.reasoning_effort.gemini;
    case "zai":
      return caps.reasoning_effort.zai;
  }
}

// ── reasoning_effort ─────────────────────────────────────────────────────────

/** Accepted reasoning_effort values for an sdk, honoring a per-model override
 *  (first whose `match` is a substring of `modelId` wins). */
export function reasoningDomain(sdk: Sdk, modelId?: string): readonly string[] {
  if (modelId !== undefined) {
    const lower = modelId.toLowerCase();
    for (const ov of caps.reasoning_effort.model_override ?? []) {
      if (lower.includes(ov.match.toLowerCase())) return ov.domain;
    }
  }
  return sdkEffort(sdk).domain;
}

/** The wire value to send for `effort` on `sdk` (applies the fold map; identity
 *  for in-domain values without a fold), or `undefined` if out of domain. */
export function foldEffort(sdk: Sdk, effort: string, modelId?: string): string | undefined {
  if (!reasoningDomain(sdk, modelId).includes(effort)) return undefined;
  return sdkEffort(sdk).fold?.[effort] ?? effort;
}

/** Anthropic "enabled"-mode `budget_tokens` for a named effort (default 8192). */
export function effortBudget(effort: string): number {
  return caps.reasoning_effort.anthropic.budget?.[effort] ?? 8192;
}

/** The Gemini thinkingLevel name for `effort` (case-insensitive), or undefined. */
export function geminiLevelName(effort: string): string | undefined {
  const e = effort.toLowerCase();
  return reasoningDomain("gemini").includes(e) ? e : undefined;
}

// ── Claude version rules ─────────────────────────────────────────────────────

interface ClaudeVersion {
  family: ClaudeFamily;
  major: number;
  minor: number;
}

/** Mirror of the Rust `parse_claude_version`: see that doc-comment. */
export function parseClaudeModel(modelId: string): ClaudeVersion | undefined {
  const slash = modelId.lastIndexOf("/");
  const lower = (slash >= 0 ? modelId.slice(slash + 1) : modelId).toLowerCase();

  let family: ClaudeFamily | undefined;
  if (lower.includes("opus")) family = "opus";
  else if (lower.includes("sonnet")) family = "sonnet";
  else if (lower.includes("haiku")) family = "haiku";
  else return undefined;

  let major: number | undefined;
  let minor = 0;
  for (const tok of lower.split(/[-./]/)) {
    if (tok.length === 0 || tok.length > 2 || !/^[0-9]+$/.test(tok)) continue;
    const n = Number.parseInt(tok, 10);
    if (Number.isNaN(n)) continue;
    if (major === undefined) major = n;
    else {
      minor = n;
      break;
    }
  }
  if (major === undefined) return undefined;
  return { family, major, minor };
}

function familyInSet(set: string, family: ClaudeFamily): boolean {
  return set.split("|").includes(family);
}

function ruleMatches(rule: ClaudeRule, idLower: string, v: ClaudeVersion | undefined): boolean {
  if (rule.contains !== undefined && !idLower.includes(rule.contains)) return false;
  const needsVersion =
    rule.family !== undefined || rule.min_major !== undefined || rule.max_major !== undefined;
  if (needsVersion) {
    if (v === undefined) return false;
    if (rule.family !== undefined && !familyInSet(rule.family, v.family)) return false;
    if (rule.min_major !== undefined) {
      const minMinor = rule.min_minor ?? 0;
      if (v.major < rule.min_major || (v.major === rule.min_major && v.minor < minMinor)) return false;
    }
    if (rule.max_major !== undefined) {
      const maxMinor = rule.max_minor ?? Number.MAX_SAFE_INTEGER;
      if (v.major > rule.max_major || (v.major === rule.max_major && v.minor > maxMinor)) return false;
    }
  }
  return rule.contains !== undefined || needsVersion;
}

/** Anthropic per-model thinking-mode capability. Mirrors Rust `claude_thinking_caps`. */
export function claudeThinkingCaps(model: string): { adaptive: boolean; enabled: boolean } {
  const lower = model.toLowerCase();
  const v = parseClaudeModel(model);
  for (const rule of caps.claude.thinking_rule ?? []) {
    if (ruleMatches(rule, lower, v)) {
      return {
        adaptive: rule.adaptive ?? caps.claude.default_adaptive,
        enabled: rule.enabled ?? caps.claude.default_enabled,
      };
    }
  }
  return { adaptive: caps.claude.default_adaptive, enabled: caps.claude.default_enabled };
}

/** Whether the model's wire rejects sampler knobs. Mirrors Rust `claude_rejects_sampling`.
 *  Exported for the cross-language parity fixture; the sidecar receives requests
 *  with samplers already stripped by the daemon, so no adapter calls this. */
export function claudeRejectsSampling(model: string): boolean {
  const lower = model.toLowerCase();
  const v = parseClaudeModel(model);
  for (const rule of caps.claude.sampler_rule ?? []) {
    if (ruleMatches(rule, lower, v)) {
      return rule.rejects_sampling ?? caps.claude.default_rejects_sampling;
    }
  }
  return caps.claude.default_rejects_sampling;
}
