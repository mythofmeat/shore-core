import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { AutonomyRegistry } from "../src/autonomy/registry.ts";
import { loadProviderRegistry } from "../src/commands/providers.ts";
import type { CommandContext, RuntimeConfigState } from "../src/commands/types.ts";
import { dispatchCommand } from "../src/commands/dispatch.ts";
import { loadConfig } from "../src/config/loader.ts";
import { ConversationEngine, EngineRegistry } from "../src/engine/engine.ts";
import { Ledger } from "../src/ledger/ledger.ts";
import { PricingEngine } from "../src/ledger/pricing.ts";
import { loadCatalog } from "../src/llm/catalog.ts";

function tempDir(): string {
  return mkdtempSync(path.join(tmpdir(), "shore-commands-navigation-"));
}

function writeCharacter(configDir: string, name: string): void {
  const workspace = path.join(configDir, "characters", name, "workspace");
  fs.mkdirSync(workspace, { recursive: true });
  fs.writeFileSync(path.join(workspace, "SOUL.md"), `${name} soul`);
}

function makeHarness() {
  const root = tempDir();
  const configDir = path.join(root, "config");
  const dataDir = path.join(root, "data");
  const cacheDir = path.join(root, "cache");
  fs.mkdirSync(configDir, { recursive: true });
  writeCharacter(configDir, "Alice");
  writeCharacter(configDir, "Bob");
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
    characterName: "Alice",
    reloadRuntimeConfig(next) {
      runtime.config = next.config;
      runtime.catalog = next.catalog;
      runtime.providers = next.providers;
    },
  };
  const engine = engines.get("Alice");
  return { ctx, engine, ledger, root };
}

describe("command dispatcher navigation", () => {
  it("list_characters returns Rust wire shape with avatars optional", async () => {
    const { ctx, engine, ledger } = makeHarness();
    const data = await dispatchCommand({ ctx, engine, name: "list_characters", args: {} });
    expect(data).toMatchObject({
      characters: [{ name: "Alice" }, { name: "Bob" }],
    });
    ledger.close();
  });

  it("list_characters works before character selection", async () => {
    const { ctx, ledger } = makeHarness();
    const data = await dispatchCommand({ ctx: { ...ctx, characterName: undefined }, name: "list_characters", args: {} });
    expect((data as { characters: Array<{ name: string }> }).characters.map((c) => c.name)).toEqual([
      "Alice",
      "Bob",
    ]);
    ledger.close();
  });

  it("switch_character validates unknown characters", async () => {
    const { ctx, engine, ledger } = makeHarness();
    await expect(dispatchCommand({
      ctx,
      engine,
      name: "switch_character",
      args: { name: "Ghost" },
    })).rejects.toMatchObject({ code: "not_found" });
    ledger.close();
  });

  it("character_info reports workspace and definition summary", async () => {
    const { ctx, engine, ledger } = makeHarness();
    const data = await dispatchCommand({ ctx, engine, name: "character_info", args: {} });
    expect(data).toMatchObject({
      name: "Alice",
      active: true,
      has_definition: true,
      bootstrap_files: ["SOUL.md"],
      has_config_override: false,
      has_data: false,
    });
    ledger.close();
  });

  it("wraps outputs in command_output-compatible shape", async () => {
    const { ctx, engine, ledger } = makeHarness();
    const data = await dispatchCommand({ ctx, engine, name: "character_info", args: {} });
    expect({ type: "command_output", name: "character_info", data }).toMatchObject({
      type: "command_output",
      name: "character_info",
      data: { name: "Alice" },
    });
    ledger.close();
  });
});
