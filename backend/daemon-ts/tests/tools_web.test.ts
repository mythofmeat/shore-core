/**
 * Web tool tests — `web_search` (Tavily) and `fetch_url` (HTTP GET + HTML
 * strip).
 *
 * web_search uses a Bun.serve fake to play the Tavily response; fetch_url
 * uses a Bun.serve fake to return an HTML body. The HTML stripper has
 * dedicated unit tests too.
 */
import { afterAll, beforeAll, describe, expect, it } from "bun:test";

import type { ToolContext } from "../src/tools/registry.ts";
import { ToolError } from "../src/tools/registry.ts";
import { fetchUrlHandler, stripHtml, webSearchHandler } from "../src/tools/web.ts";

function stubCtx(apiKeyEnv = "TAVILY_API_KEY_TEST"): ToolContext {
  return {
    characterName: "test",
    characterConfigDir: "/tmp/x",
    characterDataDir: "/tmp/x",
    workspaceDir: "/tmp/x",
    configDir: "/tmp/x",
    imageDir: "/tmp/x",
    engine: undefined as unknown as ToolContext["engine"],
    searchConfig: {
      api_key_env: apiKeyEnv,
      max_results: 5,
      search_depth: "basic",
      include_answer: true,
    },
    retrievalConfig: { max_file_bytes: 1024 * 1024 },
  };
}

describe("web_search", () => {
  it("errors when query is missing", async () => {
    expect(webSearchHandler.execute({}, stubCtx())).rejects.toThrow(ToolError);
  });

  it("errors when api-key env var is unset", async () => {
    delete process.env["UNSET_KEY_FOR_TEST"];
    const ctx = stubCtx("UNSET_KEY_FOR_TEST");
    expect(
      webSearchHandler.execute({ query: "rust" }, ctx),
    ).rejects.toThrow(/UNSET_KEY_FOR_TEST/);
  });
});

describe("fetch_url", () => {
  it("errors when url is missing", async () => {
    expect(fetchUrlHandler.execute({}, stubCtx())).rejects.toThrow(ToolError);
  });

  it("fetches a page and strips HTML when content-type is text/html", async () => {
    const server = Bun.serve({
      port: 0,
      fetch: () =>
        new Response("<html><body><h1>Hello</h1><p>World</p></body></html>", {
          headers: { "Content-Type": "text/html" },
        }),
    });
    try {
      const url = `http://${server.hostname}:${server.port}/`;
      const r = JSON.parse(
        await fetchUrlHandler.execute({ url }, stubCtx()),
      );
      expect(r.url).toBe(url);
      expect(r.content_type).toContain("html");
      expect(r.content).toContain("Hello");
      expect(r.content).toContain("World");
      expect(r.content).not.toContain("<h1>");
      expect(r.truncated).toBe(false);
    } finally {
      await server.stop();
    }
  });

  it("does NOT strip non-HTML content", async () => {
    const server = Bun.serve({
      port: 0,
      fetch: () =>
        new Response("plain text body", {
          headers: { "Content-Type": "text/plain" },
        }),
    });
    try {
      const url = `http://${server.hostname}:${server.port}/`;
      const r = JSON.parse(
        await fetchUrlHandler.execute({ url }, stubCtx()),
      );
      expect(r.content).toBe("plain text body");
    } finally {
      await server.stop();
    }
  });

  it("propagates non-2xx HTTP errors", async () => {
    const server = Bun.serve({
      port: 0,
      fetch: () => new Response("not found", { status: 404 }),
    });
    try {
      const url = `http://${server.hostname}:${server.port}/`;
      expect(fetchUrlHandler.execute({ url }, stubCtx())).rejects.toThrow(
        /HTTP 404/,
      );
    } finally {
      await server.stop();
    }
  });
});

describe("stripHtml", () => {
  it("removes inline tags", () => {
    const out = stripHtml("<p>Hello <b>world</b></p>");
    expect(out).toContain("Hello");
    expect(out).toContain("world");
    expect(out).not.toContain("<");
  });

  it("removes script/style/head blocks entirely", () => {
    const html =
      "<html><head><title>T</title></head><body><script>var x=1;</script><style>.foo{}</style><p>Visible</p></body></html>";
    const out = stripHtml(html);
    expect(out).toContain("Visible");
    expect(out).not.toContain("var x");
    expect(out).not.toContain(".foo");
    expect(out).not.toContain("<title>");
  });

  it("decodes common HTML entities", () => {
    const out = stripHtml("<p>A &amp; B &lt; C &gt; D &quot;E&quot;</p>");
    expect(out).toBe('A & B < C > D "E"');
  });

  it("collapses runs of whitespace", () => {
    const out = stripHtml("<p>  lots   of    spaces  </p>");
    expect(out).not.toContain("  ");
    expect(out).toBe("lots of spaces");
  });

  it("survives unicode chars whose lowercasing changes byte length", () => {
    // ẞ (U+1E9E, 3 bytes in UTF-8) → ß (U+00DF, 2 bytes). The Rust impl
    // had a byte-offset bug here; we run on JS strings so it shouldn't
    // matter, but pin the behavior anyway.
    const html = `${"ẞ".repeat(10)}<b>x</b>`;
    const out = stripHtml(html);
    expect(out).toContain("x");
  });
});
