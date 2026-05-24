/**
 * Per-turn chat context loader.
 *
 * Reads the four prompt files (SOUL.md / USER.md / AGENTS.md / TOOLS.md)
 * from the character's workspace and MEMORY.md from the active-prompt
 * snapshot, calls `assemblePrompt`, and returns the assembled prompt
 * ready for the LLM call. Mirrors
 * `backend/daemon/src/handler/context.rs::prepare_chat_context`.
 *
 * Why the active snapshot for MEMORY.md: writes to MEMORY.md (compaction
 * + workspace.write/edit on the prompt-visible index) hit disk
 * immediately, but the deferred-edits queue holds them out of the prompt
 * until the next compaction boundary fires `applyDeferredEdits`. The
 * active snapshot is the version the prompt reads. SOUL/USER/AGENTS/
 * TOOLS still come straight from workspace for now — direct reads are
 * stable enough within a single turn, and protected-file edits land on
 * the same deferred queue (the workspace copy and the snapshot drift
 * out of sync until apply, but the active snapshot file isn't yet
 * consulted for those slots — that wiring lands when the snapshot path
 * grows ergonomic helpers for the four non-MEMORY files).
 */
import fs from "node:fs";
import path from "node:path";

import { loadMemoryIndex } from "../memory/deferred_edits.ts";

import {
  type AssembledPrompt,
  assemblePrompt,
  type PromptParams,
} from "./prompt.ts";
import type { Message } from "./types.ts";

export interface ChatContextParams {
  characterName: string;
  /** `$CONFIG_DIR/characters/<character>/`. */
  characterConfigDir: string;
  /** `$CONFIG_DIR` itself — required for the MEMORY.md active-snapshot read. */
  configDir: string;
  /** `<dataDir>/<character>/` — required for the MEMORY.md active-snapshot read. */
  characterDataDir: string;
  displayName: string;
  isPrivate: boolean;
  hasPriorContext: boolean;
  messages: Message[];
  maxContextTokens?: number;
  maxOutputTokens?: number;
}

export interface ChatContext {
  prompt: AssembledPrompt;
  characterDefinition: string | undefined;
  userDefinition: string | undefined;
}

const PROMPT_FILES = {
  soul: "SOUL.md",
  user: "USER.md",
  agents: "AGENTS.md",
  tools: "TOOLS.md",
} as const;

/** Read a file or return undefined if it doesn't exist. */
function tryRead(file: string): string | undefined {
  try {
    return fs.readFileSync(file, "utf8");
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw e;
  }
}

/**
 * Load the character's prompt files from `<characterConfigDir>/workspace/`
 * and assemble the prompt. Missing files become `undefined` for that
 * slot, exactly as the Rust impl does.
 */
export function buildChatContext(params: ChatContextParams): ChatContext {
  const workspace = path.join(params.characterConfigDir, "workspace");

  const characterDefinition = tryRead(path.join(workspace, PROMPT_FILES.soul));
  const userDefinition = tryRead(path.join(workspace, PROMPT_FILES.user));
  const systemPrompt = tryRead(path.join(workspace, PROMPT_FILES.agents));
  const toolsGuidance = tryRead(path.join(workspace, PROMPT_FILES.tools));
  const memoryIndex = loadMemoryIndex(
    params.characterDataDir,
    params.configDir,
    params.characterName,
  );

  const promptParams: PromptParams = {
    character_name: params.characterName,
    display_name: params.displayName,
    is_private: params.isPrivate,
    has_prior_context: params.hasPriorContext,
    messages: params.messages,
    ...(systemPrompt !== undefined ? { system_prompt: systemPrompt } : {}),
    ...(toolsGuidance !== undefined ? { tools_guidance: toolsGuidance } : {}),
    ...(characterDefinition !== undefined
      ? { character_definition: characterDefinition }
      : {}),
    ...(userDefinition !== undefined ? { user_definition: userDefinition } : {}),
    ...(memoryIndex !== undefined ? { memory_index: memoryIndex } : {}),
    ...(params.maxContextTokens !== undefined
      ? { max_context_tokens: params.maxContextTokens }
      : {}),
    ...(params.maxOutputTokens !== undefined
      ? { max_output_tokens: params.maxOutputTokens }
      : {}),
  };

  const prompt = assemblePrompt(promptParams);

  return {
    prompt,
    characterDefinition,
    userDefinition,
  };
}
