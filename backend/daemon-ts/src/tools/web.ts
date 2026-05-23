/**
 * Web tools — `web_search` (Tavily) and `fetch_url` (HTTP GET + HTML strip).
 *
 * Ported from `backend/daemon/src/tools/web.rs`. Both use Bun's global
 * `fetch` with a 30 s `AbortSignal.timeout`. `fetch_url` strips HTML
 * server-side and truncates to 50 000 characters; the HTML stripper is
 * the careful unicode-safe version (mirrors strip_html in web.rs:199-280).
 */

import type { ToolContext, ToolHandler } from "./registry.ts";
import { ToolError } from "./registry.ts";

export const WEB_SEARCH_DESCRIPTION =
  "Search the web and return a list of results with titles, URLs, and content snippets. Use it to gather more info about a topic, when you're uncertain about a fact, or whenever the conversation touches on recent events, specific people, products, or things that may have happened after your training cutoff. Don't hedge or caveat about whether you 'should' look something up — searching is free, so if it would help, just do it. Pair with `fetch_url` to read the full content of a result page when a snippet isn't enough.";

export const FETCH_URL_DESCRIPTION =
  "Fetch and read the content of a web page, returning its text. Use when a `web_search` result looks relevant and you need the full context, when {{user}} pastes or mentions a URL you should actually read, or when you want to follow a specific page you already have in mind. Best paired with `web_search` to find candidate URLs first. Returns plain text; complex interactive pages, paywalled content, or heavily JS-rendered sites may return limited or no content.";

const MAX_CONTENT_CHARS = 50_000;
const REQUEST_TIMEOUT_MS = 30_000;

// ---------------------------------------------------------------------------
// web_search
// ---------------------------------------------------------------------------

export const webSearchHandler: ToolHandler = {
  name: "web_search",
  description: WEB_SEARCH_DESCRIPTION,
  inputSchema: {
    type: "object",
    properties: {
      query: { type: "string", description: "The search query." },
      max_results: {
        type: "integer",
        description: "Maximum number of results to return.",
        default: 5,
      },
    },
    required: ["query"],
  },
  async execute(input: unknown, ctx: ToolContext): Promise<string> {
    const obj = (input ?? {}) as Record<string, unknown>;
    const query = obj["query"];
    if (typeof query !== "string" || query.length === 0) {
      throw new ToolError("InvalidArgs", "missing 'query' field");
    }
    const cfg = ctx.searchConfig;
    const apiKey = process.env[cfg.api_key_env];
    if (apiKey === undefined || apiKey.length === 0) {
      throw new ToolError(
        "InvalidArgs",
        `web_search requires the ${cfg.api_key_env} environment variable to be set`,
      );
    }
    const maxResults =
      typeof obj["max_results"] === "number" && Number.isFinite(obj["max_results"])
        ? Math.max(1, Math.floor(obj["max_results"] as number))
        : cfg.max_results;

    let res: Response;
    try {
      res = await fetch("https://api.tavily.com/search", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          api_key: apiKey,
          query,
          max_results: maxResults,
          search_depth: cfg.search_depth,
          include_answer: cfg.include_answer,
        }),
        signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
      });
    } catch (e) {
      throw new ToolError(
        "Http",
        `Tavily request failed: ${(e as Error).message}`,
      );
    }

    if (!res.ok) {
      const errBody = await res.text().catch(() => "");
      throw new ToolError(
        "Http",
        `Tavily API returned HTTP ${res.status}: ${errBody}`,
      );
    }

    let body: unknown;
    try {
      body = await res.json();
    } catch (e) {
      throw new ToolError(
        "Http",
        `failed to parse Tavily response: ${(e as Error).message}`,
      );
    }

    const rawResults = (body as { results?: unknown }).results;
    const results = Array.isArray(rawResults)
      ? rawResults.map((r) => {
          const rec = (r ?? {}) as Record<string, unknown>;
          return {
            title: typeof rec["title"] === "string" ? rec["title"] : "",
            url: typeof rec["url"] === "string" ? rec["url"] : "",
            content: typeof rec["content"] === "string" ? rec["content"] : "",
          };
        })
      : [];

    const out: Record<string, unknown> = { query, results };
    const answer = (body as { answer?: unknown }).answer;
    if (typeof answer === "string") out["answer"] = answer;
    return JSON.stringify(out);
  },
};

