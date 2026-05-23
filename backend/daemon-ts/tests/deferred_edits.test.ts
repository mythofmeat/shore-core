/**
 * Deferred edits + active prompt snapshot tests.
 *
 * Mirror of `backend/daemon/src/memory/deferred_edits.rs::tests`. Pins
 * the observable behavior the prompt path depends on:
 *   - protected-path normalization across all the prefix shapes,
 *   - queue dedupe (`SOUL.md` vs. `workspace/SOUL.md`),
 *   - the seed-vs-refresh distinction for the active snapshot,
 *   - the MEMORY.md zero-byte sentinel that blocks live activation,
 *   - workspace migration + global-user seeding + memories tree copy.
 */
import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import {
  AGENTS_FILE,
  HEARTBEAT_FILE,
  MEMORY_INDEX_FILE,
  SOUL_FILE,
  TOOLS_FILE,
  USER_FILE,
  activePromptFile,
  applyDeferredEdits,
  characterMemoryDir,
  characterWorkspaceDir,
  ensureActivePromptSnapshot,
  ensureCharacterWorkspace,
  isProtectedPath,
  loadMemoryIndex,
  pendingDeferredEditPaths,
  queueDeferredEdit,
} from "../src/memory/deferred_edits.ts";

function freshDirs(): { tmp: string; charDir: string; configDir: string } {
  const tmp = mkdtempSync(path.join(tmpdir(), "shore-deferred-test-"));
  return {
    tmp,
    charDir: path.join(tmp, "data", "TestChar"),
    configDir: path.join(tmp, "config"),
  };
}

describe("protected path normalization", () => {
  it("matches every prefix shape and rejects unrelated paths", () => {
    expect(isProtectedPath("SOUL.md")).toBe(true);
    expect(isProtectedPath("workspace/SOUL.md")).toBe(true);
    expect(isProtectedPath("/workspace/USER.md")).toBe(true);
    expect(isProtectedPath("workspace\\AGENTS.md")).toBe(true);
    expect(isProtectedPath("TOOLS.md")).toBe(true);
    expect(isProtectedPath("notes.md")).toBe(false);

    // Mixed prefixes — single-pass strip used to leak past these.
    expect(isProtectedPath("workspace/./SOUL.md")).toBe(true);
    expect(isProtectedPath("./SOUL.md")).toBe(true);
    expect(isProtectedPath("./workspace/SOUL.md")).toBe(true);
    expect(isProtectedPath("/./workspace/SOUL.md")).toBe(true);
    expect(isProtectedPath("workspace/workspace/SOUL.md")).toBe(true);
  });
});

describe("queueDeferredEdit + applyDeferredEdits", () => {
  it("refreshes the snapshot from canonical and clears the queue", () => {
    const { charDir, configDir } = freshDirs();
    const workspace = characterWorkspaceDir(configDir, "TestChar");
    fs.mkdirSync(workspace, { recursive: true });
    for (const [name, body] of [
      [SOUL_FILE, "new soul"],
      [USER_FILE, "new user"],
      [AGENTS_FILE, "new agents"],
      [TOOLS_FILE, "new tools"],
      [HEARTBEAT_FILE, "new heartbeat"],
    ] as const) {
      fs.writeFileSync(path.join(workspace, name), body);
    }

    queueDeferredEdit(charDir, "workspace/SOUL.md");
    queueDeferredEdit(charDir, "USER.md");
    applyDeferredEdits(charDir, configDir, "TestChar");

    expect(fs.readFileSync(activePromptFile(charDir, SOUL_FILE), "utf8")).toBe(
      "new soul",
    );
    expect(fs.readFileSync(activePromptFile(charDir, USER_FILE), "utf8")).toBe(
      "new user",
    );
    expect(fs.existsSync(path.join(charDir, "deferred_edits.jsonl"))).toBe(false);
  });

  it("dedupes pending paths across prefix variants", () => {
    const { tmp } = freshDirs();
    const charDir = path.join(tmp, "char");
    fs.mkdirSync(charDir, { recursive: true });

    queueDeferredEdit(charDir, "workspace/SOUL.md");
    queueDeferredEdit(charDir, "SOUL.md");
    queueDeferredEdit(charDir, "AGENTS.md");
    queueDeferredEdit(charDir, "workspace/MEMORY.md");

    expect(pendingDeferredEditPaths(charDir)).toEqual([
      "AGENTS.md",
      "MEMORY.md",
      "SOUL.md",
    ]);
  });

  it("ignores non-prompt-visible paths", () => {
    const { tmp } = freshDirs();
    const charDir = path.join(tmp, "char");
    fs.mkdirSync(charDir, { recursive: true });

    queueDeferredEdit(charDir, "notes.md");
    queueDeferredEdit(charDir, "memory/people/alice.md");

    expect(pendingDeferredEditPaths(charDir)).toEqual([]);
    expect(fs.existsSync(path.join(charDir, "deferred_edits.jsonl"))).toBe(false);
  });
});

