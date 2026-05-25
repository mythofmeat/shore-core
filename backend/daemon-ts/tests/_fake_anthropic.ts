/**
 * Fake Anthropic server for offline cache-placement tests.
 *
 * Spins up a Bun.serve() instance on a random port that:
 *   - Captures every POST /v1/messages request body verbatim.
 *   - Returns a canned SSE stream for each request, drained from a
 *     pre-staged queue so each tool-loop iteration can get a different
 *     response shape (e.g. iter 1 = tool_use, iter 2 = text).
 *
 * What the tests assert is that the bytes WE send to /v1/messages have
 * the cache_control breakpoints + block ordering + reasoning round-trip
 * the adapter is supposed to produce. The Anthropic cache server's own
 * behavior is deterministic and not under test — the property we own is
 * the request body.
 */

export type CannedBlock =
  | { type: "text"; text: string }
  | { type: "thinking"; thinking: string; signature?: string }
  | { type: "redacted_thinking"; data: string }
  | { type: "tool_use"; id: string; name: string; input: unknown };

export interface CannedResponse {
  blocks: CannedBlock[];
  stopReason: "end_turn" | "tool_use" | "max_tokens" | "stop_sequence";
  usage?: {
    input_tokens?: number;
    output_tokens?: number;
    cache_read_input_tokens?: number;
    cache_creation_input_tokens?: number;
  };
}

export interface FakeServer {
  baseUrl: string;
  /** Captured request bodies, in order of arrival. */
  bodies: unknown[];
  close(): Promise<void>;
}

/**
 * Build the single-JSON-message wire body for a non-streaming canned
 * response. Mirrors Anthropic's documented Messages non-streaming
 * response (`POST /v1/messages` with no `stream: true`).
 */
function buildJsonMessage(resp: CannedResponse): unknown {
  const content = resp.blocks.map((b) => {
    switch (b.type) {
      case "text":
        return { type: "text", text: b.text };
      case "thinking":
        return {
          type: "thinking",
          thinking: b.thinking,
          ...(b.signature !== undefined ? { signature: b.signature } : {}),
        };
      case "redacted_thinking":
        return { type: "redacted_thinking", data: b.data };
      case "tool_use":
        return { type: "tool_use", id: b.id, name: b.name, input: b.input ?? {} };
    }
  });
  return {
    id: "msg_fake",
    type: "message",
    role: "assistant",
    model: "fake-model",
    content,
    stop_reason: resp.stopReason,
    stop_sequence: null,
    usage: {
      input_tokens: resp.usage?.input_tokens ?? 10,
      output_tokens: resp.usage?.output_tokens ?? 5,
      cache_read_input_tokens: resp.usage?.cache_read_input_tokens ?? 0,
      cache_creation_input_tokens: resp.usage?.cache_creation_input_tokens ?? 0,
    },
  };
}

/**
 * Build the SSE wire bytes for one canned response. Mirrors Anthropic's
 * documented Messages streaming format closely enough that
 * `@anthropic-ai/sdk` parses it without complaint.
 */
