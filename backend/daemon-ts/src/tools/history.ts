/**
 * `conversation_search` — search compacted segments + the active
 * conversation window for the current character.
 *
 * Ported from `backend/daemon/src/tools/history.rs`. Renamed from
 * `search_history` per the 4c.2 rename decision (too easily conflated
 * with `file_search` aka workspace `search`).
 *
 * Reads the same on-disk format the Rust daemon writes:
 *   - `<characterDataDir>/compaction.json` — segment manifest (optional)
 *   - `<characterDataDir>/segments/<file>.jsonl` — frozen segments
 *   - `<characterDataDir>/active.jsonl` — current rolling window
 *
 * The compaction subsystem doesn't ship in the TS daemon until Phase 6,
 * but the read path is harmless to land now: if no manifest exists we
 * just search the active window.
 */

import path from "node:path";

import { loadActiveMessages } from "../engine/messages.ts";
import { SegmentReader } from "../engine/segments.ts";
import type { Message, MessageAlternative } from "../engine/types.ts";

import type { ToolContext, ToolHandler } from "./registry.ts";
import { ToolError } from "./registry.ts";

const DEFAULT_MAX_RESULTS = 20;
const MAX_RESULTS = 100;
const EXCERPT_CHARS = 360;

export const CONVERSATION_SEARCH_DESCRIPTION =
  "Search your own conversation history, including compacted older segments and the active conversation window. Use this when the user asks what you discussed before, when you need to verify a past exchange, when you need messages from a specific time range, or when the answer may be in the transcript rather than in curated memory files. Provide `query` for keyword search, `start_time` and/or `end_time` for an inclusive RFC3339 time range, or combine them to narrow keyword matches to a window. Returns matching messages with role, timestamp, source, and a short excerpt. Excerpts are clipped — when the answer is non-trivial, follow up with another `conversation_search` for adjacent terms/time windows or `file_search`/`read` against memory to get the full picture.";

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

interface TimeRange {
  start?: Date;
  end?: Date;
}

interface SearchFilters {
  query?: string;
  queryLower?: string;
  range: TimeRange;
}

function isRangeEmpty(r: TimeRange): boolean {
  return r.start === undefined && r.end === undefined;
}

function rangeContains(r: TimeRange, ts: Date): boolean {
  if (r.start !== undefined && ts < r.start) return false;
  if (r.end !== undefined && ts > r.end) return false;
  return true;
}

function parseOptionalString(
  obj: Record<string, unknown>,
  field: string,
): string | undefined {
  if (!(field in obj)) return undefined;
  const v = obj[field];
  if (typeof v !== "string") {
    throw new ToolError("InvalidArgs", `${field} must be a string`);
  }
  const trimmed = v.trim();
  if (trimmed.length === 0) return undefined;
  return trimmed;
}

function parseTimeBound(
  obj: Record<string, unknown>,
  field: string,
): Date | undefined {
  const raw = parseOptionalString(obj, field);
  if (raw === undefined) return undefined;
  const d = new Date(raw);
  if (Number.isNaN(d.getTime())) {
    throw new ToolError(
      "InvalidArgs",
      `${field} must be an RFC3339 timestamp: ${raw}`,
    );
  }
  return d;
}

function maxResultsFrom(obj: Record<string, unknown>): number {
  const raw = obj["max_results"];
  const n =
    typeof raw === "number" && Number.isFinite(raw)
      ? Math.floor(raw)
      : DEFAULT_MAX_RESULTS;
  return Math.min(MAX_RESULTS, Math.max(1, n));
}

// ---------------------------------------------------------------------------
// Excerpting
// ---------------------------------------------------------------------------

function excerptFor(content: string, query: string | undefined): string {
  const chars = [...content];
  if (query === undefined) {
    const slice = chars.slice(0, EXCERPT_CHARS).join("");
    return chars.length > EXCERPT_CHARS ? slice + "..." : slice;
  }
  const lower = content.toLowerCase();
  const q = query.toLowerCase();
  const idx = lower.indexOf(q);
  if (idx < 0) {
    return chars.slice(0, EXCERPT_CHARS).join("");
  }
  // Position the excerpt 80 chars before the match.
  const prefixChars = [...content.slice(0, idx)].length;
  const startChar = Math.max(0, prefixChars - 80);
  const slice = chars.slice(startChar, startChar + EXCERPT_CHARS).join("");
  let out = slice;
  if (startChar > 0) out = `...${out}`;
  if (chars.length > startChar + EXCERPT_CHARS) out += "...";
  return out;
}

// ---------------------------------------------------------------------------
// Match pushing
// ---------------------------------------------------------------------------

interface SearchStats {
  skipped_invalid_timestamps: number;
}

function matchesQuery(content: string, queryLower: string | undefined): boolean {
  if (queryLower === undefined) return true;
  return content.toLowerCase().includes(queryLower);
}

