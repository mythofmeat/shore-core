/**
 * Port of `backend/daemon/src/engine/prompt.rs::mod tests` (~28 cases).
 *
 * Time-marker tests render in the local timezone of whichever process runs
 * them — same convention as the Rust tests. Both formatter and assertion
 * reach for `Local`, so they stay portable across host TZs.
 */
import { describe, expect, it } from "bun:test";
import type { ContentBlock, Message, Role } from "../src/engine/types.ts";
import {
  assemblePrompt,
  estimateMessageTokens,
  estimateTokens,
  formatTimeMarker,
  type PromptParams,
  relativeGapPhrase,
  renderTemplate,
  trimMessages,
  xmlTagFromName,
} from "../src/engine/prompt.ts";

// ── helpers ──────────────────────────────────────────────────────────────

function makeMsg(role: Role, content: string): Message {
  return {
    msg_id: crypto.randomUUID(),
    role,
    content,
    images: [],
    content_blocks: [],
    timestamp: "2026-01-01T00:00:00Z",
  };
}

function makeMsgAt(role: Role, content: string, timestamp: string): Message {
  return {
    msg_id: crypto.randomUUID(),
    role,
    content,
    images: [],
    content_blocks: [{ type: "text", text: content }],
    timestamp,
  };
}

function testVars(): Record<string, string> {
  return {
    char: "Shore",
    character_name: "Shore",
    user: "Alice",
  };
}

function makeParams(messages: Message[]): PromptParams {
  return {
    character_name: "TestChar",
    display_name: "TestUser",
    is_private: false,
    has_prior_context: false,
    messages,
  };
}

/** Match Rust's `current_ts.with_timezone(&Local).format("%A %Y-%m-%d · %-I:%M %p")`. */
function localTimeStr(rfc3339: string): string {
  const result = formatTimeMarker(null, new Date(rfc3339));
  return result.slice(1, -1); // strip `[` and `]`
}

// ── Template rendering ────────────────────────────────────────────────────

describe("renderTemplate", () => {
  it("substitutes variables", () => {
    expect(renderTemplate("Hello, {{char}}!", testVars())).toBe("Hello, Shore!");
  });

  it("substitutes {{character_name}} for backward compat", () => {
    expect(renderTemplate("Hello, {{character_name}}!", testVars())).toBe(
      "Hello, Shore!",
    );
  });

  it("leaves unknown vars unchanged", () => {
    expect(renderTemplate("Hello, {{unknown}}!", testVars())).toBe(
      "Hello, {{unknown}}!",
    );
  });

  it("includes conditional block when key is set and non-empty", () => {
    const vars = { ...testVars(), character_definition: "A helpful companion" };
    const template =
      "Start.{{#if character_definition}}\nDef: {{character_definition}}{{/if}}\nEnd.";
    expect(renderTemplate(template, vars)).toBe(
      "Start.\nDef: A helpful companion\nEnd.",
    );
  });

  it("drops conditional block when key is absent", () => {
    const template =
      "Start.{{#if character_definition}}\nDef: {{character_definition}}{{/if}}\nEnd.";
    expect(renderTemplate(template, testVars())).toBe("Start.\nEnd.");
  });

  it("drops conditional block when key is empty string", () => {
    const vars = { ...testVars(), recap: "" };
    const template = "Before{{#if recap}} RECAP: {{recap}}{{/if}} After";
    expect(renderTemplate(template, vars)).toBe("Before After");
  });

  it("renders the built-in system template through assemblePrompt", () => {
    // Builtin is internal; exercise via assemblePrompt with no override.
    const result = assemblePrompt(makeParams([]));
    const sys = result.system[0]!.content;
    expect(sys).toContain("You are TestChar, in conversation with TestUser.");
    expect(sys).toContain("Communicate directly");
  });
});

// ── XML tag helper ────────────────────────────────────────────────────────

describe("xmlTagFromName", () => {
  it("lowercases and replaces special chars with underscore", () => {
    expect(xmlTagFromName("Alice", "character")).toBe("alice");
    expect(xmlTagFromName("Dr. Bob", "character")).toBe("dr_bob");
    expect(xmlTagFromName("Shore v2", "character")).toBe("shore_v2");
  });

  it("falls back when result would be empty", () => {
    expect(xmlTagFromName("", "character")).toBe("character");
    expect(xmlTagFromName("...", "user")).toBe("user");
  });

  it("collapses runs of underscores", () => {
    expect(xmlTagFromName("a - b", "x")).toBe("a_b");
  });
});

