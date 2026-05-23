/**
 * `active.jsonl` reader + per-message normalize.
 *
 * Mirror of `backend/daemon/src/engine/messages.rs::MessageStore::load` and
 * `core/protocol/src/types.rs::Message::normalize`. Read-only for Phase 2:
 * we parse the file, normalize each entry, and hand back a flat array. The
 * write path (append, atomic rewrite, compaction) lives in later phases.
 */

import fs from "node:fs";

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

// ── narrowing helpers ─────────────────────────────────────────────────

function asString(v: unknown): string | undefined {
  return typeof v === "string" ? v : undefined;
}

function asRole(v: unknown): Message["role"] {
  if (v === "user" || v === "assistant" || v === "system") return v;
  throw new Error(`invalid role: ${JSON.stringify(v)}`);
}
