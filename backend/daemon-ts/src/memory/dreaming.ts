/**
 * AI librarian dreaming pass.
 *
 * Port of the production `run_librarian_sweep` path in
 * `backend/daemon/src/memory/dreaming.rs`. The older deterministic
 * diagnostic sweep is intentionally not ported here; scheduled/commanded
 * dreaming in Rust uses this AI-librarian path.
 */

import fs from "node:fs";
import path from "node:path";

import type { ConversationEngine } from "../engine/engine.ts";
import type { ContentBlock } from "../engine/types.ts";
import type { Embedder } from "../llm/embed.ts";
import type { ChatEvent, ChatRequest, ProviderClient, TurnMessage, UsageStats } from "../llm/types.ts";
import type { ResolvedModel } from "../llm/catalog.ts";
import {
  defaultRegistry,
  defaultRetrievalConfig,
  defaultSearchConfig,
  normalizeRetrievalConfig,
  ToolRegistry,
  type ImageGenConfig,
  type RetrievalConfig,
  type SearchConfig,
  type ToolContext,
} from "../tools/registry.ts";
import {
  appendDreamEntry,
  dreamsLogPath,
} from "./dreams_log.ts";
import {
  characterMemoryDir,
  characterWorkspaceDir,
  ensureActivePromptSnapshot,
  loadActivePromptFile,
  MEMORY_INDEX_FILE,
  noteMemoryIndexDeferred,
  SOUL_FILE,
  USER_FILE,
} from "./deferred_edits.ts";
import {
  MarkdownMemoryStore,
  MarkdownStoreError,
  type MarkdownEntry,
} from "./markdown_store.ts";
import { workspaceIndexPath as buildWorkspaceIndexPath } from "./workspace_index.ts";

const DREAM_DATA_DIR = "dreams";
const DREAM_STATE_FILE = "state.json";
const DREAM_STATE_REL = "dreams/state.json";
const LEGACY_DREAM_STATE_REL = ".dreams/state.json";
const MAX_INDEX_FILES = 40;

const LIBRARIAN_TEMPLATE = `You are {{character}}, running a private memory maintenance pass for {{display_name}}.

This is not a chat turn. Do not send a user-facing message.

You are running a character-led memory librarian pass. Your task is to make your markdown memory easier for future-you to search and recall.

Use your memory tools to inspect existing files before changing them. Organize durable long-term facts so they are easy to find. Prefer updating existing files over creating duplicates. Move durable facts out of daily or raw notes into appropriate long-term files when useful. Deduplicate repeated facts. Mark stale, superseded, or incorrect information clearly rather than preserving contradictions as equally current. Leave uncertain cases in a review or needs-review area.

Maintain \`MEMORY.md\` (at the workspace root, alongside \`SOUL.md\`/\`USER.md\`/\`AGENTS.md\`/\`TOOLS.md\`/\`HEARTBEAT.md\`) as the prompt-visible memory index. It should include:
- an overview of important memory files and what they contain
- recently updated or worth-reading files
- ongoing conversational throughlines that remain relevant
- unresolved memory-maintenance questions or contradictions

\`MEMORY.md\` is not the full memory itself. It must not duplicate \`SOUL.md\`, \`USER.md\`, \`AGENTS.md\`, \`TOOLS.md\`, or \`HEARTBEAT.md\`; those are protected prompt files with separate roles.
\`MEMORY.md\` is prompt-visible through an active snapshot. Updating \`MEMORY.md\` changes the canonical file now, but the new index only becomes prompt-active after the next compaction boundary.

Finish with a concise summary covering:
- files inspected
- files changed
- important moves, dedupes, or supersessions
- unresolved issues
- whether \`MEMORY.md\` was updated

The daemon writes a timestamped audit entry to the dreams log automatically once you finish -- you do not (and cannot) write \`DREAMS.md\` yourself.

Generated dreaming artifacts are not durable memory sources. Do not mine legacy \`.dreams/**\`, \`dreams.md\`, \`MEMORY.md\`, or \`dreaming/**\` as facts; you may read \`MEMORY.md\` for index continuity.

You may edit any workspace file, including the protected prompt files (\`SOUL.md\`, \`USER.md\`, \`AGENTS.md\`, \`TOOLS.md\`, \`HEARTBEAT.md\`). Edits to those files are staged through an active-prompt snapshot and take effect at the next compaction or reload boundary, not immediately within this pass. Be deliberate when changing them.

The memory folder is self-organizing. Do not impose a rigid folder taxonomy. Inspect the existing layout and improve it sympathetically.`;

