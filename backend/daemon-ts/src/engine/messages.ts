/**
 * `active.jsonl` reader + per-message normalize + MessageStore write path.
 *
 * Mirror of `backend/daemon/src/engine/messages.rs::MessageStore` and
 * `core/protocol/src/types.rs::Message::normalize`.
 *
 * The on-disk format strips the derived `content` field and any inline
 * image `data` (see Rust's `serialize_for_storage`); wire snapshots keep
 * them. Persist is a FULL REWRITE via atomic tmp+rename — not append-only,
 * because rewrites for regen/edit/delete need the same atomic guarantee.
 */

import fs from "node:fs";

import { atomicWrite } from "./atomic.ts";
import { mergeToolLoopMessages } from "./merge.ts";
import type { ContentBlock, Message, MessageAlternative } from "./types.ts";

export interface PendingAlt {
  alternatives: MessageAlternative[];
}

export interface AltSelection {
  msg_id: string;
  alt_index: number;
  alt_count: number;
  content: string;
}

export interface AttachedAlt {
  alt_index: number;
  alt_count: number;
}

/** Read `active.jsonl`, return normalized messages. Missing file → []. */
export function loadActiveMessages(activeJsonlPath: string): Message[] {
  let content: string;
  try {
    content = fs.readFileSync(activeJsonlPath, "utf8");
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return [];
    throw e;
  }

  const out: Message[] = [];
  for (const line of content.split("\n")) {
    if (line.trim() === "") continue;
    const raw = JSON.parse(line) as Record<string, unknown>;
    out.push(normalizeMessage(raw));
  }
  return out;
}

/**
 * Bring a freshly-deserialized message to canonical form:
 *  - If `content_blocks` is empty and `content` is non-empty (legacy disk
 *    layout), wrap `content` into a single Text block.
 *  - If `content_blocks` is non-empty, derive `content` from blocks
 *    (this is the storage format — disk drops the redundant `content`).
 *  - Default `images` and `alternatives` to empty arrays.
 *  - Normalize each alternative recursively.
 *  - If any alternatives exist, force `alt_count` to alternatives.length
 *    and clamp `alt_index` into range (defaulting to last entry).
 */
export function normalizeMessage(raw: Record<string, unknown>): Message {
  const msg_id = asString(raw["msg_id"]) ?? "";
  const role = asRole(raw["role"]);
  let content = asString(raw["content"]) ?? "";
  const images = Array.isArray(raw["images"]) ? (raw["images"] as Message["images"]) : [];
  let blocks: ContentBlock[] = Array.isArray(raw["content_blocks"])
    ? (raw["content_blocks"] as ContentBlock[])
    : [];

  if (blocks.length === 0 && content !== "") {
    blocks = [{ type: "text", text: content }];
  } else if (blocks.length > 0) {
    content = deriveContentFromBlocks(blocks, /* includeToolResults */ true);
  }

  const alternatives = Array.isArray(raw["alternatives"])
    ? (raw["alternatives"] as Record<string, unknown>[]).map(normalizeAlternative)
    : [];

  const msg: Message = {
    msg_id,
    role,
    content,
    images,
    content_blocks: blocks,
    timestamp: asString(raw["timestamp"]) ?? "",
  };

  if (alternatives.length > 0) {
    msg.alternatives = alternatives;
    const count = alternatives.length;
    const rawIndex = typeof raw["alt_index"] === "number" ? raw["alt_index"] : count - 1;
    msg.alt_index = Math.max(0, Math.min(rawIndex, count - 1));
    msg.alt_count = count;
  }

  return msg;
}

function normalizeAlternative(raw: Record<string, unknown>): MessageAlternative {
  let content = asString(raw["content"]) ?? "";
  const images = Array.isArray(raw["images"]) ? (raw["images"] as MessageAlternative["images"]) : [];
  let blocks: ContentBlock[] = Array.isArray(raw["content_blocks"])
    ? (raw["content_blocks"] as ContentBlock[])
    : [];

  if (blocks.length === 0 && content !== "") {
    blocks = [{ type: "text", text: content }];
  } else if (blocks.length > 0) {
    content = deriveContentFromBlocks(blocks, true);
  }

  return {
    content,
    images,
    content_blocks: blocks,
    timestamp: asString(raw["timestamp"]) ?? "",
  };
}

/**
 * Mirror of `derive_content_from_blocks_with`. Joins trimmed `Text` block
 * contents with newlines, optionally including `ToolResult` contents.
 * Other block types (Thinking, RedactedThinking, ToolUse) contribute
 * nothing.
 */
