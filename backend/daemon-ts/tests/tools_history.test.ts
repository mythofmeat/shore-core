/**
 * conversation_search tests.
 *
 * Drives the handler against a synthesized character data directory
 * (active.jsonl + segments/0001.jsonl + compaction.json). Mirrors
 * `backend/daemon/src/tools/history.rs` test cases.
 */
import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import type { Message, MessageAlternative } from "../src/engine/types.ts";
import { conversationSearchHandler } from "../src/tools/history.ts";
import type { ToolContext } from "../src/tools/registry.ts";
import { ToolError } from "../src/tools/registry.ts";

function msg(id: string, role: Message["role"], content: string): Message {
  return {
    msg_id: id,
    role,
    content,
    images: [],
    content_blocks: [{ type: "text", text: content }],
    timestamp: "2026-01-01T00:00:00Z",
  };
}

function msgAt(
  id: string,
  role: Message["role"],
  content: string,
  ts: string,
): Message {
  const m = msg(id, role, content);
  m.timestamp = ts;
  return m;
}

function setup(): { dir: string; ctx: ToolContext } {
  const dir = mkdtempSync(path.join(tmpdir(), "shore-hist-test-"));
  const ctx: ToolContext = {
    characterName: "test",
    characterConfigDir: dir,
    characterDataDir: dir,
    workspaceDir: path.join(dir, "workspace"),
    configDir: dir,
    imageDir: path.join(dir, "images"),
    engine: undefined as unknown as ToolContext["engine"],
    searchConfig: {
      api_key_env: "TAVILY_API_KEY",
      max_results: 5,
      search_depth: "basic",
      include_answer: true,
    },
    retrievalConfig: { max_file_bytes: 1024 * 1024 },
  };
  return { dir, ctx };
}

function writeActive(dir: string, messages: Message[]): void {
  // Match Rust's storage shape — drop `content` since it's derivable.
  const lines = messages
    .map((m) => {
      // eslint-disable-next-line @typescript-eslint/no-unused-vars
      const { content: _c, ...rest } = m;
      return JSON.stringify(rest);
    })
    .join("\n");
  fs.writeFileSync(path.join(dir, "active.jsonl"), lines + "\n");
}

function writeSegment(dir: string, index: number, messages: Message[]): void {
  const segDir = path.join(dir, "segments");
  fs.mkdirSync(segDir, { recursive: true });
  const filename = `${String(index).padStart(4, "0")}.jsonl`;
  const lines = messages
    .map((m) => {
      // eslint-disable-next-line @typescript-eslint/no-unused-vars
      const { content: _c, ...rest } = m;
      return JSON.stringify(rest);
    })
    .join("\n");
  fs.writeFileSync(path.join(segDir, filename), lines + "\n");
  const manifest = {
    segments: [
      {
        file: filename,
        message_count: messages.length,
        compacted_at: "2026-01-01T00:00:00Z",
      },
    ],
    total_compacted_messages: messages.length,
  };
  fs.writeFileSync(
    path.join(dir, "compaction.json"),
    JSON.stringify(manifest),
  );
}

