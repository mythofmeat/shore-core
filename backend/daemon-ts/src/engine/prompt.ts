/**
 * Prompt assembly — port of `backend/daemon/src/engine/prompt.rs`.
 *
 * Deterministic by design: same inputs produce byte-identical output so the
 * Anthropic prompt cache hash stays stable across turns. Time markers use
 * the message timestamp, not wall-clock now, for the same reason.
 *
 * Mirrors the Rust impl 1:1 — see `tests/prompt.test.ts` and the parity
 * fixtures under `tests/fixtures/prompt/` for the spec.
 */
import type { ContentBlock, ImageRef, Message, Role } from "./types.ts";

const DEFAULT_MAX_CONTEXT_TOKENS = 200_000;
const DEFAULT_MAX_OUTPUT_TOKENS = 4096;
const CHARS_PER_TOKEN = 4;

/** 30 minutes — gap above which a relative-time phrase is injected. */
const TIME_GAP_THRESHOLD_SECS = 1800;
/** 1 hour — minimum elapsed time before the next periodic anchor. */
const HOURLY_MARKER_INTERVAL_SECS = 3600;

/**
 * Built-in system template. Keep in sync with
 * `backend/daemon/prompts/engine/builtin_system.md` — Rust drops the trailing
 * newline at compile time for cache-key stability, so we drop it here too.
 */
const BUILTIN_SYSTEM_TEMPLATE =
  "You are {{char}}, in conversation with {{user}}.\n" +
  "This is a text conversation. Communicate directly rather than narrating actions or using roleplay formatting.\n" +
  "Be consistent with established details and avoid fabricating memory.";

export interface SystemBlock {
  label: string;
  content: string;
}

export interface PromptMessage {
  role: Role;
  content: string;
  images: ImageRef[];
  content_blocks: ContentBlock[];
}

export interface AssembledPrompt {
  system: SystemBlock[];
  messages: PromptMessage[];
}

export interface PromptParams {
  character_name: string;
  display_name: string;
  system_prompt?: string;
  tools_guidance?: string;
  character_definition?: string;
  user_definition?: string;
  memory_index?: string;
  is_private: boolean;
  has_prior_context: boolean;
  messages: Message[];
  max_context_tokens?: number;
  max_output_tokens?: number;
}

export function assemblePrompt(params: PromptParams): AssembledPrompt {
  const maxContext = params.max_context_tokens ?? DEFAULT_MAX_CONTEXT_TOKENS;
  const maxOutput = params.max_output_tokens ?? DEFAULT_MAX_OUTPUT_TOKENS;

  const vars: Record<string, string> = {
    char: params.character_name,
    character_name: params.character_name,
    user: params.display_name,
    date: "",
    time: "",
  };

  const template = params.system_prompt ?? BUILTIN_SYSTEM_TEMPLATE;
  const renderedSystem = renderTemplate(template, vars);

  const system: SystemBlock[] = [];
  system.push({ label: "system", content: renderedSystem });

  if (nonEmpty(params.tools_guidance)) {
    system.push({ label: "tools_guidance", content: params.tools_guidance });
  }

  if (nonEmpty(params.character_definition)) {
    const tag = xmlTagFromName(params.character_name, "character");
    system.push({
      label: "character",
      content: `<${tag}>\n${params.character_definition}\n</${tag}>`,
    });
  }

  if (nonEmpty(params.user_definition)) {
    const tag = xmlTagFromName(params.display_name, "user");
    system.push({
      label: "user",
      content: `<${tag}>\n${params.user_definition}\n</${tag}>`,
    });
  }

  if (!params.is_private && nonEmpty(params.memory_index)) {
    system.push({
      label: "memory_index",
      content:
        "<memory_index>\n" +
        "The following is your prompt-visible memory index from workspace/MEMORY.md. " +
        "It is a map of memory files, recently updated files, and still-relevant conversational throughlines; " +
        "it does not replace SOUL.md, USER.md, AGENTS.md, TOOLS.md, or HEARTBEAT.md.\n\n" +
        `${params.memory_index}\n` +
        "</memory_index>",
    });
  }

  const systemJoined = system.map((b) => b.content).join("\n");
  const systemTokens = estimateTokens(systemJoined);
  const availableForMessages = Math.max(
    0,
    maxContext - maxOutput - systemTokens,
  );

  const messages = trimMessages(
    params.messages,
    availableForMessages,
    params.has_prior_context,
  );

  return { system, messages };
}

function nonEmpty(s: string | undefined): s is string {
  return s !== undefined && s.length > 0;
}