export interface DreamingConfig {
  enabled: boolean;
  frequency: string;
  max_tool_rounds: number;
}

export function defaultDreamingConfig(): DreamingConfig {
  return {
    enabled: false,
    frequency: "0 3 * * *",
    max_tool_rounds: 12,
  };
}

export interface DreamState {
  last_run_at?: string;
  runs: number;
  last_candidates_path?: string;
  last_signals_path?: string;
  last_promotions_path?: string;
  seen_candidates: Record<string, unknown>;
}

export interface DreamPhaseSummary {
  phase: string;
  summary: string;
  candidate_count: number;
  promoted_count: number;
  rejected_count: number;
  paths: string[];
}

export interface DreamSweepResult {
  character: string;
  dry_run: boolean;
  ran_at: string;
  mode: string;
  phase_summaries: DreamPhaseSummary[];
  candidate_count: number;
  indexed_count: number;
  promoted_count: number;
  rejected_count: number;
  candidates: unknown[];
  rem_themes: unknown[];
  promotions: unknown[];
  rejected: unknown[];
  indexed: string[];
  promoted: string[];
  paths_written: string[];
  would_write_paths: string[];
  staged_path?: string;
  dreams_path?: string;
  memory_path?: string;
  inspected: string[];
  changed: string[];
  tools_used: string[];
  tool_rounds: number;
  audit_appended: boolean;
  final_report?: string;
}

interface LibrarianLoopResult {
  finalReport?: string;
  inspected: string[];
  changed: string[];
  toolsUsed: string[];
  toolRounds: number;
  calls: LibrarianCall[];
}

interface LibrarianCall {
  usage: UsageStats;
  stopReason: string;
  totalMs: number;
  ttftMs: number;
}

export interface LibrarianSweepOptions {
  configDir: string;
  dataDir: string;
  cacheDir: string;
  character: string;
  displayName: string;
  resolved: ResolvedModel;
  apiKey: string;
  provider: ProviderClient;
  engine: ConversationEngine;
  dreamingConfig?: DreamingConfig;
  dryRun?: boolean;
  force?: boolean;
  searchConfig?: SearchConfig;
  retrievalConfig?: RetrievalConfig;
  imageGenConfig?: ImageGenConfig;
  embedder?: Embedder;
}

type MemorySnapshot = Map<string, string>;

