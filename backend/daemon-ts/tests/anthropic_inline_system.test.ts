/**
 * Unit tests for `convertInlineSystemMessages` — the Anthropic adapter's
 * hard requirement, since the Messages API rejects role:"system" in the
 * messages array. Mirrors Rust `providers/anthropic.rs` test cases.
 */
import { describe, expect, it } from "bun:test";
import {
  convertInlineSystemMessages,
  wrapInlineSystemInstruction,
} from "../src/llm/providers/anthropic.ts";
import type { TurnMessage } from "../src/llm/types.ts";

describe("wrapInlineSystemInstruction", () => {
  it("uses the canonical sentinel exactly", () => {
    expect(wrapInlineSystemInstruction("Be concise.")).toBe(
      "<system_instruction>Be concise.</system_instruction>",
    );
  });

  it("handles empty text", () => {
    expect(wrapInlineSystemInstruction("")).toBe(
      "<system_instruction></system_instruction>",
    );
  });
});

describe("convertInlineSystemMessages", () => {
  it("returns input unchanged when no system messages present", () => {
    const turns: TurnMessage[] = [
      { role: "user", content: [{ type: "text", text: "hi" }] },
      { role: "assistant", content: [{ type: "text", text: "hello" }] },
    ];
    const result = convertInlineSystemMessages(turns);
    expect(result.length).toBe(2);
    expect(result[0]!.role).toBe("user");
    expect(result[1]!.role).toBe("assistant");
  });

  it("standalone trailing system message becomes wrapped user", () => {
    const turns: TurnMessage[] = [
      { role: "assistant", content: [{ type: "text", text: "hello" }] },
      { role: "system", content: [{ type: "text", text: "Be concise." }] },
    ];
    const result = convertInlineSystemMessages(turns);
    expect(result.length).toBe(2);
    expect(result[1]!.role).toBe("user");
    expect(result[1]!.content).toBe(
      "<system_instruction>Be concise.</system_instruction>",
    );
  });

  it("system after user message merges into the preceding user turn", () => {
    const turns: TurnMessage[] = [
      { role: "user", content: [{ type: "text", text: "Question." }] },
      { role: "system", content: [{ type: "text", text: "Be concise." }] },
    ];
    const result = convertInlineSystemMessages(turns);
    expect(result.length).toBe(1);
    expect(result[0]!.role).toBe("user");
    expect(result[0]!.content).toEqual([
      { type: "text", text: "Question." },
      {
        type: "text",
        text: "<system_instruction>Be concise.</system_instruction>",
      },
    ]);
  });

  it("system after assistant becomes a new wrapped user", () => {
    const turns: TurnMessage[] = [
      { role: "user", content: [{ type: "text", text: "First user." }] },
      { role: "assistant", content: [{ type: "text", text: "Assistant." }] },
      { role: "system", content: [{ type: "text", text: "Recap." }] },
    ];
    const result = convertInlineSystemMessages(turns);
    expect(result.length).toBe(3);
    expect(result[2]!.role).toBe("user");
    expect(result[2]!.content).toBe(
      "<system_instruction>Recap.</system_instruction>",
    );
  });

  it("preserves structured preceding user content when merging", () => {
    const turns: TurnMessage[] = [
      {
        role: "user",
        content: [
          {
            type: "tool_result",
            tool_use_id: "tu_1",
            content: "result",
          },
          { type: "text", text: "context" },
        ],
      },
      { role: "system", content: [{ type: "text", text: "Wrap up." }] },
    ];
    const result = convertInlineSystemMessages(turns);
    expect(result.length).toBe(1);
    expect(result[0]!.content.length).toBe(3);
    expect(result[0]!.content[0]!.type).toBe("tool_result");
    expect(result[0]!.content[1]!.type).toBe("text");
    expect(result[0]!.content[2]).toEqual({
      type: "text",
      text: "<system_instruction>Wrap up.</system_instruction>",
    });
  });

  it("concatenates multiple text blocks from one system turn", () => {
    const turns: TurnMessage[] = [
      {
        role: "system",
        content: [
          { type: "text", text: "First. " },
          { type: "text", text: "Second." },
        ],
      },
    ];
    const result = convertInlineSystemMessages(turns);
    expect(result.length).toBe(1);
    expect(result[0]!.role).toBe("user");
    expect(result[0]!.content).toBe(
      "<system_instruction>First. Second.</system_instruction>",
    );
  });

  it("does not mutate the input array", () => {
    const turns: TurnMessage[] = [
      { role: "user", content: [{ type: "text", text: "u1" }] },
      { role: "system", content: [{ type: "text", text: "s1" }] },
    ];
    const before = JSON.stringify(turns);
    convertInlineSystemMessages(turns);
    expect(JSON.stringify(turns)).toBe(before);
  });
});