/**
 * Mustache subset:
 *   - `{{key}}` → vars[key], or left unchanged if key absent
 *   - `{{#if key}}…{{/if}}` → block kept iff vars[key] is set and non-empty
 *
 * Single-pass: the loop pairs the first `{{#if` with the first following
 * `{{/if}}`, so nested conditionals are NOT supported. Matches Rust
 * prompt.rs:263-302 exactly.
 */
export function renderTemplate(
  template: string,
  vars: Record<string, string>,
): string {
  let result = template;

  while (true) {
    const ifStart = result.indexOf("{{#if ");
    if (ifStart === -1) break;

    const afterOpen = ifStart + 6;
    const nameEnd = result.indexOf("}}", afterOpen);
    if (nameEnd === -1) break;

    const name = result.slice(afterOpen, nameEnd).trim();
    const openTagEnd = nameEnd + 2;

    const closeTag = "{{/if}}";
    const closePos = result.indexOf(closeTag, openTagEnd);
    if (closePos === -1) break;

    const blockContent = result.slice(openTagEnd, closePos);
    const after = result.slice(closePos + closeTag.length);

    const value = vars[name];
    if (value !== undefined && value.length > 0) {
      const varTag = `{{${name}}}`;
      const expanded = blockContent.split(varTag).join(value);
      result = result.slice(0, ifStart) + expanded + after;
    } else {
      result = result.slice(0, ifStart) + after;
    }
  }

  for (const [key, value] of Object.entries(vars)) {
    const tag = `{{${key}}}`;
    result = result.split(tag).join(value);
  }

  return result;
}

/**
 * Convert a free-form name to a safe XML tag: lowercase, non-alphanumeric →
 * `_`, collapse `__` runs, trim edges. Falls back if the result is empty.
 *
 * Matches Rust `xml_tag_from_name` (`prompt.rs:313`) — restricted to ASCII
 * alphanumerics so e.g. accented characters become `_`.
 */
export function xmlTagFromName(name: string, fallback: string): string {
  let tag = "";
  for (const ch of name.toLowerCase()) {
    if ((ch >= "a" && ch <= "z") || (ch >= "0" && ch <= "9")) {
      tag += ch;
    } else {
      tag += "_";
    }
  }

  while (tag.includes("__")) {
    tag = tag.replaceAll("__", "_");
  }
  tag = trimChar(tag, "_");

  return tag.length === 0 ? fallback : tag;
}

function trimChar(s: string, ch: string): string {
  let start = 0;
  let end = s.length;
  while (start < end && s[start] === ch) start++;
  while (end > start && s[end - 1] === ch) end--;
  return s.slice(start, end);
}

const TEXT_ENCODER = new TextEncoder();

/** ~4 bytes per token, byte-length not char-length (matches Rust). */
export function estimateTokens(text: string): number {
  const bytes = TEXT_ENCODER.encode(text).length;
  return Math.ceil(bytes / CHARS_PER_TOKEN);
}

/**
 * Sum token estimates across content blocks; fall back to `content` string
 * when no blocks are present. `redacted_thinking` counts as 0 (opaque to us).
 */
export function estimateMessageTokens(msg: Message): number {
  if (msg.content_blocks.length === 0) {
    return estimateTokens(msg.content);
  }
  let total = 0;
  for (const b of msg.content_blocks) {
    switch (b.type) {
      case "text":
        total += estimateTokens(b.text);
        break;
      case "thinking":
        total += estimateTokens(b.thinking);
        break;
      case "tool_use":
        total += estimateTokens(b.name) + estimateTokens(JSON.stringify(b.input));
        break;
      case "redacted_thinking":
        total += 0;
        break;
      case "tool_result":
        total += estimateTokens(b.content);
        break;
    }
  }
  return total;
}

/**
 * Trim messages newest-first to fit `tokenBudget`, drop leading tool-loop
 * orphans, then inject time markers on user messages.
 *
 * Always returns at least one message (even if it busts the budget) so the
 * model has something to respond to. Markers are deterministic — same input
 * timestamps always produce the same markers.
 */
