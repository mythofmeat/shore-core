/**
 * Live cache-regression test — runs real Anthropic-API calls.
 *
 * Gated by env: skipped unless OPENROUTER_API_KEY (or ANTHROPIC_API_KEY)
 * is set. The whole point of this rewrite is to make sure interleaved
 * thinking → tool_use → tool_result → thinking → text doesn't lose the
 * prompt cache across turns. This test is the regression's tripwire.
 *
 * Default model: anthropic/claude-haiku-4.5 via OpenRouter (cheap).
 * Set SHORE_TEST_MODEL=anthropic/claude-sonnet-4.5 to validate against
 * the model class closest to production opus (recommended before any
 * cache-affecting change ships).
 *
 * Each scenario sends two turns. Turn 1 establishes the cache; turn 2
 * MUST report cache_read_input_tokens > 0 against the prefix that was
 * established by turn 1. We're indifferent to the exact number — we
 * only assert the cache *engaged*. The Rust regression we're killing
 * reported cache_read = 0 on turn 2 after a tool loop.
 *
 * Two non-obvious requirements baked into this test, learned the hard
 * way during phase 4a:
 *
 *   1. **Fresh-cache nonce.** Every test run injects a UUID into the
 *      system prompt so the cache prefix is unique. Without this, a
 *      warmed cache from an earlier run masks the cache_read=0 signal
 *      we're trying to catch — the test would pass even with a broken
 *      adapter.
 *
 *   2. **Prompt size well above the documented threshold.** Anthropic
 *      docs say haiku-4.5 caches prompts ≥2048 input tokens. In
 *      practice via OpenRouter, prompts at ~4000 tokens still return
 *      cache_creation=0 on the FIRST call — the actual threshold is
 *      higher (or there's hysteresis). We pad to ~11k tokens to be
 *      unambiguously over the line.
 */

import { describe, expect, it } from "bun:test";

import { AnthropicProvider } from "../src/llm/providers/anthropic.ts";
import { rollDiceHandler, ToolRegistry, type ToolContext } from "../src/tools/registry.ts";
import { runToolLoop } from "../src/llm/tool_loop.ts";

/**
 * Minimal registry — just `roll_dice` — for the cache regression test.
 * The full `defaultRegistry()` would inject the other 14 tools into the
 * schema, which is fine but adds noise to a test whose only job is to
 * force a single-tool loop.
 */
function diceRegistry(): ToolRegistry {
  const reg = new ToolRegistry();
  reg.register(rollDiceHandler);
  return reg;
}

/**
 * Empty stub context — `roll_dice` doesn't touch any context fields, so
 * the runtime values don't matter. Required by `runToolLoop` post-4c.2.
 */
function stubCtx(): ToolContext {
  return {
    characterName: "test",
    characterConfigDir: "/tmp/test-config",
    characterDataDir: "/tmp/test-data",
    workspaceDir: "/tmp/test-workspace",
    configDir: "/tmp/test-config",
    imageDir: "/tmp/test-images",
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    engine: undefined as any,
    searchConfig: {
      api_key_env: "TAVILY_API_KEY",
      max_results: 5,
      search_depth: "basic",
      include_answer: true,
    },
    retrievalConfig: { max_file_bytes: 1024 * 1024 },
  };
}
import type { ChatRequest, TurnMessage } from "../src/llm/types.ts";

const apiKey = process.env["OPENROUTER_API_KEY"] ?? process.env["ANTHROPIC_API_KEY"] ?? "";
const useOpenRouter = !!process.env["OPENROUTER_API_KEY"];
const baseUrl = useOpenRouter ? "https://openrouter.ai/api/v1" : undefined;
const model = process.env["SHORE_TEST_MODEL"] ?? "anthropic/claude-haiku-4.5";

/**
 * Fresh-cache nonce: makes the cache prefix unique per test run. See
 * the header comment for why this is load-bearing.
 *
 * One nonce per `bun test` invocation (not per scenario) so all
 * scenarios share a single cache prefix from their first turn —
 * matching how a real conversation builds up cache.
 */
const NONCE = process.env["SHORE_TEST_NONCE"] ?? crypto.randomUUID();

