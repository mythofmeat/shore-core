import type { IncomingMessage, ServerResponse } from "node:http";
import { childWithRid } from "./logger.js";

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

function stubEndpoint(_req: IncomingMessage, res: ServerResponse): void {
  json(res, 501, { error: "not_implemented", message: "Endpoint not yet implemented" });
}

const routes: Route[] = [
  { method: "GET", path: "/v1/health", handler: handleHealth },
  { method: "POST", path: "/v1/generate", handler: stubEndpoint },
  { method: "POST", path: "/v1/stream", handler: stubEndpoint },
  { method: "POST", path: "/v1/embed", handler: stubEndpoint },
  { method: "POST", path: "/v1/image/generate", handler: stubEndpoint },
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