describe("conversation_search", () => {
  it("requires character_data_dir", async () => {
    const ctx: ToolContext = {
      characterName: "test",
      characterConfigDir: "",
      characterDataDir: "",
      workspaceDir: "",
      configDir: "",
      imageDir: "",
      engine: undefined as unknown as ToolContext["engine"],
      searchConfig: {
        api_key_env: "TAVILY_API_KEY",
        max_results: 5,
        search_depth: "basic",
        include_answer: true,
      },
      retrievalConfig: { max_file_bytes: 1024 * 1024 },
    };
    expect(
      conversationSearchHandler.execute({ query: "anything" }, ctx),
    ).rejects.toThrow(ToolError);
  });

  it("requires query OR time range", async () => {
    const { ctx } = setup();
    expect(
      conversationSearchHandler.execute({}, ctx),
    ).rejects.toThrow(/query, start_time, end_time/);
  });

  it("rejects an invalid start_time", async () => {
    const { ctx } = setup();
    expect(
      conversationSearchHandler.execute(
        { start_time: "not-a-timestamp" },
        ctx,
      ),
    ).rejects.toThrow(ToolError);
  });

  it("rejects start_time > end_time", async () => {
    const { ctx } = setup();
    expect(
      conversationSearchHandler.execute(
        {
          start_time: "2026-05-13T10:00:00+10:00",
          end_time: "2026-05-13T09:00:00+10:00",
        },
        ctx,
      ),
    ).rejects.toThrow(/before or equal/);
  });

  it("finds matches across both segments and active.jsonl", async () => {
    const { dir, ctx } = setup();
    writeSegment(dir, 1, [
      msg("old", "user", "We talked about tea last winter."),
    ]);
    writeActive(dir, [msg("active", "assistant", "Tea came up again today.")]);

    const r = JSON.parse(
      await conversationSearchHandler.execute({ query: "tea" }, ctx),
    );
    expect(r.results.length).toBe(2);
    expect(r.results[0].msg_id).toBe("old");
    expect(r.results[0].source).toBe("segment:0");
    expect(r.results[1].msg_id).toBe("active");
    expect(r.results[1].source).toBe("active");
  });

  it("finds messages by time range without a query", async () => {
    const { dir, ctx } = setup();
    writeActive(dir, [
      msgAt(
        "too_early",
        "user",
        "before the window",
        "2026-05-13T08:30:00+10:00",
      ),
      msgAt(
        "inside",
        "assistant",
        "inside the window",
        "2026-05-13T09:30:00+10:00",
      ),
      msgAt(
        "too_late",
        "user",
        "after the window",
        "2026-05-13T10:30:00+10:00",
      ),
    ]);

    const r = JSON.parse(
      await conversationSearchHandler.execute(
        {
          start_time: "2026-05-13T09:00:00+10:00",
          end_time: "2026-05-13T10:00:00+10:00",
        },
        ctx,
      ),
    );
    expect(r.results.length).toBe(1);
    expect(r.results[0].msg_id).toBe("inside");
    expect(r.query).toBeNull();
    expect(r.time_range.inclusive).toBe(true);
  });

  it("combines query and time range (intersection)", async () => {
    const { dir, ctx } = setup();
    writeActive(dir, [
      msgAt(
        "too_early",
        "user",
        "tea before breakfast",
        "2026-05-13T08:30:00+10:00",
      ),
      msgAt(
        "match",
        "assistant",
        "tea during the window",
        "2026-05-13T09:30:00+10:00",
      ),
      msgAt(
        "wrong_query",
        "user",
        "coffee during the window",
        "2026-05-13T09:45:00+10:00",
      ),
    ]);

    const r = JSON.parse(
      await conversationSearchHandler.execute(
        {
          query: "tea",
          start_time: "2026-05-13T09:00:00+10:00",
          end_time: "2026-05-13T10:00:00+10:00",
        },
        ctx,
      ),
    );
    expect(r.results.length).toBe(1);
    expect(r.results[0].msg_id).toBe("match");
  });

  it("surfaces stored alternatives", async () => {
    const { dir, ctx } = setup();
    const m = msg("active", "assistant", "Tea came up again today.");
    const alts: MessageAlternative[] = [
      {
        content: m.content,
        images: [],
        content_blocks: m.content_blocks,
        timestamp: m.timestamp,
      },
      {
        content: "Coffee came up in a regenerated reply.",
        images: [],
        content_blocks: [
          { type: "text", text: "Coffee came up in a regenerated reply." },
        ],
        timestamp: "2026-01-01T00:01:00Z",
      },
    ];
    m.alternatives = alts;
    m.alt_count = alts.length;
    m.alt_index = 0;
    writeActive(dir, [m]);

    const r = JSON.parse(
      await conversationSearchHandler.execute({ query: "coffee" }, ctx),
    );
    expect(r.results.length).toBe(1);
    expect(r.results[0].source).toBe("active:alt:1");
    expect(r.results[0].alternative_index).toBe(1);
  });
});
