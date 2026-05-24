/**
 * Compaction parser tests — mirror of
 * `backend/daemon/src/memory/compaction/parser.rs::tests`.
 */
import { describe, expect, it } from "bun:test";

import {
  extractWriteOps,
  extractXmlTag,
  parseCompactionResponse,
} from "../src/memory/compaction/parser.ts";

function memoryResponse(): string {
  return `<memory>
<write path="daily/2026-03-25.md">
# Conversation on 2026-03-25

- User discussed their day
- They mentioned having a busy morning
</write>

<write path="preferences/beverages.md">
# Beverage Preferences

- User prefers tea over coffee
- This is a stable preference
</write>
</memory>`;
}

describe("extractXmlTag", () => {
  it("extracts content between tags", () => {
    expect(extractXmlTag("before <recap>hello world</recap> after", "recap")).toBe(
      "hello world",
    );
  });

  it("returns undefined when tag not found", () => {
    expect(extractXmlTag("no tags here", "recap")).toBeUndefined();
  });

  it("returns undefined when tag content is empty", () => {
    expect(extractXmlTag("<recap></recap>", "recap")).toBeUndefined();
  });

  it("trims surrounding whitespace", () => {
    expect(extractXmlTag("<recap>\n  trimmed content  \n</recap>", "recap")).toBe(
      "trimmed content",
    );
  });
});

describe("parseCompactionResponse", () => {
  it("parses a memory block with multiple writes", () => {
    const ops = parseCompactionResponse(memoryResponse());
    expect(ops.length).toBe(2);
    expect(ops[0]!.path).toBe("daily/2026-03-25.md");
    expect(ops[0]!.content).toContain("User discussed their day");
    expect(ops[1]!.path).toBe("preferences/beverages.md");
    expect(ops[1]!.content).toContain("User prefers tea");
  });

  it("returns empty for an empty memory block", () => {
    expect(parseCompactionResponse("<memory></memory>")).toEqual([]);
  });

  it("ignores legacy recap responses with no memory block", () => {
    expect(
      parseCompactionResponse("<recap>The conversation was about cats</recap>"),
    ).toEqual([]);
  });

  it("parses memory without legacy recap", () => {
    const ops = parseCompactionResponse(
      `<memory>
<write path="test.md">
- Something happened
</write>
</memory>`,
    );
    expect(ops.length).toBe(1);
    expect(ops[0]!.path).toBe("test.md");
  });
});

describe("extractWriteOps", () => {
  it("tolerates nested xml inside content", () => {
    const ops = extractWriteOps(`<write path="test.md">
# Test

- Line with <b>bold</b> text
</write>`);
    expect(ops.length).toBe(1);
    expect(ops[0]!.path).toBe("test.md");
    expect(ops[0]!.content).toContain("<b>bold</b>");
  });

  it("extracts multiple writes", () => {
    const ops = extractWriteOps(`<write path="a.md">Content A</write>
<write path="b.md">Content B</write>`);
    expect(ops.length).toBe(2);
    expect(ops[0]).toEqual({ path: "a.md", content: "Content A" });
    expect(ops[1]).toEqual({ path: "b.md", content: "Content B" });
  });

  it("skips writes missing the closing tag", () => {
    expect(extractWriteOps(`<write path="a.md">Content A`)).toEqual([]);
  });

  it("skips writes missing the path attribute", () => {
    expect(extractWriteOps(`<write>Content A</write>`)).toEqual([]);
  });

  it("emits an op with empty content when content is empty", () => {
    const ops = extractWriteOps(`<write path="a.md"></write>`);
    expect(ops.length).toBe(1);
    expect(ops[0]).toEqual({ path: "a.md", content: "" });
  });
});