export function deriveContentFromBlocks(blocks: ContentBlock[], includeToolResults: boolean): string {
  const parts: string[] = [];
  for (const block of blocks) {
    if (block.type === "text") {
      const trimmed = block.text.trim();
      if (trimmed !== "") parts.push(trimmed);
    } else if (includeToolResults && block.type === "tool_result") {
      const trimmed = block.content.trim();
      if (trimmed !== "") parts.push(trimmed);
    }
  }
  return parts.join("\n");
}

// ── storage / write path ──────────────────────────────────────────────

/**
 * Serialize a Message for on-disk storage.
 *
 * Mirror of `Message::serialize_for_storage`: drop the redundant `content`
 * field (it's derived from `content_blocks` on load) and strip inline
 * image `data` (disk uses paths, the wire embeds bytes).
 */
export function serializeForStorage(msg: Message): string {
  const stored: Record<string, unknown> = {
    msg_id: msg.msg_id,
    role: msg.role,
    timestamp: msg.timestamp,
    images: msg.images.map((img) => {
      const out: Record<string, unknown> = { path: img.path };
      if (img.caption !== undefined) out["caption"] = img.caption;
      return out;
    }),
    content_blocks: msg.content_blocks,
  };
  if (msg.alt_index !== undefined) stored["alt_index"] = msg.alt_index;
  if (msg.alt_count !== undefined) stored["alt_count"] = msg.alt_count;
  if (msg.alternatives && msg.alternatives.length > 0) {
    stored["alternatives"] = msg.alternatives.map(serializeAlternativeForStorage);
  }
  return JSON.stringify(stored);
}

function serializeAlternativeForStorage(alt: MessageAlternative): Record<string, unknown> {
  return {
    content: alt.content,
    images: alt.images.map((img) => {
      const out: Record<string, unknown> = { path: img.path };
      if (img.caption !== undefined) out["caption"] = img.caption;
      return out;
    }),
    content_blocks: alt.content_blocks,
    timestamp: alt.timestamp,
  };
}

/**
 * In-memory message store backed by `active.jsonl`. Persists via full
 * rewrite (atomic tmp+rename) on every mutation — matches the Rust impl,
 * which uses the same atomic write for regen/edit/delete and keeps the
 * code path uniform.
 */
export class MessageStore {
  constructor(
    private readonly activeJsonlPath: string,
    private messages: Message[] = [],
  ) {}

  static load(activeJsonlPath: string): MessageStore {
    return new MessageStore(activeJsonlPath, loadActiveMessages(activeJsonlPath));
  }

  all(): Message[] {
    return this.messages;
  }

  count(): number {
    return this.messages.length;
  }

  turnCount(): number {
    return this.messages.filter(isRealUserTurn).length;
  }

  /** Clear all messages and truncate the backing file. */
  clear(): void {
    this.messages = [];
    this.persist();
  }

  /** Append a message and persist. */
  append(msg: Message): void {
    this.messages.push(msg);
    this.persist();
  }

  /**
   * Insert a message at its chronological position by timestamp.
   * Unparseable new timestamps append; unparseable existing timestamps
   * compare as <= so malformed legacy lines are never silently reordered
   * after newer data.
   */
  insertByTimestamp(msg: Message): void {
    const newTs = Date.parse(msg.timestamp);
    let insertPos = this.messages.length;
    if (Number.isFinite(newTs)) {
      insertPos = 0;
      for (let i = this.messages.length - 1; i >= 0; i--) {
        const existingTs = Date.parse(this.messages[i]!.timestamp);
        if (!Number.isFinite(existingTs) || existingTs <= newTs) {
          insertPos = i + 1;
          break;
        }
      }
    }
    this.messages.splice(insertPos, 0, msg);
    this.persist();
  }

  /** Edit the content of a message by id. */
  edit(msgId: string, newContent: string): void {
    const msg = this.messages.find((m) => m.msg_id === msgId);
    if (msg === undefined) throw messageNotFound(msgId);
    msg.content = newContent;
    msg.content_blocks = [{ type: "text", text: newContent }];
    this.persist();
  }

  /** Delete a raw active message by id. */
  delete(msgId: string): void {
    const idx = this.messages.findIndex((m) => m.msg_id === msgId);
    if (idx < 0) throw messageNotFound(msgId);
    this.messages.splice(idx, 1);
    this.persist();
  }

