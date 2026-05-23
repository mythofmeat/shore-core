/**
 * Basic tool tests — check_time, roll_dice, set_next_wake.
 *
 * Mirrors `backend/daemon/src/tools/basic.rs` test cases. `set_next_wake`
 * is the load-bearing one: the schema is always present (for cache
 * stability) but the handler must refuse the call when the autonomy
 * hook is missing.
 */
import { describe, expect, it } from "bun:test";

import {
  checkTimeHandler,
  rollDiceHandler,
  setNextWakeHandler,
} from "../src/tools/basic.ts";
import {
  formatFriendlyDatetime,
  parseDiceNotation,
} from "../src/tools/basic.ts";
import type { ToolContext } from "../src/tools/registry.ts";
import { ToolError } from "../src/tools/registry.ts";

function stubCtx(
  scheduleNextWake?: ToolContext["scheduleNextWake"],
): ToolContext {
  return {
    characterName: "test",
    characterConfigDir: "/tmp/x",
    characterDataDir: "/tmp/x",
    workspaceDir: "/tmp/x",
    configDir: "/tmp/x",
    imageDir: "/tmp/x",
    engine: undefined as unknown as ToolContext["engine"],
    searchConfig: {
      api_key_env: "TAVILY_API_KEY",
      max_results: 5,
      search_depth: "basic",
      include_answer: true,
    },
    retrievalConfig: { max_file_bytes: 1024 * 1024 },
    ...(scheduleNextWake !== undefined ? { scheduleNextWake } : {}),
  };
}

describe("check_time", () => {
  it("returns a friendly datetime string", async () => {
    const out = JSON.parse(await checkTimeHandler.execute({}, stubCtx()));
    expect(typeof out).toBe("string");
    expect(out).toMatch(/ at /);
    expect(out).toMatch(/,/);
  });

  it("formats with day name, ordinal suffix, 12-hour time, AM/PM", () => {
    // Mon Apr 4 2026, 16:34
    const d = new Date(2026, 3, 4, 16, 34, 0);
    const s = formatFriendlyDatetime(d);
    expect(s).toBe("Saturday, April 4th, 2026 at 4:34 PM");
  });

  it("uses 'th' for 11/12/13 and 'st/nd/rd/th' otherwise", () => {
    const cases: Array<[number, string]> = [
      [1, "1st"],
      [2, "2nd"],
      [3, "3rd"],
      [4, "4th"],
      [11, "11th"],
      [12, "12th"],
      [13, "13th"],
      [21, "21st"],
      [22, "22nd"],
      [23, "23rd"],
    ];
    for (const [day, expected] of cases) {
      const d = new Date(2026, 0, day, 12, 0, 0);
      const s = formatFriendlyDatetime(d);
      expect(s).toContain(expected);
    }
  });
});

describe("roll_dice notation parsing", () => {
  it("parses standard 2d6", () => {
    expect(parseDiceNotation("2d6")).toEqual({
      count: 2,
      sides: 6,
      modifier: 0,
    });
  });
  it("parses 1d20+5", () => {
    expect(parseDiceNotation("1d20+5")).toEqual({
      count: 1,
      sides: 20,
      modifier: 5,
    });
  });
  it("parses 4d6-1", () => {
    expect(parseDiceNotation("4d6-1")).toEqual({
      count: 4,
      sides: 6,
      modifier: -1,
    });
  });
  it("treats `d8` as implicit count=1", () => {
    expect(parseDiceNotation("d8")).toEqual({
      count: 1,
      sides: 8,
      modifier: 0,
    });
  });
  it("rejects notation without `d`", () => {
    expect(() => parseDiceNotation("26")).toThrow();
  });
  it("rejects zero count", () => {
    expect(() => parseDiceNotation("0d6")).toThrow();
  });
  it("rejects zero sides", () => {
    expect(() => parseDiceNotation("2d0")).toThrow();
  });
});

describe("roll_dice handler", () => {
  it("returns rolls and total for 2d6", async () => {
    const r = JSON.parse(
      await rollDiceHandler.execute({ notation: "2d6" }, stubCtx()),
    );
    expect(r.notation).toBe("2d6");
    expect(r.rolls).toBeInstanceOf(Array);
    expect(r.rolls.length).toBe(2);
    for (const roll of r.rolls) {
      expect(roll).toBeGreaterThanOrEqual(1);
      expect(roll).toBeLessThanOrEqual(6);
    }
    expect(r.total).toBeGreaterThanOrEqual(2);
    expect(r.total).toBeLessThanOrEqual(12);
  });

  it("applies the modifier to the total", async () => {
    const r = JSON.parse(
      await rollDiceHandler.execute({ notation: "1d6+10" }, stubCtx()),
    );
    expect(r.total).toBeGreaterThanOrEqual(11);
    expect(r.total).toBeLessThanOrEqual(16);
  });

  it("errors on missing notation", async () => {
    expect(rollDiceHandler.execute({}, stubCtx())).rejects.toThrow(ToolError);
  });

  it("errors on invalid notation", async () => {
    expect(
      rollDiceHandler.execute({ notation: "bogus" }, stubCtx()),
    ).rejects.toThrow(ToolError);
  });
});

describe("set_next_wake gating", () => {
  it("errors when no schedule hook is wired (normal user turn)", async () => {
    expect(
      setNextWakeHandler.execute(
        { hours_from_now: 4, reason: "thinking" },
        stubCtx(),
      ),
    ).rejects.toThrow(/heartbeat ticks/);
  });

  it("calls the hook and returns its data when available", async () => {
    let captured: { hours: number; reason: string } | undefined;
    const ctx = stubCtx(async (hours, reason) => {
      captured = { hours, reason };
      return { scheduled: true, hours, reason };
    });
    const r = JSON.parse(
      await setNextWakeHandler.execute(
        { hours_from_now: 4.5, reason: "thinking" },
        ctx,
      ),
    );
    expect(captured?.hours).toBe(4.5);
    expect(captured?.reason).toBe("thinking");
    expect(r.scheduled).toBe(true);
  });

  it("clamps hours_from_now to [1, 48]", async () => {
    let captured: number | undefined;
    const ctx = stubCtx(async (hours) => {
      captured = hours;
      return {};
    });
    await setNextWakeHandler.execute(
      { hours_from_now: 0.1, reason: "too soon" },
      ctx,
    );
    expect(captured).toBe(1);
    await setNextWakeHandler.execute(
      { hours_from_now: 9999, reason: "too far" },
      ctx,
    );
    expect(captured).toBe(48);
  });

  it("errors on missing hours_from_now or reason", async () => {
    const ctx = stubCtx(async () => ({}));
    expect(
      setNextWakeHandler.execute({ reason: "x" }, ctx),
    ).rejects.toThrow(ToolError);
    expect(
      setNextWakeHandler.execute({ hours_from_now: 2 }, ctx),
    ).rejects.toThrow(ToolError);
  });
});
