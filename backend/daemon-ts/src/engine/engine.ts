/**
 * Per-character conversation engine (Phase 2: read-only).
 *
 * Mirror of `backend/daemon/src/engine/mod.rs::ConversationEngine`. For
 * Phase 2 the engine only needs to load `active.jsonl` and build the
 * `History` snapshot. Append/regen/compaction are later phases.
 */

import path from "node:path";

import { loadActiveMessages } from "./messages.ts";
import { mergeToolLoopMessages } from "./merge.ts";
import type { Message } from "./types.ts";

export interface HistorySnapshot {
  messages: Message[];
  active_start: number;
  selected_character: string;
  revision: number;
}

export class ConversationEngine {
  private readonly messages: Message[];

  constructor(
    private readonly characterName: string,
    /** `<data_dir>/<character_name>/`. */
    private readonly characterDir: string,
  ) {
    this.messages = loadActiveMessages(path.join(characterDir, "active.jsonl"));
  }

  /**
   * The wire-shape History snapshot for handshake / broadcast.
   *
   * Mirrors `ConversationEngine::history_snapshot`. `active_start` is
   * always 0 for snapshots (archived scrollback isn't included); the
   * Rust skip-if-zero rule in `core/protocol/src/server_msg.rs` will
   * drop it from the wire when serialized.
   */
  historySnapshot(): HistorySnapshot {
    const merged = mergeToolLoopMessages(this.messages);
    // TODO(phase-5+): embedMessagesImageData once we have image support.
    return {
      messages: merged,
      active_start: 0,
      selected_character: this.characterName,
      revision: 0,
    };
  }
}

/** Construct an engine for a character whose data lives under `<dataDir>/<name>/`. */
export function engineForCharacter(dataDir: string, characterName: string): ConversationEngine {
  return new ConversationEngine(characterName, path.join(dataDir, characterName));
}
