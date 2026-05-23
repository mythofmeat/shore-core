/**
 * Workspace tool tests — read, write, edit, list_files, delete, file_search.
 *
 * Covers happy path + every documented safety check from
 * `backend/daemon/src/tools/workspace.rs`:
 *   - path traversal (`..`),
 *   - absolute paths,
 *   - symlink escape (Unix only),
 *   - prompt-visible file refusal on delete,
 *   - file_search skipping symlinks during enumeration,
 *   - file_search mtime ordering,
 *   - file_search graceful degradation for hybrid mode.
 */
import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { resolvePath } from "../src/tools/paths.ts";
import type { ToolContext } from "../src/tools/registry.ts";
import { ToolError } from "../src/tools/registry.ts";
import {
  deleteHandler,
  editHandler,
  fileSearchHandler,
  listFilesHandler,
  readHandler,
  writeHandler,
} from "../src/tools/workspace.ts";

function setup(): { workspace: string; data: string; ctx: ToolContext } {
  const root = mkdtempSync(path.join(tmpdir(), "shore-ws-test-"));
  const workspace = path.join(root, "workspace");
  const data = path.join(root, "data");
  fs.mkdirSync(workspace, { recursive: true });
  fs.mkdirSync(data, { recursive: true });
  const ctx = {
    characterName: "test",
    characterConfigDir: root,
    characterDataDir: data,
    workspaceDir: workspace,
    configDir: root,
    imageDir: path.join(data, "images"),
    engine: undefined as unknown as ToolContext["engine"],
    searchConfig: {
      api_key_env: "TAVILY_API_KEY",
      max_results: 5,
      search_depth: "basic",
      include_answer: true,
    },
    retrievalConfig: { max_file_bytes: 1024 * 1024 },
  };
  return { workspace, data, ctx };
}

describe("resolvePath safety", () => {
  it("rejects `..` traversal", () => {
    const { workspace } = setup();
    expect(() => resolvePath(workspace, "../etc/passwd")).toThrow(ToolError);
  });

  it("rejects deeply-nested traversal", () => {
    const { workspace } = setup();
    expect(() => resolvePath(workspace, "foo/../../etc/passwd")).toThrow(
      ToolError,
    );
  });

  it("rejects absolute paths", () => {
    const { workspace } = setup();
    expect(() => resolvePath(workspace, "/etc/passwd")).toThrow(ToolError);
  });

  it("rejects empty workspace dir", () => {
    expect(() => resolvePath("", "foo")).toThrow(ToolError);
  });

  it("accepts a normal relative path", () => {
    const { workspace } = setup();
    const p = resolvePath(workspace, "notes/ideas.md");
    expect(p).toBe(path.join(workspace, "notes/ideas.md"));
  });

  it("strips the `workspace/` display prefix", () => {
    const { workspace } = setup();
    const p = resolvePath(workspace, "workspace/notes.md");
    expect(p).toBe(path.join(workspace, "notes.md"));
  });

  it("routes `memory/` under workspace/memory", () => {
    const { workspace } = setup();
    const p = resolvePath(workspace, "memory/people/ren.md");
    expect(p).toBe(path.join(workspace, "memory/people/ren.md"));
  });
});