// Verbose system prompt. Anthropic docs say haiku-4.5 caches prompts
// ≥2048 input tokens, but empirically OpenRouter requires ≥~4000 to
// reliably write the first cache. We use 100 reps to land at ~11k
// tokens and stay well clear of the boundary.
const SYSTEM = (() => {
  const para = `You are Casey, a meticulous tabletop-game referee. Your job is to roll
dice on behalf of the player whenever they request a check, an attack roll,
damage, or a saving throw. You always use the roll_dice tool. You never
make up dice results — every random outcome must come from the tool.

When the player asks for a result, think carefully about how to interpret
their request: what kind of check is implied, how many dice, what sides,
whether any modifier should be applied after the roll. Then call
roll_dice with the appropriate count and sides. After the tool returns,
narrate the result in-character and explain what it means for the
player's situation. Be concise but flavorful.

You speak with a measured, slightly formal voice. You refer to the player
by name when possible. You use period-appropriate vocabulary for whatever
setting is in play — Tolkien-esque for fantasy, hard-boiled for noir,
clipped and technical for sci-fi. You never break character, even when
the player asks meta questions about the rules; you reframe the answer
in-fiction.

Your rulings are firm and final. You don't second-guess the dice. If the
player rolls poorly, you describe the consequences with sympathy but
without softening; if they roll well, you let the triumph land without
overselling it. You treat the dice as a kind of impartial oracle whose
verdicts you merely translate.`;
  // Pad to ~11k tokens of stable system text. 30 reps got us ~4k
  // tokens which sat in OpenRouter's "documented eligible but
  // practically not cached" gray zone and produced false-positive test
  // passes. 100 reps clears it.
  return `nonce: ${NONCE}\n\n` + Array.from({ length: 100 }, () => para).join("\n\n");
})();

const PROVIDER = new AnthropicProvider();

function makeRequest(messages: TurnMessage[]): ChatRequest {
  return {
    system: SYSTEM,
    messages,
    tools: diceRegistry()
      .list()
      .map((t) => ({
        name: t.name,
        description: t.description,
        inputSchema: t.inputSchema,
      })),
    thinking: { enabled: true, effort: "low" },
    cacheTtl: "5m",
    modelId: model,
    apiKey,
    ...(baseUrl ? { baseUrl } : {}),
    maxTokens: 4096,
  };
}

