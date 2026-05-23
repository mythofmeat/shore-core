/**
 * SegmentReader tests — mirror of
 * `backend/daemon/src/engine/segments.rs::tests`.
 */
import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { SegmentReader } from "../src/engine/segments.ts";

function freshDir(): string {
  return mkdtempSync(path.join(tmpdir(), "shore-segs-test-"));
}

describe("SegmentReader", () => {
  it("missing compaction.json yields an empty reader", () => {
    const dir = freshDir();
    const reader = SegmentReader.load(dir);
    expect(reader.segmentCount()).toBe(0);
    expect(reader.totalMessageCount()).toBe(0);
  });

  it("loads metadata from compaction.json", () => {
    const dir = freshDir();
    const manifest = {
      segments: [
        {
          file: "0001.jsonl",
          message_count: 10,
          compacted_at: "2026-03-26T00:00:00Z",
        },
      ],
      total_compacted_messages: 10,
    };
    fs.writeFileSync(
      path.join(dir, "compaction.json"),
      JSON.stringify(manifest, null, 2),
    );

    const reader = SegmentReader.load(dir);
    expect(reader.segmentCount()).toBe(1);
    expect(reader.totalMessageCount()).toBe(10);
  });

  it("readSegment loads + normalizes messages from a segment file", () => {
    const dir = freshDir();
    const segs = path.join(dir, "segments");
    fs.mkdirSync(segs, { recursive: true });
    const msg = {
      msg_id: "m1",
      role: "user",
      content: "old message",
      images: [],
      timestamp: "2026-01-01T00:00:00Z",
    };
    fs.writeFileSync(path.join(segs, "0001.jsonl"), `${JSON.stringify(msg)}\n`);

    fs.writeFileSync(
      path.join(dir, "compaction.json"),
      JSON.stringify({
        segments: [
          {
            file: "0001.jsonl",
            message_count: 1,
            compacted_at: "2026-03-26T00:00:00Z",
          },
        ],
        total_compacted_messages: 1,
      }),
    );

    const reader = SegmentReader.load(dir);
    const msgs = reader.readSegment(0);
    expect(msgs.length).toBe(1);
    expect(msgs[0]!.msg_id).toBe("m1");
    expect(msgs[0]!.content).toBe("old message");
  });

  it("readSegment throws on out-of-bounds index", () => {
    const dir = freshDir();
    const reader = SegmentReader.load(dir);
    expect(() => reader.readSegment(0)).toThrow();
  });
});
