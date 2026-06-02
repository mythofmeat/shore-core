/**
 * Conversion regression test — the no-live-model proof that the deepseek/kimi
 * tool-loop bug is dead in the official-SDK adapter.
 *
 * Fixtures under tests/fixtures/ are STRUCTURE-preserving, text-redacted copies
 * of real failing requests pulled from production (`meat`): canonical
 * Anthropic-shape turns including thinking blocks, parallel tool_use, and
 * tool_result turns. We run them through the OpenAI adapter's `turnToOpenAI`
 * converter and assert the wire shape strict OpenAI-compatible backends
 * (deepseek, kimi, glm) actually accept.
 *
 * The retired Rust adapter failed these two ways — replaying prior thinking as
 * deepseek's output-only `reasoning_content`, and emitting `"content": null` on
 * tool-call-only assistant turns. This test pins that the TS converter does
 * NEITHER.
 */

import { describe, expect, test } from "bun:test";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

import { turnToOpenAI } from "../src/llm/providers/openai.ts";
import type { TurnMessage } from "../src/llm/types.ts";

const FIXTURE_DIR = join(import.meta.dir, "fixtures");

interface Fixture {
  source: string;
  sdk: string;
  model: string;
  messages: TurnMessage[];
}

function loadFixtures(): Array<{ name: string; fx: Fixture }> {
  return readdirSync(FIXTURE_DIR)
    .filter((f) => f.endsWith(".json"))
    .sort()
    .map((f) => ({
      name: f.replace(/\.json$/, ""),
      fx: JSON.parse(readFileSync(join(FIXTURE_DIR, f), "utf8")) as Fixture,
    }));
}

/** Flatten a fixture's turns through the converter, as buildOpenAICall does. */
function convert(fx: Fixture): Array<Record<string, unknown>> {
  return fx.messages.flatMap(
    (turn) => turnToOpenAI(turn) as unknown as Array<Record<string, unknown>>,
  );
}

const fixtures = loadFixtures();

test("fixtures are present", () => {
  expect(fixtures.length).toBeGreaterThan(0);
});

describe("OpenAI conversion regression (real production sequences)", () => {
  for (const { name, fx } of fixtures) {
    describe(name, () => {
      const out = convert(fx);

      // ── The headline bug: deepseek reasoning_content replay ──────────────
      test("emits no reasoning_content / reasoning field on any message", () => {
        for (const msg of out) {
          expect(msg).not.toHaveProperty("reasoning_content");
          expect(msg).not.toHaveProperty("reasoning");
        }
      });

      // ── Secondary divergence: content:null on tool-call-only turns ───────
      test("never emits content:null (assistant tool-call turns omit content)", () => {
        for (const msg of out) {
          if (msg["role"] === "assistant" && "content" in msg) {
            expect(msg["content"]).not.toBeNull();
          }
        }
      });

      // ── Tool pairing must be valid OpenAI ────────────────────────────────
      test("tool_use → assistant.tool_calls; tool_result → role:tool, ids paired", () => {
        const announced = new Set<string>();
        const srcToolUseIds: string[] = [];
        const srcToolResultIds: string[] = [];
        for (const turn of fx.messages) {
          for (const b of turn.content) {
            if (b.type === "tool_use") srcToolUseIds.push(b.id);
            if (b.type === "tool_result") srcToolResultIds.push(b.tool_use_id);
          }
        }

        const emittedToolCallIds: string[] = [];
        const toolMsgIds: string[] = [];
        for (const msg of out) {
          if (msg["role"] === "assistant" && Array.isArray(msg["tool_calls"])) {
            for (const tc of msg["tool_calls"] as Array<Record<string, unknown>>) {
              expect(tc["type"]).toBe("function");
              const id = String(tc["id"]);
              announced.add(id);
              emittedToolCallIds.push(id);
            }
          }
          if (msg["role"] === "tool") {
            const id = String(msg["tool_call_id"]);
            toolMsgIds.push(id);
            // Every tool result must reference a tool_call announced earlier.
            expect(announced.has(id)).toBe(true);
          }
        }

        // Nothing dropped or invented in either direction.
        expect(emittedToolCallIds.sort()).toEqual([...srcToolUseIds].sort());
        expect(toolMsgIds.sort()).toEqual([...srcToolResultIds].sort());
      });

      // ── General OpenAI shape sanity ──────────────────────────────────────
      test("every emitted message has a valid role", () => {
        for (const msg of out) {
          expect(["system", "user", "assistant", "tool"]).toContain(
            String(msg["role"]),
          );
        }
      });
    });
  }
});
