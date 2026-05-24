/**
 * Production `ConversationManager` — archive a compacted slice and retain
 * the recent tail in `active.jsonl`.
 *
 * Port of `backend/daemon/src/memory/compaction_impls.rs::RealConversationManager`.
 *
 * Reads the pre-captured `activeContent` (eliminates the TOCTOU race with
 * `compact()`'s parsing pass), splits at `keepLastN`, writes the head into
 * a new numbered segment file under `segments/`, updates `compaction.json`,
 * and atomically rewrites `active.jsonl` with the retained tail.
 */

import fs from "node:fs";
import path from "node:path";
import { randomUUID } from "node:crypto";

import { atomicWrite } from "../../engine/atomic.ts";
import {
  CompactionError,
  type ConversationManager,
  type RetentionParams,
} from "./types.ts";

const ACTIVE_FILE = "active.jsonl";
const COMPACTION_MANIFEST_FILE = "compaction.json";
const SEGMENTS_DIR = "segments";

interface SegmentEntry {
  file: string;
  message_count: number;
  compacted_at: string;
}

interface CompactionManifest {
  segments: SegmentEntry[];
  total_compacted_messages: number;
}

export class RealConversationManager implements ConversationManager {
  constructor(private readonly characterDir: string) {}

  async archiveAndRetain(
    _conversationId: string,
    params: RetentionParams,
  ): Promise<string> {
    const lines = params.activeContent
      .split("\n")
      .filter((l) => l.trim().length > 0);

    const keep = Math.min(params.keepLastN, lines.length);
    const splitAt = lines.length - keep;
    const archive = lines.slice(0, splitAt);
    const retained = lines.slice(splitAt);

    if (archive.length > 0) {
      let manifest: CompactionManifest = {
        segments: [],
        total_compacted_messages: 0,
      };
      const manifestPath = path.join(this.characterDir, COMPACTION_MANIFEST_FILE);
      if (fs.existsSync(manifestPath)) {
        try {
          const raw = fs.readFileSync(manifestPath, "utf8");
          const parsed = JSON.parse(raw) as CompactionManifest;
          manifest = parsed;
        } catch (e) {
          throw new CompactionError(
            "conversationManager",
            `failed to parse compaction.json: ${(e as Error).message}`,
          );
        }
      }

      const segmentIndex = manifest.segments.length + 1;
      const segmentFile = `${String(segmentIndex).padStart(4, "0")}.jsonl`;
      const segmentsDir = path.join(this.characterDir, SEGMENTS_DIR);
      try {
        fs.mkdirSync(segmentsDir, { recursive: true });
      } catch (e) {
        throw new CompactionError(
          "conversationManager",
          `failed to create segments dir: ${(e as Error).message}`,
        );
      }

      const segmentContent = archive.join("\n") + "\n";
      try {
        fs.writeFileSync(path.join(segmentsDir, segmentFile), segmentContent);
      } catch (e) {
        throw new CompactionError(
          "conversationManager",
          `failed to write segment file: ${(e as Error).message}`,
        );
      }

      manifest.segments.push({
        file: segmentFile,
        message_count: archive.length,
        compacted_at: new Date().toISOString(),
      });
      manifest.total_compacted_messages += archive.length;

      const manifestJson = JSON.stringify(manifest, null, 2);
      try {
        fs.writeFileSync(manifestPath, manifestJson);
      } catch (e) {
        throw new CompactionError(
          "conversationManager",
          `failed to write compaction.json: ${(e as Error).message}`,
        );
      }
    }

    const retainedContent =
      retained.length > 0 ? retained.join("\n") + "\n" : "";
    try {
      atomicWrite(path.join(this.characterDir, ACTIVE_FILE), retainedContent);
    } catch (e) {
      throw new CompactionError(
        "conversationManager",
        `failed to write retained messages: ${(e as Error).message}`,
      );
    }

    return randomUUID();
  }
}