export async function runLibrarianSweep(
  opts: LibrarianSweepOptions,
): Promise<DreamSweepResult | undefined> {
  const cfg = opts.dreamingConfig ?? defaultDreamingConfig();
  const dryRun = opts.dryRun ?? false;
  const force = opts.force ?? false;
  if (!force && !dryRun && !cfg.enabled) return undefined;

  const characterDataDir = path.join(opts.dataDir, opts.character);
  const memoryDir = characterMemoryDir(opts.configDir, opts.character);
  const workspaceDir = characterWorkspaceDir(opts.configDir, opts.character);
  const memoryIndexPath = path.join(workspaceDir, MEMORY_INDEX_FILE);
  const statePath = dreamStatePath(opts.dataDir, opts.character);
  const state = readState(opts.dataDir, opts.configDir, opts.character);
  const store = MarkdownMemoryStore.open(memoryDir);
  const before = snapshotMemoryFiles(store, memoryIndexPath);
  const ranAtDate = new Date();
  const ranAt = ranAtDate.toISOString();

  const request = buildLibrarianRequest(opts, characterDataDir, dryRun, ranAt);
  const registry = defaultRegistry({
    characterName: opts.character,
    displayName: opts.displayName,
  });
  const toolContext = buildLibrarianToolContext(opts, registry);

  const loopResult = await runPrivateLibrarianLoop({
    provider: opts.provider,
    request,
    registry,
    toolContext,
    character: opts.character,
    maxToolRounds: cfg.max_tool_rounds,
    dryRun,
  });
  if (dryRun) {
    return {
      character: opts.character,
      dry_run: true,
      ran_at: ranAt,
      mode: "ai_librarian",
      phase_summaries: [
        {
          phase: "librarian",
          summary: `dry-run AI librarian pass inspected memory with ${loopResult.toolRounds} tool round(s); writes were disabled`,
          candidate_count: 0,
          promoted_count: 0,
          rejected_count: 0,
          paths: [],
        },
      ],
      candidate_count: 0,
      indexed_count: 0,
      promoted_count: 0,
      rejected_count: 0,
      candidates: [],
      rem_themes: [],
      promotions: [],
      rejected: [],
      indexed: [],
      promoted: [],
      paths_written: [],
      would_write_paths: [
        memoryIndexPath,
        dreamsLogPath(opts.dataDir, opts.character),
        statePath,
      ],
      inspected: loopResult.inspected,
      changed: [],
      tools_used: loopResult.toolsUsed,
      tool_rounds: loopResult.toolRounds,
      audit_appended: false,
      ...(loopResult.finalReport !== undefined
        ? { final_report: loopResult.finalReport }
        : {}),
    };
  }

  const memoryCreatedByFallback = ensureMemoryIndexAfterLibrarian(
    store,
    memoryIndexPath,
    opts.character,
    ranAt,
  );
  if (memoryCreatedByFallback) {
    noteMemoryIndexDeferred(characterDataDir);
  }

  await appendLibrarianAudit({
    dataDir: opts.dataDir,
    character: opts.character,
    timestamp: ranAtDate,
    ranAt,
    inspected: loopResult.inspected,
    changed: loopResult.changed,
    memoryCreatedByFallback,
    finalReport: loopResult.finalReport,
  });

  const nextState: DreamState = {
    ...state,
    last_run_at: ranAt,
    runs: state.runs + 1,
  };
  delete nextState.last_candidates_path;
  delete nextState.last_signals_path;
  delete nextState.last_promotions_path;
  writeState(opts.dataDir, opts.character, nextState);

  const after = snapshotMemoryFiles(store, memoryIndexPath);
  const changed = changedPaths(before, after);
  if (!changed.includes(DREAM_STATE_REL)) changed.push(DREAM_STATE_REL);
  const pathsWritten = changed.map((changedPath) => {
    if (changedPath === MEMORY_INDEX_FILE) return memoryIndexPath;
    if (changedPath === DREAM_STATE_REL) return statePath;
    return path.join(memoryDir, changedPath);
  });
  const indexedCount = after.has(MEMORY_INDEX_FILE) ? 1 : 0;

  return {
    character: opts.character,
    dry_run: false,
    ran_at: ranAt,
    mode: "ai_librarian",
    phase_summaries: [
      {
        phase: "librarian",
        summary: `AI librarian pass used ${loopResult.toolRounds} tool round(s), changed ${changed.length} file(s), and needed a DREAMS.md audit fallback`,
        candidate_count: 0,
        promoted_count: indexedCount,
        rejected_count: 0,
        paths: pathsWritten,
      },
    ],
    candidate_count: 0,
    indexed_count: indexedCount,
    promoted_count: 0,
    rejected_count: 0,
    candidates: [],
    rem_themes: [],
    promotions: [],
    rejected: [],
    indexed: indexedCount > 0 ? [MEMORY_INDEX_FILE] : [],
    promoted: [],
    paths_written: pathsWritten,
    would_write_paths: [],
    staged_path: statePath,
    dreams_path: dreamsLogPath(opts.dataDir, opts.character),
    memory_path: memoryIndexPath,
    inspected: loopResult.inspected,
    changed,
    tools_used: loopResult.toolsUsed,
    tool_rounds: loopResult.toolRounds,
    audit_appended: true,
    ...(loopResult.finalReport !== undefined
      ? { final_report: loopResult.finalReport }
      : {}),
  };
}

