import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { loadProviderRegistry } from "../src/commands/providers.ts";
import type { CommandError } from "../src/commands/types.ts";
import { loadCatalog } from "../src/llm/catalog.ts";
import {
  findEffectiveModel,
  listEffectiveModels,
} from "../src/llm/effective_catalog.ts";

function tempDir(): string {
  return mkdtempSync(path.join(tmpdir(), "shore-effective-catalog-"));
}

function makeHarness(providersToml: string, chatToml: string) {
  const root = tempDir();
  const configDir = path.join(root, "config");
  const cacheDir = path.join(root, "cache");
  fs.mkdirSync(configDir, { recursive: true });
  fs.writeFileSync(path.join(configDir, "config.toml"), `${providersToml}\n${chatToml}`);
  const configSource = { configDir };
  return {
    catalog: loadCatalog(configSource),
    providers: loadProviderRegistry(configSource),
    cacheDir,
  };
}

function writeCacheFor(
  cacheDir: string,
  provider: string,
  models: Array<string | { model_id: string; context_length?: number; max_output_tokens?: number }>,
): void {
  const cachePath = path.join(cacheDir, "providers", provider, "models.json");
  fs.mkdirSync(path.dirname(cachePath), { recursive: true });
  fs.writeFileSync(cachePath, JSON.stringify({
    version: 1,
    provider_key: provider,
    fetched_at: "2026-05-25T00:00:00Z",
    base_url: "https://example.test/v1",
    models: models.map((model) => {
      const data = typeof model === "string" ? { model_id: model } : model;
      return {
        provider_key: provider,
        model_id: data.model_id,
        sdk: "openai",
        base_url: "https://example.test/v1",
        context_length: data.context_length ?? 200_000,
        max_output_tokens: data.max_output_tokens ?? 8192,
        raw_provider_metadata: null,
        discovered_at: "2026-05-25T00:00:00Z",
      };
    }),
  }));
}

function providerConfig(name: string, extra = ""): string {
  return `
[providers.${name}]
api_key_env = "${name.toUpperCase()}_KEY"
base_url = "https://example.test/${name}/v1"
${extra}

[providers.${name}.discovery]
enabled = true
`;
}

function commandError(fn: () => unknown): CommandError {
  let thrown: unknown;
  try {
    fn();
  } catch (e) {
    thrown = e;
  }
  expect(thrown).toBeDefined();
  return thrown as CommandError;
}