describe("ensureCharacterWorkspace", () => {
  it("migrates legacy bootstrap files + global user + memories tree", () => {
    const { charDir, configDir } = freshDirs();
    const charConfig = path.join(configDir, "characters", "TestChar");

    fs.mkdirSync(path.join(charConfig, "prompts"), { recursive: true });
    fs.writeFileSync(path.join(charConfig, "character.md"), "orig soul");
    fs.writeFileSync(path.join(charConfig, "user.md"), "orig user");
    fs.writeFileSync(path.join(charConfig, "prompts", "system.md"), "orig agents");
    fs.mkdirSync(configDir, { recursive: true });
    fs.writeFileSync(path.join(configDir, "user.md"), "global user");
    fs.mkdirSync(path.join(charDir, "memories", "daily"), { recursive: true });
    fs.writeFileSync(
      path.join(charDir, "memories", "daily", "2026-01-01.md"),
      "note",
    );

    ensureCharacterWorkspace(charDir, configDir, "TestChar");

    const workspace = characterWorkspaceDir(configDir, "TestChar");
    expect(fs.readFileSync(path.join(workspace, SOUL_FILE), "utf8")).toBe(
      "orig soul",
    );
    expect(fs.readFileSync(path.join(workspace, USER_FILE), "utf8")).toBe(
      "orig user",
    );
    expect(fs.readFileSync(path.join(workspace, AGENTS_FILE), "utf8")).toBe(
      "orig agents",
    );
    expect(fs.existsSync(path.join(workspace, TOOLS_FILE))).toBe(true);
    expect(fs.existsSync(path.join(workspace, HEARTBEAT_FILE))).toBe(true);
    expect(
      fs.readFileSync(
        path.join(
          characterMemoryDir(configDir, "TestChar"),
          "daily",
          "2026-01-01.md",
        ),
        "utf8",
      ),
    ).toBe("note");
  });

  it("seeds USER.md from the global user.md only when missing", () => {
    const { charDir, configDir } = freshDirs();
    fs.mkdirSync(configDir, { recursive: true });
    fs.writeFileSync(path.join(configDir, "user.md"), "global user");
    const workspace = characterWorkspaceDir(configDir, "TestChar");
    fs.mkdirSync(workspace, { recursive: true });
    fs.writeFileSync(path.join(workspace, USER_FILE), "existing user");

    ensureCharacterWorkspace(charDir, configDir, "TestChar");

    expect(fs.readFileSync(path.join(workspace, USER_FILE), "utf8")).toBe(
      "existing user",
    );
  });
});

describe("ensureActivePromptSnapshot", () => {
  it("seeds the snapshot once and ignores subsequent canonical edits", () => {
    const { charDir, configDir } = freshDirs();
    const workspace = characterWorkspaceDir(configDir, "TestChar");
    fs.mkdirSync(workspace, { recursive: true });
    for (const [name, body] of [
      [SOUL_FILE, "workspace soul"],
      [USER_FILE, "workspace user"],
      [AGENTS_FILE, "workspace agents"],
      [TOOLS_FILE, "workspace tools"],
      [HEARTBEAT_FILE, "workspace heartbeat"],
    ] as const) {
      fs.writeFileSync(path.join(workspace, name), body);
    }

    ensureActivePromptSnapshot(charDir, configDir, "TestChar");
    fs.writeFileSync(path.join(workspace, SOUL_FILE), "edited later");
    ensureActivePromptSnapshot(charDir, configDir, "TestChar");

    expect(fs.readFileSync(activePromptFile(charDir, SOUL_FILE), "utf8")).toBe(
      "workspace soul",
    );
  });

  it("cleans up the legacy RECENT_MEMORY.md snapshot", () => {
    const { charDir, configDir } = freshDirs();
    const workspace = characterWorkspaceDir(configDir, "TestChar");
    fs.mkdirSync(workspace, { recursive: true });
    for (const [name, body] of [
      [SOUL_FILE, "soul"],
      [USER_FILE, "user"],
      [AGENTS_FILE, "agents"],
      [TOOLS_FILE, "tools"],
      [HEARTBEAT_FILE, "heartbeat"],
    ] as const) {
      fs.writeFileSync(path.join(workspace, name), body);
    }
    const activeDir = path.join(charDir, "active_prompt");
    fs.mkdirSync(activeDir, { recursive: true });
    fs.writeFileSync(path.join(activeDir, "RECENT_MEMORY.md"), "stale");

    ensureActivePromptSnapshot(charDir, configDir, "TestChar");

    expect(fs.existsSync(path.join(activeDir, "RECENT_MEMORY.md"))).toBe(false);
  });
});

