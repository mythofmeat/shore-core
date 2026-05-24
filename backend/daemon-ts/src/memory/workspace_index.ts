/**
 * Workspace-wide embedding index + hybrid search.
 *
 * Port of `backend/daemon/src/memory/workspace_index.rs`. The persisted
 * index is a JSON file under the Shore cache dir. It is rebuildable and
 * non-authoritative: stale vectors are refreshed from workspace files, and
 * vanished files are pruned during each query.
 */

import fs from "node:fs";
import path from "node:path";

import { atomicWrite } from "../engine/atomic.ts";
import type { Embedder } from "../llm/embed.ts";
import type { RetrievalConfig } from "../tools/registry.ts";

const EMBED_BATCH_MAX_ITEMS = 32;
const EMBED_BATCH_MAX_CHARS = 96_000;
const INDEX_FILE = "workspace_index.json";

export type HybridMode = "hybrid" | "vector";

export interface IndexedEntry {
  hash: string;
  size: number;
  modified_at_secs: number;
  model_id: string;
  max_embed_chars_per_file?: number;
  embedded: boolean;
  reason?: string;
  embedding: number[];
}

export interface WorkspaceIndex {
  entries: Record<string, IndexedEntry>;
}

export interface ScoredFile {
  displayPath: string;
  fsPath: string;
  content?: string;
  semanticScore?: number;
  lexicalScore: number;
  combinedScore: number;
  embedded: boolean;
  skipReason?: string;
}

export interface HybridSearchResult {
  files: ScoredFile[];
  searchedFiles: number;
  embeddedFiles: number;
  skippedBinaryOrLarge: number;
}

interface FileCandidate {
  displayPath: string;
  fsPath: string;
  size: number;
  modifiedAtSecs: number;
  content?: string;
  skipReason?: string;
}

class AsyncMutex {
  private tail: Promise<void> = Promise.resolve();

  async runExclusive<T>(fn: () => Promise<T>): Promise<T> {
    const previous = this.tail;
    let release!: () => void;
    this.tail = new Promise<void>((resolve) => {
      release = resolve;
    });
    await previous.catch(() => undefined);
    try {
      return await fn();
    } finally {
      release();
    }
  }
}

const indexLocks = new Map<string, AsyncMutex>();

export function workspaceIndexPath(cacheDir: string, character: string): string {
  return path.join(cacheDir, "characters", character, INDEX_FILE);
}

