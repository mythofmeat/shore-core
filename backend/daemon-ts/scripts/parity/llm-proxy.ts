import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

export type AnthropicCannedBlock =
  | { type: "text"; text: string }
  | { type: "thinking"; thinking: string; signature?: string }
  | { type: "redacted_thinking"; data: string }
  | { type: "tool_use"; id: string; name: string; input: unknown };

export interface AnthropicCannedResponse {
  provider: "anthropic";
  model: string;
  blocks: AnthropicCannedBlock[];
  stop_reason: "end_turn" | "tool_use" | "max_tokens" | "stop_sequence";
  usage?: {
    input_tokens?: number;
    output_tokens?: number;
    cache_read_input_tokens?: number;
    cache_creation_input_tokens?: number;
  };
}

export interface OpenAICompatibleCannedResponse {
  provider: "openai_compatible";
  model: string;
  text: string;
  finish_reason?: "stop" | "length" | "tool_calls" | "content_filter";
  usage?: {
    prompt_tokens?: number;
    completion_tokens?: number;
    cached_tokens?: number;
    cache_write_tokens?: number;
  };
}

export type CannedLlmResponse = AnthropicCannedResponse | OpenAICompatibleCannedResponse;

export interface CapturedLlmRequest {
  key: string;
  method: string;
  path: string;
  body: unknown;
  canonical: string;
}

export interface ParityLlmProxy {
  baseUrl: string;
  requests: CapturedLlmRequest[];
  stop(): Promise<void>;
}

export interface StartParityLlmProxyOptions {
  response: CannedLlmResponse | CannedLlmResponse[];
  fixtureDir?: string;
  recordMissing?: boolean;
}

interface StoredFixture {
  status: number;
  headers: Record<string, string>;
  body: string;
}

export function loadCannedResponse(path: string): CannedLlmResponse {
  const responses = loadCannedResponses(path);
  if (responses.length !== 1) {
    throw new Error(`${path} contains ${responses.length} responses; expected exactly one`);
  }
  return responses[0]!;
}

export function loadCannedResponses(path: string): CannedLlmResponse[] {
  const parsed = JSON.parse(readFileSync(path, "utf8")) as unknown;
  if (Array.isArray(parsed)) return parsed as CannedLlmResponse[];
  if (isObject(parsed) && Array.isArray(parsed["responses"])) {
    return parsed["responses"] as CannedLlmResponse[];
  }
  return [parsed as CannedLlmResponse];
}

export function startParityLlmProxy(opts: StartParityLlmProxyOptions): ParityLlmProxy {
  const requests: CapturedLlmRequest[] = [];
  const responses = Array.isArray(opts.response) ? opts.response : [opts.response];
  let responseIndex = 0;

  if (opts.fixtureDir !== undefined) mkdirSync(opts.fixtureDir, { recursive: true });

  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch: async (req) => {
      const url = new URL(req.url);
      const response = responses[Math.min(responseIndex, responses.length - 1)];
      if (response === undefined || req.method !== "POST" || !matchesResponseRoute(response, url)) {
        return new Response("not found", { status: 404 });
      }
      responseIndex++;

      let body: unknown;
      try {
        body = await req.json();
      } catch {
        body = null;
      }

      const canonical = canonicalRequest(req.method, url, body);
      const key = sha256(canonical);
      requests.push({
        key,
        method: req.method,
        path: url.pathname + url.search,
        body,
        canonical,
      });

      const fixturePath = opts.fixtureDir === undefined ? undefined : join(opts.fixtureDir, `${key}.json`);
      const fixture = fixturePath === undefined ? undefined : readStoredFixture(fixturePath);
      if (fixture !== undefined) return responseFromFixture(fixture);

      // Compaction (and any other LedgerClient.generate path) calls the
      // provider without `stream: true`. Anthropic returns a single JSON
      // message object then; OpenAI-compatible returns one ChatCompletion
      // object. Branch on the request body to keep one canned response
      // usable for both shapes.
      const streaming = isStreamingRequest(body);
      const defaultFixture: StoredFixture = streaming
        ? {
            status: 200,
            headers: { "content-type": "text/event-stream" },
            body: buildSse(response),
          }
        : {
            status: 200,
            headers: { "content-type": "application/json" },
            body: buildJson(response),
          };
      if (opts.recordMissing === true && fixturePath !== undefined) {
        writeFileSync(fixturePath, JSON.stringify(defaultFixture, null, 2) + "\n");
      }

      return responseFromFixture(defaultFixture);
    },
  });

  return {
    baseUrl: `http://${server.hostname}:${server.port}/v1`,
    requests,
    async stop() {
      await server.stop();
    },
  };
}

