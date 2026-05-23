/**
 * `generate_image` — generate an image and save it to the character's
 * images directory.
 *
 * Ported from `backend/daemon/src/tools/images.rs`. The Rust impl uses
 * a generic `LlmClient::image_generate()` that dispatches on the configured
 * provider; we drive the OpenAI SDK directly here. Most image-gen
 * providers (OpenAI, OpenRouter image models, Together, etc.) expose
 * OpenAI-compatible endpoints, so a single adapter covers the common case.
 *
 * If `ctx.imageGenConfig` is undefined the handler errors with
 * `Io: "no [image_generation] profile configured"` — mirrors Rust's
 * `image_gen_config()` returning None.
 */

import fs from "node:fs";
import path from "node:path";

import OpenAI from "openai";

import type { ToolContext, ToolHandler } from "./registry.ts";
import { ToolError } from "./registry.ts";

export const GENERATE_IMAGE_DESCRIPTION =
  "Generate an image from a text description via a separate image-generation model, save it to your images directory, and send it to {{user}}. Be specific about subject, mood, and composition.";

export const generateImageHandler: ToolHandler = {
  name: "generate_image",
  description: GENERATE_IMAGE_DESCRIPTION,
  inputSchema: {
    type: "object",
    properties: {
      prompt: {
        type: "string",
        description: "Text prompt for image generation.",
      },
      size: {
        type: "string",
        description: "Image dimensions (e.g. '1024x1024').",
        default: "1024x1024",
      },
      caption: {
        type: "string",
        description: "Optional caption to send with the generated image.",
      },
    },
    required: ["prompt"],
  },
  async execute(input: unknown, ctx: ToolContext): Promise<string> {
    const obj = (input ?? {}) as Record<string, unknown>;
    const prompt = obj["prompt"];
    if (typeof prompt !== "string" || prompt.length === 0) {
      throw new ToolError("InvalidArgs", "missing 'prompt' field");
    }

    const config = ctx.imageGenConfig;
    if (config === undefined) {
      throw new ToolError("Io", "no [image_generation] profile configured");
    }

    const size =
      typeof obj["size"] === "string" ? (obj["size"] as string) : config.size;
    const caption =
      typeof obj["caption"] === "string" ? (obj["caption"] as string) : undefined;

    const started = Date.now();
    const client = new OpenAI({
      apiKey: config.api_key,
      ...(config.base_url !== undefined ? { baseURL: config.base_url } : {}),
    });

    let response;
    try {
      response = await client.images.generate({
        model: config.model_id,
        prompt,
        size: size as "1024x1024" | "1792x1024" | "1024x1792" | "auto",
        n: 1,
        ...(config.quality !== undefined
          ? { quality: config.quality as "standard" | "hd" }
          : {}),
      });
    } catch (e) {
      throw new ToolError(
        "Http",
        `image generation failed: ${(e as Error).message}`,
      );
    }
    const elapsed = Date.now() - started;

    const datum = response.data?.[0];
    if (datum === undefined) {
      throw new ToolError("Http", "image generation returned no data");
    }
    const revisedPrompt = datum.revised_prompt ?? prompt;

    let imageBytes: Buffer;
    let extension: string;
    if (datum.b64_json !== undefined && datum.b64_json !== null) {
      imageBytes = Buffer.from(datum.b64_json, "base64");
      extension = "png";
    } else if (datum.url !== undefined && datum.url !== null) {
      const url = datum.url;
      if (url.startsWith("data:")) {
        const decoded = decodeDataUrl(url);
        imageBytes = decoded.bytes;
        extension = decoded.extension;
      } else {
        try {
          const res = await fetch(url, {
            signal: AbortSignal.timeout(60_000),
          });
          if (!res.ok) {
            throw new ToolError(
              "Http",
              `failed to download image: HTTP ${res.status}`,
            );
          }
          imageBytes = Buffer.from(await res.arrayBuffer());
          extension = "png";
        } catch (e) {
          if (e instanceof ToolError) throw e;
          throw new ToolError(
            "Http",
            `failed to download image: ${(e as Error).message}`,
          );
        }
      }
    } else {
      throw new ToolError("Http", "image generation returned neither url nor b64_json");
    }

    const generatedDir = path.join(ctx.imageDir, "generated");
    try {
      fs.mkdirSync(generatedDir, { recursive: true });
    } catch (e) {
      throw new ToolError(
        "Io",
        `failed to create directory: ${(e as Error).message}`,
      );
    }

    const ts = formatTimestamp(new Date());
    const filename = `${ts}.${extension}`;
    const savePath = path.join(generatedDir, filename);
    try {
      fs.writeFileSync(savePath, imageBytes);
    } catch (e) {
      throw new ToolError(
        "Io",
        `failed to save image: ${(e as Error).message}`,
      );
    }

    return JSON.stringify({
      path: savePath,
      caption,
      revised_prompt: revisedPrompt,
      timing_ms: elapsed,
      sent: true,
    });
  },
};

function decodeDataUrl(url: string): { bytes: Buffer; extension: string } {
  const rest = url.startsWith("data:image/")
    ? url.slice("data:image/".length)
    : undefined;
  if (rest === undefined) {
    throw new ToolError("Io", "data URL is not an image");
  }
  const sepIdx = rest.indexOf(";base64,");
  if (sepIdx < 0) {
    throw new ToolError("Io", "data URL missing ;base64, separator");
  }
  const mimeSubtype = rest.slice(0, sepIdx);
  const b64 = rest.slice(sepIdx + ";base64,".length);
  const extension = mimeSubtype === "jpeg" ? "jpg" : mimeSubtype;
  let bytes: Buffer;
  try {
    bytes = Buffer.from(b64, "base64");
  } catch (e) {
    throw new ToolError(
      "Io",
      `failed to decode base64 image: ${(e as Error).message}`,
    );
  }
  return { bytes, extension };
}

function formatTimestamp(d: Date): string {
  const pad = (n: number): string => String(n).padStart(2, "0");
  return (
    `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}_` +
    `${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}`
  );
}
