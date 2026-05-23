/**
 * MarkdownMemoryStore tests — mirror of
 * `backend/daemon/src/memory/markdown_store.rs::tests`.
 *
 * Covers: write/read round-trip, recursive listing, internal-dream
 * filtering, ranked text search, delete with empty-parent cleanup,
 * path-traversal rejection, symlink-escape rejection (Unix only).
 */
import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import {
  MarkdownMemoryStore,
  MarkdownStoreError,
} from "../src/memory/markdown_store.ts";

function freshStore(): { store: MarkdownMemoryStore; root: string } {
  const root = mkdtempSync(path.join(tmpdir(), "shore-mem-test-"));
  const store = MarkdownMemoryStore.open(path.join(root, "memories"));
  return { store, root };
}

describe("MarkdownMemoryStore", () => {
  it("open creates the directory if missing", () => {
    const { store } = freshStore();
    expect(fs.existsSync(store.baseDir())).toBe(true);
  });

  it("write/read round-trip preserves content and path", () => {
    const { store } = freshStore();
    store.write("topics/gaming/doom.md", "# Doom\n\n- UV-Max speedrunner\n");
    const entry = store.read("topics/gaming/doom.md");
    expect(entry.content).toBe("# Doom\n\n- UV-Max speedrunner\n");
    expect(entry.path).toBe("topics/gaming/doom.md");
  });

  it("read reports a non-empty modifiedAt", () => {
    const { store } = freshStore();
    store.write("a.md", "A");
    const entry = store.read("a.md");
    expect(entry.modifiedAt.length).toBeGreaterThan(0);
  });

  it("listAll returns entries recursively, sorted by path", () => {
    const { store } = freshStore();
    store.write("a.md", "A");
    store.write("deep/b.md", "B");
    const entries = store.listAll();
    expect(entries.map((e) => e.path)).toEqual(["a.md", "deep/b.md"]);
  });

  it("listAll excludes dreaming-internal files and the index", () => {
    const { store } = freshStore();
    store.write("a.md", "A");
    store.write("DREAMS.md", "review");
    store.write("dreams.md", "lowercase review");
    store.write("MEMORY.md", "index");
    store.write(".dreams/candidates.md", "internal");
    store.write("dreaming/rem/today.md", "report");

    const entries = store.listAll();
    expect(entries.map((e) => e.path)).toEqual(["a.md"]);

    expect(store.searchText("review internal report")).toEqual([]);
    expect(store.searchText("index")).toEqual([]);
  });

  it("searchText finds matches by content", () => {
    const { store } = freshStore();
    store.write("a.md", "Ren likes chocolate");
    store.write("b.md", "Alice prefers tea");
    const results = store.searchText("chocolate");
    expect(results.map((e) => e.path)).toEqual(["a.md"]);
  });

  it("delete removes the file and rejects subsequent reads", () => {
    const { store } = freshStore();
    store.write("temp.md", "temp");
    store.delete("temp.md");
    expect(() => store.read("temp.md")).toThrow();
  });

  it("rejects path traversal in read and write", () => {
    const { store } = freshStore();
    expect(() => store.read("../secret.md")).toThrow();
    expect(() => store.write("../secret.md", "x")).toThrow();
  });

  if (process.platform !== "win32") {
    it("rejects symlink escape for an existing file", () => {
      const { store, root } = freshStore();
      const outsideDir = path.join(root, "outside");
      fs.mkdirSync(outsideDir, { recursive: true });
      fs.writeFileSync(path.join(outsideDir, "secret.md"), "secret");
      symlinkSync(
        path.join(outsideDir, "secret.md"),
        path.join(store.baseDir(), "secret.md"),
      );

      let readErr: unknown;
      try {
        store.read("secret.md");
      } catch (e) {
        readErr = e;
      }
      expect(readErr).toBeInstanceOf(MarkdownStoreError);
      expect((readErr as MarkdownStoreError).kind).toBe("pathTraversal");

      let writeErr: unknown;
      try {
        store.write("secret.md", "new secret");
      } catch (e) {
        writeErr = e;
      }
      expect(writeErr).toBeInstanceOf(MarkdownStoreError);
      expect((writeErr as MarkdownStoreError).kind).toBe("pathTraversal");

      expect(fs.readFileSync(path.join(outsideDir, "secret.md"), "utf8")).toBe(
        "secret",
      );
    });

    it("rejects symlink escape for a new file under a linked dir", () => {
      const { store, root } = freshStore();
      const outsideDir = path.join(root, "outside");
      fs.mkdirSync(outsideDir, { recursive: true });
      symlinkSync(outsideDir, path.join(store.baseDir(), "linked"));

      let err: unknown;
      try {
        store.write("linked/new.md", "new secret");
      } catch (e) {
        err = e;
      }
      expect(err).toBeInstanceOf(MarkdownStoreError);
      expect((err as MarkdownStoreError).kind).toBe("pathTraversal");
      expect(fs.existsSync(path.join(outsideDir, "new.md"))).toBe(false);
    });

    it("listAll rejects a symlinked-directory escape", () => {
      const { store, root } = freshStore();
      const outsideDir = path.join(root, "outside");
      fs.mkdirSync(outsideDir, { recursive: true });
      fs.writeFileSync(path.join(outsideDir, "secret.md"), "secret");

      store.write("inside.md", "inside");
      symlinkSync(outsideDir, path.join(store.baseDir(), "linked"));

      let err: unknown;
      try {
        store.listAll();
      } catch (e) {
        err = e;
      }
      expect(err).toBeInstanceOf(MarkdownStoreError);
      expect((err as MarkdownStoreError).kind).toBe("pathTraversal");
    });
  }
});