function matchesResponseRoute(response: CannedLlmResponse, url: URL): boolean {
  switch (response.provider) {
    case "anthropic":
      return url.pathname.endsWith("/v1/messages");
    case "openai_compatible":
      return url.pathname.endsWith("/chat/completions");
  }
}

function buildSse(resp: CannedLlmResponse): string {
  switch (resp.provider) {
    case "anthropic":
      return buildAnthropicSse(resp);
    case "openai_compatible":
      return buildOpenAICompatibleSse(resp);
  }
}

function buildJson(resp: CannedLlmResponse): string {
  switch (resp.provider) {
    case "anthropic":
      return buildAnthropicJson(resp);
    case "openai_compatible":
      return buildOpenAICompatibleJson(resp);
  }
}

function isStreamingRequest(body: unknown): boolean {
  return isObject(body) && body["stream"] === true;
}

export function canonicalizeJson(value: unknown): string {
  return JSON.stringify(sortJson(value));
}

function canonicalRequest(method: string, url: URL, body: unknown): string {
  return [
    method.toUpperCase(),
    url.pathname + url.search,
    canonicalizeJson(body),
  ].join("\n");
}

function readStoredFixture(path: string): StoredFixture | undefined {
  try {
    return JSON.parse(readFileSync(path, "utf8")) as StoredFixture;
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw e;
  }
}

function responseFromFixture(fixture: StoredFixture): Response {
  return new Response(fixture.body, {
    status: fixture.status,
    headers: fixture.headers,
  });
}

function buildAnthropicSse(resp: AnthropicCannedResponse): string {
  const lines: string[] = [];
  const emit = (event: string, payload: unknown): void => {
    lines.push(`event: ${event}`);
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
      id: "msg_parity",
      type: "message",
      role: "assistant",
      content: [],
      model: resp.model,
      stop_reason: null,
      stop_sequence: null,
      usage,
    },
  });

  resp.blocks.forEach((block, index) => {
    switch (block.type) {
      case "text":
        emit("content_block_start", {
          type: "content_block_start",
          index,
          content_block: { type: "text", text: "" },
        });
        emit("content_block_delta", {
          type: "content_block_delta",
          index,
          delta: { type: "text_delta", text: block.text },
        });
        emit("content_block_stop", { type: "content_block_stop", index });
        break;
      case "thinking":
        emit("content_block_start", {
          type: "content_block_start",
          index,
          content_block: { type: "thinking", thinking: "", signature: "" },
        });
        emit("content_block_delta", {
          type: "content_block_delta",
          index,
          delta: { type: "thinking_delta", thinking: block.thinking },
        });
        if (block.signature !== undefined) {
          emit("content_block_delta", {
            type: "content_block_delta",
            index,
            delta: { type: "signature_delta", signature: block.signature },
          });
        }
        emit("content_block_stop", { type: "content_block_stop", index });
        break;
      case "redacted_thinking":
        emit("content_block_start", {
          type: "content_block_start",
          index,
          content_block: { type: "redacted_thinking", data: block.data },
        });
        emit("content_block_stop", { type: "content_block_stop", index });
        break;
      case "tool_use":
        emit("content_block_start", {
          type: "content_block_start",
          index,
          content_block: {
            type: "tool_use",
            id: block.id,
            name: block.name,
            input: {},
          },
        });
        emit("content_block_delta", {
          type: "content_block_delta",
          index,
          delta: {
            type: "input_json_delta",
            partial_json: JSON.stringify(block.input ?? {}),
          },
        });
        emit("content_block_stop", { type: "content_block_stop", index });
        break;
    }
  });

  emit("message_delta", {
    type: "message_delta",
    delta: { stop_reason: resp.stop_reason, stop_sequence: null },
    usage: { output_tokens: usage.output_tokens },
  });
  emit("message_stop", { type: "message_stop" });

  return lines.join("\n") + "\n";
}