export async function hybridSearch(
  workspaceDir: string,
  retrievalConfig: RetrievalConfig,
  query: string,
  mode: HybridMode,
  embedder: Embedder,
  indexPath: string,
  pathFilter?: string,
): Promise<HybridSearchResult> {
  if (workspaceDir.length === 0) {
    throw new Error("workspace not configured");
  }
  if (!fs.existsSync(workspaceDir)) {
    return {
      files: [],
      searchedFiles: 0,
      embeddedFiles: 0,
      skippedBinaryOrLarge: 0,
    };
  }

  const lock = indexLockFor(indexPath);
  return lock.runExclusive(async () => {
    let index = loadIndex(indexPath);
    const modelId = embedder.modelId();
    let candidates = enumerateFiles(workspaceDir, retrievalConfig);
    let skippedBinaryOrLarge = 0;
    let indexDirty = false;

    const current = new Set(candidates.map((f) => f.displayPath));
    const retained: Record<string, IndexedEntry> = {};
    for (const [entryPath, entry] of Object.entries(index.entries)) {
      if (current.has(entryPath)) retained[entryPath] = entry;
    }
    if (Object.keys(retained).length !== Object.keys(index.entries).length) {
      index = { entries: retained };
      indexDirty = true;
    }

    const scopePrefix = pathFilter?.replace(/\/+$/, "");
    if (scopePrefix !== undefined && scopePrefix.length > 0) {
      const withSlash = `${scopePrefix}/`;
      candidates = candidates.filter(
        (f) => f.displayPath === scopePrefix || f.displayPath.startsWith(withSlash),
      );
    }

    const stale: Array<{ displayPath: string; size: number; mtime: number }> = [];
    const staleDocs: string[] = [];

    for (const file of candidates) {
      if (file.skipReason === "oversize") {
        skippedBinaryOrLarge += 1;
        index.entries[file.displayPath] = {
          hash: skipTag(file.size, file.modifiedAtSecs),
          size: file.size,
          modified_at_secs: file.modifiedAtSecs,
          model_id: modelId,
          max_embed_chars_per_file: retrievalConfig.max_embed_chars_per_file,
          embedded: false,
          reason: "oversize",
          embedding: [],
        };
        indexDirty = true;
        continue;
      }

      const entry = index.entries[file.displayPath];
      const fresh =
        entry !== undefined &&
        entry.embedded &&
        entry.size === file.size &&
        entry.modified_at_secs === file.modifiedAtSecs &&
        entry.model_id === modelId &&
        entry.max_embed_chars_per_file === retrievalConfig.max_embed_chars_per_file;

      let bytes: Buffer;
      try {
        bytes = fs.readFileSync(file.fsPath);
      } catch {
        file.skipReason = "read failed";
        if (index.entries[file.displayPath] !== undefined) {
          delete index.entries[file.displayPath];
          indexDirty = true;
        }
        continue;
      }

      let text: string;
      try {
        text = decodeUtf8(bytes);
      } catch {
        skippedBinaryOrLarge += 1;
        const reason = binarySkipReason(retrievalConfig.binary);
        file.skipReason = reason;
        index.entries[file.displayPath] = {
          hash: skipTag(file.size, file.modifiedAtSecs),
          size: file.size,
          modified_at_secs: file.modifiedAtSecs,
          model_id: modelId,
          max_embed_chars_per_file: retrievalConfig.max_embed_chars_per_file,
          embedded: false,
          reason,
          embedding: [],
        };
        indexDirty = true;
        continue;
      }

      if (!fresh) {
        stale.push({
          displayPath: file.displayPath,
          size: file.size,
          mtime: file.modifiedAtSecs,
        });
        staleDocs.push(
          documentForEmbedding(
            file.displayPath,
            text,
            retrievalConfig.max_embed_chars_per_file,
          ),
        );
      }
      file.content = text;
    }

    if (indexDirty) {
      saveIndex(indexPath, index);
      indexDirty = false;
    }

    if (staleDocs.length > 0) {
      const vectors = await embedDocuments(embedder, staleDocs);
      if (vectors.length !== stale.length) {
        throw new Error(
          `embedding count mismatch: got ${vectors.length}, expected ${stale.length}`,
        );
      }
      for (let i = 0; i < stale.length; i++) {
        const staleFile = stale[i]!;
        index.entries[staleFile.displayPath] = {
          hash: skipTag(staleFile.size, staleFile.mtime),
          size: staleFile.size,
          modified_at_secs: staleFile.mtime,
          model_id: modelId,
          max_embed_chars_per_file: retrievalConfig.max_embed_chars_per_file,
          embedded: true,
          embedding: vectors[i]!,
        };
      }
      indexDirty = true;
    }

    if (indexDirty) {
      saveIndex(indexPath, index);
    }

    const queryVectors = await embedder.embed([query]);
    const queryVector = queryVectors[0];
    if (queryVector === undefined) {
      throw new Error("embedding response did not include query vector");
    }

    const qLower = query.toLowerCase();
    const terms = tokenizeQuery(qLower);
    const scored = candidates.map((file): ScoredFile => {
      const lexical =
        file.content !== undefined
          ? lexicalScore(file.displayPath, file.content, qLower, terms)
          : 0;
      const entry = index.entries[file.displayPath];
      const semantic =
        entry !== undefined && entry.embedded
          ? cosineSimilarity(queryVector, entry.embedding)
          : undefined;
      return {
        displayPath: file.displayPath,
        fsPath: file.fsPath,
        ...(file.content !== undefined ? { content: file.content } : {}),
        ...(semantic !== undefined ? { semanticScore: semantic } : {}),
        lexicalScore: lexical,
        combinedScore: 0,
        embedded: entry?.embedded ?? false,
        ...(file.skipReason !== undefined ? { skipReason: file.skipReason } : {}),
      };
    });

    const maxLex = Math.max(1, ...scored.map((f) => f.lexicalScore));
    const embeddedFiles = scored.filter((f) => f.semanticScore !== undefined).length;
    const [lexicalWeight, semanticWeight] = modeWeights(mode);
    for (const file of scored) {
      const lexNorm = file.lexicalScore / maxLex;
      const semNorm = Math.max(0, file.semanticScore ?? 0);
      file.combinedScore = lexNorm * lexicalWeight + semNorm * semanticWeight;
    }

    const searchedFiles = scored.length;
    const files = scored
      .filter((f) => f.combinedScore > 0)
      .sort((a, b) => {
        const score = b.combinedScore - a.combinedScore;
        if (score !== 0) return score;
        return a.displayPath.localeCompare(b.displayPath);
      });

    return {
      files,
      searchedFiles,
      embeddedFiles,
      skippedBinaryOrLarge,
    };
  });
}