function buildLibrarianRequest(
  opts: LibrarianSweepOptions,
  characterDataDir: string,
  dryRun: boolean,
  ranAt: string,
): ChatRequest {
  try {
    ensureActivePromptSnapshot(characterDataDir, opts.configDir, opts.character);
  } catch (e) {
    console.warn(
      `[dreaming] failed to prepare active prompt snapshot for ${opts.character}: ${(e as Error).message}`,
    );
  }

  const characterDefinition = loadActivePromptFile(characterDataDir, SOUL_FILE);
  const userDefinition = loadActivePromptFile(characterDataDir, USER_FILE);
  const system = buildLibrarianPrompt(
    opts.character,
    opts.displayName,
    characterDefinition,
    userDefinition,
    dryRun,
    ranAt,
  );
  const userPrompt = dryRun
    ? "Run the dry-run memory librarian pass now. Inspect memory files with read-only tools and finish with a proposed plan. Do not write, edit, or emit a user-facing message."
    : "Run the memory librarian pass now. Use memory tools to inspect and improve workspace/memory, update MEMORY.md (at the workspace root), and finish with a concise summary of what you inspected and changed. The daemon writes the dreams audit log automatically; do not try to write DREAMS.md yourself. Do not emit a user-facing message.";

  return {
    system,
    messages: [{ role: "user", content: [{ type: "text", text: userPrompt }] }],
    tools: buildLibrarianToolDefs(opts.character, opts.displayName, dryRun),
    thinking: { enabled: false },
    cacheTtl: opts.resolved.cacheTtl ?? "",
    modelId: opts.resolved.modelId,
    apiKey: opts.apiKey,
    maxTokens: opts.resolved.maxTokens ?? 4096,
    ...(opts.resolved.baseUrl !== undefined
      ? { baseUrl: opts.resolved.baseUrl }
      : {}),
    ...(opts.resolved.temperature !== undefined
      ? { temperature: opts.resolved.temperature }
      : {}),
    ...(opts.resolved.topP !== undefined ? { topP: opts.resolved.topP } : {}),
  };
}

export function buildLibrarianPrompt(
  character: string,
  displayName: string,
  characterDefinition: string | undefined,
  userDefinition: string | undefined,
  dryRun: boolean,
  _ranAt: string,
): string {
  let prompt = `${LIBRARIAN_TEMPLATE.replaceAll("{{character}}", character).replaceAll(
    "{{display_name}}",
    displayName,
  )}\n`;

  if (dryRun) {
    prompt +=
      "\nThis is a dry run. Write and edit tools are unavailable. Inspect files and produce a concise internal plan of what would change.\n";
  }

  if (characterDefinition !== undefined && characterDefinition.trim().length > 0) {
    prompt += `\n<character_identity>\n${characterDefinition}\n</character_identity>\n`;
  }
  if (userDefinition !== undefined && userDefinition.trim().length > 0) {
    prompt += `\n<user_profile>\n${userDefinition}\n</user_profile>\n`;
  }
  return prompt;
}

function buildLibrarianToolDefs(
  character: string,
  displayName: string,
  dryRun: boolean,
): ChatRequest["tools"] {
  return defaultRegistry({ characterName: character, displayName })
    .list()
    .filter((tool) => librarianToolAllowed(tool.name, dryRun))
    .map((tool) => ({
      name: tool.name,
      description: tool.description,
      inputSchema: tool.inputSchema,
    }));
}

function librarianToolAllowed(name: string, dryRun: boolean): boolean {
  if (dryRun) {
    return ["read", "list_files", "file_search", "conversation_search", "check_time"].includes(
      name,
    );
  }
  return [
    "read",
    "write",
    "edit",
    "list_files",
    "file_search",
    "conversation_search",
    "check_time",
  ].includes(name);
}

function buildLibrarianToolContext(
  opts: LibrarianSweepOptions,
  _registry: ToolRegistry,
): ToolContext {
  const characterDataDir = path.join(opts.dataDir, opts.character);
  const retrievalConfig = normalizeRetrievalConfig(
    opts.retrievalConfig ?? defaultRetrievalConfig(),
  );
  return {
    characterName: opts.character,
    characterConfigDir: path.join(opts.configDir, "characters", opts.character),
    characterDataDir,
    workspaceDir: characterWorkspaceDir(opts.configDir, opts.character),
    configDir: opts.configDir,
    imageDir: path.join(characterDataDir, "images"),
    engine: opts.engine,
    searchConfig: opts.searchConfig ?? defaultSearchConfig(),
    retrievalConfig,
    ...(opts.imageGenConfig !== undefined
      ? { imageGenConfig: opts.imageGenConfig }
      : {}),
    ...(opts.embedder !== undefined ? { embedder: opts.embedder } : {}),
    ...(opts.embedder !== undefined
      ? { workspaceIndexPath: buildWorkspaceIndexPath(opts.cacheDir, opts.character) }
      : {}),
  };
}

