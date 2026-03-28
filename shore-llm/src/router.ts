import type { IncomingMessage, ServerResponse } from "node:http";
import { childWithRid } from "./logger.js";
import {
  getProvider,
  type ProviderRequest,
  type EmbedRequest,
  type ImageGenerateRequest,
} from "./providers/index.js";
import * as openai from "./providers/openai.js";
import * as openrouter from "./providers/openrouter.js";

type Handler = (
  req: IncomingMessage,
  res: ServerResponse,
  body: string,
) => void | Promise<void>;

interface Route {
  method: string;
  path: string;
  handler: Handler;
}

function json(res: ServerResponse, status: number, data: unknown): void {
  const payload = JSON.stringify(data);
  res.writeHead(status, {
    "Content-Type": "application/json",
    "Content-Length": Buffer.byteLength(payload),
  });
  res.end(payload);
}

function handleHealth(_req: IncomingMessage, res: ServerResponse): void {
  json(res, 200, { status: "ok" });
}

async function handleEmbed(
  _req: IncomingMessage,
  res: ServerResponse,
  body: string,
): Promise<void> {
  const req = JSON.parse(body) as EmbedRequest;
  const client = openai.createClient(req.api_key, req.base_url);
  const result = await openai.embed(client, req);
  json(res, 200, result);
}

async function handleImageGenerate(
  _req: IncomingMessage,
  res: ServerResponse,
  body: string,
): Promise<void> {
  const req = JSON.parse(body) as ImageGenerateRequest;

  if (req.provider === "openrouter") {
    const result = await openrouter.imageGenerate(req);
    json(res, 200, result);
  } else {
    // OpenAI path (also works for any OpenAI-compatible base_url).
    const client = openai.createClient(req.api_key, req.base_url);
    const result = await openai.imageGenerate(client, req);
    json(res, 200, result);
  }
}

async function handleGenerate(
  _req: IncomingMessage,
  res: ServerResponse,
  body: string,
): Promise<void> {
  const req = JSON.parse(body) as ProviderRequest;
  const provider = getProvider(req.provider);
  if (!provider) {
    json(res, 400, {
      error: "unsupported_provider",
      message: `Provider "${req.provider}" is not supported`,
    });
    return;
  }
  const result = await provider.generate(req);
  json(res, 200, result);
}

async function handleStream(
  _req: IncomingMessage,
  res: ServerResponse,
  body: string,
): Promise<void> {
  const req = JSON.parse(body) as ProviderRequest;
  const provider = getProvider(req.provider);
  if (!provider) {
    json(res, 400, {
      error: "unsupported_provider",
      message: `Provider "${req.provider}" is not supported`,
    });
    return;
  }
  await provider.stream(req, res);
}

const routes: Route[] = [
  { method: "GET", path: "/v1/health", handler: handleHealth },
  { method: "POST", path: "/v1/generate", handler: handleGenerate },
  { method: "POST", path: "/v1/stream", handler: handleStream },
  { method: "POST", path: "/v1/embed", handler: handleEmbed },
  { method: "POST", path: "/v1/image/generate", handler: handleImageGenerate },
];

function readBody(req: IncomingMessage): Promise<string> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    req.on("data", (chunk: Buffer) => chunks.push(chunk));
    req.on("end", () => resolve(Buffer.concat(chunks).toString()));
    req.on("error", reject);
  });
}

export async function dispatch(
  req: IncomingMessage,
  res: ServerResponse,
): Promise<void> {
  const rid = req.headers["x-request-id"] as string | undefined;
  const log = childWithRid(rid);

  const method = req.method ?? "GET";
  const url = req.url ?? "/";

  log.info({ method, url }, "request");

  const route = routes.find((r) => r.method === method && r.path === url);

  if (!route) {
    json(res, 404, { error: "not_found", message: `No route for ${method} ${url}` });
    return;
  }

  const body = await readBody(req);

  // Validate JSON body for POST requests
  if (method === "POST" && body.length > 0) {
    try {
      JSON.parse(body);
    } catch {
      json(res, 400, { error: "invalid_json", message: "Request body is not valid JSON" });
      return;
    }
  }

  await route.handler(req, res, body);
}
