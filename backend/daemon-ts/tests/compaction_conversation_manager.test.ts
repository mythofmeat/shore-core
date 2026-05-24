/**
 * RealConversationManager tests — mirror of
 * `backend/daemon/src/memory/compaction_impls.rs::tests` (the
 * `archive_and_retain` block).
 */
import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { RealConversationManager } from "../src/memory/compaction/conversation_manager.ts";

function freshDir(): string {
  return mkdtempSync(path.join(tmpdir(), "shore-conv-mgr-test-"));
}

const MSG1 =
  '{"msg_id":"m1","role":"User","content":"hello","images":[],"timestamp":"t1"}';
const MSG2 =
  '{"msg_id":"m2","role":"Assistant","content":"hi","images":[],"timestamp":"t2"}';
const MSG3 =
  '{"msg_id":"m3","role":"User","content":"bye","images":[],"timestamp":"t3"}';

describe("RealConversationManager.archiveAndRetain", () => {
  it("splits messages into segment + retained tail", async () => {
    const dir = freshDir();
    const content = `${MSG1}\n${MSG2}\n${MSG3}\n`;
    fs.writeFileSync(path.join(dir, "active.jsonl"), content);

    const mgr = new RealConversationManager(dir);
    const newId = await mgr.archiveAndRetain("test-conv", {
      keepLastN: 1,
      activeContent: content,
    });
    expect(newId).not.toBe("");

    const seg = fs.readFileSync(path.join(dir, "segments", "0001.jsonl"), "utf8");
    expect(seg).toContain("m1");
    expect(seg).toContain("m2");
    expect(seg).not.toContain("m3");

    const active = fs.readFileSync(path.join(dir, "active.jsonl"), "utf8");
    expect(active).not.toContain("m1");
    expect(active).not.toContain("m2");
    expect(active).toContain("m3");

    const manifest = JSON.parse(
      fs.readFileSync(path.join(dir, "compaction.json"), "utf8"),
    ) as {
      segments: Array<{ file: string; message_count: number }>;
      total_compacted_messages: number;
    };
    expect(manifest.segments.length).toBe(1);
    expect(manifest.segments[0]!.message_count).toBe(2);
    expect(manifest.total_compacted_messages).toBe(2);
  });

  it("keep_last_n > available retains everything, no segment", async () => {
    const dir = freshDir();
    const content = `${MSG1}\n${MSG2}\n`;
    fs.writeFileSync(path.join(dir, "active.jsonl"), content);

    const mgr = new RealConversationManager(dir);
    await mgr.archiveAndRetain("conv", {
      keepLastN: 5,
      activeContent: content,
    });

    const active = fs.readFileSync(path.join(dir, "active.jsonl"), "utf8");
    expect(active).toContain("m1");
    expect(active).toContain("m2");
    expect(fs.existsSync(path.join(dir, "segments"))).toBe(false);
  });

  it("increments the segment number when a manifest exists", async () => {
    const dir = freshDir();
    fs.writeFileSync(
      path.join(dir, "compaction.json"),
      JSON.stringify(
        {
          segments: [
            {
              file: "0001.jsonl",
              message_count: 5,
              compacted_at: "2026-01-01T00:00:00Z",
            },
          ],
          total_compacted_messages: 5,
        },
        null,
        2,
      ),
    );
    fs.mkdirSync(path.join(dir, "segments"), { recursive: true });

    const content = `${MSG3}\n`;
    fs.writeFileSync(path.join(dir, "active.jsonl"), content);

    const mgr = new RealConversationManager(dir);
    await mgr.archiveAndRetain("conv", {
      keepLastN: 0,
      activeContent: content,
    });

    expect(fs.existsSync(path.join(dir, "segments", "0002.jsonl"))).toBe(true);
    const manifest = JSON.parse(
      fs.readFileSync(path.join(dir, "compaction.json"), "utf8"),
    ) as {
      segments: Array<{ file: string }>;
      total_compacted_messages: number;
    };
    expect(manifest.segments.length).toBe(2);
    expect(manifest.segments[1]!.file).toBe("0002.jsonl");
    expect(manifest.total_compacted_messages).toBe(6);
  });

  it("includes malformed JSONL lines verbatim (mirrors Rust line-count semantics)", async () => {
    const dir = freshDir();
    const garbage = "corrupted{{{not valid json at all";
    const content = `${MSG1}\n${garbage}\n${MSG2}\n`;
    fs.writeFileSync(path.join(dir, "active.jsonl"), content);

    const mgr = new RealConversationManager(dir);
    await mgr.archiveAndRetain("conv", {
      keepLastN: 1,
      activeContent: content,
    });

    const seg = fs.readFileSync(path.join(dir, "segments", "0001.jsonl"), "utf8");
    expect(seg).toContain("m1");
    expect(seg).toContain("corrupted{{{");
    expect(seg).not.toContain("m2");

    const active = fs.readFileSync(path.join(dir, "active.jsonl"), "utf8");
    expect(active).toContain("m2");
    expect(active).not.toContain("m1");
  });

  it("empty active content is a no-op (no segment, empty retained file)", async () => {
    const dir = freshDir();
    const mgr = new RealConversationManager(dir);
    await mgr.archiveAndRetain("conv", {
      keepLastN: 1,
      activeContent: "",
    });
    expect(fs.existsSync(path.join(dir, "segments"))).toBe(false);
  });
});