  /** Keep only messages with index < `count` and persist. */
  truncate(count: number): void {
    if (count >= this.messages.length) return;
    this.messages = this.messages.slice(0, count);
    this.persist();
  }

  /** Clone conversation messages through the last real user turn. */
  messagesThroughLastUserTurn(): Message[] {
    const keep = this.lastRealUserTurnKeepIndex();
    return this.messages.slice(0, keep).map(cloneMessage);
  }

  /** Remove every message after the last real user turn. */
  truncateAfterLastUserTurn(): number {
    const keep = this.lastRealUserTurnKeepIndex();
    const removed = this.messages.length - keep;
    if (removed > 0) {
      this.messages = this.messages.slice(0, keep);
      this.persist();
    }
    return removed;
  }

  /** Replace every message after the last real user turn. */
  replaceAfterLastUserTurn(newMessages: Message[]): number {
    const keep = this.lastRealUserTurnKeepIndex();
    const removed = this.messages.length - keep;
    this.messages = this.messages.slice(0, keep);
    this.messages.push(...newMessages);
    this.persist();
    return removed;
  }

  /** Set alternate-response metadata on a message. */
  setAlt(msgId: string, index: number, count: number): void {
    const msg = this.messages.find((m) => m.msg_id === msgId);
    if (msg === undefined) throw messageNotFound(msgId);
    msg.alt_index = index;
    msg.alt_count = count;
    this.persist();
  }

  /** Increment alt_count and point at the newest candidate. */
  addAltCandidate(msgId: string): number {
    const msg = this.messages.find((m) => m.msg_id === msgId);
    if (msg === undefined) throw messageNotFound(msgId);
    const newCount = (msg.alt_count ?? 1) + 1;
    msg.alt_count = newCount;
    msg.alt_index = newCount - 1;
    this.persist();
    return newCount;
  }

  /** Capture alternatives for the assistant response about to be regenerated. */
  pendingRegenAlt(): PendingAlt | undefined {
    const keep = this.lastRealUserTurnKeepIndex();
    const tail = this.messages.slice(keep);
    const active = [...mergeToolLoopMessages(tail)]
      .reverse()
      .find((m) => m.role === "assistant");
    if (active === undefined) return undefined;

    const alternatives = (active.alternatives ?? []).map(cloneAlternative);
    const current = alternativeFromMessage(active);
    if (alternatives.length === 0) {
      alternatives.push(current);
    } else {
      const fallback = Math.max(0, alternatives.length - 1);
      const idx = Math.min(active.alt_index ?? fallback, fallback);
      alternatives[idx] = current;
    }
    return { alternatives };
  }

  /**
   * Attach prior alternatives plus the active generated response to the
   * final assistant message in `messages`.
   */
  static attachGeneratedAlt(
    messages: Message[],
    prior: MessageAlternative[],
  ): AttachedAlt | undefined {
    const merged = mergeToolLoopMessages(messages);
    const active = [...merged].reverse().find((m) => m.role === "assistant");
    if (active === undefined) return undefined;

    const alternatives = prior.map(cloneAlternative);
    const altIndex = alternatives.length;
    alternatives.push(alternativeFromMessage(active));
    const altCount = alternatives.length;

    const target = [...messages]
      .reverse()
      .find((m) => m.role === "assistant" && m.msg_id === active.msg_id);
    if (target === undefined) return undefined;

    target.alt_index = altIndex;
    target.alt_count = altCount;
    target.alternatives = alternatives;
    return { alt_index: altIndex, alt_count: altCount };
  }

  /** Select a stored alternate response on a message. */
  selectAlt(msgId: string, index: number): AltSelection {
    const merged = mergeToolLoopMessages(this.messages);
    const target = merged.find((m) => m.msg_id === msgId);
    if (target === undefined) throw messageNotFound(msgId);

    const alternatives = target.alternatives ?? [];
    const altCount = alternatives.length;
    if (altCount === 0) {
      throw new Error(`message ${msgId} has no alternate responses`);
    }
    if (index >= altCount) {
      throw new Error(
        `alternate index ${index + 1} out of range (message has ${altCount} alternate response(s))`,
      );
    }

    const currentIndex = target.alt_index ?? 0;
    if (currentIndex === index) {
      return {
        msg_id: target.msg_id,
        alt_index: index,
        alt_count: altCount,
        content: target.content,
      };
    }

    const selected = messageFromAlternative(target, index);
    const keep = this.lastRealUserTurnKeepIndex();
    const tailMerged = mergeToolLoopMessages(this.messages.slice(keep));
    const currentTail = [...tailMerged]
      .reverse()
      .find((m) => m.role === "assistant");

    if (currentTail?.msg_id === msgId) {
      this.messages = this.messages.slice(0, keep);
      this.messages.push(selected);
    } else {
      const rawIdx = this.messages.findIndex((m) => m.msg_id === msgId);
      if (rawIdx < 0) throw messageNotFound(msgId);
      this.messages[rawIdx] = selected;
    }

    this.persist();
    return {
      msg_id: selected.msg_id,
      alt_index: index,
      alt_count: altCount,
      content: selected.content,
    };
  }

