/**
 * Markdown memory store — filesystem-based memory entries.
 *
 * Port of `backend/daemon/src/memory/markdown_store.rs`. Each entry is a
 * `.md` file under `characters/<C>/workspace/memory/`. Pure markdown — no
 * frontmatter — the assistant decides the layout.
 *
 * Path-safety mirrors the Rust impl:
 *   - reject `..` traversal,
 *   - reject absolute paths,
 *   - canonicalize and reject results that escape `base_dir`
 *     (catches symlink escape, including when listing a directory
 *     whose contents canonicalize outside).
 *
 * Internal dream artifacts (`.dreams/`, `dreaming/`, top-level `DREAMS.md`,
 * `MEMORY.md`) are skipped by `listAll` and `searchText` so they don't
 * pollute the model's view of memory.
 */

import fs from "node:fs";
import path from "node:path";

export interface MarkdownEntry {
  /** Relative path within the memory directory (e.g. "topics/gaming/doom.md"). */
  path: string;
  content: string;
  size: number;
  /** Last-modified RFC3339 timestamp in local tz, or "" if unavailable. */
  modifiedAt: string;
}

export class MarkdownStoreError extends Error {
  public readonly kind: "io" | "pathTraversal" | "notFound";
  constructor(kind: "io" | "pathTraversal" | "notFound", message: string) {
    super(`${kind}: ${message}`);
    this.kind = kind;
    this.name = "MarkdownStoreError";
  }
}

/** Markdown memory store rooted at a single directory. */
export class MarkdownMemoryStore {
  private readonly canonicalBase: string;

  private constructor(private readonly baseDir_: string) {
    this.canonicalBase = baseDir_;
  }

  /**
   * Open (or create) a store at `baseDir`. The directory is created if
   * missing and its canonical path is captured for subsequent escape
   * checks.
   */
  static open(baseDir: string): MarkdownMemoryStore {
    if (!fs.existsSync(baseDir)) {
      fs.mkdirSync(baseDir, { recursive: true });
    }
    let canonical: string;
    try {
      canonical = fs.realpathSync(baseDir);
    } catch (e) {
      throw new MarkdownStoreError("io", (e as Error).message);
    }
    return new MarkdownMemoryStore(canonical);
  }

  baseDir(): string {
    return this.canonicalBase;
  }

  /** List all `.md` files in the store, recursively. */
  listAll(): MarkdownEntry[] {
    const entries: MarkdownEntry[] = [];
    this.collectMdFiles(this.canonicalBase, entries);
    entries.sort((a, b) => a.path.localeCompare(b.path));
    return entries;
  }

  /** Read a single entry by relative path. */
  read(relPath: string): MarkdownEntry {
    const full = this.resolvePath(relPath);
    if (!fs.existsSync(full)) {
      throw new MarkdownStoreError("notFound", relPath);
    }
    let content: string;
    let stat: fs.Stats;
    try {
      content = fs.readFileSync(full, "utf8");
      stat = fs.statSync(full);
    } catch (e) {
      throw new MarkdownStoreError("io", (e as Error).message);
    }
    return {
      path: relPath,
      content,
      size: stat.size,
      modifiedAt: formatModifiedAt(stat.mtime),
    };
  }

  /** Write (create or overwrite) an entry. */
  write(relPath: string, content: string): void {
    const full = this.resolvePath(relPath);
    const parent = path.dirname(full);
    try {
      fs.mkdirSync(parent, { recursive: true });
      fs.writeFileSync(full, content);
    } catch (e) {
      throw new MarkdownStoreError("io", (e as Error).message);
    }
  }

  /** Delete an entry. Empty parent directory is cleaned up opportunistically. */
  delete(relPath: string): void {
    const full = this.resolvePath(relPath);
    if (!fs.existsSync(full)) {
      throw new MarkdownStoreError("notFound", relPath);
    }
    try {
      fs.rmSync(full);
    } catch (e) {
      throw new MarkdownStoreError("io", (e as Error).message);
    }
    const parent = path.dirname(full);
    if (parent !== this.canonicalBase) {
      try {
        fs.rmdirSync(parent);
      } catch {
        // Non-empty or already gone — fine.
      }
    }
  }

  /**
   * Ranked text search across all entries.
   *
   * Scores: path hit (50) + title hit (40) + content hit (30) for the
   * full query, plus per-term hits (path 12, title 10, content 4) for
   * each tokenized term ≥2 chars. Returns entries sorted by descending
   * score, ties broken by path.
   */
  searchText(query: string): MarkdownEntry[] {
    const all = this.listAll();
    const q = query.toLowerCase();
    const terms = tokenizeQuery(q);
    const scored: Array<{ score: number; entry: MarkdownEntry }> = [];
    for (const entry of all) {
      const score = entrySearchScore(entry, q, terms);
      if (score > 0) scored.push({ score, entry });
    }
    scored.sort((a, b) => {
      if (a.score !== b.score) return b.score - a.score;
      return a.entry.path.localeCompare(b.entry.path);
    });
    return scored.map((s) => s.entry);
  }

  // -------------------------------------------------------------------------
  // Internal
  // -------------------------------------------------------------------------