function enumerateFiles(
  workspaceDir: string,
  retrievalConfig: RetrievalConfig,
): FileCandidate[] {
  const pending = [workspaceDir];
  const out: FileCandidate[] = [];
  let totalBytes = 0;

  while (pending.length > 0) {
    if (out.length >= retrievalConfig.max_indexed_files) break;
    if (totalBytes >= retrievalConfig.max_total_indexed_bytes) break;

    const here = pending.pop()!;
    let stat: fs.Stats;
    try {
      stat = fs.lstatSync(here);
    } catch {
      continue;
    }

    if (stat.isSymbolicLink()) continue;

    if (stat.isDirectory()) {
      let names: string[];
      try {
        names = fs.readdirSync(here);
      } catch {
        continue;
      }
      for (const name of names) pending.push(path.join(here, name));
      continue;
    }

    if (!stat.isFile()) continue;

    const size = stat.size;
    const skipReason = size > retrievalConfig.max_file_bytes ? "oversize" : undefined;
    if (skipReason === undefined) {
      totalBytes = Math.min(
        Number.MAX_SAFE_INTEGER,
        totalBytes + Math.max(0, size),
      );
    }

    out.push({
      displayPath: displayPathFor(workspaceDir, here),
      fsPath: here,
      size,
      modifiedAtSecs: Math.floor(stat.mtimeMs / 1000),
      ...(skipReason !== undefined ? { skipReason } : {}),
    });
  }

  return out;
}

function lexicalScore(
  filePath: string,
  content: string,
  qLower: string,
  terms: string[],
): number {
  const pathLower = filePath.toLowerCase();
  const contentLower = content.toLowerCase();
  const titleLower =
    content
      .split("\n")
      .find((line) => line.trimStart().startsWith("#"))
      ?.toLowerCase() ?? "";

  let score = 0;
  if (pathLower.includes(qLower)) score += 50;
  if (titleLower.includes(qLower)) score += 40;
  if (contentLower.includes(qLower)) score += 30;
  for (const term of terms) {
    if (pathLower.includes(term)) score += 12;
    if (titleLower.includes(term)) score += 10;
    if (contentLower.includes(term)) score += 4;
  }
  return score;
}

function tokenizeQuery(query: string): string[] {
  return query
    .split(/[^\p{Letter}\p{Number}_-]+/u)
    .filter((term) => term.length >= 2);
}

function documentForEmbedding(
  filePath: string,
  content: string,
  maxEmbedCharsPerFile: number,
): string {
  const trimmed = [...content].slice(0, maxEmbedCharsPerFile).join("");
  return `path: ${filePath}\n\n${trimmed}`;
}

async function embedDocuments(
  embedder: Embedder,
  docs: string[],
): Promise<number[][]> {
  const vectors: number[][] = [];
  let start = 0;

  while (start < docs.length) {
    let end = start;
    let batchChars = 0;

    while (end < docs.length && end - start < EMBED_BATCH_MAX_ITEMS) {
      const docChars = [...docs[end]!].length;
      if (end > start && batchChars + docChars > EMBED_BATCH_MAX_CHARS) {
        break;
      }
      batchChars += docChars;
      end += 1;
    }

    if (end === start) end += 1;
    const inputs = docs.slice(start, end);
    const batchVectors = await embedder.embed(inputs);
    if (batchVectors.length !== inputs.length) {
      throw new Error(
        `embedding count mismatch: got ${batchVectors.length}, expected ${inputs.length}`,
      );
    }
    vectors.push(...batchVectors);
    start = end;
  }

  return vectors;
}

