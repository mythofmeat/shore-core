/**
 * Phase 9b ConversationEngine parity tests.
 *
 * Mirrors the Rust unit-test block around
 * `backend/daemon/src/engine/{mod.rs,messages.rs}` for the API surface
 * needed by conversation commands: rewrite-generation semantics, segment
 * display history, chronological inserts, edit/delete/truncate/reset, and
 * alternate-response bookkeeping.
 */
import { describe, expect, test } from "bun:test";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { ConversationEngine } from "../src/engine/engine.ts";
import { MessageStore } from "../src/engine/messages.ts";
import type { Message, MessageAlternative, Role } from "../src/engine/types.ts";

function tempDir(prefix = "shore-engine-phase9b-"): string {
  return mkdtempSync(join(tmpdir(), prefix));
}

function makeEngine(dir = tempDir()): ConversationEngine {
  return new ConversationEngine("TestChar", dir);
}

function msg(
  id: string,
  role: Role,
  content: string,
  timestamp = "2026-01-01T00:00:00Z",
): Message {
  return {
    msg_id: id,
    role,
    content,
    images: [],
    content_blocks: content === "" ? [] : [{ type: "text", text: content }],
    timestamp,
  };
}

function toolResult(id: string): Message {
  return {
    msg_id: id,
    role: "user",
    content: "",
    images: [],
    content_blocks: [
      { type: "tool_result", tool_use_id: "tool_1", content: "result" },
    ],
    timestamp: "2026-01-01T00:00:02Z",
  };
}

function alt(content: string, timestamp: string): MessageAlternative {
  return {
    content,
    images: [],
    content_blocks: [{ type: "text", text: content }],
    timestamp,
  };
}

function readIds(dir: string): string[] {
  return readFileSync(join(dir, "active.jsonl"), "utf8")
    .split("\n")
    .filter((line) => line.length > 0)
    .map((line) => (JSON.parse(line) as { msg_id: string }).msg_id);
}

