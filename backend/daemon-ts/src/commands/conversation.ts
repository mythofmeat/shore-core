import { mergeToolLoopMessages } from "../engine/merge.ts";
import type { ConversationEngine } from "../engine/engine.ts";
import type { Message, Role } from "../engine/types.ts";
import {
  asArgs,
  CommandError,
  mapUnknownError,
  requiredString,
} from "./types.ts";

const DEFAULT_LOG_TURNS = 64;

export function getMessage(engine: ConversationEngine, rawArgs: unknown): unknown {
  const args = asArgs(rawArgs);
  const rawRef = requiredString(args, "ref");
  const role = roleFilter(args);
  let merged = mergeToolLoopMessages(engine.messages());
  if (role !== undefined) merged = merged.filter((msg) => msg.role === role);
  const msgId = resolveRef(merged, rawRef);
  const msg = merged.find((m) => m.msg_id === msgId);
  if (msg === undefined) throw new CommandError("not_found", `Message not found: ${msgId}`);
  return msg;
}

export function log(engine: ConversationEngine, rawArgs: unknown): Record<string, unknown> {
  const args = asArgs(rawArgs);
  const { messages, active_start } = engine.displayHistory();
  const end = messages.length;
  const start = pageStartByArgs(messages, end, args);
  const role = roleFilter(args);
  return historyPagePayload(messages, active_start, start, end, role);
}

export function historyPage(engine: ConversationEngine, rawArgs: unknown): Record<string, unknown> {
  const args = asArgs(rawArgs);
  const { messages, active_start } = engine.displayHistory();
  const end = resolveHistoryBefore(args, active_start, messages.length);
  const start = pageStartByArgs(messages, end, args);
  const role = roleFilter(args);
  return historyPagePayload(messages, active_start, start, end, role);
}

export async function editMessage(
  engine: ConversationEngine,
  rawArgs: unknown,
): Promise<Record<string, unknown>> {
  const args = asArgs(rawArgs);
  const rawRef = requiredString(args, "ref");
  const content = requiredString(args, "content");
  const msgId = resolveRef(mergeToolLoopMessages(engine.messages()), rawRef);
  try {
    await engine.editMessage(msgId, content);
  } catch (e) {
    mapUnknownError(e);
  }
  return { ref: msgId, edited: true };
}

export async function deleteMessages(
  engine: ConversationEngine,
  rawArgs: unknown,
): Promise<Record<string, unknown>> {
  const args = asArgs(rawArgs);
  const refs = parseRefs(args);
  const merged = mergeToolLoopMessages(engine.messages());
  const resolved = refs.map((ref) => resolveRef(merged, ref));
  const deleted: string[] = [];
  for (const msgId of resolved) {
    try {
      await engine.deleteMessage(msgId);
    } catch (e) {
      mapUnknownError(e);
    }
    deleted.push(msgId);
  }
  return { deleted };
}

export function listAlternatives(
  engine: ConversationEngine,
  rawArgs: unknown,
): Record<string, unknown> {
  const args = asArgs(rawArgs);
  const rawRef = typeof args["ref"] === "string" ? args["ref"] : undefined;
  try {
    return engine.listAlternatives(rawRef) as unknown as Record<string, unknown>;
  } catch (e) {
    mapUnknownError(e);
  }
}

export async function selectAlternative(
  engine: ConversationEngine,
  rawArgs: unknown,
): Promise<Record<string, unknown>> {
  const args = asArgs(rawArgs);
  const merged = mergeToolLoopMessages(engine.messages());
  const msgId = resolveAssistantRef(merged, typeof args["ref"] === "string" ? args["ref"] : undefined);
  const msg = merged.find((m) => m.msg_id === msgId);
  if (msg === undefined) throw new CommandError("not_found", `Message not found: ${msgId}`);
  const altCount = msg.alternatives?.length ?? 0;
  if (altCount === 0) {
    throw new CommandError("invalid_request", `message ${msgId} has no alternate responses`);
  }
  const current = Math.min(msg.alt_index ?? 0, Math.max(0, altCount - 1));
  const target = resolveAltTarget(args, current, altCount);
  try {
    const selection = await engine.selectAlt(msgId, target);
    return {
      ref: selection.msg_id,
      alt_index: selection.alt_index,
      position: selection.alt_index + 1,
      alt_count: selection.alt_count,
      content: selection.content,
    };
  } catch (e) {
    mapUnknownError(e);
  }
}

export async function injectSystem(
  engine: ConversationEngine,
  rawArgs: unknown,
  now: () => string,
): Promise<Record<string, unknown>> {
  const args = asArgs(rawArgs);
  const text = requiredString(args, "text");
  await engine.appendMessage({
    msg_id: `m_${crypto.randomUUID()}`,
    role: "system",
    content: text,
    images: [],
    content_blocks: [{ type: "text", text }],
    timestamp: now(),
  });
  return { injected: true };
}

function parseRefs(args: Record<string, unknown>): string[] {
  const refs = args["refs"];
  if (Array.isArray(refs)) {
    return refs.map((value) => {
      if (typeof value !== "string") {
        throw new CommandError("invalid_request", "refs must be an array of strings");
      }
      return value;
    });
  }
  if (typeof refs === "string") return [refs];
  throw new CommandError("invalid_request", "Missing required argument: refs");
}

