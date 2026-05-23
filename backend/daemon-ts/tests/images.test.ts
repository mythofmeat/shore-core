/**
 * Image resolution + adapter wire-shape tests.
 *
 * Verifies that `resolveImage` reads files + detects MIME, and that
 * both adapters wrap resolved images into their respective wire shapes
 * (Anthropic: image block with base64 source; OpenAI: image_url part
 * in a multipart user message).
 */
import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import path from "node:path";

import { AnthropicProvider } from "../src/llm/providers/anthropic.ts";
import { OpenAIProvider } from "../src/llm/providers/openai.ts";
import { resolveImage } from "../src/llm/images.ts";
import type { ChatEvent, ChatRequest, TurnMessage } from "../src/llm/types.ts";
import { startFakeAnthropic } from "./_fake_anthropic.ts";

// 1×1 transparent PNG (the smallest valid PNG we can ship inline).
const TINY_PNG_B64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

function tempPng(): string {
  const p = path.join(
    fs.mkdtempSync(path.join(process.env["TMPDIR"] ?? "/tmp", "shore-img-")),
    "tiny.png",
  );
  fs.writeFileSync(p, Buffer.from(TINY_PNG_B64, "base64"));
  return p;
}

async function drain(stream: AsyncIterable<ChatEvent>): Promise<ChatEvent> {
  let done: ChatEvent | undefined;
  for await (const ev of stream) {
    if (ev.kind === "done") done = ev;
  }
  if (!done) throw new Error("no 'done' event");
  return done;
}

describe("resolveImage", () => {
  it("reads a PNG from disk and detects mime", () => {
    const p = tempPng();
    const got = resolveImage({ path: p });
    expect(got).toBeDefined();
    expect(got!.mediaType).toBe("image/png");
    expect(got!.base64).toBe(TINY_PNG_B64);
  });

  it("uses inline data when provided (skips filesystem read)", () => {
    const got = resolveImage({ path: "/does/not/exist.png", data: TINY_PNG_B64 });
    expect(got).toBeDefined();
    expect(got!.mediaType).toBe("image/png");
    expect(got!.base64).toBe(TINY_PNG_B64);
  });

  it("returns undefined for unknown extensions", () => {
    expect(resolveImage({ path: "/tmp/file.xyz" })).toBeUndefined();
  });

  it("returns undefined for missing files", () => {
    expect(resolveImage({ path: "/tmp/definitely-not-here.png" })).toBeUndefined();
  });

  it("skips images larger than the cap", () => {
    const p = tempPng();
    expect(resolveImage({ path: p }, 10)).toBeUndefined();
  });
});

describe("anthropic adapter: image wire shape", () => {
  it("prepends image block as base64 source before text in user content", async () => {
    const server = await startFakeAnthropic([
      { blocks: [{ type: "text", text: "ack" }], stopReason: "end_turn" },
    ]);
    try {
      const turns: TurnMessage[] = [
        {
          role: "user",
          content: [{ type: "text", text: "what do you see?" }],
          images: [{ path: "/tmp/x.png", data: TINY_PNG_B64 }],
        },
      ];
      const req: ChatRequest = {
        system: "S",
        messages: turns,
        tools: [],
        thinking: { enabled: false },
        cacheTtl: "",
        modelId: "fake",
        apiKey: "fake",
        maxTokens: 256,
        baseUrl: server.baseUrl,
      };
      await drain(new AnthropicProvider().stream(req));

      const body = server.bodies[0] as {
        messages: Array<{
          role: string;
          content: Array<{ type: string; source?: { type: string; media_type: string; data: string }; text?: string }>;
        }>;
      };
      const userContent = body.messages[0]!.content;
      expect(userContent.length).toBe(2);
      expect(userContent[0]!.type).toBe("image");
      expect(userContent[0]!.source).toEqual({
        type: "base64",
        media_type: "image/png",
        data: TINY_PNG_B64,
      });
      expect(userContent[1]!.type).toBe("text");
      expect(userContent[1]!.text).toBe("what do you see?");
    } finally {
      await server.close();
    }
  });
});

describe("openai adapter: image wire shape", () => {
  it("emits a multipart user message with image_url part before text", async () => {
    // Use the fake Anthropic server as a stand-in HTTP echo — the OpenAI
    // SDK speaks a different protocol, so we need a tiny OpenAI-shaped
    // server.
    let captured: unknown;
    const server = Bun.serve({
      port: 0,
      fetch: async (req) => {
        const body = await req.json();
        captured = body;
        return new Response(
          "data: " +
            JSON.stringify({
              id: "x",
              choices: [
                { delta: { content: "ack" }, finish_reason: null, index: 0 },
              ],
              usage: null,
            }) +
            "\n\ndata: " +
            JSON.stringify({
              id: "x",
              choices: [{ delta: {}, finish_reason: "stop", index: 0 }],
              usage: {
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
              },
            }) +
            "\n\ndata: [DONE]\n\n",
          { headers: { "Content-Type": "text/event-stream" } },
        );
      },
    });
    try {
      const turns: TurnMessage[] = [
        {
          role: "user",
          content: [{ type: "text", text: "describe this" }],
          images: [{ path: "/tmp/x.png", data: TINY_PNG_B64 }],
        },
      ];
      const req: ChatRequest = {
        system: "S",
        messages: turns,
        tools: [],
        thinking: { enabled: false },
        cacheTtl: "",
        modelId: "fake",
        apiKey: "fake",
        maxTokens: 256,
        baseUrl: `http://${server.hostname}:${server.port}/v1`,
      };
      await drain(new OpenAIProvider().stream(req));

      const body = captured as {
        messages: Array<{
          role: string;
          content: string | Array<{ type: string; text?: string; image_url?: { url: string } }>;
        }>;
      };
      // [0] = system, [1] = user with multipart content.
      const userMsg = body.messages.find((m) => m.role === "user")!;
      expect(Array.isArray(userMsg.content)).toBe(true);
      const parts = userMsg.content as Array<{ type: string; text?: string; image_url?: { url: string } }>;
      expect(parts.length).toBe(2);
      expect(parts[0]!.type).toBe("image_url");
      expect(parts[0]!.image_url!.url).toBe(
        `data:image/png;base64,${TINY_PNG_B64}`,
      );
      expect(parts[1]!.type).toBe("text");
      expect(parts[1]!.text).toBe("describe this");
    } finally {
      await server.stop();
    }
  });
});