describe("llm/effective_catalog", () => {
  it("synthetic_discovered_qualified_name_is_not_a_resolver_input", () => {
    const h = makeHarness(providerConfig("openrouter"), "");
    writeCacheFor(h.cacheDir, "openrouter", ["anthropic/claude-sonnet-4.5"]);

    const err = commandError(() => {
      findEffectiveModel(h, "chat.openrouter.anthropic/claude-sonnet-4.5", true);
    });
    expect(err.code).toBe("not_found");
  });

  it("short_static_alias_resolves_to_static_entry", () => {
    const h = makeHarness(
      providerConfig("openrouter"),
      `
[chat.openrouter.sonnet]
model_id = "anthropic/claude-sonnet-4.5"
cache_ttl = "1h"
max_tokens = 16384
`,
    );
    writeCacheFor(h.cacheDir, "openrouter", ["anthropic/claude-sonnet-4.5"]);

    const m = findEffectiveModel(h, "sonnet", false);
    expect(m.qualifiedName).toBe("chat.openrouter.sonnet");
    expect(m.cacheTtl).toBe("1h");
    expect(m.maxTokens).toBe(16384);
  });

  it("bare_upstream_id_returns_static_override_when_static_and_discovered_share_provider_model", () => {
    const h = makeHarness(
      providerConfig("openrouter"),
      `
[chat.openrouter.sonnet]
model_id = "anthropic/claude-sonnet-4.5"
cache_ttl = "1h"
max_tokens = 16384
`,
    );
    writeCacheFor(h.cacheDir, "openrouter", ["anthropic/claude-sonnet-4.5"]);

    const m = findEffectiveModel(h, "anthropic/claude-sonnet-4.5", false);
    expect(m.name).toBe("sonnet");
    expect(m.qualifiedName).toBe("chat.openrouter.sonnet");
    expect(m.maxTokens).toBe(16384);
  });

  it("provider_prefixed_upstream_id_returns_static_override_when_static_and_discovered_share_provider_model", () => {
    const h = makeHarness(
      providerConfig("openrouter"),
      `
[chat.openrouter.sonnet]
model_id = "anthropic/claude-sonnet-4.5"
cache_ttl = "1h"
max_tokens = 16384
`,
    );
    writeCacheFor(h.cacheDir, "openrouter", ["anthropic/claude-sonnet-4.5"]);

    const m = findEffectiveModel(h, "openrouter:anthropic/claude-sonnet-4.5", false);
    expect(m.name).toBe("sonnet");
    expect(m.qualifiedName).toBe("chat.openrouter.sonnet");
  });

  it("ambiguous_bare_id_across_providers_errors", () => {
    const h = makeHarness(
      `${providerConfig("openrouter")}\n${providerConfig("together")}`,
      "",
    );
    writeCacheFor(h.cacheDir, "openrouter", ["meta-llama/llama-3-70b"]);
    writeCacheFor(h.cacheDir, "together", ["meta-llama/llama-3-70b"]);

    const err = commandError(() => {
      findEffectiveModel(h, "meta-llama/llama-3-70b", false);
    });
    expect(err.code).toBe("invalid_request");
    expect(err.message).toContain("openrouter:meta-llama/llama-3-70b");
    expect(err.message).toContain("together:meta-llama/llama-3-70b");
  });

  it("provider_prefix_disambiguates_two_providers", () => {
    const h = makeHarness(
      `${providerConfig("openrouter")}\n${providerConfig("together")}`,
      "",
    );
    writeCacheFor(h.cacheDir, "openrouter", ["meta-llama/llama-3-70b"]);
    writeCacheFor(h.cacheDir, "together", ["meta-llama/llama-3-70b"]);

    const m = findEffectiveModel(h, "together:meta-llama/llama-3-70b", false);
    expect(m.providerKey).toBe("together");
  });

  it("one_visible_one_hidden_resolves_to_visible_not_ambiguous", () => {
    const h = makeHarness(
      `
${providerConfig("openrouter")}
[providers.together]
api_key_env = "TOGETHER_KEY"
base_url = "https://example.test/together/v1"

[providers.together.discovery]
enabled = true
ignore = ["meta-llama/*"]
`,
      "",
    );
    writeCacheFor(h.cacheDir, "openrouter", ["meta-llama/llama-3-70b"]);
    writeCacheFor(h.cacheDir, "together", ["meta-llama/llama-3-70b"]);

    const visible = findEffectiveModel(h, "meta-llama/llama-3-70b", false);
    expect(visible.providerKey).toBe("openrouter");

    const err = commandError(() => {
      findEffectiveModel(h, "meta-llama/llama-3-70b", true);
    });
    expect(err.code).toBe("invalid_request");
  });

  it("provider_prefix_for_hidden_model_rejected_without_include_hidden", () => {
    const h = makeHarness(
      `
[providers.openrouter]
api_key_env = "OR_KEY"
base_url = "https://openrouter.ai/api/v1"

[providers.openrouter.discovery]
enabled = true
ignore = ["meta-llama/*"]
`,
      "",
    );
    writeCacheFor(h.cacheDir, "openrouter", ["meta-llama/llama-3-70b"]);

    const err = commandError(() => {
      findEffectiveModel(h, "openrouter:meta-llama/llama-3-70b", false);
    });
    expect(err.code).toBe("not_found");
    expect(err.message).toContain("hidden");

    const m = findEffectiveModel(h, "openrouter:meta-llama/llama-3-70b", true);
    expect(m.providerKey).toBe("openrouter");
  });

  it("disabled_provider_and_disabled_discovery_caches_are_ignored", () => {
    const disabledProvider = makeHarness(
      `
[providers.openrouter]
enabled = false
api_key_env = "OR_KEY"
base_url = "https://openrouter.ai/api/v1"

[providers.openrouter.discovery]
enabled = true
`,
      "",
    );
    writeCacheFor(disabledProvider.cacheDir, "openrouter", ["anthropic/claude-sonnet-4.5"]);

    expect(commandError(() => {
      findEffectiveModel(disabledProvider, "anthropic/claude-sonnet-4.5", false);
    }).code).toBe("not_found");
    expect(commandError(() => {
      findEffectiveModel(disabledProvider, "openrouter:anthropic/claude-sonnet-4.5", false);
    }).code).toBe("not_found");
    expect(listEffectiveModels(disabledProvider, true).map((m) => m.resolved.modelId)).not.toContain(
      "anthropic/claude-sonnet-4.5",
    );

    const disabledDiscovery = makeHarness(
      `
[providers.openrouter]
api_key_env = "OR_KEY"
base_url = "https://openrouter.ai/api/v1"

[providers.openrouter.discovery]
enabled = false
`,
      "",
    );
    writeCacheFor(disabledDiscovery.cacheDir, "openrouter", ["anthropic/claude-sonnet-4.5"]);

    expect(commandError(() => {
      findEffectiveModel(disabledDiscovery, "anthropic/claude-sonnet-4.5", false);
    }).code).toBe("not_found");
    expect(listEffectiveModels(disabledDiscovery, true).map((m) => m.resolved.modelId)).not.toContain(
      "anthropic/claude-sonnet-4.5",
    );
  });

  it("list_effective_models_appends_discovered_after_static_without_sorting", () => {
    const h = makeHarness(
      providerConfig("aaa"),
      `
[chat.zzz.manual]
model_id = "zzz/manual"
`,
    );
    writeCacheFor(h.cacheDir, "aaa", ["aaa/model"]);

    expect(listEffectiveModels(h, false).map((entry) => entry.resolved.qualifiedName)).toEqual([
      "chat.zzz.manual",
      "chat.aaa.aaa/model",
    ]);
  });
});