  /**
   * Re-read the active.jsonl file from disk, replacing the in-memory
   * messages. Used after compaction archives part of the active log into
   * a frozen segment — the store needs to forget the pre-compaction tail.
   */
  reload(): void {
    this.messages = loadActiveMessages(this.activeJsonlPath);
  }

  private lastRealUserTurnKeepIndex(): number {
    for (let i = this.messages.length - 1; i >= 0; i--) {
      if (isRealUserTurn(this.messages[i]!)) return i + 1;
    }
    return 0;
  }

  private persist(): void {
    const buf = this.messages.map(serializeForStorage).join("\n") + (this.messages.length > 0 ? "\n" : "");
    atomicWrite(this.activeJsonlPath, buf);
  }
}

function isRealUserTurn(msg: Message): boolean {
  return msg.role === "user" && !isToolResultOnly(msg);
}

function isToolResultOnly(msg: Message): boolean {
  return (
    msg.role === "user"
    && msg.content_blocks.length > 0
    && msg.content_blocks.every((b) => b.type === "tool_result")
  );
}

function alternativeFromMessage(msg: Message): MessageAlternative {
  let contentBlocks: ContentBlock[] = msg.content_blocks
    .filter((block): block is Extract<ContentBlock, { type: "text" }> =>
      block.type === "text" && block.text.trim() !== ""
    )
    .map((block) => ({ type: "text", text: block.text }));
  let content = deriveContentFromBlocks(contentBlocks, false);
  if (content === "" && msg.content.trim() !== "") {
    content = msg.content;
    contentBlocks = [{ type: "text", text: msg.content }];
  }
  return {
    content,
    images: msg.images.map((img) => ({ ...img })),
    content_blocks: contentBlocks,
    timestamp: msg.timestamp,
  };
}

function messageFromAlternative(template: Message, index: number): Message {
  const alternatives = (template.alternatives ?? []).map(cloneAlternative);
  const alt = alternatives[index]!;
  const msg: Message = {
    msg_id: template.msg_id,
    role: "assistant",
    content: alt.content,
    images: alt.images.map((img) => ({ ...img })),
    content_blocks: alt.content_blocks.map((block) => ({ ...block }) as ContentBlock),
    alt_index: index,
    alt_count: alternatives.length,
    alternatives,
    timestamp: alt.timestamp === "" ? template.timestamp : alt.timestamp,
  };
  normalizeMessageObject(msg);
  return msg;
}

function normalizeMessageObject(msg: Message): void {
  if (msg.content_blocks.length === 0 && msg.content !== "") {
    msg.content_blocks = [{ type: "text", text: msg.content }];
  } else if (msg.content_blocks.length > 0) {
    msg.content = deriveContentFromBlocks(msg.content_blocks, true);
  }
}

function cloneMessage(msg: Message): Message {
  return {
    ...msg,
    images: msg.images.map((img) => ({ ...img })),
    content_blocks: msg.content_blocks.map((block) => ({ ...block }) as ContentBlock),
    ...(msg.alternatives !== undefined
      ? { alternatives: msg.alternatives.map(cloneAlternative) }
      : {}),
  };
}

function cloneAlternative(alt: MessageAlternative): MessageAlternative {
  return {
    content: alt.content,
    images: alt.images.map((img) => ({ ...img })),
    content_blocks: alt.content_blocks.map((block) => ({ ...block }) as ContentBlock),
    timestamp: alt.timestamp,
  };
}

function messageNotFound(msgId: string): Error {
  return new Error(`message not found: ${msgId}`);
}

// ── narrowing helpers ─────────────────────────────────────────────────

function asString(v: unknown): string | undefined {
  return typeof v === "string" ? v : undefined;
}

function asRole(v: unknown): Message["role"] {
  if (v === "user" || v === "assistant" || v === "system") return v;
  throw new Error(`invalid role: ${JSON.stringify(v)}`);
}
