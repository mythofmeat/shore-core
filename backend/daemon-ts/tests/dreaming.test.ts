import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { engineForCharacter } from "../src/engine/engine.ts";
import type { ContentBlock } from "../src/engine/types.ts";
import type { ResolvedModel } from "../src/llm/catalog.ts";
import type { ChatEvent, ChatRequest, GenerateResult, ProviderClient } from "../src/llm/types.ts";
import {
  characterMemoryDir,
  characterWorkspaceDir,
  loadMemoryIndex,
  pendingDeferredEditPaths,
  SOUL_FILE,
} from "../src/memory/deferred_edits.ts";
import {
  defaultDreamingConfig,
  runLibrarianSweep,
} from "../src/memory/dreaming.ts";
import { dreamsLogPath } from "../src/memory/dreams_log.ts";
import { Ledger } from "../src/ledger/ledger.ts";

class QueuedProvider implements ProviderClient {
  readonly requests: ChatRequest[] = [];
  private readonly responses: Array<{ content: ContentBlock[]; stopReason: string }> = [];

  enqueueToolUse(id: string, name: string, input: unknown): void {
    this.responses.push({
      content: [{ type: "tool_use", id, name, input }],
      stopReason: "tool_use",
    });
  }

  enqueueText(text: string): void {
    this.responses.push({
      content: [{ type: "text", text }],
      stopReason: "end_turn",
    });
  }

  private nextResponse(): { content: ContentBlock[]; stopReason: string } {
    return this.responses.shift() ?? {
      content: [{ type: "text" as const, text: "done" }],
      stopReason: "end_turn",
    };
  }

  async *stream(req: ChatRequest): AsyncIterable<ChatEvent> {
    this.requests.push(req);
    const response = this.nextResponse();
    for (const block of response.content) {
      if (block.type === "tool_use") {
        yield { kind: "tool_use_start", id: block.id, name: block.name };
      }
    }
    yield {
      kind: "done",
      content: response.content,
      stopReason: response.stopReason,
      usage: {
        inputTokens: 1,
        outputTokens: 1,
        cacheReadInputTokens: 0,
        cacheCreationInputTokens: 0,
      },
    };
  }

  async generate(req: ChatRequest): Promise<GenerateResult> {
    this.requests.push(req);
    const response = this.nextResponse();
    return {
      content: response.content,
      stopReason: response.stopReason,
      usage: {
        inputTokens: 1,
        outputTokens: 1,
        cacheReadInputTokens: 0,
        cacheCreationInputTokens: 0,
      },
    };
  }
}

function fakeResolved(): ResolvedModel {
  return {
    name: "fake",
    qualifiedName: "chat.fake.fake",
    category: "chat",
    providerKey: "fake",
    sdk: "openai",
    modelId: "fake-model",
    apiKeyEnv: "FAKE_API_KEY",
    baseUrl: undefined,
    maxTokens: 1024,
    maxContextTokens: 200_000,
    temperature: 1,
    topP: undefined,
    reasoningEffort: undefined,
    budgetTokens: undefined,
    cacheTtl: "",
    openrouterProvider: undefined,
  };
}

function setup(): {
  root: string;
  configDir: string;
  dataDir: string;
  cacheDir: string;
  provider: QueuedProvider;
} {
  const root = mkdtempSync(path.join(tmpdir(), "shore-dreaming-test-"));
  const configDir = path.join(root, "config");
  const dataDir = path.join(root, "data");
  const cacheDir = path.join(root, "cache");
  fs.mkdirSync(characterMemoryDir(configDir, "alice"), { recursive: true });
  fs.mkdirSync(path.join(dataDir, "alice"), { recursive: true });
  return {
    root,
    configDir,
    dataDir,
    cacheDir,
    provider: new QueuedProvider(),
  };
}