function modeWeights(mode: HybridMode): [number, number] {
  if (mode === "vector") return [0, 1];
  return [0.45, 0.55];
}

function skipTag(size: number, mtimeSecs: number): string {
  return `mtime:${mtimeSecs}:${size}`;
}

function indexLockFor(indexPath: string): AsyncMutex {
  const key = path.resolve(indexPath);
  let lock = indexLocks.get(key);
  if (lock === undefined) {
    lock = new AsyncMutex();
    indexLocks.set(key, lock);
  }
  return lock;
}

export function cosineSimilarity(a: number[], b: number[]): number {
  if (a.length !== b.length) return 0;
  let dot = 0;
  let na = 0;
  let nb = 0;
  for (let i = 0; i < a.length; i++) {
    const x = a[i]!;
    const y = b[i]!;
    dot += x * y;
    na += x * x;
    nb += y * y;
  }
  if (na === 0 || nb === 0) return 0;
  return dot / (Math.sqrt(na) * Math.sqrt(nb));
}

function loadIndex(indexPath: string): WorkspaceIndex {
  try {
    const parsed = JSON.parse(fs.readFileSync(indexPath, "utf8")) as unknown;
    if (!isRecord(parsed) || !isRecord(parsed["entries"])) {
      return { entries: {} };
    }
    const entries: Record<string, IndexedEntry> = {};
    for (const [entryPath, rawEntry] of Object.entries(parsed["entries"])) {
      if (!isRecord(rawEntry)) continue;
      const entry = parseIndexedEntry(rawEntry);
      if (entry !== undefined) entries[entryPath] = entry;
    }
    return { entries };
  } catch {
    return { entries: {} };
  }
}

function parseIndexedEntry(raw: Record<string, unknown>): IndexedEntry | undefined {
  if (
    typeof raw["hash"] !== "string" ||
    typeof raw["size"] !== "number" ||
    typeof raw["modified_at_secs"] !== "number" ||
    typeof raw["model_id"] !== "string" ||
    typeof raw["embedded"] !== "boolean" ||
    !Array.isArray(raw["embedding"])
  ) {
    return undefined;
  }

  const embedding: number[] = [];
  for (const n of raw["embedding"]) {
    if (typeof n !== "number" || !Number.isFinite(n)) return undefined;
    embedding.push(n);
  }

  const entry: IndexedEntry = {
    hash: raw["hash"],
    size: raw["size"],
    modified_at_secs: raw["modified_at_secs"],
    model_id: raw["model_id"],
    embedded: raw["embedded"],
    embedding,
  };
  if (typeof raw["max_embed_chars_per_file"] === "number") {
    entry.max_embed_chars_per_file = raw["max_embed_chars_per_file"];
  }
  if (typeof raw["reason"] === "string") {
    entry.reason = raw["reason"];
  }
  return entry;
}

function saveIndex(indexPath: string, index: WorkspaceIndex): void {
  atomicWrite(indexPath, JSON.stringify(index, null, 2));
}

function displayPathFor(workspaceDir: string, filePath: string): string {
  const rel = path.relative(workspaceDir, filePath);
  if (rel.length === 0 || rel.startsWith("..") || path.isAbsolute(rel)) {
    return filePath.replace(/\\/g, "/");
  }
  return rel.replace(/\\/g, "/");
}

function decodeUtf8(bytes: Buffer): string {
  return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
}

function binarySkipReason(binary: RetrievalConfig["binary"]): string {
  switch (binary) {
    case "skip":
      return "non-utf8";
    case "metadata":
      return "binary-metadata-only";
    case "try_embed":
      return "binary-embedding-unsupported";
  }
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}