// ── Token estimation ──────────────────────────────────────────────────────

describe("estimateTokens", () => {
  it("uses ~4 bytes per token, ceil", () => {
    expect(estimateTokens("Hello world!")).toBe(3);
    expect(estimateTokens("")).toBe(0);
    expect(estimateTokens("Hello")).toBe(2);
  });

  it("uses byte length, not char count", () => {
    expect(estimateTokens("Hello")).toBe(2);

    // CJK: 3 bytes per char → 15 bytes / 4 = 4 tokens.
    const cjk = "日本語の文";
    expect(new TextEncoder().encode(cjk).length).toBe(15);
    expect(estimateTokens(cjk)).toBe(4);

    // Emoji: 4 bytes per codepoint → 16 bytes / 4 = 4 tokens.
    const emoji = "😀😁😂🤣";
    expect(new TextEncoder().encode(emoji).length).toBe(16);
    expect(estimateTokens(emoji)).toBe(4);
  });

  it("documents known undercounting on short words", () => {
    expect(estimateTokens("I am a")).toBe(2); // real ≈ 3
  });

  it("treats JSON like any other text", () => {
    const json = '{"name":"test","values":[1,2,3],"nested":{"key":"val"}}';
    const bytes = new TextEncoder().encode(json).length;
    expect(estimateTokens(json)).toBe(Math.ceil(bytes / 4));
  });

  it("treats code like any other text", () => {
    const code =
      "fn estimate_tokens(text: &str) -> usize {\n    text.len().div_ceil(4)\n}";
    const bytes = new TextEncoder().encode(code).length;
    expect(estimateTokens(code)).toBe(Math.ceil(bytes / 4));
  });
});

describe("estimateMessageTokens", () => {
  it("treats redacted_thinking as zero", () => {
    const msg: Message = {
      msg_id: "m1",
      role: "assistant",
      content: "",
      images: [],
      content_blocks: [{ type: "redacted_thinking", data: "opaque" }],
      timestamp: "2026-01-01T00:00:00Z",
    };
    expect(estimateMessageTokens(msg)).toBe(0);
  });

  it("sums mixed block types", () => {
    const blocks: ContentBlock[] = [
      { type: "thinking", thinking: "A".repeat(40) },
      { type: "text", text: "B".repeat(80) },
      { type: "tool_use", id: "tu1", name: "check_time", input: { tz: "UTC" } },
      { type: "redacted_thinking", data: "ignored" },
    ];
    const msg: Message = {
      msg_id: "m1",
      role: "assistant",
      content: "",
      images: [],
      content_blocks: blocks,
      timestamp: "2026-01-01T00:00:00Z",
    };
    const inputStr = JSON.stringify({ tz: "UTC" });
    const expected =
      10 + 20 + Math.ceil("check_time".length / 4) + Math.ceil(inputStr.length / 4);
    expect(estimateMessageTokens(msg)).toBe(expected);
  });

  it("prefers content_blocks over content when present", () => {
    const msg: Message = {
      msg_id: "m1",
      role: "assistant",
      content: "short",
      images: [],
      content_blocks: [
        { type: "text", text: "A".repeat(40) },
        { type: "thinking", thinking: "B".repeat(20) },
      ],
      timestamp: "2026-01-01T00:00:00Z",
    };
    expect(estimateMessageTokens(msg)).toBe(15);
  });

  it("falls back to content when no blocks", () => {
    expect(estimateMessageTokens(makeMsg("user", "X".repeat(20)))).toBe(5);
  });

  it("tool_use blocks have non-zero size", () => {
    const msg: Message = {
      msg_id: "m1",
      role: "assistant",
      content: "",
      images: [],
      content_blocks: [
        { type: "tool_use", id: "tu_1", name: "check_time", input: {} },
      ],
      timestamp: "2026-01-01T00:00:00Z",
    };
    expect(estimateMessageTokens(msg)).toBeGreaterThan(0);
  });

  it("tool_result blocks count their content", () => {
    const msg: Message = {
      msg_id: "m1",
      role: "user",
      content: "",
      images: [],
      content_blocks: [
        {
          type: "tool_result",
          tool_use_id: "tu_1",
          content: "2026-03-27T12:00:00Z",
        },
      ],
      timestamp: "2026-01-01T00:00:00Z",
    };
    expect(estimateMessageTokens(msg)).toBe(5);
  });
});

