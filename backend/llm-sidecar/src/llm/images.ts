/**
 * Image attachment helpers for LLM requests.
 *
 * Reads an `ImageRef` (path on disk OR inline base64 data already on the
 * ref) and produces the canonical mime type + base64 bytes. Adapters
 * wrap those into their respective image block shapes:
 *   - Anthropic: `{type:"image", source:{type:"base64", media_type, data}}`
 *   - OpenAI:    `{type:"image_url", image_url:{url:"data:<mime>;base64,<data>"}}`
 *
 * The Rust impl had a resize + cache layer (`handler/resize.rs`); we omit
 * it for 4c.1 polish. Images larger than `DEFAULT_MAX_IMAGE_BYTES` are
 * skipped with a console warning rather than failing the whole turn.
 */
import fs from "node:fs";
import path from "node:path";

import type { ImageRef } from "../engine/types.ts";

/** Default cap matches Anthropic's documented 5 MiB per-image limit. */
const DEFAULT_MAX_IMAGE_BYTES = 5 * 1024 * 1024;

const MIME_BY_EXT: Record<string, string> = {
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".jpeg": "image/jpeg",
  ".webp": "image/webp",
  ".gif": "image/gif",
};

export interface ResolvedImage {
  mediaType: string;
  base64: string;
}

/**
 * Resolve an `ImageRef` into base64 bytes + media type, ready for either
 * adapter to wrap. Returns `undefined` if the file is missing, the MIME
 * type can't be detected, or the file exceeds the size cap — none of
 * these should fail the whole turn (consistent with the Rust impl,
 * which logs + drops the image).
 */
export function resolveImage(
  ref: ImageRef,
  maxBytes: number = DEFAULT_MAX_IMAGE_BYTES,
): ResolvedImage | undefined {
  const mediaType = MIME_BY_EXT[path.extname(ref.path).toLowerCase()];
  if (!mediaType) {
    console.warn(`[image] unsupported extension for ${ref.path}; skipping`);
    return undefined;
  }

  if (ref.data !== undefined && ref.data.length > 0) {
    // Estimate decoded size from the base64 length so a giant inline image is
    // dropped before we hand it to an adapter (4 base64 chars ≈ 3 bytes).
    const normalized = ref.data.replace(/\s+/g, "");
    const padding = normalized.endsWith("==") ? 2 : normalized.endsWith("=") ? 1 : 0;
    const inlineBytes = Math.floor((normalized.length * 3) / 4) - padding;
    if (inlineBytes > maxBytes) {
      console.warn(
        `[image] inline image ${ref.path} is ${inlineBytes} bytes; exceeds cap ${maxBytes}; skipping`,
      );
      return undefined;
    }
    return { mediaType, base64: ref.data };
  }

  try {
    // Check the on-disk size before reading so an oversized file never gets
    // loaded into memory in full.
    const size = fs.statSync(ref.path).size;
    if (size > maxBytes) {
      console.warn(
        `[image] ${ref.path} is ${size} bytes; exceeds cap ${maxBytes}; skipping`,
      );
      return undefined;
    }
    const bytes = fs.readFileSync(ref.path);
    return { mediaType, base64: bytes.toString("base64") };
  } catch (e) {
    console.warn(`[image] could not read ${ref.path}: ${(e as Error).message}`);
    return undefined;
  }
}

/**
 * Derive an `ImageRef` from an inline `{filename, data}` entry from the
 * ClientMessage. The data is already base64; we keep the original
 * filename so the model still sees a meaningful path.
 */
export function imageRefFromInline(entry: {
  filename: string;
  data: string;
}): ImageRef {
  return { path: entry.filename, data: entry.data };
}
