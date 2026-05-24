/**
 * Pin the engine/context.ts → memory-snapshot wiring landed in Phase 6b.
 *
 * The chat context reads MEMORY.md via `loadMemoryIndex` (active snapshot
 * if present, else canonical). After a workspace `write` to MEMORY.md
 * queues a deferred edit, the active snapshot is the zero-byte sentinel,
 * so the prompt does NOT see the new content until `applyDeferredEdits`
 * fires at the compaction boundary.
 */
import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { buildChatContext } from "../src/engine/context.ts";
import {
  applyDeferredEdits,
  ensureActivePromptSnapshot,
  ensureCharacterWorkspace,
  MEMORY_INDEX_FILE,
  noteMemoryIndexDeferred,
} from "../src/memory/deferred_edits.ts";

function freshRoot(): { dataDir: string; configDir: string; character: string } {
  const tmp = mkdtempSync(path.join(tmpdir(), "shore-ctx-mem-test-"));
  return {
    dataDir: path.join(tmp, "data"),
    configDir: path.join(tmp, "config"),
    character: "Alpha",
  };
}

function memoryFromContext(args: {
  dataDir: string;
  configDir: string;
  character: string;
}): string | undefined {
  const ctx = buildChatContext({
    characterName: args.character,
    characterConfigDir: path.join(args.configDir, "characters", args.character),
    configDir: args.configDir,
    characterDataDir: path.join(args.dataDir, args.character),
    displayName: "Tester",
    isPrivate: false,
    hasPriorContext: false,
    messages: [],
  });
  return ctx.prompt.system.find((b) => b.label === "memory_index")?.content;
}

describe("buildChatContext + memory snapshot", () => {
  it("reads the canonical MEMORY.md when no snapshot exists", () => {
    const { dataDir, configDir, character } = freshRoot();
    ensureCharacterWorkspace(
      path.join(dataDir, character),
      configDir,
      character,
    );
    const workspaceMemory = path.join(
      configDir,
      "characters",
      character,
      "workspace",
      MEMORY_INDEX_FILE,
    );
    fs.writeFileSync(workspaceMemory, "# Canonical memory\n");

    const memory = memoryFromContext({ dataDir, configDir, character });
    expect(memory).toBeDefined();
    expect(memory).toContain("Canonical memory");
  });

  it("snapshot blocks deferred edits until applyDeferredEdits fires", () => {
    const { dataDir, configDir, character } = freshRoot();
    const characterDataDir = path.join(dataDir, character);

    ensureCharacterWorkspace(characterDataDir, configDir, character);
    const workspaceMemory = path.join(
      configDir,
      "characters",
      character,
      "workspace",
      MEMORY_INDEX_FILE,
    );
    fs.writeFileSync(workspaceMemory, "# Original memory\n");

    // Seed the active snapshot from the canonical file.
    ensureActivePromptSnapshot(characterDataDir, configDir, character);

    // Sanity: the seeded snapshot matches canonical.
    expect(memoryFromContext({ dataDir, configDir, character })).toContain(
      "Original memory",
    );

    // Simulate a write to MEMORY.md: canonical changes + the deferred-edit
    // sentinel call runs. The chat context must continue to read the OLD
    // snapshot content (the whole point of the deferred queue is that
    // canonical edits don't leak into the prompt until apply fires).
    fs.writeFileSync(workspaceMemory, "# Updated memory\n");
    noteMemoryIndexDeferred(characterDataDir);

    const stillOld = memoryFromContext({ dataDir, configDir, character });
    expect(stillOld).toContain("Original memory");
    expect(stillOld).not.toContain("Updated memory");

    // Now fire applyDeferredEdits — the snapshot refreshes from canonical.
    applyDeferredEdits(characterDataDir, configDir, character);

    expect(memoryFromContext({ dataDir, configDir, character })).toContain(
      "Updated memory",
    );
  });

  it("brand-new MEMORY.md write is invisible (sentinel) until apply", () => {
    const { dataDir, configDir, character } = freshRoot();
    const characterDataDir = path.join(dataDir, character);

    // Canonical workspace exists but MEMORY.md does not (no
    // ensureActivePromptSnapshot called → no snapshot file yet).
    ensureCharacterWorkspace(characterDataDir, configDir, character);

    // Workspace.write of MEMORY.md: queue + zero-byte sentinel.
    const workspaceMemory = path.join(
      configDir,
      "characters",
      character,
      "workspace",
      MEMORY_INDEX_FILE,
    );
    fs.writeFileSync(workspaceMemory, "# Fresh memory\n");
    noteMemoryIndexDeferred(characterDataDir);

    // Sentinel exists → loadMemoryIndex returns undefined → no memory in prompt.
    expect(memoryFromContext({ dataDir, configDir, character })).toBeUndefined();

    // After apply, the sentinel is replaced by the real content.
    applyDeferredEdits(characterDataDir, configDir, character);
    expect(memoryFromContext({ dataDir, configDir, character })).toContain(
      "Fresh memory",
    );
  });
});