// ── Message trimming ──────────────────────────────────────────────────────

describe("trimMessages", () => {
  it("accounts for content_blocks size", () => {
    const small = makeMsg("user", "Hello");
    const big: Message = {
      msg_id: "m_big",
      role: "assistant",
      content: "",
      images: [],
      content_blocks: [{ type: "text", text: "X".repeat(400) }],
      timestamp: "2026-01-01T00:00:00Z",
    };
    const recent = makeMsg("user", "Recent");

    const result = trimMessages([small, big, recent], 10, false);
    expect(result[result.length - 1]!.content.endsWith("Recent")).toBe(true);
    expect(result.length).toBeLessThan(3);
  });

  it("returns all messages when budget allows", () => {
    const msgs = [makeMsg("user", "Hello"), makeMsg("assistant", "Hi there")];
    const result = trimMessages(msgs, 1000, false);
    expect(result.length).toBe(2);
    expect(result[0]!.content).toBe("Hello");
    expect(result[1]!.content).toBe("Hi there");
  });

  it("drops oldest first when budget is tight", () => {
    const msgs = [
      makeMsg("user", "A".repeat(100)),
      makeMsg("assistant", "B".repeat(100)),
      makeMsg("user", "Recent"),
    ];
    const result = trimMessages(msgs, 30, false);
    expect(result.length).toBeLessThan(3);
    expect(result[result.length - 1]!.content.endsWith("Recent")).toBe(true);
  });

  it("always includes at least one message", () => {
    const msgs = [makeMsg("user", "A".repeat(1000))];
    expect(trimMessages(msgs, 0, false).length).toBe(1);
  });

  it("preserves chronological order", () => {
    const msgs = [
      makeMsg("user", "First"),
      makeMsg("assistant", "Second"),
      makeMsg("user", "Third"),
    ];
    const result = trimMessages(msgs, 10000, false);
    expect(result[0]!.content).toBe("First");
    expect(result[1]!.content).toBe("Second");
    expect(result[2]!.content).toBe("Third");
  });
});

// ── Time-gap markers ──────────────────────────────────────────────────────

describe("formatTimeMarker", () => {
  it("omits relative phrase under threshold", () => {
    const ts = new Date("2026-04-04T09:30:00-07:00");
    const expected = `[${localTimeStr("2026-04-04T09:30:00-07:00")}]`;
    expect(formatTimeMarker(1799, ts)).toBe(expected);
    expect(formatTimeMarker(0, ts)).toBe(expected);
    expect(formatTimeMarker(null, ts)).toBe(expected);
  });

  it("renders 'about an hour later' near 1h", () => {
    const ts = new Date("2026-04-04T10:30:00-07:00");
    const result = formatTimeMarker(3600, ts);
    expect(result).toContain("about an hour later");
    expect(result).toContain(localTimeStr("2026-04-04T10:30:00-07:00"));
  });

  it("renders multi-hour phrase", () => {
    const ts = new Date("2026-04-04T21:14:00-07:00");
    const result = formatTimeMarker(6 * 3600, ts);
    expect(result).toContain("6 hours later");
    expect(result).toContain(localTimeStr("2026-04-04T21:14:00-07:00"));
  });

  it("renders 'about a day later' at ~24h", () => {
    const ts = new Date("2026-04-05T09:00:00-07:00");
    const result = formatTimeMarker(24 * 3600, ts);
    expect(result).toContain("about a day later");
    expect(result).toContain(localTimeStr("2026-04-05T09:00:00-07:00"));
  });

  it("renders multi-day phrase", () => {
    const ts = new Date("2026-04-07T09:00:00-07:00");
    const result = formatTimeMarker(3 * 86400, ts);
    expect(result).toContain("3 days later");
    expect(result).toContain(localTimeStr("2026-04-07T09:00:00-07:00"));
  });
});

