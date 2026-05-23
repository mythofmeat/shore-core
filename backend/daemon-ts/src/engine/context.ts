/**
 * Per-turn chat context loader.
 *
 * Reads the four prompt files (SOUL.md / USER.md / AGENTS.md / TOOLS.md)
 * + MEMORY.md from the character's workspace, calls `assemblePrompt`, and
 * returns the assembled prompt ready for the LLM call. Mirrors
 * `backend/daemon/src/handler/context.rs::prepare_chat_context` but
 * skipped the deferred-edits snapshot dance — until Phase 6 lands real
 * compaction-time file edits, reading directly from workspace is
 * stable-enough across a single turn.
 */
import fs from "node:fs";
import path from "node:path";

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
  memory: "MEMORY.md",
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
  const memoryIndex = tryRead(path.join(workspace, PROMPT_FILES.memory));

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
