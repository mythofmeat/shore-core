/**
 * CompactionManager tests — mirror of
 * `backend/daemon/src/memory/compaction/mod.rs::tests`.
 *
 * Pins behavior tests pin in Rust:
 *  - prompt-building helpers (build_prompt, build_existing_memory_context)
 *  - turn-split + tool-loop handling
 *  - should_force_compact / has_enough_turns gating
 *  - compact() end-to-end: writes, archive, dream log, deferred-edit queue
 *  - dry run, private skip, insufficient messages
 *  - rollback on conversation-manager failure
 *  - cache-preserving mode: only the final user message is passed to the LLM
 */
import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import {
  CompactionError,
  type CompactionLlm,
  type CompactionOutcome,
  type ConversationManager,
  type ConversationMessage,
  type RetentionParams,
} from "../src/memory/compaction/types.ts";
import {
  CompactionManager,
  type CompactOptions,
} from "../src/memory/compaction/manager.ts";
import {
  DEFAULT_COMPACT_PROMPT,
  DEFAULT_COMPACT_SYSTEM,
} from "../src/memory/compaction/parser.ts";
import { MarkdownMemoryStore } from "../src/memory/markdown_store.ts";
import { pendingDeferredEditPaths } from "../src/memory/deferred_edits.ts";
import { readDreamsLog } from "../src/memory/dreams_log.ts";
import type { ChatRequest } from "../src/llm/types.ts";

function freshDir(): string {
  return mkdtempSync(path.join(tmpdir(), "shore-compact-test-"));
}

function makeMessages(count: number): ConversationMessage[] {
  const out: ConversationMessage[] = [];
  for (let i = 0; i < count; i++) {
    out.push({
      role: i % 2 === 0 ? "user" : "assistant",
      content: `Message ${i}`,
      timestamp: new Date().toISOString(),
      isToolResultOnly: false,
    });
  }
  return out;
}

const XML_RESPONSE = `<memory>
<write path="memory/daily/2026-03-25.md">
# Conversation on 2026-03-25

- User discussed their day
- They mentioned having a busy morning
</write>

<write path="memory/preferences/beverages.md">
# Beverage Preferences

- User prefers tea over coffee
- This is a stable preference
</write>
</memory>`;

// ---------------------------------------------------------------------------
// Mocks
// ---------------------------------------------------------------------------

class MockLlm implements CompactionLlm {
  constructor(public response: string) {}
  async summarize(): Promise<string> {
    return this.response;
  }
}

class CapturingLlm implements CompactionLlm {
  lastUserMessage: string | undefined;
  lastMessageCount: number | undefined;
  sawCachedRequest = false;

  constructor(public response: string) {}

  async summarize(
    _system: string,
    messages: Array<{ role: "user" | "assistant"; content: string }>,
    cachedRequest: ChatRequest | undefined,
  ): Promise<string> {
    for (let i = messages.length - 1; i >= 0; i--) {
      const m = messages[i]!;
      if (m.role === "user") {
        this.lastUserMessage = m.content;
        break;
      }
    }
    this.lastMessageCount = messages.length;
    this.sawCachedRequest = cachedRequest !== undefined;
    return this.response;
  }
}

class MockConversationMgr implements ConversationManager {
  calls: Array<{ id: string; keepLastN: number }> = [];
  constructor(public nextId: string) {}
  async archiveAndRetain(id: string, params: RetentionParams): Promise<string> {
    this.calls.push({ id, keepLastN: params.keepLastN });
    return this.nextId;
  }
}

class FailingConversationMgr implements ConversationManager {
  async archiveAndRetain(): Promise<string> {
    throw new CompactionError(
      "conversationManager",
      "simulated archive failure",
    );
  }
}

function defaultConfig(keepRecentTurns = 2): {
  enabled: boolean;
  idleTriggerSecs: number;
  minTurns: number;
  maxTurns: number;
  maxContextTokens: number;
  keepRecentTurns: number;
} {
  return {
    enabled: true,
    idleTriggerSecs: 1800,
    minTurns: 8,
    maxTurns: 16,
    maxContextTokens: 200_000,
    keepRecentTurns,
  };
}

