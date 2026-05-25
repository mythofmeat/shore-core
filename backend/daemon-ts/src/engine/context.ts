/**
 * Per-turn chat context loader.
 *
 * Ensures the active-prompt snapshot exists, reads SOUL.md / USER.md /
 * AGENTS.md / TOOLS.md / MEMORY.md from that snapshot, calls
 * `assemblePrompt`, and returns the assembled prompt ready for the LLM
 * call. Mirrors `backend/daemon/src/handler/context.rs::prepare_chat_context`.
 */

import {
  AGENTS_FILE,
  ensureActivePromptSnapshot,
  loadActivePromptFile,
  loadMemoryIndex,
  SOUL_FILE,
  TOOLS_FILE,
  USER_FILE,
} from "../memory/deferred_edits.ts";

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

/**
 * Load the character's active prompt files and assemble the prompt.
 * Missing files become `undefined` for that slot, exactly as the Rust
 * impl does.
 */
export function buildChatContext(params: ChatContextParams): ChatContext {
  try {
    ensureActivePromptSnapshot(
      params.characterDataDir,
      params.configDir,
      params.characterName,
    );
  } catch (e) {
    console.warn(
      `[shore-daemon-ts] failed to prepare active prompt snapshot for ${params.characterName}: ${(e as Error).message}`,
    );
  }

  const characterDefinition = loadActivePromptFile(params.characterDataDir, SOUL_FILE);
  const userDefinition = loadActivePromptFile(params.characterDataDir, USER_FILE);
  const systemPrompt = loadActivePromptFile(params.characterDataDir, AGENTS_FILE);
  const toolsGuidance = loadActivePromptFile(params.characterDataDir, TOOLS_FILE);
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