describe("ConversationEngine Phase 9b parity", () => {
  test("tracks raw message count and real-user turn count", async () => {
    const engine = makeEngine();
    await engine.appendMessage(msg("u1", "user", "hello"));
    await engine.appendMessage(toolResult("tr1"));
    await engine.appendMessage(msg("a1", "assistant", "hi"));

    expect(engine.messageCount()).toBe(3);
    expect(engine.turnCount()).toBe(1);
    expect(engine.messages().map((m) => m.msg_id)).toEqual(["u1", "tr1", "a1"]);
  });

  test("displayHistory includes archived segments before the active tail", async () => {
    const dir = tempDir();
    const segmentsDir = join(dir, "segments");
    mkdirSync(segmentsDir, { recursive: true });
    writeFileSync(
      join(segmentsDir, "0001.jsonl"),
      `${JSON.stringify(msg("old_u", "user", "old question"))}\n`
        + `${JSON.stringify(msg("old_a", "assistant", "old answer"))}\n`,
    );
    writeFileSync(
      join(dir, "compaction.json"),
      JSON.stringify({
        segments: [
          {
            file: "0001.jsonl",
            message_count: 2,
            compacted_at: "2026-01-01T00:00:00Z",
          },
        ],
        total_compacted_messages: 2,
      }),
    );

    const engine = new ConversationEngine("TestChar", dir);
    await engine.appendMessage(msg("active_u", "user", "new question"));

    expect(engine.segments().segmentCount()).toBe(1);
    const display = engine.displayHistory();
    expect(display.messages.map((m) => m.msg_id)).toEqual([
      "old_u",
      "old_a",
      "active_u",
    ]);
    expect(display.active_start).toBe(2);
  });

  test("historySnapshot accepts config but remains active-only", async () => {
    const engine = makeEngine();
    await engine.appendMessage(msg("u1", "user", "hi"));

    const snapshot = engine.historySnapshot({ active_model: "model/a", private: false });
    expect(snapshot.config).toEqual({ active_model: "model/a", private: false });
    expect(snapshot.messages.map((m) => m.msg_id)).toEqual(["u1"]);
    expect(snapshot.active_start).toBe(0);
    expect(snapshot.selected_character).toBe("TestChar");
  });

  test("insertMessageByTimestamp inserts after the last <= timestamp", async () => {
    const dir = tempDir();
    const engine = new ConversationEngine("TestChar", dir);
    await engine.appendMessage(msg("late", "assistant", "late", "2026-01-01T00:00:03Z"));
    await engine.insertMessageByTimestamp(
      msg("early", "user", "early", "2026-01-01T00:00:01Z"),
    );
    await engine.insertMessageByTimestamp(
      msg("middle", "assistant", "middle", "2026-01-01T00:00:02Z"),
    );
    await engine.insertMessageByTimestamp(
      msg("bad", "assistant", "bad timestamp", "not-a-date"),
    );

    expect(engine.messages().map((m) => m.msg_id)).toEqual([
      "early",
      "middle",
      "late",
      "bad",
    ]);
    expect(readIds(dir)).toEqual(["early", "middle", "late", "bad"]);
    expect(engine.historyRewriteGeneration()).toBe(0);
  });

  test("historyRewriteGeneration advances only for history rewrites", async () => {
    const engine = makeEngine();

    await engine.appendMessage(msg("u1", "user", "hello"));
    await engine.appendMessage(msg("a1", "assistant", "hi"));
    expect(engine.currentRevision()).toBe(2);
    expect(engine.historyRewriteGeneration()).toBe(0);

    await engine.truncateAfterLastUserTurn();
    expect(engine.historyRewriteGeneration()).toBe(1);

    await engine.editMessage("u1", "hello again");
    expect(engine.historyRewriteGeneration()).toBe(2);

    await engine.deleteMessage("u1");
    expect(engine.historyRewriteGeneration()).toBe(3);
  });

  test("truncateAfterLastUserTurn removes assistant and tool-loop tail only when present", async () => {
    const engine = makeEngine();
    await engine.appendMessage(msg("u1", "user", "roll"));
    await engine.appendMessage({
      ...msg("tool_use", "assistant", ""),
      content_blocks: [{ type: "tool_use", id: "tool_1", name: "roll", input: {} }],
    });
    await engine.appendMessage(toolResult("tool_result"));
    await engine.appendMessage(msg("a1", "assistant", "rolled"));

    expect(engine.messagesThroughLastUserTurn().map((m) => m.msg_id)).toEqual(["u1"]);
    expect(await engine.truncateAfterLastUserTurn()).toBe(3);
    expect(engine.messages().map((m) => m.msg_id)).toEqual(["u1"]);
    const rewriteAfterTruncate = engine.historyRewriteGeneration();

    expect(await engine.truncateAfterLastUserTurn()).toBe(0);
    expect(engine.historyRewriteGeneration()).toBe(rewriteAfterTruncate);
  });

  test("reset clears active.jsonl and bumps rewrite generation", async () => {
    const dir = tempDir();
    const engine = new ConversationEngine("TestChar", dir);
    await engine.appendMessage(msg("u1", "user", "hello"));
    await engine.reset();

    expect(engine.messages()).toEqual([]);
    expect(readFileSync(join(dir, "active.jsonl"), "utf8")).toBe("");
    expect(engine.historyRewriteGeneration()).toBe(1);
  });

  test("reload refreshes active messages and segment metadata", async () => {
    const dir = tempDir();
    const engine = new ConversationEngine("TestChar", dir);
    await engine.appendMessage(msg("u1", "user", "before"));

    writeFileSync(
      join(dir, "active.jsonl"),
      `${JSON.stringify(msg("u2", "user", "after"))}\n`,
    );
    mkdirSync(join(dir, "segments"), { recursive: true });
    writeFileSync(
      join(dir, "compaction.json"),
      JSON.stringify({
        segments: [
          {
            file: "0001.jsonl",
            message_count: 0,
            compacted_at: "2026-01-01T00:00:00Z",
          },
        ],
        total_compacted_messages: 0,
      }),
    );

    await engine.reload();
    expect(engine.messages().map((m) => m.msg_id)).toEqual(["u2"]);
    expect(engine.segments().segmentCount()).toBe(1);
    expect(engine.historyRewriteGeneration()).toBe(1);
  });

  test("state changes broadcast active-only history with config object", async () => {
    const broadcasts: unknown[] = [];
    const engine = new ConversationEngine("TestChar", tempDir(), {
      onBroadcast: (snapshot) => broadcasts.push(snapshot),
    });

    await engine.appendMessage(msg("u1", "user", "hello"));
    engine.broadcastHistory();

    expect(broadcasts).toHaveLength(2);
    expect(broadcasts[0]).toMatchObject({
      config: {},
      revision: 1,
      selected_character: "TestChar",
    });
    expect(broadcasts[1]).toMatchObject({ config: {}, revision: 1 });
  });
});

