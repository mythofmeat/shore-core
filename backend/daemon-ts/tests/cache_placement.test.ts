/**
 * Offline cache-placement tests.
 *
 * These pin the EXACT request bodies the Anthropic adapter produces — the
 * thing the Anthropic cache server hashes to decide hit vs miss. The
 * live `cache_regression.test.ts` proves the server actually caches on
 * these bodies; this file proves the bodies are stable in the way the
 * server expects.
 *
 * Run as part of CI (no API key required).
 */
import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { CacheForensics } from "../src/ledger/cache_forensics.ts";
import { AnthropicProvider } from "../src/llm/providers/anthropic.ts";
import type {
  ChatEvent,
  ChatRequest,
  TurnMessage,
} from "../src/llm/types.ts";
import {
  type CannedResponse,
  findCacheControlPaths,
  startFakeAnthropic,
} from "./_fake_anthropic.ts";

async function drain(stream: AsyncIterable<ChatEvent>): Promise<ChatEvent> {
  let done: ChatEvent | undefined;
  for await (const ev of stream) {
    if (ev.kind === "done") done = ev;
  }
  if (!done) throw new Error("stream ended without 'done' event");
  return done;
}

function baseRequest(messages: TurnMessage[]): ChatRequest {
  return {
    system: "You are Casey.",
    messages,
    tools: [],
    thinking: { enabled: false },
    cacheTtl: "1h",
    modelId: "fake-model",
    apiKey: "fake-key",
    maxTokens: 1024,
  };
}

const SAMPLE_TOOLS = [
  {
    name: "roll_dice",
    description: "Roll N dice with S sides.",
    inputSchema: {
      type: "object",
      properties: { count: { type: "integer" }, sides: { type: "integer" } },
      required: ["count", "sides"],
    },
  },
  {
    name: "check_time",
    description: "Get the current time.",
    inputSchema: { type: "object", properties: {} },
  },
];

