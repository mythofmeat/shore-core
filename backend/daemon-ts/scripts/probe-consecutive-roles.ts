#!/usr/bin/env bun
/**
 * Empirical probe: do Anthropic and OpenAI accept consecutive same-role
 * messages? And does the cache hit on a second call with the same prefix?
 *
 * Run:  bun run scripts/probe-consecutive-roles.ts
 * Requires: OPENROUTER_API_KEY in env (and SHORE_TEST_MODEL optional).
 */
import { AnthropicProvider } from "../src/llm/providers/anthropic.ts";
import { OpenAIProvider } from "../src/llm/providers/openai.ts";
import type {
  ChatEvent,
  ChatRequest,
  TurnMessage,
  UsageStats,
} from "../src/llm/types.ts";

const apiKey = process.env["OPENROUTER_API_KEY"] ?? "";
if (!apiKey) {
  console.error("OPENROUTER_API_KEY not set");
  process.exit(1);
}

const NONCE = crypto.randomUUID();
// Padded above haiku's empirical cache threshold (~4k tokens — docs say
// 2k but reality is higher). The cache_regression test uses the same
// shape to stay clear of the gray zone.
const PARA =
  "You are a terse oracle, charged with answering with extreme economy " +
  "of language. Your answers should never exceed twenty words. You speak " +
  "with the gravity of a sibyl and the clarity of an engineer. You do not " +
  "hedge, equivocate, or pad your responses with throat-clearing phrases. " +
  "When a question is poorly formed, you ask one clarifying question and " +
  "stop. When a question is clear, you answer in a single dense sentence. " +
  "You are aware that the people who consult you are busy and skeptical, " +
  "and you treat their time as a sacred resource.";
const SYSTEM =
  `nonce: ${NONCE}\n\n` +
  Array.from({ length: 100 }, () => PARA).join("\n\n");

async function drain(
  stream: AsyncIterable<ChatEvent>,
): Promise<{ text: string; usage: UsageStats; stopReason: string }> {
  let text = "";
  let usage: UsageStats | null = null;
  let stopReason = "";
  for await (const ev of stream) {
    if (ev.kind === "text_delta") text += ev.text;
    if (ev.kind === "done") {
      const textBlock = ev.content.find((b) => b.type === "text");
      if (textBlock?.type === "text") text = textBlock.text;
      usage = ev.usage;
      stopReason = ev.stopReason;
    }
  }
  if (!usage) throw new Error("no done event");
  return { text, usage, stopReason };
}

function anthropicReq(messages: TurnMessage[]): ChatRequest {
  return {
    system: SYSTEM,
    messages,
    tools: [],
    thinking: { enabled: false },
    cacheTtl: "5m",
    modelId: "anthropic/claude-haiku-4.5",
    apiKey,
    baseUrl: "https://openrouter.ai/api/v1",
    maxTokens: 256,
  };
}

function openaiReq(messages: TurnMessage[]): ChatRequest {
  return {
    system: SYSTEM,
    messages,
    tools: [],
    thinking: { enabled: false },
    cacheTtl: "",
    modelId: "openai/gpt-5.4-mini",
    apiKey,
    baseUrl: "https://openrouter.ai/api/v1",
    maxTokens: 256,
  };
}

async function probe(
  label: string,
  provider: { stream: (r: ChatRequest) => AsyncIterable<ChatEvent> },
  reqFor: (m: TurnMessage[]) => ChatRequest,
  shape: TurnMessage[],
  followUp: TurnMessage,
): Promise<void> {
  console.log(`\n── ${label} ──`);
  try {
    const r1 = await drain(provider.stream(reqFor(shape)));
    console.log(`  turn1 ok | stop=${r1.stopReason} | usage=`, r1.usage);

    const shape2: TurnMessage[] = [
      ...shape,
      {
        role: "assistant",
        content: [{ type: "text", text: r1.text.slice(0, 50) }],
      },
      followUp,
    ];
    const r2 = await drain(provider.stream(reqFor(shape2)));
    console.log(`  turn2 ok | stop=${r2.stopReason} | usage=`, r2.usage);
    if (r2.usage.cacheReadInputTokens > 0) {
      console.log(`  → CACHE HIT (${r2.usage.cacheReadInputTokens} tokens)`);
    } else {
      console.log(`  → cache_read=0`);
    }
  } catch (e) {
    const msg = (e as Error).message;
    console.log(`  REJECTED: ${msg.slice(0, 200)}`);
  }
}

