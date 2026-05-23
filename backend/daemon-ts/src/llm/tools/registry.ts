/**
 * Phase-4a tool registry — intentionally minimal.
 *
 * Phase 5 ports the full surface (read/write/edit/list_files/search/
 * delete/exec/web_search/fetch_url/check_time/roll_dice/activity_heatmap/
 * generate_image/search_history). For now we ship ONE tool whose only
 * job is to force a thinking → tool_use → tool_result → thinking → text
 * sequence inside a single turn, so the cache regression test has
 * something to bite on.
 *
 * `roll_dice` is the right shape for this:
 *   - cheap to execute, deterministic-ish output for tests
 *   - the model can't shortcut it (it can't predict the result)
 *   - asking for multiple rolls forces a few tool_use iterations,
 *     exercising the loop-entry AND loop-exit caching paths
 */

export interface ToolHandler {
  name: string;
  description: string;
  inputSchema: Record<string, unknown>;
  execute(input: unknown): Promise<string>;
}

export class ToolRegistry {
  private readonly tools = new Map<string, ToolHandler>();

  register(handler: ToolHandler): void {
    this.tools.set(handler.name, handler);
  }

  get(name: string): ToolHandler | undefined {
    return this.tools.get(name);
  }

  list(): ToolHandler[] {
    return [...this.tools.values()];
  }
}

// ── Phase 4a stub tool ─────────────────────────────────────────────────

export const rollDice: ToolHandler = {
  name: "roll_dice",
  description:
    "Roll one or more dice. Returns the individual results and the sum.",
  inputSchema: {
    type: "object",
    properties: {
      count: {
        type: "integer",
        minimum: 1,
        maximum: 100,
        description: "How many dice to roll.",
      },
      sides: {
        type: "integer",
        minimum: 2,
        maximum: 1000,
        description: "How many sides each die has.",
      },
    },
    required: ["count", "sides"],
    additionalProperties: false,
  },
  async execute(input) {
    const obj = (input ?? {}) as { count?: number; sides?: number };
    const count = Math.max(1, Math.min(100, Math.floor(obj.count ?? 1)));
    const sides = Math.max(2, Math.min(1000, Math.floor(obj.sides ?? 6)));
    const rolls: number[] = [];
    for (let i = 0; i < count; i++) {
      rolls.push(1 + Math.floor(Math.random() * sides));
    }
    const sum = rolls.reduce((a, b) => a + b, 0);
    return JSON.stringify({ rolls, sum });
  },
};

export function defaultRegistry(): ToolRegistry {
  const reg = new ToolRegistry();
  reg.register(rollDice);
  return reg;
}
