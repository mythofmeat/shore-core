/**
 * CompactionManager — orchestrates a single compaction pass.
 *
 * Port of `backend/daemon/src/memory/compaction/mod.rs::CompactionManager`.
 *
 * Splits an active conversation into a compacted older portion (sent to the
 * LLM to extract memory writes) and a retained recent tail. Writes the LLM-
 * generated markdown files into the character workspace, queues a MEMORY.md
 * prompt refresh on the deferred-edits queue, archives the compacted slice
 * into a segment file via the `ConversationManager`, and appends an entry to
 * the dreams log.
 *
 * Failures during the markdown-write phase roll back via compensating
 * deletes (or restoring prior content).
 */

import fs from "node:fs";
import path from "node:path";

import { appendDreamEntry } from "../dreams_log.ts";
import {
  MEMORY_INDEX_DEFERRED_PATH,
  MEMORY_INDEX_FILE,
  noteMemoryIndexDeferred,
  normalizePromptVisiblePath,
} from "../deferred_edits.ts";
import type { MarkdownMemoryStore } from "../markdown_store.ts";
import { resolvePath } from "../../tools/paths.ts";

import {
  type CompactionConfig,
  type CompactionLlm,
  type CompactionOutcome,
  CompactionError,
  type ConversationManager,
  type ConversationMessage,
  type RetentionParams,
} from "./types.ts";
import {
  type MemoryFileOp,
  parseCompactionResponse,
} from "./parser.ts";
import type { ChatRequest } from "../../llm/types.ts";

const EXISTING_MEMORY_CONTEXT_MAX_FILES = 24;
const EXISTING_MEMORY_CONTEXT_MAX_CHARS_PER_FILE = 1_800;

interface CompactionWriteState {
  displayPath: string;
  /** Resolved absolute path inside the character workspace. */
  resolved: string;
  /** Prior content for restore-on-rollback, or `undefined` if not present. */
  previousContent: string | undefined;
}

export interface CompactOptions {
  conversationId: string;
  messages: ConversationMessage[];
  /** Pre-read content of active.jsonl — eliminates TOCTOU with parsing. */
  activeContent: string;
  isPrivate: boolean;
  systemTemplate: string;
  promptTemplate: string;
  charName: string;
  userName: string;
  llm: CompactionLlm;
  conversationManager: ConversationManager;
  markdownStore: MarkdownMemoryStore | undefined;
  dryRun: boolean;
  /** Override config.keepRecentTurns. */
  keepTurnsOverride?: number;
  cachedRequest?: ChatRequest;
  /**
   * Daemon data root. Required to append to the dreams log and to queue
   * the MEMORY.md prompt refresh — when omitted, both side effects are
   * skipped with a warning (matches Rust's `data_dir: Option<&Path>`).
   */
  dataDir?: string;
}

export class CompactionManager {
  constructor(public readonly config: CompactionConfig) {}

  // ── helpers exposed for tests + the autonomy gating ─────────────────

  /** True when the message is a tool-loop intermediate. */
  static isToolLoopMessage(msg: ConversationMessage): boolean {
    if (msg.role === "user") return msg.isToolResultOnly;
    if (msg.role === "assistant") return msg.content.length === 0;
    return false;
  }

  /**
   * Find the split index that retains `keepTurns` complete user turns at
   * the tail. Returns 0 if there aren't enough messages to compact.
   * `keepTurns === 0` returns `messages.length` (retain nothing).
   */
  static findTurnSplit(
    messages: ConversationMessage[],
    keepTurns: number,
  ): number {
    if (keepTurns === 0) return messages.length;
    let turnsSeen = 0;
    for (let i = messages.length - 1; i >= 0; i--) {
      const m = messages[i]!;
      if (m.role === "user" && !CompactionManager.isToolLoopMessage(m)) {
        turnsSeen += 1;
        if (turnsSeen >= keepTurns) return i;
      }
    }
    return 0;
  }

  static countTurns(messages: ConversationMessage[]): number {
    let count = 0;
    for (const m of messages) {
      if (m.role === "user" && !CompactionManager.isToolLoopMessage(m)) count += 1;
    }
    return count;
  }

  notifyActivity(): void {
    // Hook for the IdleTimer when the engine wires it up. The TS port
    // currently doesn't host the activity-notify channel here (the
    // engine carries its own ActivityNotify), so this is a no-op shim
    // for parity with Rust's `mgr.notify_activity()` call sites.
  }

