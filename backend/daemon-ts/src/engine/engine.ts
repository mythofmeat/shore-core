/**
 * Per-character conversation engine.
 *
 * Mirror of `backend/daemon/src/engine/mod.rs::ConversationEngine`. Phase
 * 3 adds the write path: appendMessage mutates in-memory state, persists
 * via MessageStore, advances revision, fires a broadcast callback.
 *
 * Per-character locking (REWRITE.md "Things to specifically preserve":
 * single-flight compaction locks keyed on character data root, from
 * PR #30): each engine serializes its own appends via a promise chain.
 * Two clients sending messages for the same character get their writes
 * ordered; cross-character writes stay concurrent.
 */

import path from "node:path";

import { MessageStore, loadActiveMessages } from "./messages.ts";
import { mergeToolLoopMessages } from "./merge.ts";
import type { Message } from "./types.ts";

export interface HistorySnapshot {
  messages: Message[];
  active_start: number;
  selected_character: string;
  revision: number;
}

export interface BroadcastTarget {
  /**
   * Called after every state mutation with the current snapshot. The
   * caller is responsible for delivering to whatever clients exist.
   *
   * Matches `engine::broadcast_history` semantics: the broadcast goes
   * out as a full History frame whose `config` field is `{}` (the
   * handshake-time fields are absent — see history_config_snapshot in
   * Rust's handshake.rs).
   */
  onBroadcast: (snapshot: HistorySnapshot) => void;
}

export class ConversationEngine {
  private readonly store: MessageStore;
  private revision = 0;

  /** Promise chain used to serialize concurrent appends. */
  private writeQueue: Promise<unknown> = Promise.resolve();

  constructor(
    private readonly characterName: string,
    /** `<data_dir>/<character_name>/`. */
    private readonly characterDir: string,
    private readonly broadcast: BroadcastTarget | undefined = undefined,
  ) {
    this.store = MessageStore.load(path.join(characterDir, "active.jsonl"));
  }

  name(): string {
    return this.characterName;
  }

  /** Current History snapshot for handshake / broadcast. */
  historySnapshot(): HistorySnapshot {
    const merged = mergeToolLoopMessages(this.store.all());
    return {
      messages: merged,
      active_start: 0,
      selected_character: this.characterName,
      revision: this.revision,
    };
  }

  /**
   * Append a message to the conversation, persist, advance revision,
   * broadcast. Serialized through the per-engine write queue so two
   * concurrent appends on the same character can't interleave their
   * persists.
   */
  appendMessage(msg: Message): Promise<void> {
    const task = async () => {
      this.store.append(msg);
      this.revision++;
      this.broadcast?.onBroadcast(this.historySnapshot());
    };
    const next = this.writeQueue.then(task, task);
    this.writeQueue = next.catch(() => undefined);
    return next;
  }

  /**
   * Drop the trailing assistant turn (and any preceding tool-loop
   * intermediates that belong to it) so a regen can start clean from
   * the last user message. Returns the dropped messages so the caller
   * can decide whether to surface them.
   *
   * Walks backward from the end: pops tool_result user turns and
   * tool_use-only assistant turns, then pops the final assistant turn
   * with text content. Stops if the history doesn't end on an
   * assistant turn (regen is a no-op in that case).
   */
  rewindLastAssistantTurn(): Promise<Message[]> {
    const task = async (): Promise<Message[]> => {
      const msgs = this.store.all();
      if (msgs.length === 0) return [];
      const last = msgs[msgs.length - 1]!;
      if (last.role !== "assistant") return [];

      let cut = msgs.length - 1;
      // Walk back past tool-loop intermediates that lead up to this asst.
      while (cut > 0) {
        const prev = msgs[cut - 1]!;
        const isToolUseOnlyAsst =
          prev.role === "assistant" &&
          prev.content_blocks.some((b) => b.type === "tool_use") &&
          !prev.content_blocks.some(
            (b) => b.type === "text" && b.text.length > 0,
          );
        const isToolResultUser =
          prev.role === "user" &&
          prev.content_blocks.length > 0 &&
          prev.content_blocks.every((b) => b.type === "tool_result");
        if (isToolUseOnlyAsst || isToolResultUser) {
          cut--;
        } else {
          break;
        }
      }
      const dropped = msgs.slice(cut);
      this.store.truncate(cut);
      this.revision++;
      this.broadcast?.onBroadcast(this.historySnapshot());
      return dropped;
    };
    const next = this.writeQueue.then(task, task);
    this.writeQueue = next.catch(() => undefined);
    return next as Promise<Message[]>;
  }
}

/**
 * Process-wide engine cache. The Rust daemon keeps engines in
 * `CharacterRegistry::engines` keyed by name; this is the TS analog.
 *
 * Engines are created lazily on first use and held for the daemon's
 * lifetime (reloaded only if a character is removed/re-added). For the
 * Phase-3 handshake + append flow that's all we need; Phase 6 will add
 * compaction-lock semantics on top.
 */
export class EngineRegistry {
  private readonly engines = new Map<string, ConversationEngine>();

  constructor(
    private readonly dataDir: string,
    private readonly broadcast: BroadcastTarget | undefined = undefined,
  ) {}

  get(characterName: string): ConversationEngine {
    let engine = this.engines.get(characterName);
    if (engine === undefined) {
      engine = new ConversationEngine(
        characterName,
        path.join(this.dataDir, characterName),
        this.broadcast,
      );
      this.engines.set(characterName, engine);
    }
    return engine;
  }

  /**
   * Return only engines that have been instantiated. Useful when we need
   * to iterate live state without forcing a cold load.
   */
  loaded(): ConversationEngine[] {
    return Array.from(this.engines.values());
  }
}

/** Construct a one-off engine without registering it (used by Phase-2 handshake snapshot). */
export function engineForCharacter(dataDir: string, characterName: string): ConversationEngine {
  return new ConversationEngine(characterName, path.join(dataDir, characterName));
}

// Re-export so callers can verify the on-disk load helper independently.
export { loadActiveMessages };