describe("cache placement (offline)", () => {
  it("plain chat: system + last-message breakpoints, exactly 2 markers", async () => {
    const server = await startFakeAnthropic([
      {
        blocks: [{ type: "text", text: "Hello back." }],
        stopReason: "end_turn",
      } satisfies CannedResponse,
    ]);
    try {
      const req: ChatRequest = {
        ...baseRequest([
          { role: "user", content: [{ type: "text", text: "Hi" }] },
        ]),
        baseUrl: server.baseUrl,
      };
      await drain(new AnthropicProvider().stream(req));

      expect(server.bodies.length).toBe(1);
      const body = server.bodies[0] as Record<string, unknown>;
      const paths = findCacheControlPaths(body);
      expect(paths.sort()).toEqual(["messages[0].content[0]", "system[0]"]);

      // The cache_control values are the canonical ephemeral/1h shape.
      const system = (body.system as Array<{ cache_control: unknown }>)[0];
      expect(system.cache_control).toEqual({ type: "ephemeral", ttl: "1h" });
    } finally {
      await server.close();
    }
  });

  it("with tools: system + last-tool + last-message, 3 markers", async () => {
    const server = await startFakeAnthropic([
      {
        blocks: [{ type: "text", text: "ack" }],
        stopReason: "end_turn",
      },
    ]);
    try {
      const req: ChatRequest = {
        ...baseRequest([
          { role: "user", content: [{ type: "text", text: "Hi" }] },
        ]),
        tools: SAMPLE_TOOLS,
        baseUrl: server.baseUrl,
      };
      await drain(new AnthropicProvider().stream(req));

      const body = server.bodies[0] as Record<string, unknown>;
      const paths = findCacheControlPaths(body);
      // The breakpoint on the final tool definition is keyed by tool
      // count — SAMPLE_TOOLS has 2, so the cache_control lands on
      // tools[1].
      expect(paths.sort()).toEqual([
        "messages[0].content[0]",
        "system[0]",
        "tools[1]",
      ]);
    } finally {
      await server.close();
    }
  });

  it("logs request-side cache forensics when enabled", async () => {
    const server = await startFakeAnthropic([
      {
        blocks: [{ type: "text", text: "Hello back." }],
        stopReason: "end_turn",
      } satisfies CannedResponse,
    ]);
    try {
      const cacheDir = mkdtempSync(path.join(tmpdir(), "shore-cache-forensics-test-"));
      const req: ChatRequest = {
        ...baseRequest([
          { role: "user", content: [{ type: "text", text: "Hi" }] },
        ]),
        baseUrl: server.baseUrl,
        cacheForensics: CacheForensics.open(cacheDir),
        forensicCharacter: "casey",
        forensicRid: "rid_1",
      };
      await drain(new AnthropicProvider().stream(req));

      const lines = fs
        .readFileSync(path.join(cacheDir, "cache_forensics.jsonl"), "utf8")
        .trim()
        .split("\n");
      expect(lines).toHaveLength(1);
      const entry = JSON.parse(lines[0]!) as Record<string, unknown>;
      expect(entry["type"]).toBe("request");
      expect(entry["character"]).toBe("casey");
      expect(entry["rid"]).toBe("rid_1");
      expect(entry["msg_count"]).toBe(1);
      expect(entry["msg_breakpoints"]).toEqual([0]);
      expect(entry["sys_breakpoints"]).toEqual([0]);
      expect(entry["sys_blocks"]).toBe(1);
      expect(entry["cache_enabled"]).toBe(true);
      expect(entry["prefix_hash"]).toMatch(/^[0-9a-f]{16}$/);
    } finally {
      await server.close();
    }
  });

  it("tool loop iter 2: 4 breakpoints incl. stable assistant turn", async () => {
    // Iteration 1's response is irrelevant for this test — we want to
    // assert on the iteration 2 REQUEST body, which is the one carrying
    // the previous assistant turn (the "stable" one).
    const server = await startFakeAnthropic([
      // Iter 1 server response (unused — we don't drive the tool loop
      // here; we just need it because each request consumes one entry).
      {
        blocks: [
          {
            type: "tool_use",
            id: "tu_1",
            name: "roll_dice",
            input: { count: 1, sides: 20 },
          },
        ],
        stopReason: "tool_use",
      },
      // Iter 2 server response.
      {
        blocks: [{ type: "text", text: "rolled a 17, solid." }],
        stopReason: "end_turn",
      },
    ]);
    try {
      const initial: TurnMessage[] = [
        { role: "user", content: [{ type: "text", text: "Roll 1d20" }] },
      ];
      // Iter 1 (we don't care about its body for this test, just the
      // response we get back).
      const provider = new AnthropicProvider();
      const iter1Req: ChatRequest = {
        ...baseRequest(initial),
        tools: SAMPLE_TOOLS,
        baseUrl: server.baseUrl,
      };
      const iter1Done = await drain(provider.stream(iter1Req));
      if (iter1Done.kind !== "done") throw new Error("expected done");

      // Build iter 2 manually: echo the assistant turn back, append a
      // synthetic tool_result.
      const turns: TurnMessage[] = [
        ...initial,
        { role: "assistant", content: iter1Done.content },
        {
          role: "user",
          content: [
            {
              type: "tool_result",
              tool_use_id: "tu_1",
              content: "17",
              is_error: false,
            },
          ],
        },
      ];
      const iter2Req: ChatRequest = {
        ...baseRequest(turns),
        tools: SAMPLE_TOOLS,
        baseUrl: server.baseUrl,
      };
      await drain(provider.stream(iter2Req));

      expect(server.bodies.length).toBe(2);
      const iter2 = server.bodies[1] as Record<string, unknown>;
      const paths = findCacheControlPaths(iter2);

      // 4 markers expected: system, last tool, last block of stable
      // assistant turn (messages[1]), last block of pending user turn
      // (messages[2]).
      expect(paths.sort()).toEqual([
        "messages[1].content[0]",
        "messages[2].content[0]",
        "system[0]",
        "tools[1]",
      ]);
    } finally {
      await server.close();
    }
  });

  it("redacted_thinking round-trips verbatim into the next request", async () => {
    // Server returns an assistant turn that includes a redacted_thinking
    // block carrying the OpenRouter-prefixed `openrouter.reasoning:` data
    // — the exact shape that fooled the Rust adapter into filtering.
    const REDACTED_DATA =
      "openrouter.reasoning:eyJzaWduYXR1cmUiOiJiYXNlNjQiLCJpZCI6Im9wZW5yb3V0ZXItdHJhY2UifQ==";
    const server = await startFakeAnthropic([
      {
        blocks: [
          { type: "thinking", thinking: "let me think", signature: "sig123" },
          { type: "redacted_thinking", data: REDACTED_DATA },
          {
            type: "tool_use",
            id: "tu_1",
            name: "roll_dice",
            input: { count: 1, sides: 20 },
          },
        ],
        stopReason: "tool_use",
      },
      {
        blocks: [{ type: "text", text: "done" }],
        stopReason: "end_turn",
      },
    ]);
    try {
      const provider = new AnthropicProvider();
      const initial: TurnMessage[] = [
        { role: "user", content: [{ type: "text", text: "Roll" }] },
      ];
      const iter1: ChatRequest = {
        ...baseRequest(initial),
        tools: SAMPLE_TOOLS,
        thinking: { enabled: true, effort: "low" },
        baseUrl: server.baseUrl,
      };
      const iter1Done = await drain(provider.stream(iter1));
      if (iter1Done.kind !== "done") throw new Error("expected done");

      // The done event must surface the redacted_thinking block
      // unchanged — this is the part the Rust impl was filtering.
      const redacted = iter1Done.content.find(
        (b) => b.type === "redacted_thinking",
      );
      expect(redacted).toBeDefined();
      if (redacted?.type !== "redacted_thinking") throw new Error("type");
      expect(redacted.data).toBe(REDACTED_DATA);

      // Echo the assistant turn back verbatim and add a tool_result.
      const turns: TurnMessage[] = [
        ...initial,
        { role: "assistant", content: iter1Done.content },
        {
          role: "user",
          content: [
            {
              type: "tool_result",
              tool_use_id: "tu_1",
              content: "17",
              is_error: false,
            },
          ],
        },
      ];
      const iter2: ChatRequest = {
        ...baseRequest(turns),
        tools: SAMPLE_TOOLS,
        thinking: { enabled: true, effort: "low" },
        baseUrl: server.baseUrl,
      };
      await drain(provider.stream(iter2));

      // Iter 2 request body must contain the redacted_thinking with the
      // exact same data bytes, in the exact same position in the
      // assistant block array (thinking → redacted → tool_use).
      const iter2Body = server.bodies[1] as {
        messages: Array<{
          role: string;
          content: Array<{ type: string; data?: string }>;
        }>;
      };
      const asstTurn = iter2Body.messages[1]!;
      expect(asstTurn.role).toBe("assistant");
      expect(asstTurn.content.map((b) => b.type)).toEqual([
        "thinking",
        "redacted_thinking",
        "tool_use",
      ]);
      expect(asstTurn.content[1]!.data).toBe(REDACTED_DATA);
    } finally {
      await server.close();
    }
  });

  it("inline-system wrap is byte-identical across separate calls", async () => {
    const server = await startFakeAnthropic([
      { blocks: [{ type: "text", text: "ok" }], stopReason: "end_turn" },
      { blocks: [{ type: "text", text: "ok" }], stopReason: "end_turn" },
    ]);
    try {
      const provider = new AnthropicProvider();
      const turns: TurnMessage[] = [
        { role: "user", content: [{ type: "text", text: "u1" }] },
        { role: "assistant", content: [{ type: "text", text: "a1" }] },
        {
          role: "system",
          content: [{ type: "text", text: "Heartbeat: be concise." }],
        },
        { role: "user", content: [{ type: "text", text: "u2" }] },
      ];
      const req: ChatRequest = { ...baseRequest(turns), baseUrl: server.baseUrl };
      await drain(provider.stream(req));
      await drain(provider.stream(req));

      expect(server.bodies.length).toBe(2);
      expect(JSON.stringify(server.bodies[0])).toBe(
        JSON.stringify(server.bodies[1]),
      );

      // And the wrap text is the canonical sentinel — pin it.
      const body0 = server.bodies[0] as {
        messages: Array<{ content: Array<{ type: string; text?: string }> }>;
      };
      const allText = body0.messages
        .flatMap((m) => m.content)
        .filter((b) => b.type === "text")
        .map((b) => b.text ?? "")
        .join("\n");
      expect(allText).toContain(
        "<system_instruction>Heartbeat: be concise.</system_instruction>",
      );
    } finally {
      await server.close();
    }
  });

  it("inline-system wrap leaves cache_control breakpoints intact", async () => {
    const server = await startFakeAnthropic([
      { blocks: [{ type: "text", text: "ok" }], stopReason: "end_turn" },
    ]);
    try {
      const turns: TurnMessage[] = [
        { role: "user", content: [{ type: "text", text: "u1" }] },
        { role: "assistant", content: [{ type: "text", text: "a1" }] },
        {
          role: "system",
          content: [{ type: "text", text: "Heartbeat: be concise." }],
        },
        { role: "user", content: [{ type: "text", text: "u2" }] },
      ];
      const req: ChatRequest = {
        ...baseRequest(turns),
        tools: SAMPLE_TOOLS,
        baseUrl: server.baseUrl,
      };
      await drain(new AnthropicProvider().stream(req));

      const body = server.bodies[0] as Record<string, unknown>;
      const paths = findCacheControlPaths(body);

      // Input was [user1, asst1, system, user2]. Conversion sees the
      // system after asst1 (NOT after a user), so it emits a new
      // wrapped user turn rather than merging. Post-convert:
      // [user1, asst1, wrapped-user, user2] — four turns.
      // stable_idx walks back from last-1 looking for an assistant; it
      // finds asst1 at messages[1]. last_idx = user2 at messages[3].
      // The wrapped-user at messages[2] sits between them, captured by
      // the messages[3] breakpoint's prefix coverage; it does NOT get
      // its own breakpoint.
      const msgs = body.messages as Array<{
        role: string;
        content: Array<unknown>;
      }>;
      expect(msgs.length).toBe(4);
      expect(msgs.map((m) => m.role)).toEqual([
        "user",
        "assistant",
        "user",
        "user",
      ]);
      expect(paths.sort()).toEqual([
        "messages[1].content[0]",
        "messages[3].content[0]",
        "system[0]",
        "tools[1]",
      ]);
    } finally {
      await server.close();
    }
  });

  it("plain chat with cacheTtl='' disables all cache_control breakpoints", async () => {
    const server = await startFakeAnthropic([
      { blocks: [{ type: "text", text: "ack" }], stopReason: "end_turn" },
    ]);
    try {
      const req: ChatRequest = {
        ...baseRequest([
          { role: "user", content: [{ type: "text", text: "Hi" }] },
        ]),
        cacheTtl: "",
        baseUrl: server.baseUrl,
      };
      await drain(new AnthropicProvider().stream(req));

      const body = server.bodies[0] as Record<string, unknown>;
      expect(findCacheControlPaths(body)).toEqual([]);
    } finally {
      await server.close();
    }
  });

  it("block order survives the SDK round-trip (thinking → tool_use → tool_result → text)", async () => {
    // Verifies the canonical Anthropic block order is preserved on the
    // next-turn request — the property that fell over in the Rust
    // adapter when redacted_thinking blocks were dropped.
    const server = await startFakeAnthropic([
      {
        blocks: [
          { type: "thinking", thinking: "plan", signature: "sig" },
          {
            type: "tool_use",
            id: "tu_a",
            name: "roll_dice",
            input: { count: 1, sides: 6 },
          },
        ],
        stopReason: "tool_use",
      },
      {
        blocks: [{ type: "text", text: "done" }],
        stopReason: "end_turn",
      },
    ]);
    try {
      const provider = new AnthropicProvider();
      const initial: TurnMessage[] = [
        { role: "user", content: [{ type: "text", text: "Roll" }] },
      ];
      const iter1: ChatRequest = {
        ...baseRequest(initial),
        tools: SAMPLE_TOOLS,
        thinking: { enabled: true, effort: "low" },
        baseUrl: server.baseUrl,
      };
      const done = await drain(provider.stream(iter1));
      if (done.kind !== "done") throw new Error("expected done");

      // Echo back and call again.
      const iter2Turns: TurnMessage[] = [
        ...initial,
        { role: "assistant", content: done.content },
        {
          role: "user",
          content: [
            {
              type: "tool_result",
              tool_use_id: "tu_a",
              content: "3",
              is_error: false,
            },
          ],
        },
      ];
      const iter2: ChatRequest = {
        ...baseRequest(iter2Turns),
        tools: SAMPLE_TOOLS,
        thinking: { enabled: true, effort: "low" },
        baseUrl: server.baseUrl,
      };
      await drain(provider.stream(iter2));

      const iter2Body = server.bodies[1] as {
        messages: Array<{ content: Array<{ type: string }> }>;
      };
      // Iter 2 must replay the assistant content in order:
      // [thinking, tool_use], then a user turn with [tool_result].
      expect(iter2Body.messages[1]!.content.map((b) => b.type)).toEqual([
        "thinking",
        "tool_use",
      ]);
      expect(iter2Body.messages[2]!.content.map((b) => b.type)).toEqual([
        "tool_result",
      ]);
    } finally {
      await server.close();
    }
  });

  // Regression pin for the `_label` wire leak. Rust strips `_label` from
  // system blocks before sending (`backend/llm/src/providers/anthropic.rs:301-306`)
  // because it's internal metadata. The TS port at one point copied
  // `_label` onto the wire, causing cross-daemon cache-key fragmentation
  // — TS-written cache entries didn't match Rust-written entries for the
  // same conversation. Every existing T3 parity check missed this
  // because their fixtures had `cache_ttl = ""`, which short-circuits
  // the breakpoint code path entirely. This test exercises the path
  // with caching on and pins the wire shape.
  it("_label never reaches the wire on system blocks", async () => {
    const server = await startFakeAnthropic([
      {
        blocks: [{ type: "text", text: "ack" }],
        stopReason: "end_turn",
      } satisfies CannedResponse,
    ]);
    try {
      const req: ChatRequest = {
        ...baseRequest([
          { role: "user", content: [{ type: "text", text: "Hi" }] },
        ]),
        system: [
          { type: "text", text: "You are Casey.", _label: "system" },
          { type: "text", text: "# TOOLS\n…", _label: "tools_guidance" },
          { type: "text", text: "<casey>…</casey>", _label: "character" },
        ],
        baseUrl: server.baseUrl,
      };
      await drain(new AnthropicProvider().stream(req));

      const body = server.bodies[0] as Record<string, unknown>;
      const system = body.system as Array<Record<string, unknown>>;
      expect(system).toHaveLength(3);
      for (const block of system) {
        expect(block).not.toHaveProperty("_label");
      }
    } finally {
      await server.close();
    }
  });
});