  /** Force-compact gate: max_turns reached AND min_turns satisfied. */
  shouldForceCompact(turnCount: number): boolean {
    return (
      this.config.maxTurns > 0 &&
      turnCount >= this.config.maxTurns &&
      this.hasEnoughTurns(turnCount)
    );
  }

  hasEnoughTurns(turnCount: number): boolean {
    return turnCount >= this.config.minTurns;
  }

  // ── prompt building ────────────────────────────────────────────────

  static buildSystem(template: string, charName: string, userName: string): string {
    return template
      .replaceAll("{{char}}", charName)
      .replaceAll("{{user}}", userName);
  }

  /**
   * Render the final compaction user message, substituting `{{char}}`,
   * `{{user}}`, and `{{existing_memories}}`. Strips the legacy
   * `{{#if recap}}...{{/if}}` block and `{{recap}}` placeholder.
   */
  static buildFinalMessage(
    template: string,
    existingMemories: string | undefined,
    charName: string,
    userName: string,
  ): string {
    const memText =
      existingMemories !== undefined && existingMemories.trim().length > 0
        ? existingMemories
        : "No existing memory files were available.";

    let out = template
      .replaceAll("{{existing_memories}}", memText)
      .replaceAll("{{char}}", charName)
      .replaceAll("{{user}}", userName);

    while (true) {
      const ifStart = out.indexOf("{{#if recap}}");
      const endif = out.indexOf("{{/if}}");
      if (ifStart < 0 || endif < 0) break;
      out = out.slice(0, ifStart) + out.slice(endif + "{{/if}}".length);
    }
    return out.replaceAll("{{recap}}", "");
  }

  /**
   * Build the structured messages array for a fresh-prefix compaction
   * LLM call: the compacted-portion role/content objects, followed by a
   * final user message rendered from `promptTemplate`.
   */
  static buildMessages(
    promptTemplate: string,
    messages: ConversationMessage[],
    existingMemories: string | undefined,
    charName: string,
    userName: string,
  ): Array<{ role: "user" | "assistant"; content: string }> {
    const out: Array<{ role: "user" | "assistant"; content: string }> = [];
    for (const m of messages) {
      if (m.role !== "user" && m.role !== "assistant") continue;
      out.push({ role: m.role, content: m.content });
    }
    const final = CompactionManager.buildFinalMessage(
      promptTemplate,
      existingMemories,
      charName,
      userName,
    );
    out.push({ role: "user", content: final });
    return out;
  }

  /** Flattened-prompt helper retained for tests that pin the legacy string shape. */
  static buildPrompt(
    template: string,
    messages: ConversationMessage[],
    existingMemories: string | undefined,
    charName: string,
    userName: string,
  ): string {
    let conversationText = "";
    for (const msg of messages) {
      conversationText += `[${msg.timestamp}] ${msg.role}: ${msg.content}\n`;
    }
    let out = template.replaceAll("{{conversation}}", conversationText);
    const memText =
      existingMemories !== undefined && existingMemories.trim().length > 0
        ? existingMemories
        : "No existing memory files were available.";
    out = out.replaceAll("{{existing_memories}}", memText);
    while (true) {
      const ifStart = out.indexOf("{{#if recap}}");
      const endif = out.indexOf("{{/if}}");
      if (ifStart < 0 || endif < 0) break;
      out = out.slice(0, ifStart) + out.slice(endif + "{{/if}}".length);
    }
    return out
      .replaceAll("{{recap}}", "")
      .replaceAll("{{char}}", charName)
      .replaceAll("{{user}}", userName);
  }

  // ── existing-memory snapshot ───────────────────────────────────────

  /** Render a bounded snapshot of current markdown memories for the LLM. */
  static buildExistingMemoryContext(
    store: MarkdownMemoryStore | undefined,
  ): string {
    if (store === undefined) return "No existing memory files were available.";
    let raw;
    try {
      raw = store.listAll();
    } catch (e) {
      return `Existing memory files could not be loaded: ${(e as Error).message}`;
    }
    const entries = raw
      .filter((e) => e.path !== "DREAMS.md")
      .sort((a, b) => a.path.localeCompare(b.path));
    if (entries.length === 0) return "No existing memory files yet.";

    const total = entries.length;
    let out = "";
    for (const entry of entries.slice(0, EXISTING_MEMORY_CONTEXT_MAX_FILES)) {
      out += `<file path="memory/${escapeAttr(entry.path)}">\n`;
      const chars = [...entry.content];
      const truncated = chars.slice(0, EXISTING_MEMORY_CONTEXT_MAX_CHARS_PER_FILE).join("");
      out += truncated;
      if (chars.length > EXISTING_MEMORY_CONTEXT_MAX_CHARS_PER_FILE) {
        out += "\n...[truncated]";
      }
      out += "\n</file>\n\n";
    }
    if (total > EXISTING_MEMORY_CONTEXT_MAX_FILES) {
      out += `${total - EXISTING_MEMORY_CONTEXT_MAX_FILES} additional memory files omitted from this snapshot.\n`;
    }
    return out.trimEnd();
  }