function historyPagePayload(
  messages: Message[],
  globalActiveStart: number,
  startIn: number,
  endIn: number,
  role: Role | undefined,
): Record<string, unknown> {
  const start = Math.min(startIn, messages.length);
  const end = Math.max(start, Math.min(endIn, messages.length));
  const page = messages.slice(start, end).filter((msg) => role === undefined || msg.role === role);
  const archivedEnd = Math.max(start, Math.min(globalActiveStart, end));
  const activePageStart = messages
    .slice(start, archivedEnd)
    .filter((msg) => role === undefined || msg.role === role)
    .length;
  const totalTurns = countUserTurns(messages);
  return {
    messages: page,
    active_start: activePageStart,
    cursor: start,
    next_before: start,
    has_more_before: start > 0,
    global_active_start: globalActiveStart,
    total_messages: totalTurns,
    total_turns: totalTurns,
  };
}

function resolveHistoryBefore(
  args: Record<string, unknown>,
  activeStart: number,
  total: number,
): number {
  const before = args["before"];
  if (before === undefined) return total;
  if (before === "active") return activeStart;
  if (typeof before === "number" && Number.isInteger(before) && before >= 0) {
    return Math.min(before, total);
  }
  throw new CommandError("invalid_request", 'before must be "active" or a message cursor');
}

function pageStartByArgs(
  messages: Message[],
  end: number,
  args: Record<string, unknown>,
): number {
  if (typeof args["turns"] === "number" && Number.isFinite(args["turns"])) {
    return pageStartByTurns(messages, end, Math.max(0, Math.floor(args["turns"])));
  }
  if (typeof args["count"] === "number" && Number.isFinite(args["count"])) {
    return Math.max(0, end - Math.max(0, Math.floor(args["count"])));
  }
  return pageStartByTurns(messages, end, DEFAULT_LOG_TURNS);
}

function pageStartByTurns(messages: Message[], endIn: number, turns: number): number {
  const end = Math.min(endIn, messages.length);
  if (turns === 0) return end;
  let seen = 0;
  for (let idx = end - 1; idx >= 0; idx--) {
    if (messages[idx]?.role === "user") {
      seen += 1;
      if (seen >= turns) return idx;
    }
  }
  return 0;
}

function roleFilter(args: Record<string, unknown>): Role | undefined {
  const role = args["role"];
  if (role === undefined) return undefined;
  if (role === "user" || role === "assistant" || role === "system") return role;
  throw new CommandError("invalid_request", "role must be one of user, assistant, or system");
}

function countUserTurns(messages: Message[]): number {
  return messages.filter((msg) => msg.role === "user" && !isToolResultOnly(msg)).length;
}

function isToolResultOnly(msg: Message): boolean {
  return (
    msg.content_blocks.length > 0
    && msg.content_blocks.every((block) => block.type === "tool_result")
  );
}

function resolveAssistantRef(messages: Message[], reference: string | undefined): string {
  if (reference === undefined || reference === "last" || reference === "latest") {
    const found = [...messages].reverse().find((msg) => msg.role === "assistant");
    if (found === undefined) {
      throw new CommandError("not_found", "No assistant messages in conversation");
    }
    return found.msg_id;
  }
  const msgId = resolveRef(messages, reference);
  const msg = messages.find((m) => m.msg_id === msgId);
  if (msg === undefined) throw new CommandError("not_found", `Message not found: ${msgId}`);
  if (msg.role !== "assistant") {
    throw new CommandError(
      "invalid_request",
      "Alternate response selection only applies to assistant messages",
    );
  }
  return msgId;
}

function resolveRef(messages: Message[], reference: string): string {
  if (reference === "last" || reference === "latest") {
    const last = messages.at(-1);
    if (last === undefined) throw new CommandError("not_found", "No messages in conversation");
    return last.msg_id;
  }
  if (/^-?\d+$/.test(reference)) {
    const n = Number.parseInt(reference, 10);
    if (n === 0) {
      throw new CommandError(
        "invalid_request",
        "Message index must be non-zero (use 1 for first, -1 for last)",
      );
    }
    const idx = n < 0 ? messages.length + n : n - 1;
    if (idx < 0 || idx >= messages.length) {
      throw new CommandError(
        "not_found",
        `Message index ${reference} out of range (conversation has ${messages.length} messages)`,
      );
    }
    return messages[idx]!.msg_id;
  }
  return reference;
}

function resolveAltTarget(
  args: Record<string, unknown>,
  current: number,
  count: number,
): number {
  if (typeof args["index"] === "number" && Number.isInteger(args["index"])) {
    const index = args["index"];
    if (index < 0 || index >= count) {
      throw new CommandError(
        "invalid_request",
        `alternate index ${index + 1} out of range (message has ${count} alternate response(s))`,
      );
    }
    return index;
  }
  if (typeof args["position"] === "number" && Number.isInteger(args["position"])) {
    const position = args["position"];
    if (position <= 0 || position > count) {
      throw new CommandError(
        "invalid_request",
        `alternate position ${position} out of range (message has ${count} alternate response(s))`,
      );
    }
    return position - 1;
  }
  const direction = typeof args["direction"] === "string" ? args["direction"] : "next";
  switch (direction) {
    case "prev":
    case "previous":
      return Math.max(0, current - 1);
    case "next":
      return Math.min(count - 1, current + 1);
    case "first":
      return 0;
    case "last":
      return count - 1;
    default:
      throw new CommandError("invalid_request", `unknown alt direction: ${direction}`);
  }
}
