/**
 * `runCompaction` end-to-end — exercises lock acquisition, active.jsonl
 * load, compaction pass, applyDeferredEdits at the boundary, and the
 * MEMORY.md snapshot activation.
 *
 * Sibling of `backend/daemon/src/memory/compaction/background.rs` (no
 * direct Rust test counterpart — the Rust test surface lives in
 * `compaction/mod.rs` since `compact()` is what the test mocks; this
 * test mirrors the run_compaction contract from outside).
 */
import { afterEach, describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import {
  _resetCompactionLocksForTest,
  runCompaction,
  tryBeginCompaction,
} from "../src/memory/compaction/index.ts";
import {
  activePromptFile,
  ensureCharacterWorkspace,
  MEMORY_INDEX_FILE,
} from "../src/memory/deferred_edits.ts";
import type { CompactionLlm } from "../src/memory/compaction/types.ts";
import type { ChatRequest } from "../src/llm/types.ts";

afterEach(() => {
  _resetCompactionLocksForTest();
});

function freshDir(): string {
  return mkdtempSync(path.join(tmpdir(), "shore-bg-test-"));
}

class FixedLlm implements CompactionLlm {
  calls = 0;
  constructor(public response: string) {}
  async summarize(
    _system: string,
    _messages: Array<{ role: "user" | "assistant"; content: string }>,
    _cached: ChatRequest | undefined,
  ): Promise<string> {
    this.calls += 1;
    return this.response;
  }
}

function defaultCfg(keepRecentTurns = 2): {
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

function seedActiveJsonl(characterDir: string, turnCount: number): void {
  fs.mkdirSync(characterDir, { recursive: true });
  const lines: string[] = [];
  for (let i = 0; i < turnCount; i++) {
    const role = i % 2 === 0 ? "user" : "assistant";
    lines.push(
      JSON.stringify({
        msg_id: `m${i}`,
        role,
        timestamp: `2026-05-24T10:00:0${i}Z`,
        images: [],
        content_blocks: [{ type: "text", text: `turn ${i}` }],
      }),
    );
  }
  fs.writeFileSync(path.join(characterDir, "active.jsonl"), lines.join("\n") + "\n");
}

describe("runCompaction", () => {
  it("returns 0 retained when active.jsonl is empty or missing", async () => {
    const root = freshDir();
    const dataDir = path.join(root, "data");
    const configDir = path.join(root, "config");
    const llm = new FixedLlm("<memory></memory>");

    const r = await runCompaction({
      character: "Alpha",
      dataDir,
      configDir,
      config: defaultCfg(),
      displayName: "Tester",
      llm,
    });
    expect(r.retainedTurns).toBe(0);
    expect(llm.calls).toBe(0);
  });

  it("compacts, archives, and applies deferred edits at the boundary", async () => {
    const root = freshDir();
    const dataDir = path.join(root, "data");
    const configDir = path.join(root, "config");
    const character = "Alpha";

    ensureCharacterWorkspace(
      path.join(dataDir, character),
      configDir,
      character,
    );

    seedActiveJsonl(path.join(dataDir, character), 10);

    const llm = new FixedLlm(`<memory>
<write path="MEMORY.md"># Memory Index

## Throughline
- Compacted throughline.
</write>
<write path="memory/topics/alpha.md"># Topic Alpha
- A note.
</write>
</memory>`);

    const r = await runCompaction({
      character,
      dataDir,
      configDir,
      config: defaultCfg(2),
      displayName: "Tester",
      llm,
    });

    expect(r.retainedTurns).toBe(2);
    expect(llm.calls).toBe(1);
    expect(r.outcome?.kind).toBe("compacted");

    // Segment landed.
    expect(
      fs.existsSync(path.join(dataDir, character, "segments", "0001.jsonl")),
    ).toBe(true);

    // active.jsonl trimmed to the retained tail.
    const active = fs.readFileSync(
      path.join(dataDir, character, "active.jsonl"),
      "utf8",
    );
    expect(active.split("\n").filter((l) => l.length > 0).length).toBe(4);

    // Workspace MEMORY.md was written by compaction.
    const workspace = path.join(configDir, "characters", character, "workspace");
    const memoryContent = fs.readFileSync(path.join(workspace, MEMORY_INDEX_FILE), "utf8");
    expect(memoryContent).toContain("Compacted throughline");

    // After applyDeferredEdits, the active_prompt snapshot mirrors the
    // canonical workspace file (no longer the zero-byte sentinel).
    const snapshot = fs.readFileSync(
      activePromptFile(path.join(dataDir, character), MEMORY_INDEX_FILE),
      "utf8",
    );
    expect(snapshot).toContain("Compacted throughline");

    // Deferred-edits queue cleared.
    expect(
      fs.existsSync(path.join(dataDir, character, "deferred_edits.jsonl")),
    ).toBe(false);
  });

  it("rejects re-entrant runs (single-flight guard)", async () => {
    const root = freshDir();
    const dataDir = path.join(root, "data");
    const character = "Beta";
    const guard = tryBeginCompaction(dataDir, character)!;
    try {
      await expect(
        runCompaction({
          character,
          dataDir,
          configDir: path.join(root, "config"),
          config: defaultCfg(),
          displayName: "T",
          llm: new FixedLlm("<memory></memory>"),
        }),
      ).rejects.toThrow(/already running/);
    } finally {
      guard.release();
    }
  });
});
