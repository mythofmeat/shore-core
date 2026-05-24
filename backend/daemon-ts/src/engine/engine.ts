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
import { SegmentReader } from "./segments.ts";
import type {
  AltSelection,
  PendingAlt,
} from "./messages.ts";
import type { ImageRef, Message } from "./types.ts";

export interface HistorySnapshot {
  messages: Message[];
  active_start: number;
  config: unknown;
  selected_character: string;
  revision: number;
}

export interface DisplayHistory {
  messages: Message[];
  active_start: number;
}

export interface ListedAlternative {
  index: number;
  position: number;
  active: boolean;
  content: string;
  images: ImageRef[];
  timestamp: string;
}

export interface AlternativeList {
  ref: string;
  alt_index: number | null;
  position: number | null;
  alt_count: number;
  alternatives: ListedAlternative[];
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
  private segmentsReader: SegmentReader;
  private revision = 0;
  private historyRewriteGenerationValue = 0;

  /** Promise chain used to serialize concurrent appends. */
  private writeQueue: Promise<unknown> = Promise.resolve();

  constructor(
    private readonly characterName: string,
    /** `<data_dir>/<character_name>/`. */
    private readonly characterDir: string,
    private readonly broadcast: BroadcastTarget | undefined = undefined,
  ) {
    this.store = MessageStore.load(path.join(characterDir, "active.jsonl"));
    this.segmentsReader = SegmentReader.load(characterDir);
  }

  name(): string {
    return this.characterName;
  }

  /** Returns `<data_dir>/<character_name>/` — the character's data root. */
  dataDir(): string {
    return this.characterDir;
  }

  /**
   * Raw count of messages in `active.jsonl` (pre-merge — counts tool-loop
   * intermediates separately). This is what the autonomy compaction trigger
   * compares against `min_turns` / `max_turns`, matching Rust's
   * `ConversationEngine::message_count`.
   */
  messageCount(): number {
    return this.store.count();
  }

  /** Raw active messages in storage order. */
  messages(): Message[] {
    return this.store.all();
  }

  /** Number of real user turns in the active context window. */
  turnCount(): number {
    return this.store.turnCount();
  }

  /** Access the frozen segment reader for archived conversation history. */
  segments(): SegmentReader {
    return this.segmentsReader;
  }

  /** Archived scrollback followed by the active context tail. */
  displayHistory(): DisplayHistory {
    const archivedRaw: Message[] = [];
    for (let index = 0; index < this.segmentsReader.segmentCount(); index++) {
      try {
        archivedRaw.push(...this.segmentsReader.readSegment(index));
      } catch {
        // Rust logs and keeps rendering the rest of history.
      }
    }

    const archived = mergeToolLoopMessages(archivedRaw);
    const activeStart = archived.length;
    archived.push(...mergeToolLoopMessages(this.store.all()));
    return { messages: archived, active_start: activeStart };
  }

  currentRevision(): number {
    return this.revision;
  }

  historyRewriteGeneration(): number {
    return this.historyRewriteGenerationValue;
  }

  /** Current History snapshot for handshake / broadcast. */
  historySnapshot(config: unknown = {}): HistorySnapshot {
    const merged = mergeToolLoopMessages(this.store.all());
    return {
      messages: merged,
      active_start: 0,
      config,
      selected_character: this.characterName,
      revision: this.revision,
    };
  }

  private advanceRevision(): void {
    this.revision += 1;
  }

  private advanceHistoryRewriteGeneration(): void {
    this.historyRewriteGenerationValue += 1;
  }

  private enqueueMutation<T>(task: () => T | Promise<T>): Promise<T> {
    const next = this.writeQueue.then(task, task);
    this.writeQueue = next.catch(() => undefined);
    return next as Promise<T>;
  }

  /**
   * Append a message to the conversation, persist, advance revision,
   * broadcast. Serialized through the per-engine write queue so two
   * concurrent appends on the same character can't interleave their
   * persists.
   */
  appendMessage(msg: Message): Promise<void> {
    return this.enqueueMutation(() => {
      this.store.append(msg);
      this.advanceRevision();
      this.broadcastHistory();
    });
  }

  /** Insert a message at its chronological position and broadcast. */
  insertMessageByTimestamp(msg: Message): Promise<void> {
    return this.enqueueMutation(() => {
      this.store.insertByTimestamp(msg);
      this.advanceRevision();
      this.broadcastHistory();
    });
  }

  /** Edit a raw active message by id. */
  editMessage(msgId: string, newContent: string): Promise<void> {
    return this.enqueueMutation(() => {
      this.store.edit(msgId, newContent);
      this.advanceHistoryRewriteGeneration();
      this.advanceRevision();
      this.broadcastHistory();
    });
  }

  /** Delete a raw active message by id. */
  deleteMessage(msgId: string): Promise<void> {
    return this.enqueueMutation(() => {
      this.store.delete(msgId);
      this.advanceHistoryRewriteGeneration();
      this.advanceRevision();
      this.broadcastHistory();
    });
  }

  /** Remove every message after the last real user turn. */
  truncateAfterLastUserTurn(): Promise<number> {
    return this.enqueueMutation(() => {
      const removed = this.store.truncateAfterLastUserTurn();
      if (removed > 0) {
        this.advanceHistoryRewriteGeneration();
        this.advanceRevision();
        this.broadcastHistory();
      }
      return removed;
    });
  }