describe.if(apiKey !== "")("Anthropic cache regression", () => {
  it("plain chat: turn 2 reads cache from turn 1", async () => {
    // Turn 1: a question the model can answer without rolling.
    const turn1: TurnMessage[] = [
      {
        role: "user",
        content: [
          {
            type: "text",
            text: "Hi Casey. Before we start, what kinds of dice do you have handy?",
          },
        ],
      },
    ];
    const r1 = await runToolLoop({
      provider: PROVIDER,
      request: makeRequest(turn1),
      registry: diceRegistry(),
      toolContext: stubCtx(),
    });
    const totalInput1 = r1.usagePerCall[0]!.inputTokens
      + r1.usagePerCall[0]!.cacheReadInputTokens
      + r1.usagePerCall[0]!.cacheCreationInputTokens;
    console.log("turn1 usage:", r1.usagePerCall[0], "total_input=", totalInput1);

    // Turn 2: extend with the assistant's previous reply + a new user message.
    const turn2 = [...turn1, ...r1.newTurns, {
      role: "user" as const,
      content: [{ type: "text" as const, text: "Great. Roll 2d6 for me, please." }],
    }];
    const r2 = await runToolLoop({
      provider: PROVIDER,
      request: makeRequest(turn2),
      registry: diceRegistry(),
      toolContext: stubCtx(),
    });
    console.log("turn2 usage:", r2.usagePerCall);

    // The FIRST provider call of turn 2 is what hits the cache prefix
    // we established in turn 1 (system + tools + initial user + assistant
    // reply). cache_read MUST be > 0; if it's 0 the prefix changed.
    expect(r2.usagePerCall[0]!.cacheReadInputTokens).toBeGreaterThan(0);
  }, 120_000);

  it("adaptive thinking + multi-iteration tool loop holds cache", async () => {
    // The pathology this rewrite exists to kill: a tool loop that
    // iterates SEVERAL times under ADAPTIVE thinking (variable budget
    // per turn). The Rust regression dropped the cache when the
    // thinking-budget-determined block shape shifted between turn-pairs.
    //
    // Force the model to call roll_dice three times in sequence (not
    // batched into one call) so the loop iterates at least 3x. Adaptive
    // thinking lets Claude vary the thinking-block size each iteration —
    // the cache prefix must survive that variation.
    // Use dependent rolls so the model literally cannot batch: each
    // outcome determines what to roll next. This guarantees 3+ tool
    // round-trips inside a single user turn.
    const initial: TurnMessage[] = [
      {
        role: "user",
        content: [
          {
            type: "text",
            text:
              "Casey, here's a branching scenario. Resolve it step by step.\n" +
              "\n" +
              "Step 1: Roll 1d20 for stealth.\n" +
              "Step 2: Look at the stealth result. " +
              "  - If the stealth roll is 10 or HIGHER, roll 1d8 for sneak-attack damage. " +
              "  - If the stealth roll is BELOW 10, roll 1d20 for an athletics check to escape instead. " +
              "Step 3: Look at the result from Step 2. " +
              "  - If Step 2 gave a damage roll of 5+, roll 1d4 for a follow-up dagger throw. " +
              "  - If Step 2 was the athletics escape and it succeeded (10+), roll 1d6 for the running distance. " +
              "  - Otherwise roll 1d20 for a perception check as you regroup.\n" +
              "\n" +
              "Each step MUST happen after seeing the prior step's result. " +
              "Do not batch. Use roll_dice three separate times. " +
              "After all three, narrate the outcome briefly in-character.",
          },
        ],
      },
    ];
    const req: ChatRequest = makeRequest(initial);
    req.thinking = { enabled: true, effort: "adaptive" };

    const r = await runToolLoop({
      provider: PROVIDER,
      request: req,
      registry: diceRegistry(),
      toolContext: stubCtx(),
    });
    console.log("adaptive multi-iter usage per call:", r.usagePerCall);

    // Per-assistant-turn shape inspection — adaptive thinking on haiku
    // emits a thinking block on the *first* assistant turn of the loop
    // and then skips it on every subsequent in-loop turn. That's a
    // genuine block-shape transition (thinking + tool_use → text +
    // tool_use), and the regression we're killing is the cache breaking
    // when the shape changes. Without observing the shape change we'd
    // be testing a softer property (cache survives identical shapes,
    // which is trivial).
    const assistantTurns = r.newTurns.filter((t) => t.role === "assistant");
    const shapes = assistantTurns.map((t) => ({
      hasThinking: t.content.some((b) => b.type === "thinking"),
      hasToolUse: t.content.some((b) => b.type === "tool_use"),
    }));
    console.log("adaptive shape per assistant turn:", shapes);

    // At least 3 iterations through the loop (3 tool calls + closing turn → ≥4 provider calls).
    expect(r.usagePerCall.length).toBeGreaterThanOrEqual(3);

    // Shape transition observed: at least one thinking-emitting AND at
    // least one non-thinking assistant turn. This is the pathology
    // (thinking → no-thinking transitions inside a single tool loop)
    // and it's what cooked the Rust daemon's prefix hash.
    const thinkingCount = shapes.filter((s) => s.hasThinking).length;
    const noThinkingCount = shapes.filter((s) => !s.hasThinking).length;
    expect(thinkingCount).toBeGreaterThan(0);
    expect(noThinkingCount).toBeGreaterThan(0);

    // KEY assertion: every provider call after the first must read
    // cache. cache_read=0 anywhere here means a shape transition
    // invalidated the prefix.
    for (let i = 1; i < r.usagePerCall.length; i++) {
      const u = r.usagePerCall[i]!;
      expect(u.cacheReadInputTokens).toBeGreaterThan(0);
    }

    // First call's cache state lets us also pin: did we WRITE the
    // initial cache (cache_creation > 0)? If cache_creation=0 on call 0
    // the prompt was below threshold and the rest of the test reduces
    // to vacuous — fail loudly.
    expect(r.usagePerCall[0]!.cacheCreationInputTokens).toBeGreaterThan(0);
  }, 240_000);

  it("tool loop: cache holds across thinking → tool_use → tool_result → text", async () => {
    // Turn 1: a request that forces ONE tool round-trip. The model will
    // emit thinking → tool_use, we'll execute roll_dice, then it emits
    // a closing text turn.
    const turn1: TurnMessage[] = [
      {
        role: "user",
        content: [
          {
            type: "text",
            text: "Quick check: please roll 1d20 for me and tell me what it means in DnD 5e terms (low / mid / high).",
          },
        ],
      },
    ];
    const r1 = await runToolLoop({
      provider: PROVIDER,
      request: makeRequest(turn1),
      registry: diceRegistry(),
      toolContext: stubCtx(),
    });
    console.log("tool-loop turn1 usage per call:", r1.usagePerCall);
    // We expect at least 2 provider calls inside the tool loop (call →
    // tool_use stop_reason → tool_result → final text). The SECOND call
    // is the one whose cache prefix should include the (newly created)
    // turn-1 prefix from the FIRST call.
    expect(r1.usagePerCall.length).toBeGreaterThanOrEqual(2);
    expect(r1.usagePerCall[1]!.cacheReadInputTokens).toBeGreaterThan(0);

    // Turn 2: follow-up that re-uses the entire tool-loop conversation
    // as cached prefix. This is the regression: tool-loop-exit cache.
    const turn2 = [...turn1, ...r1.newTurns, {
      role: "user" as const,
      content: [{ type: "text" as const, text: "Good. One more — roll 3d8." }],
    }];
    const r2 = await runToolLoop({
      provider: PROVIDER,
      request: makeRequest(turn2),
      registry: diceRegistry(),
      toolContext: stubCtx(),
    });
    console.log("tool-loop turn2 usage per call:", r2.usagePerCall);
    expect(r2.usagePerCall[0]!.cacheReadInputTokens).toBeGreaterThan(0);
  }, 240_000);
});