  // ── op filtering ───────────────────────────────────────────────────

  static dedupeFileOps(ops: MemoryFileOp[]): MemoryFileOp[] {
    const deduped: MemoryFileOp[] = [];
    for (const op of ops) {
      const idx = deduped.findIndex((e) => e.path === op.path);
      if (idx >= 0) deduped.splice(idx, 1);
      deduped.push(op);
    }
    return deduped;
  }

  /**
   * Allowed write paths from a compaction response.
   *
   *  - `MEMORY.md` (workspace root) is intentionally allowed — compaction
   *    owns the conversational throughline.
   *  - All other writes must live under `memory/`. This keeps the
   *    protected workspace-root files (SOUL.md, USER.md, AGENTS.md, …)
   *    out of compaction's reach.
   *  - Dreaming-generated artifacts under `memory/` (`dreams.md`,
   *    `.dreams/`, `dreaming/`) are blocked so compaction doesn't stomp
   *    them.
   */
  static writeAllowedPath(p: string): boolean {
    const normalized = p.trim().replace(/^\.\//, "").replace(/\\/g, "/");
    const lower = normalized.toLowerCase();
    if (lower === "memory.md") return true;
    if (!normalized.startsWith("memory/")) return false;
    const rest = normalized.slice("memory/".length);
    if (rest.length === 0) return false;
    const restLower = rest.toLowerCase();
    return !(
      restLower === "dreams.md" ||
      restLower === "dreams" ||
      restLower === "dreams/" ||
      restLower.startsWith(".dreams/") ||
      restLower.startsWith("dreaming/")
    );
  }

  static filterFileOps(ops: MemoryFileOp[]): MemoryFileOp[] {
    return ops.filter((op) => CompactionManager.writeAllowedPath(op.path));
  }

  static isMemoryIndexPath(p: string): boolean {
    return normalizePromptVisiblePath(p) === MEMORY_INDEX_DEFERRED_PATH;
  }

  // ── compact() — the main pass ──────────────────────────────────────

  async compact(opts: CompactOptions): Promise<CompactionOutcome> {
    const {
      conversationId,
      messages,
      activeContent,
      isPrivate,
      systemTemplate,
      promptTemplate,
      charName,
      userName,
      llm,
      conversationManager,
      markdownStore,
      dryRun,
      keepTurnsOverride,
      cachedRequest,
      dataDir,
    } = opts;

    if (isPrivate) {
      throw new CompactionError("privateConversation", "private conversation: skipped");
    }
    if (messages.length === 0) {
      throw new CompactionError("insufficientMessages", "insufficient messages");
    }

    const keepTurns = keepTurnsOverride ?? this.config.keepRecentTurns;
    const splitAt = CompactionManager.findTurnSplit(messages, keepTurns);
    if (splitAt === 0) {
      throw new CompactionError("insufficientMessages", "insufficient messages");
    }
    const compactedPart = messages.slice(0, splitAt);

    if (!dryRun && markdownStore === undefined) {
      throw new CompactionError(
        "markdownStore",
        "markdown memory store not available",
      );
    }

    const existingMemoryContext =
      CompactionManager.buildExistingMemoryContext(markdownStore);

    const system = CompactionManager.buildSystem(systemTemplate, charName, userName);
    const llmMessages =
      cachedRequest !== undefined
        ? [
            {
              role: "user" as const,
              content: CompactionManager.buildFinalMessage(
                promptTemplate,
                existingMemoryContext,
                charName,
                userName,
              ),
            },
          ]
        : CompactionManager.buildMessages(
            promptTemplate,
            compactedPart,
            existingMemoryContext,
            charName,
            userName,
          );

    const rawResponse = await llm.summarize(system, llmMessages, cachedRequest);

    const parsed = parseCompactionResponse(rawResponse);
    const fileOps = CompactionManager.filterFileOps(
      CompactionManager.dedupeFileOps(parsed),
    );

    const compactedTurns = CompactionManager.countTurns(compactedPart);
    const retainedTurns = CompactionManager.countTurns(messages.slice(splitAt));

    if (dryRun) {
      return {
        kind: "dryRun",
        result: {
          wouldWriteFiles: fileOps.length,
          fileOpsPreview: fileOps,
          messageCount: splitAt,
          compactedTurns,
          retainedCount: messages.length - splitAt,
          retainedTurns,
          markdownPreview: fileOps.map((op) => op.path),
        },
      };
    }

    // We checked markdownStore above; narrow.
    const store = markdownStore as MarkdownMemoryStore;
    const workspaceDir = workspaceDirFromStore(store);

    const created: CompactionWriteState[] = [];
    let memoryIndexUpdated = false;

    try {
      for (const op of fileOps) {
        const isIndex = CompactionManager.isMemoryIndexPath(op.path);
        const resolved = isIndex
          ? path.join(workspaceDir, MEMORY_INDEX_FILE)
          : resolvePath(workspaceDir, op.path);

        const previousContent = readOptional(resolved);
        const displayPath = isIndex ? MEMORY_INDEX_FILE : op.path;
        created.push({ displayPath, resolved, previousContent });

        writeFileSync(resolved, op.content);
        if (isIndex) memoryIndexUpdated = true;
      }
    } catch (e) {
      await rollbackCompaction(created);
      throw new CompactionError("markdownStore", (e as Error).message);
    }

    const retained = messages.length - splitAt;
    let newConversationId: string;
    try {
      newConversationId = await conversationManager.archiveAndRetain(
        conversationId,
        { keepLastN: retained, activeContent } satisfies RetentionParams,
      );
    } catch (e) {
      await rollbackCompaction(created);
      if (e instanceof CompactionError) throw e;
      throw new CompactionError("conversationManager", (e as Error).message);
    }

    const markdownPaths = created.map((s) => s.displayPath);

    if (memoryIndexUpdated) {
      if (dataDir !== undefined) {
        try {
          noteMemoryIndexDeferred(path.join(dataDir, charName));
        } catch (e) {
          console.warn(
            `compaction: failed to queue MEMORY.md prompt refresh: ${(e as Error).message}`,
          );
        }
      } else {
        console.warn(
          "compaction: MEMORY.md updated but data_dir was unavailable for prompt refresh queue",
        );
      }
    }

    if (dataDir !== undefined) {
      const body =
        `Compacted ${compactedTurns} turns from \`${conversationId}\`.\n\n` +
        `Updated memory files:\n${markdownPaths.map((p) => `- \`${p}\``).join("\n")}`;
      try {
        await appendDreamEntry(dataDir, charName, new Date(), "compaction", body);
      } catch (e) {
        console.warn(
          `compaction: failed to append dreams log entry: ${(e as Error).message}`,
        );
      }
    }

    return {
      kind: "compacted",
      result: {
        memoryFilesWritten: [...markdownPaths],
        conversationId,
        newConversationId,
        messageCount: splitAt,
        compactedTurns,
        retainedCount: retained,
        retainedTurns,
        markdownPaths,
      },
    };
  }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function escapeAttr(text: string): string {
  return text
    .replaceAll("&", "&amp;")
    .replaceAll('"', "&quot;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
}

function workspaceDirFromStore(store: MarkdownMemoryStore): string {
  const base = store.baseDir();
  const parent = path.dirname(base);
  if (parent === base) {
    throw new CompactionError(
      "markdownStore",
      `memory store has no workspace parent: ${base}`,
    );
  }
  return parent;
}

function readOptional(p: string): string | undefined {
  try {
    return fs.readFileSync(p, "utf8");
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw e;
  }
}

function writeFileSync(p: string, content: string): void {
  const parent = path.dirname(p);
  fs.mkdirSync(parent, { recursive: true });
  fs.writeFileSync(p, content);
}

/**
 * Compensating-delete rollback. Iterates in reverse: restores prior
 * content if the file existed before compaction, otherwise deletes the
 * file. Per-state errors are logged + skipped — rollback proceeds.
 */
async function rollbackCompaction(
  created: CompactionWriteState[],
): Promise<void> {
  for (let i = created.length - 1; i >= 0; i--) {
    const s = created[i]!;
    try {
      if (s.previousContent !== undefined) {
        writeFileSync(s.resolved, s.previousContent);
      } else {
        try {
          fs.rmSync(s.resolved);
        } catch (e) {
          if ((e as NodeJS.ErrnoException).code !== "ENOENT") throw e;
        }
      }
    } catch (e) {
      console.warn(
        `rollback: failed for ${s.displayPath}: ${(e as Error).message}`,
      );
    }
  }
}
