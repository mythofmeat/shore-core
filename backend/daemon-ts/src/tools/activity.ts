/**
 * `activity_heatmap` — returns real autonomy data when a stats hook is
 * wired into the context, otherwise an empty 24-hour heatmap.
 *
 * Ported from `backend/daemon/src/tools/activity.rs`. The autonomy
 * subsystem itself ships in Phase 8 — until then `ctx.activityStats`
 * is always undefined and this tool returns the empty shape.
 */

import type { ToolContext, ToolHandler } from "./registry.ts";

export const ACTIVITY_HEATMAP_DESCRIPTION =
  "View {{user}}'s activity heatmap — when they typically message you, broken down by hour of day and day of week. Use when you want to understand their rhythm: whether now is an unusual time for them to be talking to you, when they're most likely to be around, or whether they've been more or less active lately. Returns hour-by-hour density, classification (peak/trough/normal), total user-turn count over the window, and an engagement score. The `days` parameter controls how much history is included; 30 is a good default for seeing a steady pattern.";

function emptyHeatmap(days: number): unknown {
  const hours = [];
  for (let h = 0; h < 24; h++) {
    hours.push({ hour: h, density: 0.0, classification: "normal" });
  }
  return {
    days,
    hours,
    total_messages: 0,
    total_turns: 0,
    has_sufficient_data: false,
    engagement_score: 0.0,
    sessions_per_day: 0.0,
  };
}

export const activityHeatmapHandler: ToolHandler = {
  name: "activity_heatmap",
  description: ACTIVITY_HEATMAP_DESCRIPTION,
  inputSchema: {
    type: "object",
    properties: {
      days: {
        type: "integer",
        description: "Number of days of history to include.",
        default: 30,
      },
    },
  },
  async execute(input: unknown, ctx: ToolContext): Promise<string> {
    const obj = (input ?? {}) as { days?: unknown };
    const days =
      typeof obj.days === "number" && Number.isFinite(obj.days)
        ? Math.max(0, Math.floor(obj.days))
        : 30;

    const stats = ctx.activityStats?.(ctx.characterName);
    if (stats === undefined) return JSON.stringify(emptyHeatmap(days));

    const hours = [];
    for (let h = 0; h < 24; h++) {
      hours.push({
        hour: h,
        density: stats.hourHistogram[h] ?? 0,
        classification: stats.hourClassifications[h] ?? "normal",
      });
    }
    return JSON.stringify({
      days,
      hours,
      total_messages: stats.turnCount,
      total_turns: stats.turnCount,
      has_sufficient_data: stats.hasSufficientHeatmap,
      engagement_score: stats.engagementScore,
      sessions_per_day: stats.sessionsPerDay,
    });
  },
};