async function runPrivateLibrarianLoop(opts: {
  provider: ProviderClient;
  request: ChatRequest;
  registry: ToolRegistry;
  toolContext: ToolContext;
  character: string;
  maxToolRounds: number;
  dryRun: boolean;
}): Promise<LibrarianLoopResult> {
  const result: LibrarianLoopResult = {
    inspected: [],
    changed: [],
    toolsUsed: [],
    toolRounds: 0,
    calls: [],
  };
  let messages = opts.request.messages;

  for (let iteration = 0; iteration < opts.maxToolRounds; iteration++) {
    const req: ChatRequest = { ...opts.request, messages };
    const response = await consumeStream(opts.provider.stream(req));
    result.calls.push({
      usage: response.usage,
      stopReason: response.stopReason,
      totalMs: response.totalMs,
      ttftMs: response.ttftMs,
    });
    rememberFinalReport(result, response.content);
    messages = [...messages, { role: "assistant", content: response.content }];

    const toolUses = response.content.filter(
      (block): block is Extract<ContentBlock, { type: "tool_use" }> =>
        block.type === "tool_use",
    );
    if (response.stopReason !== "tool_use" || toolUses.length === 0) {
      return result;
    }

    result.toolRounds += 1;
    const toolResults: ContentBlock[] = [];
    for (const toolUse of toolUses) {
      result.toolsUsed.push(toolUse.name);
      recordLibrarianToolIntent(result, toolUse.name, toolUse.input);

      const { output, isError } = await executeLibrarianTool(
        opts.registry,
        opts.toolContext,
        toolUse.name,
        toolUse.input,
        opts.dryRun,
      );
      if (!isError && (toolUse.name === "write" || toolUse.name === "edit")) {
        const p = toolPath(toolUse.input);
        if (p !== undefined) pushUnique(result.changed, p);
      }

      const block: ContentBlock = {
        type: "tool_result",
        tool_use_id: toolUse.id,
        content: output,
      };
      if (isError) block.is_error = true;
      toolResults.push(block);
    }
    messages = [...messages, { role: "user", content: toolResults }];
  }

  console.warn(
    `[dreaming] private librarian tool loop hit configured cap for ${opts.character}: ${opts.maxToolRounds}`,
  );
  return result;
}

async function consumeStream(
  events: AsyncIterable<ChatEvent>,
): Promise<{
  content: ContentBlock[];
  stopReason: string;
  usage: UsageStats;
  totalMs: number;
  ttftMs: number;
}> {
  const start = Date.now();
  let firstOutputAt: number | undefined;
  for await (const event of events) {
    if (event.kind !== "done" && firstOutputAt === undefined) {
      firstOutputAt = Date.now();
    }
    if (event.kind === "done") {
      const totalMs = Date.now() - start;
      return {
        content: event.content,
        stopReason: event.stopReason,
        usage: event.usage,
        totalMs,
        ttftMs: firstOutputAt === undefined ? totalMs : firstOutputAt - start,
      };
    }
  }
  throw new Error("provider stream ended without a 'done' event");
}

function rememberFinalReport(
  loopResult: LibrarianLoopResult,
  content: ContentBlock[],
): void {
  const text = content
    .filter((block): block is Extract<ContentBlock, { type: "text" }> => block.type === "text")
    .map((block) => block.text)
    .join("")
    .trim();
  if (text.length > 0) loopResult.finalReport = text;
}

async function executeLibrarianTool(
  registry: ToolRegistry,
  ctx: ToolContext,
  name: string,
  input: unknown,
  dryRun: boolean,
): Promise<{ output: string; isError: boolean }> {
  const blocked = blockedLibrarianToolResult(name, dryRun);
  if (blocked !== undefined) return blocked;
  if (!librarianToolAllowed(name, dryRun)) {
    return {
      output: `${name} is not available during private dreaming passes`,
      isError: true,
    };
  }
  const handler = registry.get(name);
  if (handler === undefined) {
    return { output: `unknown tool "${name}"`, isError: true };
  }
  try {
    return { output: await handler.execute(input, ctx), isError: false };
  } catch (e) {
    return { output: `error: ${(e as Error).message}`, isError: true };
  }
}