describe.if(apiKey === "")("Anthropic cache regression (skipped — no API key)", () => {
  it.skip("set OPENROUTER_API_KEY or ANTHROPIC_API_KEY to run", () => {});
});

// ── OpenAI-compatible adapter live tests ──────────────────────────────
//
// We don't have a direct OpenAI key on this machine, so we route OpenAI
// models through OpenRouter (their `/chat/completions` endpoint speaks
// the OpenAI wire format). This validates the same adapter code path
// that would run against OpenAI direct, DeepSeek, xAI, etc. The Rust
// impl had specific tool-use bugs on the openai-compatible endpoints;
// these tests are the tripwire for the SDK route doing better.
//
// API-key resolution:
//   - OPENAI_API_KEY set → direct OpenAI (no base URL)
//   - OPENROUTER_API_KEY set → OpenRouter (`openai/<model>` namespace)
//   - neither → skip
//
// Prompt caching on OpenAI is server-side automatic for prompts
// ≥1024 tokens — no `cache_control` knobs. OpenRouter surfaces hits
// as `usage.prompt_tokens_details.cached_tokens`; our adapter maps
// that to `cacheReadInputTokens`.

const openaiDirectKey = process.env["OPENAI_API_KEY"] ?? "";
const openaiViaORKey = openaiDirectKey === "" ? (process.env["OPENROUTER_API_KEY"] ?? "") : "";
const openaiTransportKey = openaiDirectKey || openaiViaORKey;
const openaiBaseUrl = openaiDirectKey ? undefined : (openaiViaORKey ? "https://openrouter.ai/api/v1" : undefined);
const openaiModel = process.env["SHORE_TEST_OPENAI_MODEL"]
  ?? (openaiDirectKey ? "gpt-5.4-mini" : "openai/gpt-5.4-mini");

// One nonce per test run, same idea as the Anthropic side.
const OPENAI_NONCE = process.env["SHORE_TEST_OPENAI_NONCE"] ?? crypto.randomUUID();

// Cache threshold on OpenAI is documented as ≥1024 prompt tokens. Pad
// well above that so we can unambiguously assert cached_tokens > 0 on
// the second call.
const OPENAI_SYSTEM = `nonce: ${OPENAI_NONCE}\n\n` + Array.from({ length: 60 }, () =>
  "You are Quartermaster Vale, a curt and methodical inventory officer. " +
    "When asked to roll dice (for randomized supply outcomes, morale " +
    "checks, or skirmish resolution), call the roll_dice tool. Never " +
    "invent dice results. Report results plainly and concisely.",
).join("\n\n");

function openaiRequest(messages: TurnMessage[], modelOverride?: string): ChatRequest {
  return {
    system: OPENAI_SYSTEM,
    messages,
    tools: diceRegistry()
      .list()
      .map((t) => ({
        name: t.name,
        description: t.description,
        inputSchema: t.inputSchema,
      })),
    thinking: { enabled: false },
    cacheTtl: "",
    modelId: modelOverride ?? openaiModel,
    apiKey: openaiTransportKey,
    ...(openaiBaseUrl ? { baseUrl: openaiBaseUrl } : {}),
    maxTokens: 1024,
  };
}