describe("relativeGapPhrase", () => {
  it("matches all the threshold branches", () => {
    expect(relativeGapPhrase(3600)).toBe("about an hour later");
    expect(relativeGapPhrase(5399)).toBe("about an hour later");
    expect(relativeGapPhrase(5400)).toBe("2 hours later");
    expect(relativeGapPhrase(6 * 3600)).toBe("6 hours later");
    expect(relativeGapPhrase(64800)).toBe("about a day later");
    expect(relativeGapPhrase(129599)).toBe("about a day later");
    expect(relativeGapPhrase(129600)).toBe("2 days later");
    expect(relativeGapPhrase(3 * 86400)).toBe("3 days later");
  });
});

describe("trimMessages: time-marker injection", () => {
  it("injects big-gap marker on a user message after >30min", () => {
    const msgs = [
      makeMsgAt("user", "Good morning", "2026-04-04T09:00:00-07:00"),
      makeMsgAt("assistant", "Morning!", "2026-04-04T09:01:00-07:00"),
      makeMsgAt("user", "I'm back", "2026-04-04T15:30:00-07:00"),
    ];
    const result = trimMessages(msgs, 100_000, false);
    expect(result.length).toBe(3);
    expect(result[0]!.content.includes("later")).toBe(false);
    expect(result[0]!.content).toBe("Good morning");
    expect(result[2]!.content).toContain("hours later");
    expect(result[2]!.content).toContain(
      localTimeStr("2026-04-04T15:30:00-07:00"),
    );
    expect(result[2]!.content).toContain("I'm back");
    const first = result[2]!.content_blocks[0];
    if (first?.type !== "text") throw new Error("expected text block");
    expect(first.text).toContain("hours later");
  });

  it("does not inject marker for short gaps", () => {
    const msgs = [
      makeMsgAt("user", "Hello", "2026-04-04T09:00:00-07:00"),
      makeMsgAt("assistant", "Hi", "2026-04-04T09:01:00-07:00"),
      makeMsgAt("user", "Quick follow-up", "2026-04-04T09:10:00-07:00"),
    ];
    const result = trimMessages(msgs, 100_000, false);
    expect(result[2]!.content).toBe("Quick follow-up");
  });

  it("never marks assistant messages", () => {
    const msgs = [
      makeMsgAt("user", "Hello", "2026-04-04T09:00:00-07:00"),
      makeMsgAt("assistant", "Hey, you there?", "2026-04-04T15:00:00-07:00"),
      makeMsgAt("user", "Yeah!", "2026-04-04T15:01:00-07:00"),
    ];
    const result = trimMessages(msgs, 100_000, false);
    expect(result[1]!.content.includes("later")).toBe(false);
    expect(result[2]!.content).toBe("Yeah!");
  });

  it("anchors first user message after compaction", () => {
    const msgs = [
      makeMsgAt("assistant", "…", "2026-04-04T09:00:00-07:00"),
      makeMsgAt("user", "Continuing on", "2026-04-04T09:01:00-07:00"),
    ];
    const result = trimMessages(msgs, 100_000, true);
    expect(result.length).toBe(2);

    const expectedPrefix = `[${localTimeStr("2026-04-04T09:01:00-07:00")}]`;
    expect(result[1]!.content.startsWith(expectedPrefix)).toBe(true);
    expect(result[1]!.content).toContain("Continuing on");
    expect(result[1]!.content.includes("later")).toBe(false);

    const first = result[1]!.content_blocks[0];
    if (first?.type !== "text") throw new Error("expected text block");
    expect(first.text.startsWith(expectedPrefix)).toBe(true);
    expect(first.text).toContain("Continuing on");
  });

  it("does not anchor first user message on fresh conversation", () => {
    const msgs = [
      makeMsgAt("user", "Hello", "2026-04-04T09:00:00-07:00"),
      makeMsgAt("assistant", "Hi", "2026-04-04T09:01:00-07:00"),
    ];
    const result = trimMessages(msgs, 100_000, false);
    expect(result[0]!.content).toBe("Hello");
  });

  it("fires hourly tick after an earlier marker", () => {
    const msgs = [
      makeMsgAt("user", "Morning", "2026-04-04T09:00:00-07:00"),
      makeMsgAt("assistant", "Hey", "2026-04-04T09:01:00-07:00"),
      makeMsgAt("user", "Back", "2026-04-04T15:30:00-07:00"),
      makeMsgAt("assistant", "wb", "2026-04-04T15:35:00-07:00"),
      makeMsgAt("user", "What's up", "2026-04-04T15:50:00-07:00"),
      makeMsgAt("assistant", "not much", "2026-04-04T16:20:00-07:00"),
      makeMsgAt("user", "Still here", "2026-04-04T16:34:00-07:00"),
    ];
    const result = trimMessages(msgs, 100_000, false);
    expect(result[2]!.content).toContain("later");
    expect(result[4]!.content).toBe("What's up");
    const prefix = `[${localTimeStr("2026-04-04T16:34:00-07:00")}]`;
    expect(result[6]!.content).toContain("Still here");
    expect(result[6]!.content.startsWith(prefix)).toBe(true);
    expect(result[6]!.content.includes("later")).toBe(false);
  });
});