  /** Clone messages through the last real user turn for regeneration. */
  messagesThroughLastUserTurn(): Message[] {
    return this.store.messagesThroughLastUserTurn();
  }

  /** Capture existing alternatives for the response being regenerated. */
  pendingRegenAlt(): PendingAlt | undefined {
    return this.store.pendingRegenAlt();
  }

  /** Replace the current response tail after a successful regeneration. */
  replaceAfterLastUserTurn(newMessages: Message[]): Promise<number> {
    return this.enqueueMutation(() => {
      const removed = this.store.replaceAfterLastUserTurn(newMessages);
      this.advanceHistoryRewriteGeneration();
      this.advanceRevision();
      this.broadcastHistory();
      return removed;
    });
  }

  /** Set alternate-response state on a message. */
  setAlt(msgId: string, index: number, count: number): Promise<void> {
    return this.enqueueMutation(() => {
      this.store.setAlt(msgId, index, count);
      this.advanceRevision();
      this.broadcastHistory();
    });
  }

  /** Add an alternate-response candidate to a message. */
  addAltCandidate(msgId: string): Promise<number> {
    return this.enqueueMutation(() => {
      const count = this.store.addAltCandidate(msgId);
      this.advanceRevision();
      this.broadcastHistory();
      return count;
    });
  }

  /** Select a stored alternate response. */
  selectAlt(msgId: string, index: number): Promise<AltSelection> {
    return this.enqueueMutation(() => {
      const selection = this.store.selectAlt(msgId, index);
      this.advanceHistoryRewriteGeneration();
      this.advanceRevision();
      this.broadcastHistory();
      return selection;
    });
  }

  /**
   * List stored alternate responses for an assistant message.
   * `reference` accepts Rust command refs: omitted/latest, positive or
   * negative 1-based indices, or a literal msg_id.
   */
  listAlternatives(reference?: string): AlternativeList {
    const merged = mergeToolLoopMessages(this.store.all());
    const msgId = this.resolveAssistantRef(merged, reference);
    const msg = merged.find((m) => m.msg_id === msgId);
    if (msg === undefined) throw new Error(`message not found: ${msgId}`);

    const alternatives = msg.alternatives ?? [];
    const altCount = alternatives.length;
    const current = Math.min(msg.alt_index ?? 0, Math.max(0, altCount - 1));
    return {
      ref: msgId,
      alt_index: msg.alt_index ?? null,
      position: msg.alt_index === undefined ? null : msg.alt_index + 1,
      alt_count: altCount,
      alternatives: alternatives.map((alt, index) => ({
        index,
        position: index + 1,
        active: index === current,
        content: alt.content,
        images: alt.images.map((img) => ({ ...img })),
        timestamp: alt.timestamp,
      })),
    };
  }

  /** Clear all active messages and broadcast. */
  reset(): Promise<void> {
    return this.enqueueMutation(() => {
      this.store.clear();
      this.advanceHistoryRewriteGeneration();
      this.advanceRevision();
      this.broadcastHistory();
    });
  }

  /**
   * Re-read `active.jsonl` from disk and broadcast the fresh history.
   * Used after compaction archives part of the active log into a frozen
   * segment — the in-memory MessageStore otherwise still holds the
   * pre-compaction view, and the next generation would re-send the
   * already-archived turns to the model.
   *
   * Mirror of `engine/mod.rs::reload`. We don't refresh a segment cache
   * because TS reads segments on-demand via `SegmentReader.load`; nothing
   * stale needs invalidating there.
   */
  reload(): Promise<void> {
    return this.enqueueMutation(() => {
      this.store.reload();
      this.segmentsReader = SegmentReader.load(this.characterDir);
      this.advanceHistoryRewriteGeneration();
      this.advanceRevision();
      this.broadcastHistory();
    });
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
    return this.enqueueMutation((): Message[] => {
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
      this.advanceHistoryRewriteGeneration();
      this.advanceRevision();
      this.broadcastHistory();
      return dropped;
    });
  }

  /** Broadcast the current active History snapshot with Rust's `{}` config. */
  broadcastHistory(): void {
    this.broadcast?.onBroadcast(this.historySnapshot({}));
  }

  private resolveAssistantRef(messages: Message[], reference: string | undefined): string {
    if (reference === undefined || reference === "last" || reference === "latest") {
      const found = [...messages].reverse().find((m) => m.role === "assistant");
      if (found === undefined) throw new Error("No assistant messages in conversation");
      return found.msg_id;
    }

    const msgId = this.resolveRef(messages, reference);
    const msg = messages.find((m) => m.msg_id === msgId);
    if (msg === undefined) throw new Error(`message not found: ${msgId}`);
    if (msg.role !== "assistant") {
      throw new Error("Alternate response selection only applies to assistant messages");
    }
    return msgId;
  }

  private resolveRef(messages: Message[], reference: string): string {
    if (reference === "last" || reference === "latest") {
      const last = messages.at(-1);
      if (last === undefined) throw new Error("No messages in conversation");
      return last.msg_id;
    }

    const parsed = Number.parseInt(reference, 10);
    if (String(parsed) === reference) {
      if (parsed === 0) {
        throw new Error("Message index must be non-zero (use 1 for first, -1 for last)");
      }
      const idx = parsed < 0 ? messages.length + parsed : parsed - 1;
      if (idx < 0 || idx >= messages.length) {
        throw new Error(
          `Message index ${reference} out of range (conversation has ${messages.length} messages)`,
        );
      }
      return messages[idx]!.msg_id;
    }

    return reference;
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
