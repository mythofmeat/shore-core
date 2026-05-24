/**
 * Compaction LLM response parser + default templates.
 *
 * Port of `backend/daemon/src/memory/compaction/parser.rs`.
 *
 * Template content is inlined verbatim (no trailing newline) from
 * `backend/daemon/prompts/memory/compaction/compact_system.md` and
 * `.../compact.md` so the TS daemon ships as a single bundle.
 */

// ---------------------------------------------------------------------------
// Default templates
// ---------------------------------------------------------------------------

/**
 * Default compaction system prompt template. Mirrors
 * `prompts/memory/compaction/compact_system.md` (sans trailing newline).
 *
 * Placeholders: `{{char}}`, `{{user}}`.
 */
export const DEFAULT_COMPACT_SYSTEM = `You are {{char}}. This conversation with {{user}} is about to be archived and your active context will be cleared. Before that happens, you must save anything important to your long-term memory files AND update MEMORY.md so a future-you can pick up where this conversation left off.

You have access to your memories directory. Use the <memory> section below to write or update markdown files. Be concise and organized.

Guidelines:
- Prefer updating existing files over creating new ones. Use the existing memory snapshot below to merge new information into the right files.
- Use clear filenames and folder structure (e.g., memory/people/{{user}}.md, memory/topics/gaming/doom.md).
- Each file should have a heading and bullet points.
- Include timestamps or session context when relevant.
- If {{user}} corrected previous information, update the file rather than appending.
- Update MEMORY.md (workspace root) with the conversational throughline: ongoing topics, unresolved threads, anything future-you should remember to continue. MEMORY.md is the prompt-visible index — keep it concise.
- Dreaming will reorganize MEMORY.md later; your job is to make sure the carry-forward context is captured before the conversation is archived.

Your response MUST contain a <memory> block containing zero or more <write> operations.

Each <write> creates or overwrites a single file. The content is pure markdown — no YAML frontmatter.

<memory>
<write path="memory/people/{{user}}.md">
# {{user}}

- Likes tea (mentioned on 2026-04-22)
- Works in software
</write>

<write path="MEMORY.md">
# Memory Index

## Throughline
- Currently helping {{user}} debug a Rust ownership issue in the renderer.
- Picked up where last session left off on the Doom speedrun project.

## Recent files
- memory/people/{{user}}.md
- memory/topics/gaming/doom.md
</write>
</memory>

If nothing new needs to be saved, output an empty <memory></memory> block.`;

/**
 * Default compaction final-message template. Mirrors
 * `prompts/memory/compaction/compact.md` (sans trailing newline).
 *
 * Placeholders: `{{char}}`, `{{user}}`, `{{existing_memories}}`.
 */
export const DEFAULT_COMPACT_PROMPT = `The conversation above is now complete and will be archived. Please review it and save anything important to your memory files.

Existing memory files:
<existing_memories>
{{existing_memories}}
</existing_memories>`;

// ---------------------------------------------------------------------------
// XML parsing helpers
// ---------------------------------------------------------------------------

/** Extract content between `<tag>` and `</tag>` (first occurrence). */
export function extractXmlTag(text: string, tag: string): string | undefined {
  const open = `<${tag}>`;
  const close = `</${tag}>`;
  const start = text.indexOf(open);
  if (start < 0) return undefined;
  const contentStart = start + open.length;
  const endRel = text.slice(contentStart).indexOf(close);
  if (endRel < 0) return undefined;
  const content = text.slice(contentStart, contentStart + endRel).trim();
  return content.length > 0 ? content : undefined;
}

// ---------------------------------------------------------------------------
// Memory file ops
// ---------------------------------------------------------------------------

/** A single memory file operation extracted from the LLM compaction response. */
export interface MemoryFileOp {
  path: string;
  content: string;
}

/**
 * Parse raw LLM response into memory file operations.
 *
 * Expected format: a `<memory>` block containing one or more
 * `<write path="...">` blocks. Legacy responses may include a `<recap>` block;
 * compaction ignores it because current compaction writes markdown files
 * directly, including workspace-root `MEMORY.md` when the model provides a
 * carry-forward throughline.
 */
export function parseCompactionResponse(raw: string): MemoryFileOp[] {
  const memoryBlock = extractXmlTag(raw, "memory") ?? "";
  return extractWriteOps(memoryBlock);
}

/** Extract `<write path="...">...</write>` blocks from a `<memory>` section. */
export function extractWriteOps(text: string): MemoryFileOp[] {
  const ops: MemoryFileOp[] = [];
  let searchFrom = 0;

  while (true) {
    const startRel = text.slice(searchFrom).indexOf("<write ");
    if (startRel < 0) break;
    const absStart = searchFrom + startRel;

    const pathStartRel = text.slice(absStart).indexOf('path="');
    if (pathStartRel < 0) {
      searchFrom = absStart + 1;
      continue;
    }
    const pathStart = absStart + pathStartRel + 'path="'.length;
    const pathEndRel = text.slice(pathStart).indexOf('"');
    if (pathEndRel < 0) {
      searchFrom = absStart + 1;
      continue;
    }
    const pathEnd = pathStart + pathEndRel;
    const filePath = text.slice(pathStart, pathEnd).trim();

    const tagCloseRel = text.slice(absStart).indexOf(">");
    if (tagCloseRel < 0) {
      searchFrom = absStart + 1;
      continue;
    }
    const contentStart = absStart + tagCloseRel + 1;

    const close = "</write>";
    const contentEndRel = text.slice(contentStart).indexOf(close);
    if (contentEndRel < 0) {
      searchFrom = absStart + 1;
      continue;
    }
    const contentEnd = contentStart + contentEndRel;

    const content = text.slice(contentStart, contentEnd).trim();
    if (filePath.length > 0) {
      ops.push({ path: filePath, content });
    }
    searchFrom = contentEnd + close.length;
  }

  return ops;
}