// ── Full assembly ─────────────────────────────────────────────────────────

describe("assemblePrompt", () => {
  it("builds a basic prompt with character + user blocks", () => {
    const messages = [makeMsg("user", "Hello"), makeMsg("assistant", "Hi!")];
    const result = assemblePrompt({
      character_name: "TestChar",
      display_name: "TestUser",
      character_definition: "A friendly test character.",
      user_definition: "A developer.",
      is_private: false,
      has_prior_context: false,
      messages,
      max_context_tokens: 200_000,
      max_output_tokens: 4096,
    });

    expect(result.system[0]!.content).toContain("TestChar");
    expect(result.system[0]!.content).toContain("TestUser");
    expect(result.system[0]!.label).toBe("system");

    const charBlock = result.system.find((b) => b.label === "character")!;
    expect(charBlock.content).toContain("A friendly test character.");
    expect(charBlock.content).toContain("<testchar>");

    const userBlock = result.system.find((b) => b.label === "user")!;
    expect(userBlock.content).toContain("A developer.");
    expect(userBlock.content).toContain("<testuser>");

    expect(result.messages.length).toBe(2);
    expect(result.messages[0]!.role).toBe("user");
    expect(result.messages[0]!.content).toBe("Hello");
  });

  it("uses a custom system_prompt template", () => {
    const params: PromptParams = {
      ...makeParams([]),
      display_name: "User",
      system_prompt: "Custom prompt for {{character_name}}.",
    };
    const result = assemblePrompt(params);
    expect(result.system[0]!.content).toBe("Custom prompt for TestChar.");
  });

  it("character template overrides the global default", () => {
    const params: PromptParams = {
      ...makeParams([]),
      system_prompt: "Character-specific template.",
    };
    const result = assemblePrompt(params);
    expect(result.system[0]!.content).toBe("Character-specific template.");
  });

  it("injects memory_index block with boilerplate", () => {
    const params: PromptParams = {
      ...makeParams([]),
      memory_index: "- `topics/rust.md` - Rust throughline.",
    };
    const result = assemblePrompt(params);
    const idx = result.system.find((b) => b.label === "memory_index")!;
    expect(idx.content).toContain("topics/rust.md");
    expect(idx.content).toContain("prompt-visible memory index");
    expect(idx.content).toContain("<memory_index>");
  });

  it("blanks {{date}} and {{time}} for cache stability", () => {
    const params: PromptParams = {
      ...makeParams([]),
      system_prompt: "Today is {{date}} at {{time}}.",
    };
    const result = assemblePrompt(params);
    const text = result.system[0]!.content;
    expect(text).not.toContain("{{date}}");
    expect(text).not.toContain("{{time}}");
    expect(text).toBe("Today is  at .");
  });

  it("emits all 5 blocks when all params are filled", () => {
    const params: PromptParams = {
      ...makeParams([]),
      tools_guidance: "Use tools carefully.",
      character_definition: "A character.",
      user_definition: "A user.",
      memory_index: "Index",
    };
    const result = assemblePrompt(params);
    expect(result.system.length).toBe(5);
    expect(result.system[0]!.label).toBe("system");
    expect(result.system[1]!.label).toBe("tools_guidance");
    expect(result.system[2]!.label).toBe("character");
    expect(result.system[3]!.label).toBe("user");
    expect(result.system[4]!.label).toBe("memory_index");
  });

  it("private conversation suppresses memory_index", () => {
    const params: PromptParams = {
      ...makeParams([]),
      memory_index: "We talked about Rust.",
      is_private: true,
    };
    const result = assemblePrompt(params);
    expect(result.system.every((b) => b.label !== "memory_index")).toBe(true);
  });

  it("private conversation has no 'Relevant Memories'", () => {
    const params: PromptParams = {
      ...makeParams([]),
      character_definition: "Friendly character",
      is_private: true,
    };
    const result = assemblePrompt(params);
    const all = result.system.map((b) => b.content).join("");
    expect(all).not.toContain("Relevant Memories");
  });

  it("respects the token budget", () => {
    const messages: Message[] = Array.from({ length: 100 }, (_, i) =>
      makeMsg(
        i % 2 === 0 ? "user" : "assistant",
        `Message number ${i} with some padding text to use tokens.`,
      ),
    );
    const result = assemblePrompt({
      ...makeParams(messages),
      max_context_tokens: 500,
      max_output_tokens: 100,
    });
    expect(result.messages.length).toBeLessThan(100);
    expect(result.messages[result.messages.length - 1]!.content).toBe(
      messages[messages.length - 1]!.content,
    );
  });

  it("does not inject heartbeat/journal/story prompts", () => {
    const result = assemblePrompt(makeParams([]));
    const all = result.system
      .map((b) => b.content.toLowerCase())
      .join(" ");
    expect(all).not.toContain("heartbeat");
    expect(all).not.toContain("journal");
    expect(all).not.toContain("story");
  });
});

