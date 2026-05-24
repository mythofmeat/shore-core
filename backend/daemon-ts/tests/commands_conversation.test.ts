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

function tempDir(): string {
  return mkdtempSync(path.join(tmpdir(), "shore-commands-conversation-"));
}

function msg(id: string, role: Message["role"], content: string): Message {
  return {
    msg_id: id,
    role,
    content,
    images: [],
    content_blocks: [{ type: "text", text: content }],
    timestamp: "2026-01-01T00:00:00Z",
  };
}

function makeHarness() {
  const root = tempDir();
  const configDir = path.join(root, "config");
  const dataDir = path.join(root, "data");
  const cacheDir = path.join(root, "cache");
  fs.mkdirSync(path.join(configDir, "characters", "TestChar", "workspace"), { recursive: true });
  fs.writeFileSync(path.join(configDir, "characters", "TestChar", "workspace", "SOUL.md"), "soul");
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
  return { ctx, engine: engines.get("TestChar"), ledger };
}

describe("command dispatcher conversation", () => {
  it("log, history_page, and get use Rust paging fields", async () => {
    const { ctx, engine, ledger } = makeHarness();
    await engine.appendMessage(msg("u1", "user", "hello"));
    await engine.appendMessage(msg("a1", "assistant", "hi"));
    await engine.appendMessage(msg("u2", "user", "again"));

    const log = await dispatchCommand({ ctx, engine, name: "log", args: { count: 2 } });
    expect(log).toMatchObject({
      active_start: 0,
      cursor: 1,
      next_before: 1,
      has_more_before: true,
      total_turns: 2,
    });
    expect((log as { messages: Message[] }).messages.map((m) => m.msg_id)).toEqual(["a1", "u2"]);

    const page = await dispatchCommand({ ctx, engine, name: "history_page", args: { before: 2, count: 1 } });
    expect((page as { messages: Message[] }).messages.map((m) => m.msg_id)).toEqual(["a1"]);

    const got = await dispatchCommand({ ctx, engine, name: "get", args: { ref: "-1" } });
    expect((got as Message).msg_id).toBe("u2");
    ledger.close();
  });

  it("edit and delete resolve refs against visible message order", async () => {
    const { ctx, engine, ledger } = makeHarness();
    await engine.appendMessage(msg("u1", "user", "hello"));
    await engine.appendMessage(msg("a1", "assistant", "hi"));

    await expect(dispatchCommand({
      ctx,
      engine,
      name: "edit",
      args: { ref: "0", content: "bad" },
    })).rejects.toMatchObject({ code: "invalid_request" });

    const edited = await dispatchCommand({
      ctx,
      engine,
      name: "edit",
      args: { ref: "2", content: "hello back" },
    });
    expect(edited).toEqual({ ref: "a1", edited: true });
    expect(engine.messages()[1]?.content).toBe("hello back");

    const deleted = await dispatchCommand({ ctx, engine, name: "delete", args: { refs: ["1"] } });
    expect(deleted).toEqual({ deleted: ["u1"] });
    ledger.close();
  });

  it("inject_system canonical name and compatibility alias append system messages", async () => {
    const { ctx, engine, ledger } = makeHarness();
    const out = await dispatchCommand({
      ctx,
      engine,
      name: "inject_system",
      args: { text: "be concise" },
    });
    expect(out).toEqual({ injected: true });
    expect(engine.messages()[0]).toMatchObject({ role: "system", content: "be concise" });

    const alias = await dispatchCommand({
      ctx,
      engine,
      name: "inject_system_message",
      args: { text: "compat" },
    });
    expect(alias).toEqual({ injected: true });
    expect(engine.messages()[1]).toMatchObject({ role: "system", content: "compat" });
    ledger.close();
  });

  it("alt and list_alternatives mirror alternate response payloads", async () => {
    const { ctx, engine, ledger } = makeHarness();
    await engine.appendMessage({
      ...msg("a1", "assistant", "first"),
      alt_index: 0,
      alt_count: 2,
      alternatives: [
        {
          content: "first",
          images: [],
          content_blocks: [{ type: "text", text: "first" }],
          timestamp: "2026-01-01T00:00:00Z",
        },
        {
          content: "second",
          images: [],
          content_blocks: [{ type: "text", text: "second" }],
          timestamp: "2026-01-01T00:00:01Z",
        },
      ],
    });

    const listed = await dispatchCommand({ ctx, engine, name: "list_alternatives", args: {} });
    expect(listed).toMatchObject({ ref: "a1", alt_count: 2, position: 1 });
    expect((listed as { alternatives: Array<{ position: number }> }).alternatives.map((a) => a.position)).toEqual([1, 2]);

    const selected = await dispatchCommand({ ctx, engine, name: "alt", args: { position: 2 } });
    expect(selected).toMatchObject({
      ref: "a1",
      alt_index: 1,
      position: 2,
      alt_count: 2,
      content: "second",
    });
    ledger.close();
  });
});