describe.if(openaiTransportKey !== "")("OpenAI-compatible adapter", () => {
  it("single tool call: loop completes, returns text, reports usage", async () => {
    const { OpenAIProvider } = await import("../src/llm/providers/openai.ts");
    const provider = new OpenAIProvider();
    const r = await runToolLoop({
      provider,
      request: openaiRequest([
        {
          role: "user",
          content: [{ type: "text", text: "Roll 2d6 and tell me the sum." }],
        },
      ]),
      registry: diceRegistry(),
      toolContext: stubCtx(),
    });
    console.log("openai single-tool usage:", r.usagePerCall);
    // At least one provider call to emit tool_use, one more to read
    // tool_result and produce the closing text. Total ≥ 2.
    expect(r.usagePerCall.length).toBeGreaterThanOrEqual(2);
    const finalText = r.finalContent
      .filter((b) => b.type === "text")
      .map((b) => (b as { text: string }).text)
      .join("");
    expect(finalText.length).toBeGreaterThan(0);
  }, 120_000);

  it("multi-iteration tool loop with dependent rolls", async () => {
    // Same dependent-rolls pattern as the Anthropic adaptive scenario:
    // the model literally cannot batch these because each outcome
    // determines the next call. Forces 3+ tool round-trips so we
    // exercise tool_use → tool_result → tool_use → ... cleanly.
    const { OpenAIProvider } = await import("../src/llm/providers/openai.ts");
    const provider = new OpenAIProvider();
    const r = await runToolLoop({
      provider,
      request: openaiRequest([
        {
          role: "user",
          content: [
            {
              type: "text",
              text:
                "Vale, resolve this branching scenario step by step. " +
                "Step 1: roll 1d20 for the scouting check. " +
                "Step 2: if the scouting roll is 10+, roll 1d8 for ambush damage; otherwise roll 1d20 for an athletics check to retreat. " +
                "Step 3: if Step 2's damage was 5+, roll 1d4 for a follow-up shot; if Step 2's athletics succeeded (10+), roll 1d6 for distance; otherwise roll 1d20 for perception as you regroup. " +
                "Each step must follow seeing the prior result. Use roll_dice three separate times.",
            },
          ],
        },
      ]),
      registry: diceRegistry(),
      toolContext: stubCtx(),
    });
    console.log("openai multi-iter usage per call:", r.usagePerCall);
    expect(r.usagePerCall.length).toBeGreaterThanOrEqual(3);
    const finalText = r.finalContent
      .filter((b) => b.type === "text")
      .map((b) => (b as { text: string }).text)
      .join("");
    expect(finalText.length).toBeGreaterThan(0);
  }, 240_000);

  it("automatic prompt caching: turn 2 surfaces cached_tokens > 0", async () => {
    // OpenAI's caching is server-side automatic; no client config.
    // OpenRouter forwards usage.prompt_tokens_details.cached_tokens
    // back, our adapter maps that to cacheReadInputTokens. We send
    // a stable big prefix twice and assert the second call reads cache.
    const { OpenAIProvider } = await import("../src/llm/providers/openai.ts");
    const provider = new OpenAIProvider();

    const turn1: TurnMessage[] = [
      {
        role: "user",
        content: [{ type: "text", text: "Briefly: what kinds of dice do you use?" }],
      },
    ];
    const r1 = await runToolLoop({
      provider,
      request: openaiRequest(turn1),
      registry: diceRegistry(),
      toolContext: stubCtx(),
    });
    console.log("openai cache turn1 usage:", r1.usagePerCall);

    const turn2 = [...turn1, ...r1.newTurns, {
      role: "user" as const,
      content: [{ type: "text" as const, text: "And what's your background?" }],
    }];
    const r2 = await runToolLoop({
      provider,
      request: openaiRequest(turn2),
      registry: diceRegistry(),
      toolContext: stubCtx(),
    });
    console.log("openai cache turn2 usage:", r2.usagePerCall);

    // The cache feature is automatic but the threshold + freshness
    // semantics are out of our control. We assert *some* cache read
    // on the second turn — if cached_tokens=0 then either the prefix
    // didn't qualify (prompt too small? probably not at ~6k tokens)
    // or our adapter is dropping the field.
    expect(r2.usagePerCall[0]!.cacheReadInputTokens).toBeGreaterThan(0);
  }, 120_000);
});