function sweepOpts(
  setupResult: ReturnType<typeof setup>,
  maxToolRounds = 10,
  dryRun = false,
) {
  return {
    configDir: setupResult.configDir,
    dataDir: setupResult.dataDir,
    cacheDir: setupResult.cacheDir,
    character: "alice",
    displayName: "Ren",
    resolved: fakeResolved(),
    apiKey: "test-key",
    provider: setupResult.provider,
    engine: engineForCharacter(setupResult.dataDir, "alice"),
    dreamingConfig: {
      ...defaultDreamingConfig(),
      enabled: true,
      max_tool_rounds: maxToolRounds,
    },
    dryRun,
    force: true,
  };
}

describe("runLibrarianSweep", () => {
  it("uses private tools, updates memory, defers MEMORY.md, and writes audit", async () => {
    const s = setup();
    const mem = characterMemoryDir(s.configDir, "alice");
    const workspace = characterWorkspaceDir(s.configDir, "alice");
    fs.mkdirSync(path.join(mem, "daily"), { recursive: true });
    fs.writeFileSync(
      path.join(mem, "daily/2026-04.md"),
      "# Daily April Notes\n\n- Trevor wants Shore memory to use MEMORY.md as an index.\n- Trevor wants Shore memory to use MEMORY.md as an index.\n",
    );
    fs.writeFileSync(path.join(mem, "shore-notes.md"), "# Shore Notes\n\n- Older note.\n");

    s.provider.enqueueToolUse("t_list", "list_files", { path: "memory" });
    s.provider.enqueueToolUse("t_read", "read", { path: "memory/daily/2026-04.md" });
    s.provider.enqueueToolUse("t_search", "search", {
      path: "memory",
      query: "MEMORY.md",
      mode: "lexical",
    });
    s.provider.enqueueToolUse("t_write_notes", "write", {
      path: "memory/shore-notes.md",
      content:
        "# Shore Notes\n\n- Shore memory uses `MEMORY.md` as a prompt-visible index rather than an old recap block.\n- Duplicate daily notes about the index direction were consolidated here.\n",
    });
    s.provider.enqueueToolUse("t_write_memory", "write", {
      path: "MEMORY.md",
      content:
        "# Memory Index\n\n## Memory areas\n\n- `shore-notes.md` - Durable Shore memory architecture notes.\n\n## Current conversational throughlines\n\n- Shore memory should remain markdown-first, with dreaming acting as an AI librarian.\n",
    });
    s.provider.enqueueText("Librarian pass complete.");

    const result = await runLibrarianSweep(sweepOpts(s));

    expect(result?.mode).toBe("ai_librarian");
    expect(result?.audit_appended).toBe(true);
    expect(result?.tools_used).toEqual([
      "list_files",
      "read",
      "search",
      "write",
      "write",
    ]);
    expect(result?.tool_rounds).toBe(5);
    expect(fs.readFileSync(path.join(mem, "shore-notes.md"), "utf8")).toContain(
      "consolidated",
    );
    const memory = fs.readFileSync(path.join(workspace, "MEMORY.md"), "utf8");
    expect(memory).toContain("# Memory Index");
    expect(memory).toContain("shore-notes.md");
    expect(loadMemoryIndex(path.join(s.dataDir, "alice"), s.configDir, "alice")).toBeUndefined();
    expect(pendingDeferredEditPaths(path.join(s.dataDir, "alice"))).toContain("MEMORY.md");
    const dreams = fs.readFileSync(dreamsLogPath(s.dataDir, "alice"), "utf8");
    expect(dreams).toContain("AI librarian dreaming pass");
    expect(dreams).toContain("MEMORY.md updated:");
    expect(fs.existsSync(path.join(s.dataDir, "alice/dreams/state.json"))).toBe(true);
    expect(result?.final_report).toBe("Librarian pass complete.");
  });

  it("dry run blocks writes and does not persist state or audit", async () => {
    const s = setup();
    s.provider.enqueueToolUse("t_write", "write", {
      path: "MEMORY.md",
      content: "# Bad",
    });
    s.provider.enqueueText("Would update MEMORY.md.");

    const result = await runLibrarianSweep(sweepOpts(s, 3, true));
    const workspace = characterWorkspaceDir(s.configDir, "alice");

    expect(result?.dry_run).toBe(true);
    expect(result?.paths_written).toEqual([]);
    expect(result?.would_write_paths.some((p) => p.endsWith("MEMORY.md"))).toBe(true);
    expect(fs.existsSync(path.join(workspace, "MEMORY.md"))).toBe(false);
    expect(fs.existsSync(dreamsLogPath(s.dataDir, "alice"))).toBe(false);
    expect(fs.existsSync(path.join(s.dataDir, "alice/dreams/state.json"))).toBe(false);
  });

  it("fallback creates MEMORY.md and audit when the model writes nothing", async () => {
    const s = setup();
    const mem = characterMemoryDir(s.configDir, "alice");
    fs.writeFileSync(path.join(mem, "notes.md"), "# Notes\n\n- Durable note.\n");
    s.provider.enqueueText("I inspected nothing and forgot to write files.");

    const result = await runLibrarianSweep(sweepOpts(s, 3));
    const workspace = characterWorkspaceDir(s.configDir, "alice");
    const memory = fs.readFileSync(path.join(workspace, "MEMORY.md"), "utf8");

    expect(result?.audit_appended).toBe(true);
    expect(memory).toContain("Fallback note");
    expect(memory).toContain("notes.md");
    expect(loadMemoryIndex(path.join(s.dataDir, "alice"), s.configDir, "alice")).toBeUndefined();
    const dreams = fs.readFileSync(dreamsLogPath(s.dataDir, "alice"), "utf8");
    expect(dreams).toContain("daemon fallback");
  });

  it("records private librarian provider calls as dreaming ledger rows", async () => {
    const s = setup();
    const ledger = Ledger.openInMemory();
    s.provider.enqueueText("No changes needed.");

    await runLibrarianSweep({ ...sweepOpts(s), ledger });

    const rows = ledger.recent(1);
    expect(rows).toHaveLength(1);
    expect(rows[0]?.call_type).toBe("dreaming");
    expect(rows[0]?.character).toBe("alice");
    expect(rows[0]?.input_tokens).toBe(1);
    expect(rows[0]?.cache_state).toBeUndefined();
    ledger.close();
  });

  it("zero max tool rounds sends no provider request and still creates fallback index", async () => {
    const s = setup();
    const mem = characterMemoryDir(s.configDir, "alice");
    fs.writeFileSync(path.join(mem, "notes.md"), "# Notes\n\n- Durable note.\n");

    const result = await runLibrarianSweep(sweepOpts(s, 0));
    const workspace = characterWorkspaceDir(s.configDir, "alice");

    expect(result?.tool_rounds).toBe(0);
    expect(s.provider.requests.length).toBe(0);
    expect(fs.existsSync(path.join(workspace, "MEMORY.md"))).toBe(true);
  });

  it("protected prompt file edits remain staged through deferred edits", async () => {
    const s = setup();
    const workspace = characterWorkspaceDir(s.configDir, "alice");
    fs.mkdirSync(workspace, { recursive: true });
    fs.writeFileSync(path.join(workspace, SOUL_FILE), "original soul");
    s.provider.enqueueToolUse("t_write_soul", "write", {
      path: "SOUL.md",
      content: "new soul",
    });
    s.provider.enqueueText("Updated soul.");

    const result = await runLibrarianSweep(sweepOpts(s, 3));
    const characterDataDir = path.join(s.dataDir, "alice");

    expect(result?.tools_used).toContain("write");
    expect(fs.readFileSync(path.join(workspace, SOUL_FILE), "utf8")).toBe("new soul");
    expect(pendingDeferredEditPaths(characterDataDir)).toContain(SOUL_FILE);
    expect(fs.readFileSync(path.join(characterDataDir, "active_prompt", SOUL_FILE), "utf8")).toBe(
      "original soul",
    );
  });
});
