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
import { EngineRegistry } from "../src/engine/engine.ts";
import type { Message } from "../src/engine/types.ts";
import { Ledger } from "../src/ledger/ledger.ts";
import { PricingEngine } from "../src/ledger/pricing.ts";
import { loadCatalog } from "../src/llm/catalog.ts";
import { characterPreferencesPath, loadPreferences, modelPreference } from "../src/preferences/index.ts";

function tempDir(): string {
  return mkdtempSync(path.join(tmpdir(), "shore-commands-state-"));
}

function makeHarness() {
  const root = tempDir();
  const configDir = path.join(root, "config");
  const dataDir = path.join(root, "data");
  const cacheDir = path.join(root, "cache");
  fs.mkdirSync(path.join(configDir, "characters", "TestChar", "workspace"), { recursive: true });
  fs.writeFileSync(path.join(configDir, "characters", "TestChar", "workspace", "SOUL.md"), "soul");
  fs.writeFileSync(path.join(configDir, "config.toml"), `
[defaults]
model = "sonnet"

[chat.anthropic.sonnet]
model_id = "claude-sonnet-4-20250514"

[chat.openrouter.gpt4o]
model_id = "openai/gpt-4o"
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
    characterName: "TestChar",
    reloadRuntimeConfig(next) {
      runtime.config = next.config;
      runtime.catalog = next.catalog;
      runtime.providers = next.providers;
    },
  };
  const engine = engines.get("TestChar");
  ctx.autonomy.ensureState(engine);
  return { root, ctx, engine, ledger, runtime, configDir, dataDir };
}

function user(id: string): Message {
  return {
    msg_id: id,
    role: "user",
    content: "hello",
    images: [],
    content_blocks: [{ type: "text", text: "hello" }],
    timestamp: "2026-01-01T00:00:00Z",
  };
}

describe("command dispatcher state", () => {
  it("status, heartbeat_log, and heartbeat debug commands expose Rust payload fields", async () => {
    const { ctx, engine, ledger } = makeHarness();
    await engine.appendMessage(user("u1"));
    ctx.autonomy.notifyUserMessage("TestChar", engine.messageCount());

    const status = await dispatchCommand({ ctx, engine, name: "status", args: {} });
    expect(status).toMatchObject({
      character: "TestChar",
      message_count: 1,
      turn_count: 1,
      memory_mode: "markdown",
      tokens: { input: 0, output: 0, cache_read: 0, cache_write: 0 },
    });
    expect(typeof (status as { autonomy: { heartbeat_state: string } }).autonomy.heartbeat_state).toBe("string");

    expect(await dispatchCommand({ ctx, engine, name: "heartbeat_log", args: {} })).toEqual({ events: [] });
    expect(await dispatchCommand({ ctx, engine, name: "heartbeat_set_dormant", args: {} })).toEqual({
      status: "dormant",
      character: "TestChar",
    });
    expect(await dispatchCommand({ ctx, engine, name: "heartbeat_tick_now", args: {} })).toMatchObject({
      status: "scheduled",
      character: "TestChar",
    });
    expect(await dispatchCommand({ ctx, engine, name: "heartbeat_set_active", args: {} })).toEqual({
      status: "active",
      character: "TestChar",
    });
    ledger.close();
  });

  it("model commands list, inspect, switch, set settings, show settings, and reset", async () => {
    const { ctx, engine, ledger, dataDir } = makeHarness();
    const listed = await dispatchCommand({ ctx, engine, name: "list_models", args: {} });
    expect((listed as { active: string }).active).toBe("chat.anthropic.sonnet");
    expect((listed as { models: Array<{ source: string }> }).models.every((m) => m.source === "static")).toBe(true);

    const info = await dispatchCommand({ ctx, engine, name: "model_info", args: { name: "sonnet" } });
    expect(info).toMatchObject({
      name: "sonnet",
      qualified_name: "chat.anthropic.sonnet",
      provider_key: "anthropic",
      model_id: "claude-sonnet-4-20250514",
    });

    const switched = await dispatchCommand({ ctx, engine, name: "switch_model", args: { name: "gpt4o" } });
    expect(switched).toMatchObject({
      active: "gpt4o",
      qualified_name: "chat.openrouter.gpt4o",
      provider: "openrouter",
      model_id: "openai/gpt-4o",
      changed: true,
    });

    const set = await dispatchCommand({
      ctx,
      engine,
      name: "set_model_setting",
      args: { key: "temperature", value: 0.8 },
    });
    expect(set).toMatchObject({ changed: true, scope: "character", key: "temperature", value: 0.8 });
    const prefs = loadPreferences(characterPreferencesPath(dataDir, "TestChar"));
    expect(modelPreference(prefs, "openrouter", "openai/gpt-4o")?.sampler.temperature).toBe(0.8);

    const settings = await dispatchCommand({ ctx, engine, name: "model_settings", args: {} });
    expect(settings).toMatchObject({
      model: "chat.openrouter.gpt4o",
      provider: "openrouter",
      model_id: "openai/gpt-4o",
    });

    const reset = await dispatchCommand({ ctx, engine, name: "reset_model", args: {} });
    expect(reset).toMatchObject({ reset_to: "config default", active: null });
    ledger.close();
  });

  it("memory commands cover status, query, changelog, dream status, dreams log, and compact error", async () => {
    const { ctx, engine, ledger, configDir, dataDir } = makeHarness();
    const memoryDir = path.join(configDir, "characters", "TestChar", "workspace", "memory", "people");
    fs.mkdirSync(memoryDir, { recursive: true });
    fs.writeFileSync(path.join(memoryDir, "user.md"), "# User\n\nLikes tea.\n");

    const memoryStatus = await dispatchCommand({ ctx, engine, name: "memory", args: {} });
    expect(memoryStatus).toMatchObject({
      character: "TestChar",
      entries: 1,
      curated_files: 1,
      daily_files: 0,
      image_files: 0,
    });
    const query = await dispatchCommand({ ctx, engine, name: "memory", args: { query: "tea" } });
    expect((query as { result: string }).result).toContain("Top memory matches");

    const dreamsPath = path.join(dataDir, "TestChar", "DREAMS.md");
    fs.mkdirSync(path.dirname(dreamsPath), { recursive: true });
    fs.writeFileSync(dreamsPath, "# Dreams\n\n## 2026-01-01T00:00:00Z - update\n\nChanged memory.\n");
    const changelog = await dispatchCommand({ ctx, engine, name: "memory_changelog", args: { limit: 1 } });
    expect(changelog).toMatchObject({
      character: "TestChar",
      changelog: [{ timestamp: "2026-01-01T00:00:00Z", operation: "update" }],
    });
    const dreams = await dispatchCommand({ ctx, engine, name: "memory_dreams", args: { limit: 1 } });
    expect(dreams).toMatchObject({ character: "TestChar", exists: true });

    const dreamStatus = await dispatchCommand({ ctx, engine, name: "memory_dream", args: { status: true } });
    expect(dreamStatus).toMatchObject({
      character: "TestChar",
      enabled: false,
      frequency: "0 3 * * *",
    });

    await expect(dispatchCommand({ ctx, engine, name: "compact", args: {} })).rejects.toMatchObject({
      code: "invalid_request",
    });
    ledger.close();
  });

  it("config, diagnostics, and usage commands keep CLI-facing shapes", async () => {
    const { ctx, engine, ledger, configDir, runtime } = makeHarness();
    expect(await dispatchCommand({ ctx, engine, name: "config", args: { key: "defaults" } }))
      .toMatchObject({ key: "defaults" });
    const check = await dispatchCommand({ ctx, engine, name: "config_check", args: {} });
    expect(check).toMatchObject({ valid: true, chat_models: 2, tool_models: 0, memory_mode: "markdown" });
    const diagnostics = await dispatchCommand({ ctx, engine, name: "diagnostics", args: { count: 3 } });
    expect(diagnostics).toMatchObject({
      message: "diagnostics ring buffer not ported in TS daemon",
      api_calls: { count: 0, recent: [] },
      tool_calls: { count: 0, recent: [] },
      errors: { count: 0, recent: [] },
    });
    const usage = await dispatchCommand({ ctx, engine, name: "usage", args: {} });
    expect(usage).toMatchObject({ mode: "summary", summary: [] });

    fs.writeFileSync(path.join(configDir, "config.toml"), `
[defaults]
model = "gpt4o"

[chat.openrouter.gpt4o]
model_id = "openai/gpt-4o"
`);
    const reset = await dispatchCommand({ ctx, engine, name: "config_reset", args: {} });
    expect(reset).toMatchObject({ reset: true, invalidated: { runtime_overrides: true } });
    expect(runtime.config.app.defaults.model).toBe("gpt4o");
    ledger.close();
  });
});