function blockedLibrarianToolResult(
  name: string,
  dryRun: boolean,
): { output: string; isError: boolean } | undefined {
  if (name === "exec") {
    return {
      output: "exec is not available during private dreaming passes",
      isError: true,
    };
  }
  if (dryRun && (name === "write" || name === "edit")) {
    return {
      output: "dry-run dreaming does not write or edit files",
      isError: true,
    };
  }
  return undefined;
}

function recordLibrarianToolIntent(
  result: LibrarianLoopResult,
  name: string,
  input: unknown,
): void {
  if (name === "read" || name === "list_files") {
    const p = toolPath(input);
    if (p !== undefined) pushUnique(result.inspected, p);
  } else if (name === "file_search" || name === "conversation_search") {
    const obj = asRecord(input);
    const query =
      typeof obj["query"] === "string" ? obj["query"] : "<missing query>";
    const scope = toolPath(input) ?? "memory";
    pushUnique(result.inspected, `${name}:${scope}:${query}`);
  }
}

function toolPath(input: unknown): string | undefined {
  const obj = asRecord(input);
  const p = obj["path"];
  return typeof p === "string" ? p : undefined;
}

function pushUnique(items: string[], value: string): void {
  if (!items.includes(value)) items.push(value);
}

function snapshotMemoryFiles(
  store: MarkdownMemoryStore,
  memoryIndexPath: string,
): MemorySnapshot {
  const snapshot: MemorySnapshot = new Map();
  for (const entry of store.listAll()) {
    snapshot.set(entry.path, entry.content);
  }
  try {
    snapshot.set(MEMORY_INDEX_FILE, fs.readFileSync(memoryIndexPath, "utf8"));
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code !== "ENOENT") throw e;
  }
  return snapshot;
}

function changedPaths(before: MemorySnapshot, after: MemorySnapshot): string[] {
  const keys = new Set([...before.keys(), ...after.keys()]);
  return [...keys].filter((key) => before.get(key) !== after.get(key)).sort();
}

function ensureMemoryIndexAfterLibrarian(
  store: MarkdownMemoryStore,
  memoryIndexPath: string,
  character: string,
  ranAt: string,
): boolean {
  try {
    const content = fs.readFileSync(memoryIndexPath, "utf8");
    if (content.trim().length > 0) return false;
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code !== "ENOENT") throw e;
  }
  writeFallbackMemoryIndex(store, memoryIndexPath, character, ranAt);
  return true;
}

function writeFallbackMemoryIndex(
  store: MarkdownMemoryStore,
  memoryIndexPath: string,
  character: string,
  ranAt: string,
): void {
  const entries = store.listAll();
  let body = "";
  body += "# Memory Index\n\n";
  body += "This file is the character's map of long-term memory. It is not the full memory itself.\n";
  body += "Use it to decide which memory files to inspect before answering.\n\n";
  body += "Core user facts and standing behavior guidance are already loaded from USER.md and AGENTS.md; do not duplicate them here unless needed as pointers to memory files.\n\n";
  body += `Character: ${character}\n`;
  body += `Last updated: ${ranAt}\n`;
  body += "Fallback note: daemon created this minimal index because the AI librarian pass did not leave a usable MEMORY.md.\n\n";
  body += "## Memory areas\n\n";
  if (entries.length === 0) {
    body += "- No ordinary memory files were found yet.\n";
  } else {
    for (const entry of entries.slice(0, MAX_INDEX_FILES)) {
      body += `- \`${entry.path}\` - ${memoryFileSummary(entry)}\n`;
    }
  }
  body += "\n## Recently updated files\n\n";
  body += "- Needs review during the next AI librarian pass.\n";
  body += "\n## Current conversational throughlines\n\n";
  body += "- Needs review during the next AI librarian pass.\n";
  body += "\n## Needs review\n\n";
  body += "- Previous dreaming pass did not update MEMORY.md directly.\n";

  fs.mkdirSync(path.dirname(memoryIndexPath), { recursive: true });
  fs.writeFileSync(memoryIndexPath, body);
}