// ── Trim: orphaned tool-loop stripping ────────────────────────────────────

describe("trimMessages: orphan tool-loop stripping", () => {
  function toolResultMsg(): Message {
    return {
      msg_id: crypto.randomUUID(),
      role: "user",
      content: "",
      images: [],
      content_blocks: [
        { type: "tool_result", tool_use_id: "t1", content: "result" },
      ],
      timestamp: "2026-01-01T00:00:00Z",
    };
  }

  function toolUseOnlyMsg(): Message {
    return {
      msg_id: crypto.randomUUID(),
      role: "assistant",
      content: "",
      images: [],
      content_blocks: [
        { type: "tool_use", id: "t1", name: "search", input: { q: "test" } },
      ],
      timestamp: "2026-01-01T00:00:00Z",
    };
  }

  it("drops leading orphan tool_result user message", () => {
    const msgs = [
      makeMsg("user", "Hi"),
      toolUseOnlyMsg(),
      toolResultMsg(),
      makeMsg("assistant", "Done"),
      makeMsg("user", "Recent"),
    ];
    const result = trimMessages(msgs, 5, false);
    expect(result.length).toBeGreaterThan(0);
    const first = result[0]!;
    const isToolResult =
      first.role === "user" &&
      first.content_blocks.every((b) => b.type === "tool_result");
    expect(isToolResult).toBe(false);
    expect(result[result.length - 1]!.content.endsWith("Recent")).toBe(true);
  });

  it("drops leading orphan tool_use-only assistant chain", () => {
    const msgs = [
      makeMsg("user", "Old message here"),
      toolUseOnlyMsg(),
      toolResultMsg(),
      makeMsg("user", "Recent"),
    ];
    const result = trimMessages(msgs, 5, false);
    expect(result.length).toBeGreaterThan(0);
    expect(result[result.length - 1]!.content.endsWith("Recent")).toBe(true);
    for (const msg of result) {
      const isUserToolResult =
        msg.role === "user" &&
        msg.content_blocks.length > 0 &&
        msg.content_blocks.every((b) => b.type === "tool_result");
      const isAsstToolUseOnly =
        msg.role === "assistant" &&
        msg.content_blocks.length > 0 &&
        !msg.content_blocks.some(
          (b) => b.type === "text" && b.text.length > 0,
        ) &&
        msg.content_blocks.some((b) => b.type === "tool_use");
      expect(isUserToolResult || isAsstToolUseOnly).toBe(false);
    }
  });
});