function buildOpenAICompatibleSse(resp: OpenAICompatibleCannedResponse): string {
  const lines: string[] = [];
  const emit = (payload: unknown): void => {
    lines.push(`data: ${JSON.stringify(payload)}`);
    lines.push("");
  };

  const id = "chatcmpl-parity";
  const created = 1_778_284_800;
  const usage = {
    prompt_tokens: resp.usage?.prompt_tokens ?? 10,
    completion_tokens: resp.usage?.completion_tokens ?? 5,
    total_tokens:
      (resp.usage?.prompt_tokens ?? 10) + (resp.usage?.completion_tokens ?? 5),
    prompt_tokens_details: {
      cached_tokens: resp.usage?.cached_tokens ?? 0,
      cache_write_tokens: resp.usage?.cache_write_tokens ?? 0,
    },
  };

  emit({
    id,
    object: "chat.completion.chunk",
    created,
    model: resp.model,
    choices: [
      {
        index: 0,
        delta: { role: "assistant", content: resp.text },
        finish_reason: null,
      },
    ],
  });
  emit({
    id,
    object: "chat.completion.chunk",
    created,
    model: resp.model,
    choices: [
      {
        index: 0,
        delta: {},
        finish_reason: resp.finish_reason ?? "stop",
      },
    ],
  });
  emit({
    id,
    object: "chat.completion.chunk",
    created,
    model: resp.model,
    choices: [],
    usage,
  });
  lines.push("data: [DONE]");
  lines.push("");

  return lines.join("\n") + "\n";
}

function buildAnthropicJson(resp: AnthropicCannedResponse): string {
  const content = resp.blocks.map((block) => {
    switch (block.type) {
      case "text":
        return { type: "text", text: block.text };
      case "thinking":
        return {
          type: "thinking",
          thinking: block.thinking,
          ...(block.signature !== undefined ? { signature: block.signature } : {}),
        };
      case "redacted_thinking":
        return { type: "redacted_thinking", data: block.data };
      case "tool_use":
        return {
          type: "tool_use",
          id: block.id,
          name: block.name,
          input: block.input ?? {},
        };
    }
  });
  return JSON.stringify({
    id: "msg_parity",
    type: "message",
    role: "assistant",
    model: resp.model,
    content,
    stop_reason: resp.stop_reason,
    stop_sequence: null,
    usage: {
      input_tokens: resp.usage?.input_tokens ?? 10,
      output_tokens: resp.usage?.output_tokens ?? 5,
      cache_read_input_tokens: resp.usage?.cache_read_input_tokens ?? 0,
      cache_creation_input_tokens: resp.usage?.cache_creation_input_tokens ?? 0,
    },
  });
}

function buildOpenAICompatibleJson(resp: OpenAICompatibleCannedResponse): string {
  return JSON.stringify({
    id: "chatcmpl-parity",
    object: "chat.completion",
    created: 1_778_284_800,
    model: resp.model,
    choices: [
      {
        index: 0,
        message: { role: "assistant", content: resp.text },
        finish_reason: resp.finish_reason ?? "stop",
      },
    ],
    usage: {
      prompt_tokens: resp.usage?.prompt_tokens ?? 10,
      completion_tokens: resp.usage?.completion_tokens ?? 5,
      total_tokens:
        (resp.usage?.prompt_tokens ?? 10) + (resp.usage?.completion_tokens ?? 5),
      prompt_tokens_details: {
        cached_tokens: resp.usage?.cached_tokens ?? 0,
        cache_write_tokens: resp.usage?.cache_write_tokens ?? 0,
      },
    },
  });
}

function sortJson(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sortJson);
  if (value === null || typeof value !== "object") return value;

  const out: Record<string, unknown> = {};
  for (const key of Object.keys(value as Record<string, unknown>).sort()) {
    out[key] = sortJson((value as Record<string, unknown>)[key]);
  }
  return out;
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function sha256(s: string): string {
  return createHash("sha256").update(s).digest("hex");
}