describe("ConversationEngine alternate response parity", () => {
  test("setAlt and addAltCandidate update alt counters without rewrite bump", async () => {
    const engine = makeEngine();
    await engine.appendMessage(msg("a1", "assistant", "Response A"));

    await engine.setAlt("a1", 0, 1);
    expect(engine.messages()[0]!.alt_index).toBe(0);
    expect(engine.messages()[0]!.alt_count).toBe(1);

    const count = await engine.addAltCandidate("a1");
    expect(count).toBe(2);
    expect(engine.messages()[0]!.alt_index).toBe(1);
    expect(engine.messages()[0]!.alt_count).toBe(2);
    expect(engine.historyRewriteGeneration()).toBe(0);
  });

  test("pendingRegenAlt preserves selected prior alternatives and replaceAfterLastUserTurn stores generated alt", async () => {
    const dir = tempDir();
    const engine = new ConversationEngine("TestChar", dir);
    await engine.appendMessage(msg("u1", "user", "Prompt"));
    await engine.appendMessage(msg("a1", "assistant", "First answer"));

    const promptMessages = engine.messagesThroughLastUserTurn();
    expect(promptMessages.map((m) => m.msg_id)).toEqual(["u1"]);

    const pending = engine.pendingRegenAlt();
    expect(pending?.alternatives.map((a) => a.content)).toEqual(["First answer"]);

    const regenerated = [msg("a2", "assistant", "Second answer")];
    const attached = MessageStore.attachGeneratedAlt(regenerated, pending!.alternatives);
    expect(attached).toEqual({ alt_index: 1, alt_count: 2 });

    expect(await engine.replaceAfterLastUserTurn(regenerated)).toBe(1);
    const active = engine.messages()[1]!;
    expect(active.msg_id).toBe("a2");
    expect(active.content).toBe("Second answer");
    expect(active.alt_index).toBe(1);
    expect(active.alt_count).toBe(2);
    expect(active.alternatives?.map((a) => a.content)).toEqual([
      "First answer",
      "Second answer",
    ]);

    const selected = await engine.selectAlt("a2", 0);
    expect(selected).toEqual({
      msg_id: "a2",
      alt_index: 0,
      alt_count: 2,
      content: "First answer",
    });
    expect(engine.messages()).toHaveLength(2);
    expect(engine.messages()[1]!.content).toBe("First answer");
    expect(engine.messages()[1]!.alt_index).toBe(0);

    const reloaded = new ConversationEngine("TestChar", dir);
    expect(reloaded.messages()[1]!.content).toBe("First answer");
    expect(reloaded.messages()[1]!.alternatives).toHaveLength(2);
  });

  test("pendingRegenAlt replaces the active slot when alt_index points at an existing alternative", async () => {
    const engine = makeEngine();
    await engine.appendMessage(msg("u1", "user", "Prompt"));
    const active = msg("a2", "assistant", "Edited second answer");
    active.alt_index = 1;
    active.alt_count = 2;
    active.alternatives = [
      alt("First answer", "2026-01-01T00:00:00Z"),
      alt("Old second answer", "2026-01-01T00:00:01Z"),
    ];
    await engine.appendMessage(active);

    expect(engine.pendingRegenAlt()?.alternatives.map((a) => a.content)).toEqual([
      "First answer",
      "Edited second answer",
    ]);
  });

  test("selectAlt on the current tail replaces the raw tail with the selected message", async () => {
    const engine = makeEngine();
    await engine.appendMessage(msg("u1", "user", "Prompt"));
    const active = msg("a2", "assistant", "Second answer");
    active.alt_index = 1;
    active.alt_count = 2;
    active.alternatives = [
      alt("First answer", "2026-01-01T00:00:00Z"),
      alt("Second answer", "2026-01-01T00:00:01Z"),
    ];
    await engine.appendMessage(active);

    const beforeRewrite = engine.historyRewriteGeneration();
    await engine.selectAlt("a2", 0);

    expect(engine.messages().map((m) => m.msg_id)).toEqual(["u1", "a2"]);
    expect(engine.messages()[1]!.content).toBe("First answer");
    expect(engine.historyRewriteGeneration()).toBe(beforeRewrite + 1);
  });

  test("selectAlt on an earlier assistant rewrites that raw message in place", async () => {
    const engine = makeEngine();
    const earlier = msg("a1", "assistant", "Second earlier");
    earlier.alt_index = 1;
    earlier.alt_count = 2;
    earlier.alternatives = [
      alt("First earlier", "2026-01-01T00:00:00Z"),
      alt("Second earlier", "2026-01-01T00:00:01Z"),
    ];
    await engine.appendMessage(earlier);
    await engine.appendMessage(msg("u1", "user", "Prompt"));
    await engine.appendMessage(msg("a2", "assistant", "Tail"));

    await engine.selectAlt("a1", 0);
    expect(engine.messages().map((m) => m.msg_id)).toEqual(["a1", "u1", "a2"]);
    expect(engine.messages()[0]!.content).toBe("First earlier");
  });

  test("listAlternatives defaults to latest assistant and marks active alternative", async () => {
    const engine = makeEngine();
    await engine.appendMessage(msg("u1", "user", "Prompt"));
    const active = msg("a2", "assistant", "Second answer");
    active.alt_index = 1;
    active.alt_count = 2;
    active.alternatives = [
      alt("First answer", "2026-01-01T00:00:00Z"),
      alt("Second answer", "2026-01-01T00:00:01Z"),
    ];
    await engine.appendMessage(active);

    expect(engine.listAlternatives()).toEqual({
      ref: "a2",
      alt_index: 1,
      position: 2,
      alt_count: 2,
      alternatives: [
        {
          index: 0,
          position: 1,
          active: false,
          content: "First answer",
          images: [],
          timestamp: "2026-01-01T00:00:00Z",
        },
        {
          index: 1,
          position: 2,
          active: true,
          content: "Second answer",
          images: [],
          timestamp: "2026-01-01T00:00:01Z",
        },
      ],
    });
  });

  test("selectAlt rejects missing and out-of-range alternatives", async () => {
    const engine = makeEngine();
    await engine.appendMessage(msg("a1", "assistant", "Plain"));

    await expect(engine.selectAlt("a1", 0)).rejects.toThrow(/no alternate responses/);
    await expect(engine.selectAlt("missing", 0)).rejects.toThrow(/message not found/);

    const active = engine.messages()[0]!;
    active.alternatives = [alt("Only", "2026-01-01T00:00:00Z")];
    active.alt_index = 0;
    active.alt_count = 1;
    await expect(engine.selectAlt("a1", 1)).rejects.toThrow(/out of range/);
  });

  test("reset creates active.jsonl even when starting from an empty store", async () => {
    const dir = tempDir();
    const engine = new ConversationEngine("TestChar", dir);
    await engine.reset();
    expect(existsSync(join(dir, "active.jsonl"))).toBe(true);
  });
});
