/**
 * Basic tools — `check_time`, `roll_dice`, `set_next_wake`.
 *
 * Ported from `backend/daemon/src/tools/basic.rs`.
 *
 * `roll_dice` uses the `notation` schema (`"2d6+3"`) verbatim from Rust,
 * NOT the `{count, sides}` shape the Phase 4a stub used. The model has
 * been talking to the Rust daemon with this schema; preserve it.
 */

import type { ToolContext, ToolHandler } from "./registry.ts";
import { ToolError } from "./registry.ts";

// ---------------------------------------------------------------------------
// Descriptions (mirrors backend/daemon/prompts/tools/basic/*.md, sans trailing newline)
// ---------------------------------------------------------------------------

export const CHECK_TIME_DESCRIPTION =
  "Return the current date and time in {{user}}'s local timezone. Use when the exact time matters — working out how long it's been since an event, whether it's late, day-of-week reasoning, or anchoring a timestamped memory. Returns a human-readable string like 'Friday, April 4th, 2026 at 4:34 PM'. Takes no parameters.";

export const ROLL_DICE_DESCRIPTION =
  "Roll dice using standard dice notation, returning the individual rolls and their total. Use for anything that genuinely benefits from a random outcome — decisions, games, random prompts, divination-as-play — not as a substitute for simply making up a number when one isn't needed. Accepts `NdS[+/-M]` where N is the number of dice, S is the number of sides, and M is an optional modifier. Examples: `2d6`, `1d20+5`, `4d6-1`, or `d8` for a single die.";

export const SET_NEXT_WAKE_DESCRIPTION =
  "Schedule when your next heartbeat tick (a private moment while {{user}} is away) should occur. Call this at the end of a heartbeat tick to express your own sense of pacing — soon if you're mid-thought and want to continue, later if you're settled and content, whenever feels right. This tool is only meaningful during heartbeat ticks; calling it from a regular chat turn is rejected. Accepts hours from now (clamped between 1 and 48) and a short reason-note to your future self about why you chose this timing.";

// ---------------------------------------------------------------------------
// Friendly datetime formatting
// ---------------------------------------------------------------------------

function ordinalSuffix(n: number): string {
  const mod10 = n % 10;
  const mod100 = n % 100;
  if (mod100 === 11 || mod100 === 12 || mod100 === 13) return "th";
  if (mod10 === 1) return "st";
  if (mod10 === 2) return "nd";
  if (mod10 === 3) return "rd";
  return "th";
}

const DAY_NAMES = [
  "Sunday",
  "Monday",
  "Tuesday",
  "Wednesday",
  "Thursday",
  "Friday",
  "Saturday",
] as const;
const MONTH_NAMES = [
  "January",
  "February",
  "March",
  "April",
  "May",
  "June",
  "July",
  "August",
  "September",
  "October",
  "November",
  "December",
] as const;

/** Human-friendly local datetime — `"Friday, April 4th, 2026 at 4:34 PM"`. */
export function formatFriendlyDatetime(now: Date = new Date()): string {
  const day = now.getDate();
  const suffix = ordinalSuffix(day);
  let hour = now.getHours();
  const minute = String(now.getMinutes()).padStart(2, "0");
  const ampm = hour >= 12 ? "PM" : "AM";
  hour = hour % 12;
  if (hour === 0) hour = 12;
  const dayName = DAY_NAMES[now.getDay()];
  const monthName = MONTH_NAMES[now.getMonth()];
  return `${dayName}, ${monthName} ${day}${suffix}, ${now.getFullYear()} at ${hour}:${minute} ${ampm}`;
}

// ---------------------------------------------------------------------------
// Dice notation parsing
// ---------------------------------------------------------------------------

export interface DiceNotation {
  count: number;
  sides: number;
  modifier: number;
}

