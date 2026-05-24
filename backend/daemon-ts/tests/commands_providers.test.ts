import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { AutonomyRegistry } from "../src/autonomy/registry.ts";
import { loadProviderRegistry, providerCachePath } from "../src/commands/providers.ts";
import type { CommandContext, RuntimeConfigState } from "../src/commands/types.ts";
import { dispatchCommand } from "../src/commands/dispatch.ts";
import { loadConfig } from "../src/config/loader.ts";
import { EngineRegistry } from "../src/engine/engine.ts";
import { Ledger } from "../src/ledger/ledger.ts";
import { PricingEngine } from "../src/ledger/pricing.ts";
import { loadCatalog } from "../src/llm/catalog.ts";

function tempDir(): string {
  return mkdtempSync(path.join(tmpdir(), "shore-commands-providers-"));
}

function makeHarness() {
  const root = tempDir();
  const configDir = path.join(root, "config");
  const dataDir = path.join(root, "data");
  const cacheDir = path.join(root, "cache");
  fs.mkdirSync(configDir, { recursive: true });
  fs.writeFileSync(path.join(configDir, "config.toml"), `
[providers.openrouter]
sdk = "openai"
base_url = "https://openrouter.ai/api/v1"

[[providers.openrouter.keys]]
name = "main"
env = "SHORE_COMMANDS_PROVIDER_KEY"
warn_on_fallback = true

[providers.openrouter.discovery]
enabled = true
ignore = ["*", "!anthropic/*"]

[providers.disabled]
enabled = false
api_key_env = "DISABLED_KEY"

[chat.openrouter.kimi]
model_id = "moonshotai/kimi-k2"
`);
  const configSource = { configDir };
  const runtime: RuntimeConfigState = {
    config: loadConfig(configSource),
    catalog: loadCatalog(configSource),
    providers: loadProviderRegistry(configSource),
  };
  const engines = new EngineRegistry(dataDir);
  const ledger = Ledger.openInMemory();
  const ctx: CommandContext = {
    configSource,
    runtime,
    dataDir,
    cacheDir,
    engines,
    autonomy: new AutonomyRegistry(),
    ledger,
    pricing: new PricingEngine(ledger),
    reloadRuntimeConfig(next) {
      runtime.config = next.config;
      runtime.catalog = next.catalog;
      runtime.providers = next.providers;
    },
  };
  return { ctx, ledger, cacheDir };
}

function writeCache(cacheDir: string): void {
  const p = providerCachePath(cacheDir, "openrouter");
  fs.mkdirSync(path.dirname(p), { recursive: true });
  fs.writeFileSync(p, JSON.stringify({
    version: 1,
    provider_key: "openrouter",
    fetched_at: "2026-05-24T00:00:00Z",
    base_url: "https://openrouter.ai/api/v1",
    models: [
      {
        provider_key: "openrouter",
        model_id: "anthropic/claude-sonnet-4.5",
        display_name: "Claude Sonnet 4.5",
        sdk: "openai",
        context_length: 200000,
        max_output_tokens: 8192,
        discovered_at: "2026-05-24T00:00:00Z",
      },
      {
        provider_key: "openrouter",
        model_id: "meta-llama/free",
        sdk: "openai",
        discovered_at: "2026-05-24T00:00:00Z",
      },
    ],
  }));
}

describe("command dispatcher providers", () => {
  it("list_providers hides secrets and reports cache summary", async () => {
    const { ctx, ledger, cacheDir } = makeHarness();
    process.env["SHORE_COMMANDS_PROVIDER_KEY"] = "sk-test-secret";
    writeCache(cacheDir);

    const data = await dispatchCommand({ ctx, name: "list_providers", args: {} });
    const serialized = JSON.stringify(data);
    expect(serialized).not.toContain("sk-test-secret");
    expect(serialized).not.toContain("SHORE_COMMANDS_PROVIDER_KEY");
    expect(data).toMatchObject({
      providers: [
        {
          name: "disabled",
          enabled: false,
        },
        {
          name: "openrouter",
          enabled: true,
          discovery_enabled: true,
          keys: [{ name: "main", env_set: true, warn_on_fallback: true }],
          cache: { present: true, models: 2, visible: 1, hidden: 1 },
        },
      ],
    });
    delete process.env["SHORE_COMMANDS_PROVIDER_KEY"];
    ledger.close();
  });

  it("list_provider_models merges static, discovered, and hidden rows", async () => {
    const { ctx, ledger, cacheDir } = makeHarness();
    writeCache(cacheDir);
    const data = await dispatchCommand({
      ctx,
      name: "list_provider_models",
      args: { provider: "openrouter" },
    });
    expect(data).toMatchObject({
      provider: "openrouter",
      include_hidden: false,
      cache: { fetched_at: "2026-05-24T00:00:00Z", model_count: 2 },
    });
    expect((data as { static: unknown[] }).static).toHaveLength(1);
    expect((data as { discovered: Array<{ model_id: string }> }).discovered.map((m) => m.model_id)).toEqual([
      "anthropic/claude-sonnet-4.5",
    ]);
    expect((data as { hidden: unknown[] }).hidden).toHaveLength(1);
    ledger.close();
  });

  it("provider refresh commands are explicit TS stubs", async () => {
    const { ctx, ledger } = makeHarness();
    const one = await dispatchCommand({
      ctx,
      name: "refresh_provider_models",
      args: { provider: "openrouter" },
    });
    expect(one).toMatchObject({
      provider: "openrouter",
      status: "not_implemented",
      message: "provider model refresh not implemented in TS daemon",
    });

    const all = await dispatchCommand({ ctx, name: "refresh_all_provider_models", args: {} });
    expect(all).toMatchObject({
      results: [{ provider: "openrouter", ok: false }],
      skipped: [{ provider: "disabled", reason: "disabled" }],
    });
    ledger.close();
  });

  it("list_provider_models validates provider argument", async () => {
    const { ctx, ledger } = makeHarness();
    await expect(dispatchCommand({ ctx, name: "list_provider_models", args: {} }))
      .rejects.toMatchObject({ code: "invalid_request" });
    await expect(dispatchCommand({
      ctx,
      name: "list_provider_models",
      args: { provider: "ghost" },
    })).rejects.toMatchObject({ code: "not_found" });
    ledger.close();
  });
});