function matchesTimeRange(
  timestamp: string,
  range: TimeRange,
  stats: SearchStats,
): boolean {
  if (isRangeEmpty(range)) return true;
  const d = new Date(timestamp);
  if (Number.isNaN(d.getTime())) {
    stats.skipped_invalid_timestamps += 1;
    return false;
  }
  return rangeContains(range, d);
}

function pushMatches(
  results: Array<Record<string, unknown>>,
  messages: Message[],
  source: string,
  filters: SearchFilters,
  maxResults: number,
  stats: SearchStats,
): void {
  for (const m of messages) {
    if (results.length >= maxResults) return;
    if (
      matchesQuery(m.content, filters.queryLower) &&
      matchesTimeRange(m.timestamp, filters.range, stats)
    ) {
      results.push({
        msg_id: m.msg_id,
        role: m.role,
        timestamp: m.timestamp,
        source,
        excerpt: excerptFor(m.content, filters.query),
      });
    }
    const alts: MessageAlternative[] = m.alternatives ?? [];
    for (let i = 0; i < alts.length; i++) {
      if (results.length >= maxResults) return;
      const alt = alts[i]!;
      const ts = alt.timestamp.length > 0 ? alt.timestamp : m.timestamp;
      if (alt.content === m.content) continue;
      if (!matchesQuery(alt.content, filters.queryLower)) continue;
      if (!matchesTimeRange(ts, filters.range, stats)) continue;
      results.push({
        msg_id: m.msg_id,
        role: m.role,
        timestamp: ts,
        source: `${source}:alt:${i}`,
        alternative_index: i,
        alternative_count: alts.length,
        excerpt: excerptFor(alt.content, filters.query),
      });
    }
  }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

export const conversationSearchHandler: ToolHandler = {
  name: "conversation_search",
  description: CONVERSATION_SEARCH_DESCRIPTION,
  inputSchema: {
    type: "object",
    properties: {
      query: {
        type: "string",
        description:
          "Optional keyword or phrase to search for (case-insensitive). Omit this to return messages by time range only.",
      },
      start_time: {
        type: "string",
        description:
          "Optional inclusive lower timestamp bound in RFC3339 format, for example 2026-05-13T09:00:00+10:00.",
      },
      end_time: {
        type: "string",
        description:
          "Optional inclusive upper timestamp bound in RFC3339 format, for example 2026-05-13T17:00:00+10:00.",
      },
      max_results: {
        type: "number",
        description:
          "Maximum matching messages to return. Defaults to 20, maximum 100.",
      },
    },
    required: [],
  },
  async execute(input: unknown, ctx: ToolContext): Promise<string> {
    const characterDataDir = ctx.characterDataDir;
    if (characterDataDir.length === 0) {
      throw new ToolError(
        "InvalidArgs",
        "conversation history is not configured",
      );
    }
    const obj = (input ?? {}) as Record<string, unknown>;
    const query = parseOptionalString(obj, "query");
    const range: TimeRange = {};
    const start = parseTimeBound(obj, "start_time");
    if (start !== undefined) range.start = start;
    const end = parseTimeBound(obj, "end_time");
    if (end !== undefined) range.end = end;
    if (
      range.start !== undefined &&
      range.end !== undefined &&
      range.start > range.end
    ) {
      throw new ToolError(
        "InvalidArgs",
        "start_time must be before or equal to end_time",
      );
    }
    if (query === undefined && isRangeEmpty(range)) {
      throw new ToolError(
        "InvalidArgs",
        "provide query, start_time, end_time, or a combination",
      );
    }

    const maxResults = maxResultsFrom(obj);
    const filters: SearchFilters = {
      ...(query !== undefined ? { query, queryLower: query.toLowerCase() } : {}),
      range,
    };

    const stats: SearchStats = { skipped_invalid_timestamps: 0 };
    const results: Array<Record<string, unknown>> = [];
    let searched = 0;

    let segments: SegmentReader;
    try {
      segments = SegmentReader.load(characterDataDir);
    } catch (e) {
      throw new ToolError("Io", (e as Error).message);
    }
    for (let i = 0; i < segments.segmentCount(); i++) {
      if (results.length >= maxResults) break;
      let msgs: Message[];
      try {
        msgs = segments.readSegment(i);
      } catch (e) {
        throw new ToolError("Io", (e as Error).message);
      }
      searched += msgs.length;
      pushMatches(results, msgs, `segment:${i}`, filters, maxResults, stats);
    }

    if (results.length < maxResults) {
      const activePath = path.join(characterDataDir, "active.jsonl");
      const active = loadActiveMessages(activePath);
      searched += active.length;
      pushMatches(results, active, "active", filters, maxResults, stats);
    }

    return JSON.stringify({
      query: query ?? null,
      time_range: {
        start_time: range.start ? range.start.toISOString() : null,
        end_time: range.end ? range.end.toISOString() : null,
        inclusive: true,
      },
      results,
      count: results.length,
      searched_messages: searched,
      skipped_invalid_timestamps: stats.skipped_invalid_timestamps,
    });
  },
};
