/**
 * Wire-shape regressions for `prepareChatRequest`.
 *
 * Pins the shape of the messages array we send to providers. The
 * specific shape these tests defend is the alternating-role,
 * tool-loop-separated form that Anthropic (and most OpenAI-compatible
 * APIs) require. Storage on disk keeps the same shape; the live
 * `historySnapshot()` helper applies a merge for client display, and
 * `prepareChatRequest` must NOT use that merged form when building the
 * outbound request.
 *
 * Bug surfaced 2026-05-26 on the live cache-accounting test:
 * `prepareChatRequest` was reading `engine.historySnapshot().messages`
 * (merged for display) instead of `engine.messages()` (raw storage),
 * which collapsed an `[asst(tool_use), user(tool_result), asst(text)]`
 * sequence into a single assistant message with a `tool_result` block
 * inside it — Anthropic returns 400 "tool_result blocks can only be in
 * user messages".
 */
import { describe, expect, it } from "bun:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { ConversationEngine } from "../src/engine/engine.ts";
import type { ContentBlock, Message } from "../src/engine/types.ts";
import type { ResolvedModel } from "../src/llm/catalog.ts";
import { prepareChatRequest } from "../src/llm/generate.ts";
import { ToolRegistry } from "../src/tools/registry.ts";

function tempDir(): string {
  return mkdtempSync(path.join(tmpdir(), "shore-wire-shape-test-"));
}

function resolved(): ResolvedModel {
  return {
    name: "haiku",
    qualifiedName: "chat.anthropic.haiku",
    category: "chat",
    providerKey: "anthropic",
    sdk: "anthropic",
    modelId: "claude-haiku-4-5",
    apiKeyEnv: undefined,
    baseUrl: undefined,
    maxTokens: 4096,
    maxContextTokens: 200_000,
    temperature: 1,
    topP: undefined,
    reasoningEffort: undefined,
    budgetTokens: undefined,
    cacheTtl: "1h",
    openrouterProvider: undefined,
  };
}

function msg(role: "user" | "assistant", blocks: ContentBlock[], id: string): Message {
  return {
    msg_id: id,
    role,
    content: blocks
      .filter((b): b is Extract<ContentBlock, { type: "text" }> => b.type === "text")
      .map((b) => b.text)
      .join(""),
    images: [],
    content_blocks: blocks,
    timestamp: "2026-05-24T12:00:00Z",
  };
}

describe("prepareChatRequest wire shape", () => {
  it("preserves separate-turn tool-loop shape (no tool_result in assistant)", async () => {
    const root = tempDir();
    const dataDir = path.join(root, "data");
    const configDir = path.join(root, "config");
    const characterConfigDir = path.join(configDir, "characters", "alice");
    const engine = new ConversationEngine("alice", path.join(dataDir, "alice"));

    // Persist a realistic tool-loop conversation in the same separate-turn
    // shape the daemon writes after `runToolLoop` returns:
    //   user → asst(tool_use) → user(tool_result) → asst(text)
    await engine.appendMessage(
      msg("user", [{ type: "text", text: "Roll 1d20 for stealth." }], "m_u1"),
    );
    await engine.appendMessage(
      msg(
        "assistant",
        [{ type: "tool_use", id: "tu_1", name: "roll_dice", input: { count: 1, sides: 20 } }],
        "m_a1",
      ),
    );
    await engine.appendMessage(
      msg(
        "user",
        [{ type: "tool_result", tool_use_id: "tu_1", content: "13", is_error: false }],
        "m_u2",
      ),
    );
    await engine.appendMessage(
      msg("assistant", [{ type: "text", text: "Thirteen — passable." }], "m_a2"),
    );
    await engine.appendMessage(
      msg("user", [{ type: "text", text: "Good. What now?" }], "m_u3"),
    );

    const req = prepareChatRequest({
      engine,
      characterConfigDir,
      configDir,
      displayName: "Ren",
      resolved: resolved(),
      registry: new ToolRegistry(),
      apiKey: "test-key",
    });

    // The wire request must alternate roles exactly the way storage
    // does. If `historySnapshot().messages` (display merge) were used,
    // messages would collapse to [user, assistant(mixed), user] — the
    // bug we're guarding against.
    expect(req.messages.map((m) => m.role)).toEqual([
      "user",
      "assistant",
      "user",
      "assistant",
      "user",
    ]);

    // Block-type pin: tool_use lives on the assistant turn, tool_result
    // on the following user turn. Crossing them is the Anthropic 400.
    const blockTypes = req.messages.map((m) =>
      (m.content as ContentBlock[]).map((b) => b.type),
    );
    expect(blockTypes[0]).toEqual(["text"]);
    expect(blockTypes[1]).toEqual(["tool_use"]);
    expect(blockTypes[2]).toEqual(["tool_result"]);
    expect(blockTypes[3]).toEqual(["text"]);
    expect(blockTypes[4]).toEqual(["text"]);

    // And explicitly: no assistant message may contain a tool_result.
    for (let i = 0; i < req.messages.length; i++) {
      const m = req.messages[i]!;
      if (m.role !== "assistant") continue;
      const blocks = m.content as ContentBlock[];
      const hasToolResult = blocks.some((b) => b.type === "tool_result");
      expect(hasToolResult).toBe(false);
    }
  });
});