async function appendLibrarianAudit(opts: {
  dataDir: string;
  character: string;
  timestamp: Date;
  ranAt: string;
  inspected: string[];
  changed: string[];
  memoryCreatedByFallback: boolean;
  finalReport: string | undefined;
}): Promise<void> {
  let body = `AI librarian dreaming pass at \`${opts.ranAt}\`.\n\n`;
  body += "Files inspected:\n";
  body += markdownListOrNone(opts.inspected);
  body += "\nFiles changed by tools:\n";
  body += markdownListOrNone(opts.changed);
  body += "\nMEMORY.md updated:\n";
  body += opts.memoryCreatedByFallback
    ? "- Yes, by daemon fallback (the model left it missing or empty).\n"
    : "- Present after the pass.\n";
  if (opts.finalReport !== undefined && opts.finalReport.trim().length > 0) {
    body += "\nFinal internal report:\n";
    body += `${opts.finalReport.trim()}\n`;
  }

  await appendDreamEntry(
    opts.dataDir,
    opts.character,
    opts.timestamp,
    "AI librarian dreaming pass",
    body,
  );
}

function markdownListOrNone(items: string[]): string {
  if (items.length === 0) return "- None recorded.\n";
  return items.map((item) => `- \`${item.replaceAll("`", "'")}\``).join("\n") + "\n";
}

function readState(
  dataDir: string,
  configDir: string,
  character: string,
): DreamState {
  const statePath = dreamStatePath(dataDir, character);
  try {
    return normalizeState(JSON.parse(fs.readFileSync(statePath, "utf8")));
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code !== "ENOENT") throw e;
  }

  const store = MarkdownMemoryStore.open(characterMemoryDir(configDir, character));
  try {
    return normalizeState(JSON.parse(store.read(LEGACY_DREAM_STATE_REL).content));
  } catch (e) {
    if (e instanceof MarkdownStoreError && e.kind === "notFound") {
      return defaultDreamState();
    }
    throw e;
  }
}

function writeState(dataDir: string, character: string, state: DreamState): void {
  const p = dreamStatePath(dataDir, character);
  fs.mkdirSync(path.dirname(p), { recursive: true });
  fs.writeFileSync(p, JSON.stringify(state, null, 2));
}

function normalizeState(raw: unknown): DreamState {
  const obj = asRecord(raw);
  return {
    ...(typeof obj["last_run_at"] === "string"
      ? { last_run_at: obj["last_run_at"] }
      : {}),
    runs: typeof obj["runs"] === "number" ? obj["runs"] : 0,
    ...(typeof obj["last_candidates_path"] === "string"
      ? { last_candidates_path: obj["last_candidates_path"] }
      : {}),
    ...(typeof obj["last_signals_path"] === "string"
      ? { last_signals_path: obj["last_signals_path"] }
      : {}),
    ...(typeof obj["last_promotions_path"] === "string"
      ? { last_promotions_path: obj["last_promotions_path"] }
      : {}),
    seen_candidates: asRecord(obj["seen_candidates"]),
  };
}

function defaultDreamState(): DreamState {
  return { runs: 0, seen_candidates: {} };
}

function dreamStatePath(dataDir: string, character: string): string {
  return path.join(dataDir, character, DREAM_DATA_DIR, DREAM_STATE_FILE);
}

function memoryFileSummary(entry: MarkdownEntry): string {
  const title =
    entry.content
      .split("\n")
      .find((line) => {
        const trimmed = line.trim();
        return trimmed.startsWith("#") && trimmed.replace(/^#+/u, "").trim().length > 0;
      })
      ?.replace(/^#+/u, "")
      .trim() ?? "untitled";
  const detail = entry.content
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0 && !line.startsWith("#"))
    .map(stripListMarker)
    .find((line) => line.length > 0);
  return detail !== undefined ? `${title}; ${detail}` : title;
}

function stripListMarker(text: string): string {
  for (const prefix of ["- [ ] ", "- [x] ", "- [X] ", "- ", "* ", "+ ", "> "]) {
    if (text.startsWith(prefix)) return text.slice(prefix.length).trim();
  }
  const ordered = /^([0-9]{1,3})\. (.*)$/u.exec(text);
  if (ordered !== null) return ordered[2]!.trim();
  return text;
}

function asRecord(v: unknown): Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v)
    ? (v as Record<string, unknown>)
    : {};
}
