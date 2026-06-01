import { chmodSync, existsSync, lstatSync, unlinkSync } from "node:fs";

import { generateImage } from "./llm/image_generate.ts";
import { GeminiProvider } from "./llm/providers/gemini.ts";
import { AnthropicProvider } from "./llm/providers/anthropic.ts";
import { OpenAIProvider } from "./llm/providers/openai.ts";
import type {
  ImageRequest,
  ImageResponse,
  SidecarProvider,
  SidecarRequest,
  StreamEvent,
} from "./llm/types.ts";

export interface SidecarDeps {
  providers?: Partial<Record<SidecarRequest["sdk"], SidecarProvider>>;
  imageGenerate?: (req: ImageRequest, signal?: AbortSignal) => Promise<ImageResponse>;
}

interface HttpishError {
  status?: number;
  statusCode?: number;
  body?: unknown;
  error?: unknown;
  response?: { status?: number; data?: unknown; body?: unknown };
  message?: string;
}

const DEFAULT_PROVIDERS: Partial<Record<SidecarRequest["sdk"], SidecarProvider>> = {
  anthropic: new AnthropicProvider(),
  gemini: new GeminiProvider(),
  openai: new OpenAIProvider(),
};

const NDJSON_HEADERS = {
  "content-type": "application/x-ndjson",
  "cache-control": "no-cache",
};

export function createSidecarHandler(deps: SidecarDeps = {}): (request: Request) => Promise<Response> {
  const providers = { ...DEFAULT_PROVIDERS, ...deps.providers };
  const imageGenerate = deps.imageGenerate ?? generateImage;

  return async (request: Request): Promise<Response> => {
    const url = new URL(request.url);

    if (request.method === "GET" && url.pathname === "/healthz") {
      return new Response("ok\n", { status: 200, headers: { "content-type": "text/plain" } });
    }

    if (request.method !== "POST") {
      return textError(404, "not found");
    }

    if (url.pathname === "/v1/stream") {
      const parsed = await readJson<SidecarRequest>(request);
      if (!parsed.ok) return parsed.response;
      const provider = providers[parsed.value.sdk];
      if (!provider) return textError(501, `unsupported sdk: ${parsed.value.sdk}`);
      return streamResponse(provider, parsed.value, request.signal);
    }

    if (url.pathname === "/v1/generate") {
      const parsed = await readJson<SidecarRequest>(request);
      if (!parsed.ok) return parsed.response;
      const provider = providers[parsed.value.sdk];
      if (!provider) return textError(501, `unsupported sdk: ${parsed.value.sdk}`);
      try {
        const result = await provider.generate(parsed.value, request.signal);
        return jsonResponse(result);
      } catch (e) {
        return errorResponse(e);
      }
    }

    if (url.pathname === "/v1/image") {
      const parsed = await readJson<ImageRequest>(request);
      if (!parsed.ok) return parsed.response;
      try {
        const result = await imageGenerate(parsed.value, request.signal);
        return jsonResponse(result);
      } catch (e) {
        return errorResponse(e);
      }
    }

    return textError(404, "not found");
  };
}

export function serveSidecar(socketPath: string): ReturnType<typeof Bun.serve> {
  if (existsSync(socketPath)) {
    const stat = lstatSync(socketPath);
    if (!stat.isSocket()) {
      throw new Error(`refusing to replace non-socket path: ${socketPath}`);
    }
    unlinkSync(socketPath);
  }
  const server = Bun.serve({
    unix: socketPath,
    fetch: createSidecarHandler(),
  });
  chmodSync(socketPath, 0o600);
  return server;
}

async function streamResponse(
  provider: SidecarProvider,
  req: SidecarRequest,
  requestSignal: AbortSignal,
): Promise<Response> {
  const abort = new AbortController();
  const abortUpstream = () => abort.abort();
  if (requestSignal.aborted) abortUpstream();
  requestSignal.addEventListener("abort", abortUpstream, { once: true });

  const iterator = provider.stream(req, abort.signal)[Symbol.asyncIterator]();
  let first: IteratorResult<StreamEvent>;
  try {
    first = await iterator.next();
  } catch (e) {
    requestSignal.removeEventListener("abort", abortUpstream);
    return errorResponse(e);
  }
  if (first.done) {
    requestSignal.removeEventListener("abort", abortUpstream);
    return textError(502, "provider stream ended before start");
  }

  const encoder = new TextEncoder();
  let closed = false;
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      const write = (event: StreamEvent) => {
        controller.enqueue(encoder.encode(`${JSON.stringify(event)}\n`));
      };
      const close = () => {
        if (closed) return;
        closed = true;
        requestSignal.removeEventListener("abort", abortUpstream);
        controller.close();
      };

      write(first.value);
      void (async () => {
        try {
          for (;;) {
            if (abort.signal.aborted) break;
            const next = await iterator.next();
            if (next.done) break;
            write(next.value);
          }
        } catch {
          // StreamEvent has no error variant. Close without `done` so the Rust
          // StreamConsumer reports IncompleteStream, matching the old providers.
        } finally {
          close();
        }
      })();
    },
    cancel() {
      closed = true;
      abort.abort();
      requestSignal.removeEventListener("abort", abortUpstream);
      void iterator.return?.();
    },
  });

  return new Response(body, { status: 200, headers: NDJSON_HEADERS });
}

async function readJson<T>(request: Request): Promise<
  | { ok: true; value: T }
  | { ok: false; response: Response }
> {
  try {
    return { ok: true, value: (await request.json()) as T };
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    return { ok: false, response: textError(400, `invalid json: ${message}`) };
  }
}

function jsonResponse(value: unknown): Response {
  return new Response(JSON.stringify(value), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

function errorResponse(e: unknown): Response {
  const status = errorStatus(e);
  return textError(status, errorBody(e));
}

function textError(status: number, body: string): Response {
  return new Response(body, { status, headers: { "content-type": "text/plain" } });
}

function errorStatus(e: unknown): number {
  const err = e as HttpishError;
  const status = err.status ?? err.statusCode ?? err.response?.status;
  if (typeof status === "number" && status >= 400 && status <= 599) return status;
  return 502;
}

function errorBody(e: unknown): string {
  const err = e as HttpishError;
  const body = err.body ?? err.error ?? err.response?.data ?? err.response?.body;
  if (typeof body === "string") return body;
  if (body !== undefined) {
    try {
      return JSON.stringify(body);
    } catch {
      return String(body);
    }
  }
  return e instanceof Error ? e.message : String(e);
}

function socketPathFromArgs(args: string[]): string | undefined {
  const idx = args.indexOf("--socket");
  if (idx >= 0) return args[idx + 1];
  return process.env["SHORE_LLM_SOCKET"];
}

if (import.meta.main) {
  const socketPath = socketPathFromArgs(process.argv.slice(2));
  if (!socketPath) {
    console.error("usage: bun run src/server.ts --socket <path>");
    process.exit(2);
  }
  const server = serveSidecar(socketPath);
  console.error(`shore llm sidecar listening on ${socketPath}`);
}