// ---------------------------------------------------------------------------
// fetch_url
// ---------------------------------------------------------------------------

export const fetchUrlHandler: ToolHandler = {
  name: "fetch_url",
  description: FETCH_URL_DESCRIPTION,
  inputSchema: {
    type: "object",
    properties: {
      url: { type: "string", description: "The URL to fetch." },
    },
    required: ["url"],
  },
  async execute(input: unknown): Promise<string> {
    const obj = (input ?? {}) as Record<string, unknown>;
    const url = obj["url"];
    if (typeof url !== "string" || url.length === 0) {
      throw new ToolError("InvalidArgs", "missing 'url' field");
    }

    let res: Response;
    try {
      res = await fetch(url, {
        method: "GET",
        redirect: "follow",
        headers: { "User-Agent": "shore/2.0" },
        signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
      });
    } catch (e) {
      throw new ToolError("Http", `request failed: ${(e as Error).message}`);
    }
    if (!res.ok) {
      throw new ToolError("Http", `HTTP ${res.status} for ${url}`);
    }

    const contentType = res.headers.get("content-type") ?? "unknown";
    let body: string;
    try {
      body = await res.text();
    } catch (e) {
      throw new ToolError(
        "Http",
        `failed to read body: ${(e as Error).message}`,
      );
    }

    const isHtml = contentType.includes("html");
    let content = isHtml ? stripHtml(body) : body;

    const truncated = content.length > MAX_CONTENT_CHARS;
    if (truncated) {
      // Char-boundary safe truncation: substring() on UTF-16 code units
      // is fine for this purpose; we don't need byte-exact behavior.
      content = content.slice(0, MAX_CONTENT_CHARS);
    }

    return JSON.stringify({
      url,
      content_type: contentType,
      content,
      truncated,
    });
  },
};

/**
 * Strip HTML tags and extract readable text content.
 *
 * Phases mirror `strip_html` in web.rs:199-280:
 *   1. Remove `<script>`, `<style>`, `<head>` blocks entirely
 *   2. Strip remaining tags (each tag becomes a single space)
 *   3. Decode common HTML entities
 *   4. Collapse whitespace
 *
 * The case-insensitive tag detection compares ASCII-lowercased slices
 * of the *original* string against the tag list, so byte offsets stay
 * valid even when characters like ẞ → ß would shrink under Unicode
 * lowercasing. The TS port doesn't have the same byte-offset risk
 * (we work on the JS string directly), but we preserve the algorithmic
 * shape for parity.
 */
export function stripHtml(html: string): string {
  const skipTags = ["script", "style", "head"];
  let cleaned = "";
  let i = 0;

  while (i < html.length) {
    if (html[i] === "<") {
      const remainingLower = html.slice(i).toLowerCase();
      const skip = skipTags.find((t) => remainingLower.startsWith(`<${t}`));
      if (skip !== undefined) {
        const closeMarker = `</${skip}`;
        const endPos = remainingLower.indexOf(closeMarker);
        if (endPos >= 0) {
          const afterClose = i + endPos + closeMarker.length;
          const gt = html.indexOf(">", afterClose);
          if (gt >= 0) {
            i = gt + 1;
            continue;
          }
        }
        break;
      }
      const gt = html.indexOf(">", i);
      if (gt >= 0) {
        cleaned += " ";
        i = gt + 1;
        continue;
      }
    }
    cleaned += html[i];
    i += 1;
  }

  // Phase 2: Decode common HTML entities (verbatim from web.rs:253-262).
  const decoded = cleaned
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&apos;/g, "'")
    .replace(/&nbsp;/g, " ")
    .replace(/&#x27;/g, "'")
    .replace(/&#x2F;/g, "/");

  // Phase 3: Collapse whitespace.
  let result = "";
  let prevWs = false;
  for (const ch of decoded) {
    if (/\s/.test(ch)) {
      if (!prevWs) result += " ";
      prevWs = true;
    } else {
      result += ch;
      prevWs = false;
    }
  }
  return result.trim();
}