async function buildCompactOpts(
  overrides: Partial<CompactOptions> & {
    llm: CompactionLlm;
    convMgr: ConversationManager;
    storeDir: string;
    dataDir?: string;
  },
): Promise<CompactOptions> {
  const store = MarkdownMemoryStore.open(path.join(overrides.storeDir, "memory"));
  return {
    conversationId: overrides.conversationId ?? "conv-1",
    messages: overrides.messages ?? makeMessages(10),
    activeContent: overrides.activeContent ?? "",
    isPrivate: overrides.isPrivate ?? false,
    systemTemplate: overrides.systemTemplate ?? DEFAULT_COMPACT_SYSTEM,
    promptTemplate: overrides.promptTemplate ?? DEFAULT_COMPACT_PROMPT,
    charName: overrides.charName ?? "TestChar",
    userName: overrides.userName ?? "TestUser",
    llm: overrides.llm,
    conversationManager: overrides.convMgr,
    markdownStore: store,
    dryRun: overrides.dryRun ?? false,
    ...(overrides.keepTurnsOverride !== undefined
      ? { keepTurnsOverride: overrides.keepTurnsOverride }
      : {}),
    ...(overrides.cachedRequest !== undefined
      ? { cachedRequest: overrides.cachedRequest }
      : {}),
    ...(overrides.dataDir !== undefined ? { dataDir: overrides.dataDir } : {}),
  };
}

// ---------------------------------------------------------------------------
// build_prompt + build_existing_memory_context
// ---------------------------------------------------------------------------

describe("build_prompt", () => {
  it("substitutes conversation messages and drops the placeholder", () => {
    const messages: ConversationMessage[] = [
      {
        role: "user",
        content: "Hello!",
        timestamp: "2026-03-25T10:00:00Z",
        isToolResultOnly: false,
      },
      {
        role: "assistant",
        content: "Hi there!",
        timestamp: "2026-03-25T10:00:01Z",
        isToolResultOnly: false,
      },
    ];
    const out = CompactionManager.buildPrompt(
      "Template:\n{{conversation}}",
      messages,
      undefined,
      "Char",
      "User",
    );
    expect(out).toContain("[2026-03-25T10:00:00Z] user: Hello!");
    expect(out).toContain("[2026-03-25T10:00:01Z] assistant: Hi there!");
    expect(out).not.toContain("{{conversation}}");
  });

  it("strips legacy recap block", () => {
    const template =
      "Before\n{{#if recap}}RECAP: {{recap}}{{/if}}\nAfter\n{{conversation}}";
    const out = CompactionManager.buildPrompt(
      template,
      makeMessages(2),
      undefined,
      "Char",
      "User",
    );
    expect(out).not.toContain("RECAP");
    expect(out).not.toContain("{{#if recap}}");
    expect(out).not.toContain("{{/if}}");
    expect(out).toContain("Before");
    expect(out).toContain("After");
  });

  it("includes existing memories", () => {
    const out = CompactionManager.buildPrompt(
      "Existing:\n{{existing_memories}}\nConversation:\n{{conversation}}",
      makeMessages(2),
      '<file path="people/User.md">\n# User\n</file>',
      "Char",
      "User",
    );
    expect(out).toContain("people/User.md");
    expect(out).not.toContain("{{existing_memories}}");
  });
});

describe("build_existing_memory_context", () => {
  it("reads markdown files and excludes DREAMS.md", () => {
    const dir = freshDir();
    const store = MarkdownMemoryStore.open(path.join(dir, "memory"));
    store.write("people/User.md", "# User\n\n- Likes tea.");
    store.write("DREAMS.md", "# Dreams");

    const out = CompactionManager.buildExistingMemoryContext(store);
    expect(out).toContain("people/User.md");
    expect(out).toContain("Likes tea");
    expect(out).not.toContain("DREAMS.md");
  });
});

// ---------------------------------------------------------------------------
// Force-compact + has-enough-turns gating
// ---------------------------------------------------------------------------

