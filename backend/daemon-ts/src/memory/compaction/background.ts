/**
 * Background compaction entry point — `runCompaction`.
 *
 * Port of `backend/daemon/src/memory/compaction/background.rs::run_compaction`.
 *
 * Acquires the single-flight compaction guard for the character, loads
 * `active.jsonl` (parsing messages + capturing raw content in one read to
 * eliminate the TOCTOU window the Rust impl also addresses), runs a
 * compaction pass via `CompactionManager`, and on success calls
 * `applyDeferredEdits` so the MEMORY.md prompt refresh queued during the
 * pass becomes prompt-active.
 *
 * Differences from Rust today:
 *  - The TS daemon has no autonomy manager, so no `cachedRequest` is
 *    threaded through. The fresh-prefix LLM path is always used. See
 *    `RealCompactionLlm` for the corresponding TS-side note.
 *  - The Rust impl also fires a `NotificationEvent::CompactionComplete`;
 *    the TS notification surface lands with Phase 8, so it's logged
 *    instead of dispatched.
 */

import fs from "node:fs";
import path from "node:path";

import { loadActiveMessages } from "../../engine/messages.ts";
import type { Message } from "../../engine/types.ts";
import {
  applyDeferredEdits,
  characterMemoryDir,
} from "../deferred_edits.ts";
import { MarkdownMemoryStore } from "../markdown_store.ts";

import { CompactionManager } from "./manager.ts";
import { tryBeginCompaction } from "./lock.ts";
import { RealConversationManager } from "./conversation_manager.ts";
import {
  DEFAULT_COMPACT_PROMPT,
  DEFAULT_COMPACT_SYSTEM,
} from "./parser.ts";
import {
  type CompactionConfig,
  type CompactionLlm,
  type CompactionOutcome,
  type ConversationMessage,
} from "./types.ts";

const ACTIVE_FILE = "active.jsonl";

export interface RunCompactionOptions {
  character: string;
  /** Daemon `dirs.data`. */
  dataDir: string;
  /** Daemon `dirs.config`. */
  configDir: string;
  /** Effective compaction config (merged per-character + global). */
  config: CompactionConfig;
  displayName: string;
  llm: CompactionLlm;
  /** Optional prompt template overrides (fall back to bundled defaults). */
  systemTemplate?: string;
  promptTemplate?: string;
}

export interface RunCompactionResult {
  retainedTurns: number;
  /** Empty when there was nothing to compact. */
  outcome?: CompactionOutcome;
}

/**
 * Run a single compaction pass. Throws if another pass is already in
 * flight for the same character data root.
 *
 * Returns the retained-turn count, mirroring Rust's
 * `run_compaction(...) -> Result<usize, _>`.
 */
export async function runCompaction(
  opts: RunCompactionOptions,
): Promise<RunCompactionResult> {
  const guard = tryBeginCompaction(opts.dataDir, opts.character);
  if (guard === undefined) {
    throw new Error(`compaction already running for ${opts.character}`);
  }
  try {
    const characterDir = path.join(opts.dataDir, opts.character);
    const activePath = path.join(characterDir, ACTIVE_FILE);

    let raw: string;
    try {
      raw = fs.readFileSync(activePath, "utf8");
    } catch (e) {
      if ((e as NodeJS.ErrnoException).code === "ENOENT") raw = "";
      else throw e;
    }
    const messages: Message[] = loadActiveMessages(activePath);
    const convMessages: ConversationMessage[] = messages.map((m) => ({
      role: m.role,
      content: m.content,
      timestamp: m.timestamp,
      isToolResultOnly: isToolResultOnly(m),
    }));

    if (convMessages.length === 0) {
      return { retainedTurns: 0 };
    }

    const memoryDir = characterMemoryDir(opts.configDir, opts.character);
    let markdownStore: MarkdownMemoryStore | undefined;
    try {
      markdownStore = MarkdownMemoryStore.open(memoryDir);
    } catch {
      markdownStore = undefined;
    }

    const convMgr = new RealConversationManager(characterDir);
    const mgr = new CompactionManager(opts.config);

    const outcome = await mgr.compact({
      conversationId: opts.character,
      messages: convMessages,
      activeContent: raw,
      isPrivate: false,
      systemTemplate: opts.systemTemplate ?? DEFAULT_COMPACT_SYSTEM,
      promptTemplate: opts.promptTemplate ?? DEFAULT_COMPACT_PROMPT,
      charName: opts.character,
      userName: opts.displayName,
      llm: opts.llm,
      conversationManager: convMgr,
      markdownStore,
      dryRun: false,
      dataDir: opts.dataDir,
    });

    if (outcome.kind === "compacted") {
      try {
        applyDeferredEdits(characterDir, opts.configDir, opts.character);
      } catch (e) {
        console.warn(
          `compaction: applyDeferredEdits failed for ${opts.character}: ${(e as Error).message}`,
        );
      }
      return { retainedTurns: outcome.result.retainedTurns, outcome };
    }
    return { retainedTurns: 0, outcome };
  } finally {
    guard.release();
  }
}

function isToolResultOnly(m: Message): boolean {
  if (m.role !== "user") return false;
  if (m.content_blocks.length === 0) return false;
  return m.content_blocks.every((b) => b.type === "tool_result");
}
