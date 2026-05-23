/**
 * Tests for `ConversationEngine.rewindLastAssistantTurn()` — the regen
 * primitive. Verifies that trailing tool-loop intermediates get popped
 * together with the final assistant turn, and that the persisted file
 * reflects the truncation.
 */
import { describe, expect, it } from "bun:test";
import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { ConversationEngine } from "../src/engine/engine.ts";
import type { Message } from "../src/engine/types.ts";

function tempEngine(): ConversationEngine {
  const dir = mkdtempSync(join(tmpdir(), "shore-rewind-test-"));
  return new ConversationEngine("test", dir);
}

function userMsg(text: string): Message {
  return {
    msg_id: `m_${crypto.randomUUID()}`,
    role: "user",
    content: text,
    images: [],
    content_blocks: [{ type: "text", text }],
    timestamp: "2026-01-01T00:00:00Z",
  };
}

function asstTextMsg(text: string): Message {
  return {
    msg_id: `m_${crypto.randomUUID()}`,
    role: "assistant",
    content: text,
    images: [],
    content_blocks: [{ type: "text", text }],
    timestamp: "2026-01-01T00:00:00Z",
  };
}

function asstToolUseMsg(toolId: string): Message {
  return {
    msg_id: `m_${crypto.randomUUID()}`,
    role: "assistant",
    content: "",
    images: [],
    content_blocks: [
      { type: "tool_use", id: toolId, name: "roll_dice", input: {} },
    ],
    timestamp: "2026-01-01T00:00:00Z",
  };
}

function userToolResultMsg(toolId: string, result: string): Message {
  return {
    msg_id: `m_${crypto.randomUUID()}`,
    role: "user",
    content: "",
    images: [],
    content_blocks: [
      { type: "tool_result", tool_use_id: toolId, content: result, is_error: false },
    ],
    timestamp: "2026-01-01T00:00:00Z",
  };
}

describe("ConversationEngine.rewindLastAssistantTurn", () => {
  it("returns [] when history has no trailing assistant turn", async () => {
    const eng = tempEngine();
    await eng.appendMessage(userMsg("hi"));
    const dropped = await eng.rewindLastAssistantTurn();
    expect(dropped).toEqual([]);
    expect(eng.historySnapshot().messages.length).toBe(1);
  });

  it("drops only the trailing assistant turn for a plain reply", async () => {
    const eng = tempEngine();
    await eng.appendMessage(userMsg("hi"));
    await eng.appendMessage(asstTextMsg("hello back"));
    const dropped = await eng.rewindLastAssistantTurn();
    expect(dropped.length).toBe(1);
    expect(dropped[0]!.role).toBe("assistant");
    const remaining = eng.historySnapshot().messages;
    expect(remaining.length).toBe(1);
    expect(remaining[0]!.role).toBe("user");
  });

  it("drops the whole tool-loop tail [tool_use, tool_result, final-asst]", async () => {
    const eng = tempEngine();
    await eng.appendMessage(userMsg("roll a die"));
    await eng.appendMessage(asstToolUseMsg("tu_1"));
    await eng.appendMessage(userToolResultMsg("tu_1", "3"));
    await eng.appendMessage(asstTextMsg("you rolled a 3"));
    const dropped = await eng.rewindLastAssistantTurn();
    expect(dropped.length).toBe(3);
    expect(dropped.map((m) => m.role)).toEqual(["assistant", "user", "assistant"]);
    const remaining = eng.historySnapshot().messages;
    expect(remaining.length).toBe(1);
    expect(remaining[0]!.role).toBe("user");
    expect(remaining[0]!.content).toBe("roll a die");
  });

  it("persists the truncation to active.jsonl", async () => {
    const dir = mkdtempSync(join(tmpdir(), "shore-rewind-persist-"));
    const eng = new ConversationEngine("test", dir);
    await eng.appendMessage(userMsg("hi"));
    await eng.appendMessage(asstTextMsg("hello"));
    await eng.rewindLastAssistantTurn();
    const onDisk = readFileSync(join(dir, "active.jsonl"), "utf8");
    const lines = onDisk.split("\n").filter((l) => l.length > 0);
    expect(lines.length).toBe(1);
    const parsed = JSON.parse(lines[0]!);
    expect(parsed.role).toBe("user");
  });

  it("advances the revision so subscribers see the truncated state", async () => {
    const eng = tempEngine();
    await eng.appendMessage(userMsg("hi"));
    await eng.appendMessage(asstTextMsg("hello"));
    const beforeRev = eng.historySnapshot().revision;
    await eng.rewindLastAssistantTurn();
    expect(eng.historySnapshot().revision).toBeGreaterThan(beforeRev);
  });
});
