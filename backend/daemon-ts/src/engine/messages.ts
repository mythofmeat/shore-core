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
import type { ContentBlock, Message, MessageAlternative } from "./types.ts";

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

  /** Append a message and persist. */
  append(msg: Message): void {
    this.messages.push(msg);
    this.persist();
  }

  /** Keep only messages with index < `count` and persist. */
  truncate(count: number): void {
    if (count >= this.messages.length) return;
    this.messages = this.messages.slice(0, count);
    this.persist();
  }

  /**
   * Re-read the active.jsonl file from disk, replacing the in-memory
   * messages. Used after compaction archives part of the active log into
   * a frozen segment — the store needs to forget the pre-compaction tail.
   */
  reload(): void {
    this.messages = loadActiveMessages(this.activeJsonlPath);
  }

  private persist(): void {
    const buf = this.messages.map(serializeForStorage).join("\n") + (this.messages.length > 0 ? "\n" : "");
    atomicWrite(this.activeJsonlPath, buf);
  }
}

// ── narrowing helpers ─────────────────────────────────────────────────

function asString(v: unknown): string | undefined {
  return typeof v === "string" ? v : undefined;
}

function asRole(v: unknown): Message["role"] {
  if (v === "user" || v === "assistant" || v === "system") return v;
  throw new Error(`invalid role: ${JSON.stringify(v)}`);
}