describe("deferred edits are only activated by apply", () => {
  it("protected edits stay invisible until applyDeferredEdits runs", () => {
    const { charDir, configDir } = freshDirs();
    const workspace = characterWorkspaceDir(configDir, "TestChar");
    fs.mkdirSync(workspace, { recursive: true });
    for (const [name, body] of [
      [SOUL_FILE, "active soul"],
      [USER_FILE, "active user"],
      [AGENTS_FILE, "active agents"],
      [TOOLS_FILE, "active tools"],
      [HEARTBEAT_FILE, "active heartbeat"],
    ] as const) {
      fs.writeFileSync(path.join(workspace, name), body);
    }

    ensureActivePromptSnapshot(charDir, configDir, "TestChar");

    fs.writeFileSync(path.join(workspace, SOUL_FILE), "edited soul");
    queueDeferredEdit(charDir, "SOUL.md");
    ensureActivePromptSnapshot(charDir, configDir, "TestChar");

    expect(fs.readFileSync(activePromptFile(charDir, SOUL_FILE), "utf8")).toBe(
      "active soul",
    );
    expect(pendingDeferredEditPaths(charDir)).toEqual(["SOUL.md"]);

    applyDeferredEdits(charDir, configDir, "TestChar");

    expect(fs.readFileSync(activePromptFile(charDir, SOUL_FILE), "utf8")).toBe(
      "edited soul",
    );
    expect(fs.existsSync(path.join(charDir, "deferred_edits.jsonl"))).toBe(false);
  });

  it("MEMORY.md edits stay deferred even after subsequent ensure calls", () => {
    const { charDir, configDir } = freshDirs();
    const workspace = characterWorkspaceDir(configDir, "TestChar");
    fs.mkdirSync(workspace, { recursive: true });
    fs.writeFileSync(path.join(workspace, MEMORY_INDEX_FILE), "active index");
    ensureActivePromptSnapshot(charDir, configDir, "TestChar");

    fs.writeFileSync(path.join(workspace, MEMORY_INDEX_FILE), "edited index");
    queueDeferredEdit(charDir, "MEMORY.md");
    ensureActivePromptSnapshot(charDir, configDir, "TestChar");

    expect(loadMemoryIndex(charDir, configDir, "TestChar")).toBe("active index");
    expect(pendingDeferredEditPaths(charDir)).toEqual(["MEMORY.md"]);

    applyDeferredEdits(charDir, configDir, "TestChar");

    expect(loadMemoryIndex(charDir, configDir, "TestChar")).toBe("edited index");
  });

  it("a brand-new MEMORY.md write does not live-activate", () => {
    const { charDir, configDir } = freshDirs();
    const workspace = characterWorkspaceDir(configDir, "TestChar");

    ensureActivePromptSnapshot(charDir, configDir, "TestChar");
    fs.mkdirSync(workspace, { recursive: true });
    fs.writeFileSync(path.join(workspace, MEMORY_INDEX_FILE), "new index");
    queueDeferredEdit(charDir, "MEMORY.md");
    ensureActivePromptSnapshot(charDir, configDir, "TestChar");

    expect(loadMemoryIndex(charDir, configDir, "TestChar")).toBeUndefined();

    applyDeferredEdits(charDir, configDir, "TestChar");
    expect(loadMemoryIndex(charDir, configDir, "TestChar")).toBe("new index");
  });
});