const anth = new AnthropicProvider();
const oai = new OpenAIProvider();

// ── Anthropic side ────────────────────────────────────────────────────────
await probe(
  "Anthropic: alternating [user, asst, user] (baseline)",
  anth,
  anthropicReq,
  [{ role: "user", content: [{ type: "text", text: "ping a" }] }],
  { role: "user", content: [{ type: "text", text: "follow up a" }] },
);

await probe(
  "Anthropic: consecutive users [user, user]",
  anth,
  anthropicReq,
  [
    { role: "user", content: [{ type: "text", text: "ping b1" }] },
    { role: "user", content: [{ type: "text", text: "ping b2" }] },
  ],
  { role: "user", content: [{ type: "text", text: "follow up b" }] },
);

await probe(
  "Anthropic: [user, asst, user, user] (post-injection shape)",
  anth,
  anthropicReq,
  [
    { role: "user", content: [{ type: "text", text: "ping c1" }] },
    { role: "assistant", content: [{ type: "text", text: "ack c1" }] },
    { role: "user", content: [{ type: "text", text: "ping c2" }] },
    { role: "user", content: [{ type: "text", text: "ping c3" }] },
  ],
  { role: "user", content: [{ type: "text", text: "follow up c" }] },
);

await probe(
  "Anthropic: consecutive assistants [user, asst, asst]",
  anth,
  anthropicReq,
  [
    { role: "user", content: [{ type: "text", text: "ping d" }] },
    { role: "assistant", content: [{ type: "text", text: "ack d1" }] },
    { role: "assistant", content: [{ type: "text", text: "ack d2" }] },
  ],
  { role: "user", content: [{ type: "text", text: "follow up d" }] },
);

// ── OpenAI side ──────────────────────────────────────────────────────────
await probe(
  "OpenAI: alternating [user, asst, user] (baseline)",
  oai,
  openaiReq,
  [{ role: "user", content: [{ type: "text", text: "ping a" }] }],
  { role: "user", content: [{ type: "text", text: "follow up a" }] },
);

await probe(
  "OpenAI: consecutive users [user, user]",
  oai,
  openaiReq,
  [
    { role: "user", content: [{ type: "text", text: "ping b1" }] },
    { role: "user", content: [{ type: "text", text: "ping b2" }] },
  ],
  { role: "user", content: [{ type: "text", text: "follow up b" }] },
);

await probe(
  "OpenAI: [user, asst, user, user]",
  oai,
  openaiReq,
  [
    { role: "user", content: [{ type: "text", text: "ping c1" }] },
    { role: "assistant", content: [{ type: "text", text: "ack c1" }] },
    { role: "user", content: [{ type: "text", text: "ping c2" }] },
    { role: "user", content: [{ type: "text", text: "ping c3" }] },
  ],
  { role: "user", content: [{ type: "text", text: "follow up c" }] },
);

await probe(
  "OpenAI: consecutive assistants [user, asst, asst]",
  oai,
  openaiReq,
  [
    { role: "user", content: [{ type: "text", text: "ping d" }] },
    { role: "assistant", content: [{ type: "text", text: "ack d1" }] },
    { role: "assistant", content: [{ type: "text", text: "ack d2" }] },
  ],
  { role: "user", content: [{ type: "text", text: "follow up d" }] },
);
