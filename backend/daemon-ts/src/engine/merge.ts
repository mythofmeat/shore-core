/**
 * Tool-loop message merging for client consumption.
 *
 * Mirror of `core/protocol/src/merge.rs::merge_tool_loop_messages`.
 *
 * Storage keeps separate messages (assistant tool_use, user tool_result,
 * assistant text) for LLM API compatibility. This module collapses them
 * into single assistant messages for client display:
 *
 *   [user, asst(tool_use), user(tool_result), asst(text)]
 *     → [user, asst(thinking + tool_use + tool_result + text)]
 *
 * Messages without tool loops pass through unchanged. Tool-result-only
 * user messages are consumed by the merge and do not appear in output.
 */

import { deriveContentFromBlocks } from "./messages.ts";
import type { ContentBlock, Message } from "./types.ts";

export function mergeToolLoopMessages(messages: Message[]): Message[] {
  const out: Message[] = [];
  let i = 0;

  while (i < messages.length) {
    const msg = messages[i]!;

    if (msg.role !== "assistant") {
      if (!isToolResultOnly(msg)) out.push(msg);
      i++;
      continue;
    }

    if (!isToolLoopAssistant(msg)) {
      out.push(msg);
      i++;
      continue;
    }

    // ── Tool loop detected ────────────────────────────────────────────
    const mergedBlocks: ContentBlock[] = [];
    let lastAssistant = msg;

    while (true) {
      const current = messages[i]!;
      const nextIsResult = i + 1 < messages.length && isToolResultOnly(messages[i + 1]!);
      const results = nextIsResult ? messages[i + 1] : undefined;

      collectRound(current, results, mergedBlocks);
      lastAssistant = current;

      i += nextIsResult ? 2 : 1;
      if (i >= messages.length) break;

      const next = messages[i]!;
      if (next.role === "assistant" && isToolLoopAssistant(next)) continue;

      if (next.role === "assistant") {
        mergedBlocks.push(...next.content_blocks);
        lastAssistant = next;
        i++;
        break;
      }

      break;
    }

    const content = deriveContentFromBlocks(mergedBlocks, /* includeToolResults */ false);
    const merged: Message = {
      msg_id: lastAssistant.msg_id,
      role: "assistant",
      content,
      images: lastAssistant.images,
      content_blocks: mergedBlocks,
      timestamp: lastAssistant.timestamp,
    };
    if (lastAssistant.alt_index !== undefined) merged.alt_index = lastAssistant.alt_index;
    if (lastAssistant.alt_count !== undefined) merged.alt_count = lastAssistant.alt_count;
    if (lastAssistant.alternatives && lastAssistant.alternatives.length > 0) {
      merged.alternatives = lastAssistant.alternatives;
    }
    out.push(merged);
  }

  return out;
}

function isToolLoopAssistant(msg: Message): boolean {
  return msg.role === "assistant" && msg.content_blocks.some((b) => b.type === "tool_use");
}

function isToolResultOnly(msg: Message): boolean {
  return (
    msg.role === "user" &&
    msg.content_blocks.length > 0 &&
    msg.content_blocks.every((b) => b.type === "tool_result")
  );
}

/**
 * Collect one tool-loop round's blocks (assistant + matching results).
 *
 * Each ToolUse is followed by its matching ToolResult (matched by `id` ==
 * `tool_use_id`). Whitespace-only Text blocks are skipped (LLM noise).
 */
function collectRound(assistant: Message, results: Message | undefined, out: ContentBlock[]): void {
  for (const block of assistant.content_blocks) {
    if (block.type === "tool_use") {
      out.push(block);
      if (results) {
        const match = results.content_blocks.find(
          (b) => b.type === "tool_result" && b.tool_use_id === block.id,
        );
        if (match) out.push(match);
      }
      continue;
    }
    if (block.type === "text" && block.text.trim() === "") continue;
    out.push(block);
  }
}