describe("read/write/edit/list roundtrip", () => {
  it("writes a file and reads it back", async () => {
    const { ctx } = setup();
    const wResult = JSON.parse(
      await writeHandler.execute({ path: "test.txt", content: "hello" }, ctx),
    );
    expect(wResult.bytes_written).toBe(5);

    const rResult = JSON.parse(
      await readHandler.execute({ path: "test.txt" }, ctx),
    );
    expect(rResult.content).toBe("hello");
    expect(rResult.total_lines).toBe(1);
  });

  it("write creates intermediate directories", async () => {
    const { ctx } = setup();
    await writeHandler.execute(
      { path: "deep/nested/file.txt", content: "nested" },
      ctx,
    );
    const r = JSON.parse(
      await readHandler.execute({ path: "deep/nested/file.txt" }, ctx),
    );
    expect(r.content).toBe("nested");
  });

  it("read honors offset and limit", async () => {
    const { ctx } = setup();
    await writeHandler.execute(
      { path: "lines.txt", content: "line1\nline2\nline3\nline4\nline5" },
      ctx,
    );
    const r = JSON.parse(
      await readHandler.execute(
        { path: "lines.txt", offset: 2, limit: 2 },
        ctx,
      ),
    );
    expect(r.content).toBe("line2\nline3");
    expect(r.total_lines).toBe(5);
  });

  it("edit replaces all occurrences", async () => {
    const { ctx } = setup();
    await writeHandler.execute({ path: "f.txt", content: "foo foo foo" }, ctx);
    const e = JSON.parse(
      await editHandler.execute(
        {
          path: "f.txt",
          edits: [{ old_string: "foo", new_string: "bar" }],
        },
        ctx,
      ),
    );
    expect(e.replacements_made).toBe(3);
    const r = JSON.parse(await readHandler.execute({ path: "f.txt" }, ctx));
    expect(r.content).toBe("bar bar bar");
  });

  it("edit fails when old_string is absent", async () => {
    const { ctx } = setup();
    await writeHandler.execute({ path: "f.txt", content: "hello" }, ctx);
    expect(
      editHandler.execute(
        {
          path: "f.txt",
          edits: [{ old_string: "missing", new_string: "replaced" }],
        },
        ctx,
      ),
    ).rejects.toThrow(ToolError);
  });

  it("list_files returns sorted entries with type+size", async () => {
    const { ctx } = setup();
    await writeHandler.execute({ path: "b.txt", content: "bb" }, ctx);
    await writeHandler.execute({ path: "a.txt", content: "aaa" }, ctx);
    const r = JSON.parse(await listFilesHandler.execute({}, ctx));
    expect(r.entries.map((e: { name: string }) => e.name)).toEqual([
      "a.txt",
      "b.txt",
    ]);
    expect(r.entries[0].type).toBe("file");
    expect(r.entries[0].size).toBe(3);
  });

  it("list_files returns empty for non-existent dir", async () => {
    const { ctx } = setup();
    const r = JSON.parse(
      await listFilesHandler.execute({ path: "ghost" }, ctx),
    );
    expect(r.entries).toEqual([]);
  });
});