/** Parse `NdS[+/-M]` like `2d6`, `1d20+5`, `4d6-1`, `d8`. */
export function parseDiceNotation(notation: string): DiceNotation {
  const s = notation.trim().toLowerCase();
  const dPos = s.indexOf("d");
  if (dPos < 0) throw new Error(`Missing 'd' in notation: ${notation}`);

  const countStr = s.slice(0, dPos);
  const count = countStr === "" ? 1 : Number.parseInt(countStr, 10);
  if (!Number.isInteger(count) || Number.isNaN(count)) {
    throw new Error(`Invalid dice count: ${countStr}`);
  }
  if (count === 0) throw new Error("Dice count must be at least 1");

  const afterD = s.slice(dPos + 1);
  if (afterD.length === 0) throw new Error("Missing sides after 'd'");

  // Find the first +/- that isn't at position 0 (mirrors Rust's
  // `i > 0 && (c == '+' || c == '-')`).
  let modPos = -1;
  for (let i = 1; i < afterD.length; i++) {
    const ch = afterD[i];
    if (ch === "+" || ch === "-") {
      modPos = i;
      break;
    }
  }
  let sidesStr: string;
  let modifier = 0;
  if (modPos >= 0) {
    sidesStr = afterD.slice(0, modPos);
    const modStr = afterD.slice(modPos);
    const parsed = Number.parseInt(modStr, 10);
    if (!Number.isInteger(parsed) || Number.isNaN(parsed)) {
      throw new Error(`Invalid modifier: ${modStr}`);
    }
    modifier = parsed;
  } else {
    sidesStr = afterD;
  }

  const sides = Number.parseInt(sidesStr, 10);
  if (!Number.isInteger(sides) || Number.isNaN(sides)) {
    throw new Error(`Invalid sides: ${sidesStr}`);
  }
  if (sides === 0) throw new Error("Dice sides must be at least 1");

  return { count, sides, modifier };
}

export function executeDiceRoll(n: DiceNotation): {
  rolls: number[];
  total: number;
} {
  const rolls: number[] = [];
  for (let i = 0; i < n.count; i++) {
    rolls.push(1 + Math.floor(Math.random() * n.sides));
  }
  const total = rolls.reduce((a, b) => a + b, 0) + n.modifier;
  return { rolls, total };
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

export const checkTimeHandler: ToolHandler = {
  name: "check_time",
  description: CHECK_TIME_DESCRIPTION,
  inputSchema: {
    type: "object",
    properties: {},
    required: [],
  },
  async execute(): Promise<string> {
    return JSON.stringify(formatFriendlyDatetime());
  },
};

export const rollDiceHandler: ToolHandler = {
  name: "roll_dice",
  description: ROLL_DICE_DESCRIPTION,
  inputSchema: {
    type: "object",
    properties: {
      notation: {
        type: "string",
        description:
          "Dice notation: NdS[+/-M]. Examples: '2d6', '1d20+5', '4d6-1'.",
      },
    },
    required: ["notation"],
  },
  async execute(input: unknown): Promise<string> {
    const obj = (input ?? {}) as { notation?: unknown };
    if (typeof obj.notation !== "string") {
      throw new ToolError("InvalidArgs", "missing 'notation' parameter");
    }
    let parsed: DiceNotation;
    try {
      parsed = parseDiceNotation(obj.notation);
    } catch (e) {
      throw new ToolError(
        "InvalidArgs",
        `invalid dice notation: ${(e as Error).message}`,
      );
    }
    const { rolls, total } = executeDiceRoll(parsed);
    return JSON.stringify({
      notation: obj.notation,
      rolls,
      total,
    });
  },
};

export const setNextWakeHandler: ToolHandler = {
  name: "set_next_wake",
  description: SET_NEXT_WAKE_DESCRIPTION,
  inputSchema: {
    type: "object",
    properties: {
      hours_from_now: {
        type: "number",
        description:
          "Hours until your next private moment (1.0 to 48.0; clamped if outside range).",
      },
      reason: {
        type: "string",
        description:
          "A brief note to your future self about why you chose this timing",
      },
    },
    required: ["hours_from_now", "reason"],
  },
  async execute(input: unknown, ctx: ToolContext): Promise<string> {
    const obj = (input ?? {}) as Record<string, unknown>;
    const rawHours = obj["hours_from_now"];
    const rawReason = obj["reason"];
    if (typeof rawHours !== "number" || !Number.isFinite(rawHours)) {
      throw new ToolError(
        "InvalidArgs",
        "hours_from_now must be a number",
      );
    }
    if (typeof rawReason !== "string") {
      throw new ToolError("InvalidArgs", "reason must be a string");
    }
    const clamped = Math.min(48, Math.max(1, rawHours));

    if (ctx.scheduleNextWake === undefined) {
      // Mirrors mod.rs:297-303 — schema is always present (for cache
      // stability) but dispatch refuses outside heartbeat context.
      throw new ToolError(
        "InvalidArgs",
        "set_next_wake is only available during heartbeat ticks",
      );
    }
    const result = await ctx.scheduleNextWake(clamped, rawReason);
    return JSON.stringify(result);
  },
};