function buildSse(resp: CannedResponse): string {
  const lines: string[] = [];
  const emit = (eventName: string, payload: unknown): void => {
    lines.push(`event: ${eventName}`);
    lines.push(`data: ${JSON.stringify(payload)}`);
    lines.push("");
  };

  const usage = {
    input_tokens: resp.usage?.input_tokens ?? 10,
    output_tokens: resp.usage?.output_tokens ?? 5,
    cache_read_input_tokens: resp.usage?.cache_read_input_tokens ?? 0,
    cache_creation_input_tokens: resp.usage?.cache_creation_input_tokens ?? 0,
  };

  emit("message_start", {
    type: "message_start",
    message: {
      id: "msg_fake",
      type: "message",
      role: "assistant",
      content: [],
      model: "fake-model",
      stop_reason: null,
      stop_sequence: null,
      usage,
    },
  });

  resp.blocks.forEach((b, idx) => {
    switch (b.type) {
      case "text": {
        emit("content_block_start", {
          type: "content_block_start",
          index: idx,
          content_block: { type: "text", text: "" },
        });
        emit("content_block_delta", {
          type: "content_block_delta",
          index: idx,
          delta: { type: "text_delta", text: b.text },
        });
        emit("content_block_stop", { type: "content_block_stop", index: idx });
        break;
      }
      case "thinking": {
        emit("content_block_start", {
          type: "content_block_start",
          index: idx,
          content_block: {
            type: "thinking",
            thinking: "",
            signature: "",
          },
        });
        emit("content_block_delta", {
          type: "content_block_delta",
          index: idx,
          delta: { type: "thinking_delta", thinking: b.thinking },
        });
        if (b.signature) {
          emit("content_block_delta", {
            type: "content_block_delta",
            index: idx,
            delta: { type: "signature_delta", signature: b.signature },
          });
        }
        emit("content_block_stop", { type: "content_block_stop", index: idx });
        break;
      }
      case "redacted_thinking": {
        emit("content_block_start", {
          type: "content_block_start",
          index: idx,
          content_block: { type: "redacted_thinking", data: b.data },
        });
        emit("content_block_stop", { type: "content_block_stop", index: idx });
        break;
      }
      case "tool_use": {
        emit("content_block_start", {
          type: "content_block_start",
          index: idx,
          content_block: {
            type: "tool_use",
            id: b.id,
            name: b.name,
            input: {},
          },
        });
        emit("content_block_delta", {
          type: "content_block_delta",
          index: idx,
          delta: {
            type: "input_json_delta",
            partial_json: JSON.stringify(b.input ?? {}),
          },
        });
        emit("content_block_stop", { type: "content_block_stop", index: idx });
        break;
      }
    }
  });

  emit("message_delta", {
    type: "message_delta",
    delta: { stop_reason: resp.stopReason, stop_sequence: null },
    usage: { output_tokens: usage.output_tokens },
  });
  emit("message_stop", { type: "message_stop" });

  return lines.join("\n") + "\n";
}

/**
 * Start the fake server with a pre-staged queue of canned responses.
 * Each incoming request consumes the next response from the queue;
 * draining past the end of the queue throws (a test should never make
 * more requests than it staged responses for).
 */
export async function startFakeAnthropic(
  responses: CannedResponse[],
): Promise<FakeServer> {
  const bodies: unknown[] = [];
  const queue = [...responses];

  const server = Bun.serve({
    port: 0,
    fetch: async (req) => {
      const url = new URL(req.url);
      if (!url.pathname.endsWith("/v1/messages")) {
        return new Response("not found", { status: 404 });
      }
      const body = (await req.json()) as Record<string, unknown> | null;
      bodies.push(body);
      const next = queue.shift();
      if (!next) {
        return new Response(
          `event: error\ndata: ${JSON.stringify({ error: "fake-server: response queue empty" })}\n\n`,
          { status: 500, headers: { "Content-Type": "text/event-stream" } },
        );
      }
      // Branch on the request's `stream` field — non-streaming requests
      // get a single JSON Message, streaming requests get the SSE form.
      // The SDK uses one of two methods (`messages.create` vs
      // `messages.stream`) which is the only difference between the two
      // wire shapes.
      const streaming = body !== null && body["stream"] === true;
      if (!streaming) {
        return new Response(JSON.stringify(buildJsonMessage(next)), {
          headers: { "Content-Type": "application/json" },
        });
      }
      return new Response(buildSse(next), {
        headers: { "Content-Type": "text/event-stream" },
      });
    },
  });

  return {
    baseUrl: `http://${server.hostname}:${server.port}/v1`,
    bodies,
    async close() {
      await server.stop();
    },
  };
}

// ── assertion helpers ────────────────────────────────────────────────────

/**
 * Collect the dotted paths inside a request body where `cache_control` is
 * present. Lets tests assert on the exact 4-breakpoint slot set without
 * caring about every other field.
 */
export function findCacheControlPaths(body: unknown, base = ""): string[] {
  const out: string[] = [];
  if (body === null || typeof body !== "object") return out;
  if (Array.isArray(body)) {
    body.forEach((v, i) => {
      out.push(...findCacheControlPaths(v, `${base}[${i}]`));
    });
    return out;
  }
  for (const [k, v] of Object.entries(body as Record<string, unknown>)) {
    if (k === "cache_control" && v !== undefined) {
      out.push(base);
    } else {
      out.push(...findCacheControlPaths(v, base ? `${base}.${k}` : k));
    }
  }
  return out;
}