describe("write/edit on prompt-visible files", () => {
  it("write marks SOUL.md as deferred", async () => {
    const { ctx } = setup();
    const r = JSON.parse(
      await writeHandler.execute(
        { path: "SOUL.md", content: "I am the soul" },
        ctx,
      ),
    );
    expect(r.prompt_visible_file).toBe(true);
    expect(r.protected_file).toBe(true);
    expect(r.deferred_until_compaction).toBe(true);
    expect(r.deferred_path).toBe("SOUL.md");
    expect(r.prompt_reload_required).toBe(true);
  });

  it("write to MEMORY.md is deferred but NOT marked protected", async () => {
    const { ctx } = setup();
    const r = JSON.parse(
      await writeHandler.execute(
        { path: "MEMORY.md", content: "memory index" },
        ctx,
      ),
    );
    expect(r.prompt_visible_file).toBe(true);
    expect(r.protected_file).toBeUndefined();
    expect(r.deferred_until_compaction).toBe(true);
  });

  it("ordinary writes don't get the deferred markers", async () => {
    const { ctx } = setup();
    const r = JSON.parse(
      await writeHandler.execute(
        { path: "notes/ideas.md", content: "thoughts" },
        ctx,
      ),
    );
    expect(r.prompt_visible_file).toBeUndefined();
    expect(r.deferred_until_compaction).toBeUndefined();
  });

  it("writing SOUL.md appends an entry to deferred_edits.jsonl", async () => {
    const { data, ctx } = setup();
    await writeHandler.execute(
      { path: "SOUL.md", content: "I am the soul" },
      ctx,
    );
    const queue = fs
      .readFileSync(path.join(data, "deferred_edits.jsonl"), "utf8")
      .trim()
      .split("\n")
      .filter((l) => l.length > 0)
      .map((l) => JSON.parse(l) as Record<string, unknown>);
    expect(queue.length).toBe(1);
    expect(queue[0]!.path).toBe("SOUL.md");
    expect(typeof queue[0]!.timestamp).toBe("string");
  });

  it("writing MEMORY.md creates the active_prompt sentinel and queues", async () => {
    const { data, ctx } = setup();
    await writeHandler.execute(
      { path: "MEMORY.md", content: "memory index" },
      ctx,
    );
    expect(fs.existsSync(path.join(data, "deferred_edits.jsonl"))).toBe(true);
    // Zero-byte sentinel that keeps the un-applied canonical from leaking
    // into loadMemoryIndex.
    const sentinel = path.join(data, "active_prompt", "MEMORY.md");
    expect(fs.existsSync(sentinel)).toBe(true);
    expect(fs.statSync(sentinel).size).toBe(0);
  });

  it("editing SOUL.md (after a write) appends a second queue entry", async () => {
    const { data, ctx } = setup();
    await writeHandler.execute(
      { path: "SOUL.md", content: "first" },
      ctx,
    );
    await editHandler.execute(
      { path: "SOUL.md", edits: [{ old_string: "first", new_string: "second" }] },
      ctx,
    );
    const queue = fs
      .readFileSync(path.join(data, "deferred_edits.jsonl"), "utf8")
      .trim()
      .split("\n")
      .filter((l) => l.length > 0);
    expect(queue.length).toBe(2);
  });

  it("ordinary writes do NOT append to deferred_edits.jsonl", async () => {
    const { data, ctx } = setup();
    await writeHandler.execute(
      { path: "notes/ideas.md", content: "thoughts" },
      ctx,
    );
    expect(fs.existsSync(path.join(data, "deferred_edits.jsonl"))).toBe(false);
  });
});

describe("delete", () => {
  it("moves a file to a timestamped trash folder", async () => {
    const { workspace, data, ctx } = setup();
    await writeHandler.execute({ path: "notes/old.md", content: "stale" }, ctx);
    const r = JSON.parse(
      await deleteHandler.execute({ path: "notes/old.md" }, ctx),
    );
    expect(r.deleted).toBe(true);
    expect(fs.existsSync(path.join(workspace, "notes/old.md"))).toBe(false);
    const trashRoot = path.join(data, "trash");
    expect(fs.existsSync(trashRoot)).toBe(true);
    const trashStamps = fs.readdirSync(trashRoot);
    expect(trashStamps.length).toBe(1);
    const trashed = path.join(trashRoot, trashStamps[0]!, "notes/old.md");
    expect(fs.readFileSync(trashed, "utf8")).toBe("stale");
  });

  it("refuses prompt-visible files", async () => {
    const { ctx } = setup();
    await writeHandler.execute({ path: "SOUL.md", content: "soul" }, ctx);
    expect(deleteHandler.execute({ path: "SOUL.md" }, ctx)).rejects.toThrow(
      ToolError,
    );
    for (const f of ["USER.md", "AGENTS.md", "TOOLS.md", "HEARTBEAT.md", "MEMORY.md"]) {
      expect(deleteHandler.execute({ path: f }, ctx)).rejects.toThrow(ToolError);
    }
  });

  it("refuses directories", async () => {
    const { workspace, ctx } = setup();
    fs.mkdirSync(path.join(workspace, "notes"));
    expect(deleteHandler.execute({ path: "notes" }, ctx)).rejects.toThrow(
      ToolError,
    );
  });

  it("refuses missing files", async () => {
    const { ctx } = setup();
    expect(deleteHandler.execute({ path: "ghost.md" }, ctx)).rejects.toThrow(
      ToolError,
    );
  });

  it("refuses traversal", async () => {
    const { ctx } = setup();
    expect(
      deleteHandler.execute({ path: "../escape.md" }, ctx),
    ).rejects.toThrow(ToolError);
  });
});

