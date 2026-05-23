/**
 * Read-only access to frozen conversation segments.
 *
 * Port of `backend/daemon/src/engine/segments.rs::SegmentReader`.
 *
 * Segments are created by compaction: older messages move out of
 * `active.jsonl` into numbered JSONL files under
 * `<characterDataDir>/segments/`. Each segment is immutable; the
 * manifest at `<characterDataDir>/compaction.json` lists them in
 * compaction order.
 */

import fs from "node:fs";
import path from "node:path";

import { normalizeMessage } from "./messages.ts";
import type { Message } from "./types.ts";

const COMPACTION_MANIFEST_FILE = "compaction.json";
const SEGMENTS_DIR = "segments";

export interface SegmentEntry {
  file: string;
  message_count: number;
  compacted_at: string;
}

export interface CompactionManifest {
  segments: SegmentEntry[];
  total_compacted_messages: number;
}

/** Read-only view over a character's compacted-segment manifest + files. */
export class SegmentReader {
  private constructor(
    private readonly segmentsDir: string,
    private readonly manifest_: CompactionManifest,
  ) {}

  /**
   * Load segment metadata for a character. Absent `compaction.json` →
   * an empty reader (no segments yet). Throws on malformed JSON.
   */
  static load(characterDir: string): SegmentReader {
    const manifestPath = path.join(characterDir, COMPACTION_MANIFEST_FILE);
    const segmentsDir = path.join(characterDir, SEGMENTS_DIR);

    let manifest: CompactionManifest = {
      segments: [],
      total_compacted_messages: 0,
    };
    if (fs.existsSync(manifestPath)) {
      const raw = fs.readFileSync(manifestPath, "utf8");
      const parsed = JSON.parse(raw) as Record<string, unknown>;
      const segs = Array.isArray(parsed["segments"]) ? parsed["segments"] : [];
      const total =
        typeof parsed["total_compacted_messages"] === "number"
          ? (parsed["total_compacted_messages"] as number)
          : 0;
      manifest = {
        segments: segs.map((s) => normalizeEntry(s)),
        total_compacted_messages: total,
      };
    }
    return new SegmentReader(segmentsDir, manifest);
  }

  manifest(): CompactionManifest {
    return this.manifest_;
  }

  segmentCount(): number {
    return this.manifest_.segments.length;
  }

  totalMessageCount(): number {
    return this.manifest_.total_compacted_messages;
  }

  /** Iterate `(index, entry)` pairs in compaction order. */
  entries(): Array<{ index: number; entry: SegmentEntry }> {
    return this.manifest_.segments.map((entry, index) => ({ index, entry }));
  }

  /** Load messages from a specific segment by index. */
  readSegment(index: number): Message[] {
    const entry = this.manifest_.segments[index];
    if (entry === undefined) {
      throw new Error(`segment index ${index} out of bounds`);
    }
    const full = path.join(this.segmentsDir, entry.file);
    const content = fs.readFileSync(full, "utf8");
    const out: Message[] = [];
    for (const line of content.split("\n")) {
      const trimmed = line.trim();
      if (trimmed.length === 0) continue;
      const raw = JSON.parse(trimmed) as Record<string, unknown>;
      out.push(normalizeMessage(raw));
    }
    return out;
  }
}

function normalizeEntry(raw: unknown): SegmentEntry {
  const obj = (raw ?? {}) as Record<string, unknown>;
  return {
    file: typeof obj["file"] === "string" ? (obj["file"] as string) : "",
    message_count:
      typeof obj["message_count"] === "number"
        ? (obj["message_count"] as number)
        : 0,
    compacted_at:
      typeof obj["compacted_at"] === "string"
        ? (obj["compacted_at"] as string)
        : "",
  };
}
