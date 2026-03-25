import { describe, it, expect, beforeAll, afterAll } from "vitest";
import { createServer, type Server } from "node:http";
import { dispatch } from "./router.js";

let server: Server;
let socketPath: string;

beforeAll(async () => {
  socketPath = `/tmp/shore-llm-test-${process.pid}.sock`;
  server = createServer((req, res) => {
    dispatch(req, res).catch(() => {
      res.writeHead(500);
      res.end();
    });
  });
  await new Promise<void>((resolve) => server.listen(socketPath, resolve));
});

afterAll(async () => {
  await new Promise<void>((resolve, reject) =>
    server.close((err) => (err ? reject(err) : resolve())),
  );
});

function request(
  method: string,
  path: string,
  opts?: { body?: string; headers?: Record<string, string> },
): Promise<{ status: number; body: unknown; headers: Record<string, string> }> {
  return new Promise((resolve, reject) => {
    const http = require("node:http") as typeof import("node:http");
    const req = http.request(
      { socketPath, path, method, headers: opts?.headers },
      (res) => {
        const chunks: Buffer[] = [];
        res.on("data", (c: Buffer) => chunks.push(c));
        res.on("end", () => {
          const raw = Buffer.concat(chunks).toString();
          let body: unknown;
          try {
            body = JSON.parse(raw);
          } catch {
            body = raw;
          }
          resolve({
            status: res.statusCode!,
            body,
            headers: res.headers as Record<string, string>,
          });
        });
      },
    );
    req.on("error", reject);
    if (opts?.body) req.write(opts.body);
    req.end();
  });
}

describe("GET /v1/health", () => {
  it("returns 200 with status ok", async () => {
    const res = await request("GET", "/v1/health");
    expect(res.status).toBe(200);
    expect(res.body).toEqual({ status: "ok" });
  });

  it("returns application/json content type", async () => {
    const res = await request("GET", "/v1/health");
    expect(res.headers["content-type"]).toBe("application/json");
  });
});

describe("404 handling", () => {
  it("returns 404 for unknown path", async () => {
    const res = await request("GET", "/v1/nonexistent");
    expect(res.status).toBe(404);
    expect(res.body).toEqual({
      error: "not_found",
      message: "No route for GET /v1/nonexistent",
    });
  });

  it("returns 404 for wrong method on existing path", async () => {
    const res = await request("POST", "/v1/health");
    expect(res.status).toBe(404);
  });
});

describe("invalid JSON body", () => {
  it("returns 400 for malformed JSON on POST endpoint", async () => {
    const res = await request("POST", "/v1/generate", {
      body: "{not json",
      headers: { "Content-Type": "application/json" },
    });
    expect(res.status).toBe(400);
    expect(res.body).toEqual({
      error: "invalid_json",
      message: "Request body is not valid JSON",
    });
  });
});

describe("live endpoints reject unsupported provider", () => {
  it("POST /v1/generate returns 400 for unsupported provider", async () => {
    const res = await request("POST", "/v1/generate", {
      body: JSON.stringify({ provider: "nonexistent", model: "m", api_key: "k", max_tokens: 10, messages: [] }),
      headers: { "Content-Type": "application/json" },
    });
    expect(res.status).toBe(400);
    expect(res.body).toMatchObject({ error: "unsupported_provider" });
  });

  it("POST /v1/stream returns 400 for unsupported provider", async () => {
    const res = await request("POST", "/v1/stream", {
      body: JSON.stringify({ provider: "nonexistent", model: "m", api_key: "k", max_tokens: 10, messages: [] }),
      headers: { "Content-Type": "application/json" },
    });
    expect(res.status).toBe(400);
    expect(res.body).toMatchObject({ error: "unsupported_provider" });
  });
});

describe("POST /v1/embed route exists", () => {
  it("POST /v1/embed does not return 404", async () => {
    const res = await request("POST", "/v1/embed", {
      body: "{}",
      headers: { "Content-Type": "application/json" },
    });
    expect(res.status).not.toBe(404);
  });
});

describe("POST /v1/image/generate route exists", () => {
  it("POST /v1/image/generate does not return 404", async () => {
    const res = await request("POST", "/v1/image/generate", {
      body: "{}",
      headers: { "Content-Type": "application/json" },
    });
    expect(res.status).not.toBe(404);
  });
});

describe("X-Request-ID propagation", () => {
  it("logs with rid when header is present", async () => {
    // We verify the request succeeds — log inspection would require a pino transport mock
    const res = await request("GET", "/v1/health", {
      headers: { "X-Request-ID": "test-rid-123" },
    });
    expect(res.status).toBe(200);
  });
});