describe("file_search lexical", () => {
  it("finds substring hits across workspace and memory", async () => {
    const { ctx } = setup();
    await writeHandler.execute(
      { path: "notes/ideas.md", content: "Tea in the garden\nCoffee later" },
      ctx,
    );
    await writeHandler.execute(
      { path: "memory/people/ren.md", content: "Ren likes tea." },
      ctx,
    );
    const r = JSON.parse(
      await fileSearchHandler.execute({ query: "tea" }, ctx),
    );
    const paths = r.results.map((e: { path: string }) => e.path);
    expect(paths).toContain("notes/ideas.md");
    expect(paths).toContain("memory/people/ren.md");
  });

  it("hybrid mode without an embedder falls back to lexical with marker", async () => {
    const { ctx } = setup();
    await writeHandler.execute(
      { path: "notes.md", content: "tea time" },
      ctx,
    );
    const r = JSON.parse(
      await fileSearchHandler.execute({ query: "tea", mode: "hybrid" }, ctx),
    );
    expect(r.mode).toBe("lexical");
    expect(r.semantic_unavailable).toBe("embedder not configured");
    expect(r.results.length).toBeGreaterThan(0);
  });

  it("lexical mode does NOT set semantic_unavailable", async () => {
    const { ctx } = setup();
    await writeHandler.execute(
      { path: "notes.md", content: "tea time" },
      ctx,
    );
    const r = JSON.parse(
      await fileSearchHandler.execute({ query: "tea", mode: "lexical" }, ctx),
    );
    expect(r.mode).toBe("lexical");
    expect(r.semantic_unavailable).toBeUndefined();
  });

  it("rejects unknown mode", async () => {
    const { ctx } = setup();
    expect(
      fileSearchHandler.execute({ query: "tea", mode: "magic" }, ctx),
    ).rejects.toThrow(ToolError);
  });

  it("orders results newest file first", async () => {
    const { workspace, ctx } = setup();
    await writeHandler.execute(
      { path: "older.md", content: "tea in the garden" },
      ctx,
    );
    // Backdate older.md so mtime ordering is deterministic.
    const past = new Date(Date.now() - 60_000);
    fs.utimesSync(path.join(workspace, "older.md"), past, past);
    await writeHandler.execute(
      { path: "newer.md", content: "tea on the porch" },
      ctx,
    );
    const r = JSON.parse(
      await fileSearchHandler.execute({ query: "tea" }, ctx),
    );
    const paths = r.results.map((e: { path: string }) => e.path);
    expect(paths[0]).toBe("newer.md");
    expect(paths[1]).toBe("older.md");
  });

  it("returns count=0 and omits note when no matches", async () => {
    const { ctx } = setup();
    await writeHandler.execute(
      { path: "notes.md", content: "nothing relevant" },
      ctx,
    );
    const r = JSON.parse(
      await fileSearchHandler.execute({ query: "absent" }, ctx),
    );
    expect(r.count).toBe(0);
    expect(r.note).toBeUndefined();
    expect(r.files).toBeUndefined();
  });

  if (process.platform !== "win32") {
    it("skips symlinks pointing outside the workspace", async () => {
      const { workspace, ctx } = setup();
      const outsideDir = mkdtempSync(path.join(tmpdir(), "shore-outside-"));
      const secret = path.join(outsideDir, "secret.md");
      fs.writeFileSync(secret, "secret_token_xyzzy");
      symlinkSync(secret, path.join(workspace, "link_to_secret.md"));
      symlinkSync(outsideDir, path.join(workspace, "outside_dir"));

      const r = JSON.parse(
        await fileSearchHandler.execute({ query: "xyzzy" }, ctx),
      );
      expect(r.count).toBe(0);
      expect(r.results).toEqual([]);
    });
  }
});
