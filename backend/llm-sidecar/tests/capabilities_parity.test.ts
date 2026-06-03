import { expect, test } from "bun:test";

import fixture from "../../../core/config/capability_parity_fixture.toml";
import {
  claudeRejectsSampling,
  claudeThinkingCaps,
  parseClaudeModel,
} from "../src/llm/capabilities.ts";

interface Case {
  model: string;
  is_claude: boolean;
  rejects_sampling: boolean;
  adaptive?: boolean;
  enabled?: boolean;
}

// Shared with the Rust tests (`capabilities.rs::cross_language_parity_fixture`):
// both reimplementations of the parser + rule evaluator must agree with these.
const cases = (fixture as { case: Case[] }).case;

test("cross-language capability parity", () => {
  expect(cases.length).toBeGreaterThan(0);
  for (const c of cases) {
    expect(parseClaudeModel(c.model) !== undefined, `is_claude: ${c.model}`).toBe(c.is_claude);
    expect(claudeRejectsSampling(c.model), `rejects_sampling: ${c.model}`).toBe(c.rejects_sampling);
    if (c.adaptive !== undefined && c.enabled !== undefined) {
      expect(claudeThinkingCaps(c.model), `thinking: ${c.model}`).toEqual({
        adaptive: c.adaptive,
        enabled: c.enabled,
      });
    }
  }
});
