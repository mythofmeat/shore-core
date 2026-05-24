/**
 * SDK resolution for OpenRouter — verifies the model-prefix auto-default
 * and that explicit per-model TOML still wins.
 */
import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import {
  defaultSdkForOpenRouterModel,
  loadCatalog,
  parseCatalog,
} from "../src/llm/catalog.ts";

describe("defaultSdkForOpenRouterModel", () => {
  it("routes anthropic/* to the Anthropic SDK", () => {
    expect(defaultSdkForOpenRouterModel("anthropic/claude-haiku-4.5")).toBe(
      "anthropic",
    );
    expect(defaultSdkForOpenRouterModel("anthropic/claude-sonnet-4.5")).toBe(
      "anthropic",
    );
  });

  it("routes google/* to gemini (speculative)", () => {
    expect(defaultSdkForOpenRouterModel("google/gemini-2.5-pro")).toBe("gemini");
  });

  it("routes z-ai/* to zai (speculative)", () => {
    expect(defaultSdkForOpenRouterModel("z-ai/glm-4.6")).toBe("zai");
  });

  it("falls through to openai for everything else", () => {
    expect(defaultSdkForOpenRouterModel("openai/gpt-5.4-mini")).toBe("openai");
    expect(defaultSdkForOpenRouterModel("meta-llama/llama-4")).toBe("openai");
    expect(defaultSdkForOpenRouterModel("deepseek/deepseek-v3")).toBe("openai");
    expect(defaultSdkForOpenRouterModel("x-ai/grok-4")).toBe("openai");
  });
});

describe("parseCatalog SDK resolution for OpenRouter", () => {
  it("auto-routes anthropic/* via OpenRouter to the Anthropic SDK", () => {
    const config = {
      chat: {
        openrouter: {
          haiku45: { model_id: "anthropic/claude-haiku-4.5" },
        },
      },
    };
    const catalog = parseCatalog(config);
    const resolved = catalog.get("chat.openrouter.haiku45")!;
    expect(resolved.sdk).toBe("anthropic");
    // Anthropic SDK gets the 1h cache TTL default.
    expect(resolved.cacheTtl).toBe("1h");
  });

  it("auto-routes google/* via OpenRouter to gemini", () => {
    const config = {
      chat: {
        openrouter: {
          flash: { model_id: "google/gemini-2.5-flash" },
        },
      },
    };
    const catalog = parseCatalog(config);
    expect(catalog.get("chat.openrouter.flash")!.sdk).toBe("gemini");
  });

  it("falls back to openai for non-prefixed OpenRouter models", () => {
    const config = {
      chat: {
        openrouter: {
          gpt5mini: { model_id: "openai/gpt-5.4-mini" },
        },
      },
    };
    const catalog = parseCatalog(config);
    expect(catalog.get("chat.openrouter.gpt5mini")!.sdk).toBe("openai");
  });

  it("explicit per-model sdk = ... overrides the prefix default", () => {
    const config = {
      chat: {
        openrouter: {
          haiku45: {
            model_id: "anthropic/claude-haiku-4.5",
            sdk: "openai", // force openai-compat path
          },
        },
      },
    };
    const catalog = parseCatalog(config);
    expect(catalog.get("chat.openrouter.haiku45")!.sdk).toBe("openai");
  });

  it("explicit provider-scalar sdk = ... overrides the prefix default", () => {
    const config = {
      chat: {
        openrouter: {
          sdk: "openai",
          haiku45: { model_id: "anthropic/claude-haiku-4.5" },
        },
      },
    };
    const catalog = parseCatalog(config);
    expect(catalog.get("chat.openrouter.haiku45")!.sdk).toBe("openai");
  });

  it("does not affect non-OpenRouter providers", () => {
    const config = {
      chat: {
        anthropic: { haiku45: { model_id: "claude-haiku-4.5" } },
        openai: { gpt5mini: { model_id: "gpt-5.4-mini" } },
      },
    };
    const catalog = parseCatalog(config);
    expect(catalog.get("chat.anthropic.haiku45")!.sdk).toBe("anthropic");
    expect(catalog.get("chat.openai.gpt5mini")!.sdk).toBe("openai");
  });

  it("loads an explicit config file and conf.d overlay from the same directory", () => {
    const dir = mkdtempSync(path.join(tmpdir(), "shore-catalog-config-test-"));
    fs.mkdirSync(path.join(dir, "conf.d"));
    const file = path.join(dir, "preview.toml");
    fs.writeFileSync(file, `
[chat.openrouter.haiku45]
model_id = "anthropic/claude-haiku-4.5"
`);
    fs.writeFileSync(path.join(dir, "conf.d", "10-overlay.toml"), `
[chat.openrouter.gpt5mini]
model_id = "openai/gpt-5.4-mini"
`);

    const catalog = loadCatalog({ configDir: dir, configFile: file });
    expect(catalog.get("chat.openrouter.haiku45")?.sdk).toBe("anthropic");
    expect(catalog.get("chat.openrouter.gpt5mini")?.sdk).toBe("openai");
  });
});