  private collectMdFiles(dir: string, out: MarkdownEntry[]): void {
    let names: string[];
    try {
      names = fs.readdirSync(dir);
    } catch (e) {
      throw new MarkdownStoreError("io", (e as Error).message);
    }
    for (const name of names) {
      const full = path.join(dir, name);
      if (isInternalDreamPath(this.canonicalBase, full)) continue;

      let linkStat: fs.Stats;
      try {
        linkStat = fs.lstatSync(full);
      } catch (e) {
        throw new MarkdownStoreError("io", (e as Error).message);
      }
      if (linkStat.isSymbolicLink()) {
        let canonical: string;
        try {
          canonical = fs.realpathSync(full);
        } catch (e) {
          throw new MarkdownStoreError("io", (e as Error).message);
        }
        if (!isInside(canonical, this.canonicalBase)) {
          throw new MarkdownStoreError(
            "pathTraversal",
            `symlink escapes memory directory: ${full}`,
          );
        }
        if (fs.statSync(canonical).isDirectory()) {
          // Mirror Rust: skip directory symlinks even if they point inside.
          continue;
        }
      }

      let stat: fs.Stats;
      try {
        stat = fs.statSync(full);
      } catch (e) {
        throw new MarkdownStoreError("io", (e as Error).message);
      }
      if (stat.isDirectory()) {
        this.collectMdFiles(full, out);
      } else if (path.extname(full) === ".md") {
        const rel = path.relative(this.canonicalBase, full);
        const content = fs.readFileSync(full, "utf8");
        out.push({
          path: rel.split(path.sep).join("/"),
          content,
          size: stat.size,
          modifiedAt: formatModifiedAt(stat.mtime),
        });
      }
    }
  }

  private resolvePath(relPath: string): string {
    const rel = relPath.trim();
    if (rel.length === 0) {
      throw new MarkdownStoreError("pathTraversal", "empty path");
    }
    const segments = rel.split(/[\\/]+/);
    for (const seg of segments) {
      if (seg === "..") {
        throw new MarkdownStoreError(
          "pathTraversal",
          "path traversal (..) not allowed",
        );
      }
    }
    if (path.isAbsolute(rel)) {
      throw new MarkdownStoreError(
        "pathTraversal",
        "absolute paths not allowed",
      );
    }
    const resolved = path.join(this.canonicalBase, rel);
    this.ensureInside(resolved);
    return resolved;
  }

  /**
   * Reject paths whose canonical resolution escapes the store. When the
   * target doesn't exist yet, walk up to the nearest ancestor that does
   * and check that — covers writes through a symlinked directory.
   */
  private ensureInside(resolved: string): void {
    try {
      const canonical = fs.realpathSync(resolved);
      if (!isInside(canonical, this.canonicalBase)) {
        throw new MarkdownStoreError(
          "pathTraversal",
          "resolved path escapes memory directory",
        );
      }
      return;
    } catch (e) {
      if (e instanceof MarkdownStoreError) throw e;
      // Fall through — target doesn't exist yet.
    }
    let ancestor = resolved;
    while (true) {
      const parent = path.dirname(ancestor);
      if (parent === ancestor) return;
      try {
        const canonical = fs.realpathSync(parent);
        if (!isInside(canonical, this.canonicalBase)) {
          throw new MarkdownStoreError(
            "pathTraversal",
            "resolved path escapes memory directory",
          );
        }
        return;
      } catch (e) {
        if (e instanceof MarkdownStoreError) throw e;
        ancestor = parent;
      }
    }
  }
}

/**
 * True when `path` is an internal dreaming artifact at the top of the
 * memory store. Mirrors Rust's `is_internal_dream_path` — case-insensitive
 * match on the first path component: `.dreams`, `dreaming`, `dreams.md`,
 * `memory.md`.
 */
function isInternalDreamPath(baseDir: string, full: string): boolean {
  const rel = path.relative(baseDir, full);
  const first = rel.split(path.sep)[0] ?? "";
  const lower = first.toLowerCase();
  return (
    lower === ".dreams" ||
    lower === "dreaming" ||
    lower === "dreams.md" ||
    lower === "memory.md"
  );
}

function isInside(candidate: string, root: string): boolean {
  if (candidate === root) return true;
  const rootWithSep = root.endsWith(path.sep) ? root : root + path.sep;
  return candidate.startsWith(rootWithSep);
}

function formatModifiedAt(d: Date): string {
  // Mirror Rust: format in the local tz with offset, RFC3339.
  // We can't easily produce an offset string with toISOString; build one.
  const pad = (n: number, w = 2): string => String(n).padStart(w, "0");
  const tzOffsetMin = -d.getTimezoneOffset();
  const sign = tzOffsetMin >= 0 ? "+" : "-";
  const absMin = Math.abs(tzOffsetMin);
  const offHours = Math.floor(absMin / 60);
  const offMins = absMin % 60;
  const offsetStr = `${sign}${pad(offHours)}:${pad(offMins)}`;
  const ms = String(d.getMilliseconds()).padStart(3, "0");
  return (
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T` +
    `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${ms}` +
    offsetStr
  );
}

function tokenizeQuery(query: string): string[] {
  return query
    .split(/[^a-z0-9_-]+/i)
    .filter((term) => term.length >= 2);
}

function entrySearchScore(
  entry: MarkdownEntry,
  query: string,
  terms: string[],
): number {
  const lowerPath = entry.path.toLowerCase();
  const lowerContent = entry.content.toLowerCase();
  const title =
    (entry.content.split("\n").find((line) => line.trimStart().startsWith("#")) ??
      "").toLowerCase();

  let score = 0;
  if (lowerPath.includes(query)) score += 50;
  if (title.includes(query)) score += 40;
  if (lowerContent.includes(query)) score += 30;

  for (const term of terms) {
    if (lowerPath.includes(term)) score += 12;
    if (title.includes(term)) score += 10;
    if (lowerContent.includes(term)) score += 4;
  }
  return score;
}
