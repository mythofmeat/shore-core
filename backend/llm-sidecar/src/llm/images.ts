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
    return { mediaType, base64: ref.data };
  }

  let bytes: Buffer;
  try {
    bytes = fs.readFileSync(ref.path);
  } catch (e) {
    console.warn(`[image] could not read ${ref.path}: ${(e as Error).message}`);
    return undefined;
  }
  if (bytes.length > maxBytes) {
    console.warn(
      `[image] ${ref.path} is ${bytes.length} bytes; exceeds cap ${maxBytes}; skipping`,
    );
    return undefined;
  }
  return { mediaType, base64: bytes.toString("base64") };
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