describe("should_force_compact", () => {
  it("respects max_turns and min_turns", () => {
    const mgr = new CompactionManager({
      ...defaultConfig(2),
      minTurns: 20,
      maxTurns: 60,
    });
    expect(mgr.shouldForceCompact(0)).toBe(false);
    expect(mgr.shouldForceCompact(19)).toBe(false);
    expect(mgr.shouldForceCompact(59)).toBe(false);
    expect(mgr.shouldForceCompact(60)).toBe(true);
    expect(mgr.shouldForceCompact(100)).toBe(true);
  });

  it("disabled when max_turns is 0", () => {
    const mgr = new CompactionManager({ ...defaultConfig(), maxTurns: 0 });
    expect(mgr.shouldForceCompact(1000)).toBe(false);
  });
});

describe("has_enough_turns", () => {
  it("requires min_turns", () => {
    const mgr = new CompactionManager({
      ...defaultConfig(2),
      minTurns: 20,
    });
    expect(mgr.hasEnoughTurns(0)).toBe(false);
    expect(mgr.hasEnoughTurns(19)).toBe(false);
    expect(mgr.hasEnoughTurns(20)).toBe(true);
    expect(mgr.hasEnoughTurns(100)).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// find_turn_split
// ---------------------------------------------------------------------------

describe("find_turn_split", () => {
  it("skips tool_result-only user messages", () => {
    const messages: ConversationMessage[] = [
      { role: "user", content: "Hello", timestamp: "t0", isToolResultOnly: false },
      { role: "assistant", content: "", timestamp: "t1", isToolResultOnly: false },
      { role: "user", content: "tool output", timestamp: "t2", isToolResultOnly: true },
      {
        role: "assistant",
        content: "Based on the tool result...",
        timestamp: "t3",
        isToolResultOnly: false,
      },
      { role: "user", content: "Thanks!", timestamp: "t4", isToolResultOnly: false },
      {
        role: "assistant",
        content: "You're welcome!",
        timestamp: "t5",
        isToolResultOnly: false,
      },
    ];
    expect(CompactionManager.findTurnSplit(messages, 1)).toBe(4);
    expect(CompactionManager.findTurnSplit(messages, 2)).toBe(0);
  });

  it("keep_turns=0 returns messages.length", () => {
    const allUser: ConversationMessage[] = [
      { role: "user", content: "a", timestamp: "t0", isToolResultOnly: false },
      { role: "user", content: "b", timestamp: "t1", isToolResultOnly: false },
    ];
    expect(CompactionManager.findTurnSplit(allUser, 0)).toBe(2);

    const mixed: ConversationMessage[] = [
      { role: "user", content: "hi", timestamp: "t0", isToolResultOnly: false },
      { role: "assistant", content: "hey", timestamp: "t1", isToolResultOnly: false },
    ];
    expect(CompactionManager.findTurnSplit(mixed, 0)).toBe(2);

    const withToolLoop: ConversationMessage[] = [
      { role: "user", content: "do a thing", timestamp: "t0", isToolResultOnly: false },
      { role: "assistant", content: "", timestamp: "t1", isToolResultOnly: false },
      { role: "user", content: "tool output", timestamp: "t2", isToolResultOnly: true },
      { role: "assistant", content: "done", timestamp: "t3", isToolResultOnly: false },
    ];
    expect(CompactionManager.findTurnSplit(withToolLoop, 0)).toBe(4);

    expect(CompactionManager.findTurnSplit([], 0)).toBe(0);
  });

  it("returns 0 when all messages are tool results", () => {
    const messages: ConversationMessage[] = [
      { role: "user", content: "tool output", timestamp: "t0", isToolResultOnly: true },
      { role: "assistant", content: "response", timestamp: "t1", isToolResultOnly: false },
    ];
    expect(CompactionManager.findTurnSplit(messages, 1)).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// compact() end-to-end
// ---------------------------------------------------------------------------

describe("compact()", () => {
  it("writes markdown files and a dream entry", async () => {
    const dir = freshDir();
    const dataDir = path.join(dir, "data");
    const opts = await buildCompactOpts({
      llm: new MockLlm(XML_RESPONSE),
      convMgr: new MockConversationMgr("new-conv-1"),
      storeDir: dir,
      dataDir,
    });
    const mgr = new CompactionManager(defaultConfig(2));

    const outcome = await mgr.compact(opts);
    expect(outcome.kind).toBe("compacted");
    const r = (outcome as Extract<CompactionOutcome, { kind: "compacted" }>).result;
    expect(r.memoryFilesWritten.length).toBe(2);
    expect(r.conversationId).toBe("conv-1");
    expect(r.newConversationId).toBe("new-conv-1");
    expect(r.messageCount).toBe(6);
    expect(r.compactedTurns).toBe(3);
    expect(r.retainedCount).toBe(4);
    expect(r.retainedTurns).toBe(2);

    const store = opts.markdownStore!;
    expect(store.read("daily/2026-03-25.md").content).toContain(
      "User discussed their day",
    );
    expect(store.read("preferences/beverages.md").content).toContain(
      "User prefers tea",
    );

    const dreams = await readDreamsLog(dataDir, "TestChar");
    expect(dreams).toBeDefined();
    expect(dreams).toContain("Compacted 3 turns");
  });

  it("workspace-rooted paths do not double-nest", async () => {
    const dir = freshDir();
    const opts = await buildCompactOpts({
      llm: new MockLlm(`<memory>
<write path="memory/people/foo.md"># Foo

- Likes tea.
</write>
</memory>`),
      convMgr: new MockConversationMgr("new-conv-rooted"),
      storeDir: dir,
    });
    const mgr = new CompactionManager(defaultConfig(2));
    const outcome = await mgr.compact(opts);
    expect(outcome.kind).toBe("compacted");
    const r = (outcome as Extract<CompactionOutcome, { kind: "compacted" }>).result;
    expect(r.memoryFilesWritten).toContain("memory/people/foo.md");

    expect(fs.existsSync(path.join(dir, "memory", "people", "foo.md"))).toBe(true);
    expect(
      fs.existsSync(path.join(dir, "memory", "memory", "people", "foo.md")),
    ).toBe(false);
    expect(opts.markdownStore!.read("people/foo.md").content).toContain("Foo");
  });

  it("writes MEMORY.md but refuses generated/protected/dreaming paths", async () => {
    const dir = freshDir();
    const dataDir = path.join(dir, "data");
    const opts = await buildCompactOpts({
      llm: new MockLlm(`<memory>
<write path="MEMORY.md"># Memory Index

## Throughline
- Carry-forward note from compaction.
</write>
<write path="DREAMS.md"># Bad dream diary overwrite</write>
<write path="memory/.dreams/candidates.md">bad staged output</write>
<write path="memory/dreaming/rem/today.md">bad phase report</write>
<write path="SOUL.md"># Bad protected-file overwrite</write>
<write path="workspace/USER.md"># Bad protected-file overwrite</write>
<write path="topics/foo.md"># Bare path with no memory/ prefix</write>
<write path="memory/notes/ok.md"># OK

- Real note
</write>
</memory>`),
      convMgr: new MockConversationMgr("new-conv-filter"),
      storeDir: dir,
      dataDir,
    });
    const mgr = new CompactionManager(defaultConfig(2));
    const outcome = await mgr.compact(opts);
    expect(outcome.kind).toBe("compacted");
    const r = (outcome as Extract<CompactionOutcome, { kind: "compacted" }>).result;

    expect(r.memoryFilesWritten).toContain("MEMORY.md");
    expect(r.memoryFilesWritten).toContain("memory/notes/ok.md");
    for (const rejected of [
      "DREAMS.md",
      "memory/.dreams/candidates.md",
      "memory/dreaming/rem/today.md",
      "SOUL.md",
      "workspace/USER.md",
      "topics/foo.md",
    ]) {
      expect(r.memoryFilesWritten).not.toContain(rejected);
    }

    const memory = fs.readFileSync(path.join(dir, "MEMORY.md"), "utf8");
    expect(memory).toContain("Carry-forward note");

    const pending = pendingDeferredEditPaths(path.join(dataDir, "TestChar"));
    expect(pending).toEqual(["MEMORY.md"]);

    // Dreaming/store-prefixed paths under memory/ never landed inside the store.
    expect(() => opts.markdownStore!.read(".dreams/candidates.md")).toThrow();
    expect(() => opts.markdownStore!.read("dreaming/rem/today.md")).toThrow();
    expect(() => opts.markdownStore!.read("DREAMS.md")).toThrow();
    // The accepted memory-rooted note landed under memory/.
    expect(opts.markdownStore!.read("notes/ok.md").content).toContain("Real note");

    // Protected workspace-root files were not stomped.
    expect(fs.existsSync(path.join(dir, "SOUL.md"))).toBe(false);
    expect(fs.existsSync(path.join(dir, "USER.md"))).toBe(false);
    expect(fs.existsSync(path.join(dir, "topics", "foo.md"))).toBe(false);
  });

  it("LLM prompt includes the existing markdown snapshot", async () => {
    const dir = freshDir();
    const opts = await buildCompactOpts({
      llm: new CapturingLlm(XML_RESPONSE),
      convMgr: new MockConversationMgr("new-conv-context"),
      storeDir: dir,
    });
    opts.markdownStore!.write(
      "people/TestUser.md",
      "# TestUser\n\n- Already likes green tea.",
    );

    const mgr = new CompactionManager(defaultConfig(2));
    await mgr.compact(opts);

    const llm = opts.llm as CapturingLlm;
    const prompt = llm.lastUserMessage;
    expect(prompt).toBeDefined();
    expect(prompt).toContain("people/TestUser.md");
    expect(prompt).toContain("Already likes green tea");
    expect(prompt).not.toContain("{{existing_memories}}");
  });

  it("cache-preserving mode passes only the final user message", async () => {
    const dir = freshDir();
    const opts = await buildCompactOpts({
      llm: new CapturingLlm(XML_RESPONSE),
      convMgr: new MockConversationMgr("new-conv-cached"),
      storeDir: dir,
      cachedRequest: {
        system: "sys",
        messages: [
          {
            role: "user",
            content: [{ type: "text", text: "hi" }],
          },
        ],
        tools: [],
        thinking: { enabled: false },
        cacheTtl: "1h",
        modelId: "test",
        apiKey: "k",
        maxTokens: 1024,
      },
    });

    const mgr = new CompactionManager(defaultConfig(2));
    await mgr.compact(opts);

    const llm = opts.llm as CapturingLlm;
    expect(llm.sawCachedRequest).toBe(true);
    expect(llm.lastMessageCount).toBe(1);
    expect(llm.lastUserMessage).toContain("Existing memory files:");
  });

  it("archives with retention", async () => {
    const dir = freshDir();
    const convMgr = new MockConversationMgr("new-conv-2");
    const opts = await buildCompactOpts({
      llm: new MockLlm(XML_RESPONSE),
      convMgr,
      storeDir: dir,
    });
    const mgr = new CompactionManager(defaultConfig(3));
    const outcome = await mgr.compact(opts);
    expect(outcome.kind).toBe("compacted");
    const r = (outcome as Extract<CompactionOutcome, { kind: "compacted" }>).result;
    expect(r.newConversationId).toBe("new-conv-2");
    expect(r.retainedCount).toBe(6);
    expect(convMgr.calls.length).toBe(1);
    expect(convMgr.calls[0]).toEqual({ id: "conv-1", keepLastN: 6 });
  });

  it("keep_turns=0 retains nothing", async () => {
    const dir = freshDir();
    const opts = await buildCompactOpts({
      llm: new MockLlm(XML_RESPONSE),
      convMgr: new MockConversationMgr("new-conv-zero"),
      storeDir: dir,
      keepTurnsOverride: 0,
    });
    const mgr = new CompactionManager(defaultConfig(2));
    const outcome = await mgr.compact(opts);
    expect(outcome.kind).toBe("compacted");
    const r = (outcome as Extract<CompactionOutcome, { kind: "compacted" }>).result;
    expect(r.messageCount).toBe(10);
    expect(r.compactedTurns).toBe(5);
    expect(r.retainedCount).toBe(0);
    expect(r.retainedTurns).toBe(0);
    expect(r.memoryFilesWritten.length).toBe(2);
  });

  it("keep_turns override beats config", async () => {
    const dir = freshDir();
    const opts = await buildCompactOpts({
      llm: new MockLlm(XML_RESPONSE),
      convMgr: new MockConversationMgr("new-conv-override"),
      storeDir: dir,
      keepTurnsOverride: 3,
    });
    const mgr = new CompactionManager(defaultConfig(2));
    const outcome = await mgr.compact(opts);
    expect(outcome.kind).toBe("compacted");
    const r = (outcome as Extract<CompactionOutcome, { kind: "compacted" }>).result;
    expect(r.retainedCount).toBe(6);
    expect(r.retainedTurns).toBe(3);
  });

  it("private conversation is skipped", async () => {
    const dir = freshDir();
    const opts = await buildCompactOpts({
      llm: new MockLlm(XML_RESPONSE),
      convMgr: new MockConversationMgr("ignored"),
      storeDir: dir,
      isPrivate: true,
    });
    const mgr = new CompactionManager(defaultConfig());
    await expect(mgr.compact(opts)).rejects.toBeInstanceOf(CompactionError);
    try {
      await mgr.compact(opts);
    } catch (e) {
      expect((e as CompactionError).kind).toBe("privateConversation");
    }
    expect((opts.conversationManager as MockConversationMgr).calls.length).toBe(0);
  });

  it("dry run does not write or archive", async () => {
    const dir = freshDir();
    const opts = await buildCompactOpts({
      llm: new MockLlm(XML_RESPONSE),
      convMgr: new MockConversationMgr("ignored"),
      storeDir: dir,
      dryRun: true,
    });
    const mgr = new CompactionManager(defaultConfig(2));
    const outcome = await mgr.compact(opts);
    expect(outcome.kind).toBe("dryRun");
    const r = (outcome as Extract<CompactionOutcome, { kind: "dryRun" }>).result;
    expect(r.wouldWriteFiles).toBe(2);
    expect(r.messageCount).toBe(6);
    expect(r.compactedTurns).toBe(3);
    expect(r.retainedCount).toBe(4);
    expect(r.fileOpsPreview.length).toBe(2);
    expect(r.fileOpsPreview.every((op) => op.path.startsWith("memory/"))).toBe(true);

    expect(() => opts.markdownStore!.read("daily/2026-03-25.md")).toThrow();
    expect((opts.conversationManager as MockConversationMgr).calls.length).toBe(0);
  });

  it("rejects empty messages", async () => {
    const dir = freshDir();
    const opts = await buildCompactOpts({
      llm: new MockLlm(""),
      convMgr: new MockConversationMgr("ignored"),
      storeDir: dir,
      messages: [],
    });
    const mgr = new CompactionManager(defaultConfig());
    await expect(mgr.compact(opts)).rejects.toBeInstanceOf(CompactionError);
  });

  it("rejects fewer than keep_recent_turns user turns", async () => {
    const dir = freshDir();
    const opts = await buildCompactOpts({
      llm: new MockLlm(""),
      convMgr: new MockConversationMgr("ignored"),
      storeDir: dir,
      messages: makeMessages(5),
    });
    const mgr = new CompactionManager({
      ...defaultConfig(10),
    });
    await expect(mgr.compact(opts)).rejects.toBeInstanceOf(CompactionError);
  });

  it("rolls back overwritten markdown on archive failure", async () => {
    const dir = freshDir();
    const opts = await buildCompactOpts({
      llm: new MockLlm(XML_RESPONSE),
      convMgr: new FailingConversationMgr(),
      storeDir: dir,
    });
    const original = "# Beverage Preferences\n\n- User prefers coffee on weekends\n";
    opts.markdownStore!.write("preferences/beverages.md", original);

    const mgr = new CompactionManager(defaultConfig(2));
    await expect(mgr.compact(opts)).rejects.toBeInstanceOf(CompactionError);

    const restored = opts.markdownStore!.read("preferences/beverages.md");
    expect(restored.content).toBe(original);
  });
});