export function trimMessages(
  messages: Message[],
  tokenBudget: number,
  hasPriorContext: boolean,
): PromptMessage[] {
  type Selected = { pm: PromptMessage; ts: string };
  const selected: Selected[] = [];
  let used = 0;

  for (let i = messages.length - 1; i >= 0; i--) {
    const msg = messages[i]!;
    const msgTokens = estimateMessageTokens(msg);
    if (used + msgTokens > tokenBudget && selected.length > 0) {
      break;
    }
    used += msgTokens;
    selected.push({
      pm: {
        role: msg.role,
        content: msg.content,
        images: msg.images.slice(),
        // Deep-clone each block; the time-marker pass below mutates
        // the first text block in place, and a shallow `.slice()` would
        // leak that mutation back into the engine's persisted Message.
        content_blocks: msg.content_blocks.map((b) => ({ ...b })),
      },
      ts: msg.timestamp,
    });
  }

  selected.reverse();

  while (selected.length > 0 && isToolLoopMsgPrompt(selected[0]!.pm)) {
    selected.shift();
  }

  const lostContext = hasPriorContext || selected.length < messages.length;

  let prevTs: Date | null = null;
  let lastMarkerTs: Date | null = null;
  let firstUserPending = true;
  const result: PromptMessage[] = [];

  for (const { pm, ts } of selected) {
    const currentTs = parseRfc3339(ts);

    if (pm.role === "user" && currentTs !== null) {
      const gapSecs =
        prevTs !== null ? (currentTs.getTime() - prevTs.getTime()) / 1000 : null;
      const elapsedSinceMarker =
        lastMarkerTs !== null
          ? (currentTs.getTime() - lastMarkerTs.getTime()) / 1000
          : null;

      const bigGap = gapSecs !== null && gapSecs >= TIME_GAP_THRESHOLD_SECS;
      const hourlyTick =
        elapsedSinceMarker !== null &&
        elapsedSinceMarker >= HOURLY_MARKER_INTERVAL_SECS;
      const needsAnchor = firstUserPending && lostContext;

      if (bigGap || hourlyTick || needsAnchor) {
        const marker = formatTimeMarker(gapSecs, currentTs);
        pm.content = `${marker}\n\n${pm.content}`;
        const first = pm.content_blocks[0];
        if (first && first.type === "text") {
          first.text = `${marker}\n\n${first.text}`;
        }
        lastMarkerTs = currentTs;
      }
      firstUserPending = false;
    }

    if (currentTs !== null) {
      prevTs = currentTs;
    }
    result.push(pm);
  }

  return result;
}

function isToolLoopMsgPrompt(msg: PromptMessage): boolean {
  if (msg.content_blocks.length === 0) return false;
  if (msg.role === "user") {
    return msg.content_blocks.every((b) => b.type === "tool_result");
  }
  if (msg.role === "assistant") {
    const hasText = msg.content_blocks.some(
      (b) => b.type === "text" && b.text.length > 0,
    );
    const hasToolUse = msg.content_blocks.some((b) => b.type === "tool_use");
    return !hasText && hasToolUse;
  }
  return false;
}

/**
 * Phrase for the relative-time prefix in a marker. Matches Rust
 * `relative_gap_phrase` (`prompt.rs:362-377`) — note rounding via
 * `Math.round`, not floor/ceil.
 */
export function relativeGapPhrase(gapSecs: number): string {
  if (gapSecs < 5400) return "about an hour later";
  if (gapSecs < 64800) {
    const hours = Math.round(gapSecs / 3600);
    return `${hours} hours later`;
  }
  if (gapSecs < 129600) return "about a day later";
  const days = Math.round(gapSecs / 86400);
  return `${days} days later`;
}

const WEEKDAY_NAMES = [
  "Sunday",
  "Monday",
  "Tuesday",
  "Wednesday",
  "Thursday",
  "Friday",
  "Saturday",
];

/**
 * Marker shape: `[6 hours later · Saturday 2026-04-04 · 9:14 PM]` when the
 * gap crosses threshold, `[Saturday 2026-04-04 · 9:14 PM]` otherwise.
 *
 * Time rendered in local timezone, matching Rust's `Local` conversion.
 */
export function formatTimeMarker(
  gapSecs: number | null,
  currentTs: Date,
): string {
  const weekday = WEEKDAY_NAMES[currentTs.getDay()]!;
  const yyyy = currentTs.getFullYear();
  const mm = String(currentTs.getMonth() + 1).padStart(2, "0");
  const dd = String(currentTs.getDate()).padStart(2, "0");

  const hours24 = currentTs.getHours();
  const ampm = hours24 < 12 ? "AM" : "PM";
  let hours12 = hours24 % 12;
  if (hours12 === 0) hours12 = 12;
  const minutes = String(currentTs.getMinutes()).padStart(2, "0");

  const timeStr = `${weekday} ${yyyy}-${mm}-${dd} · ${hours12}:${minutes} ${ampm}`;

  if (gapSecs !== null && gapSecs >= TIME_GAP_THRESHOLD_SECS) {
    return `[${relativeGapPhrase(gapSecs)} · ${timeStr}]`;
  }
  return `[${timeStr}]`;
}

/**
 * Parse an RFC3339 timestamp into a Date. Returns null on failure (matches
 * Rust `DateTime::parse_from_rfc3339(...).ok()` — non-parsing timestamps are
 * skipped, not erroring).
 */
function parseRfc3339(ts: string): Date | null {
  const d = new Date(ts);
  return Number.isNaN(d.getTime()) ? null : d;
}
