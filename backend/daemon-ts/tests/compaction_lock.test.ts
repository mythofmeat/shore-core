/**
 * Compaction lock tests — mirror of
 * `backend/daemon/src/memory/compaction/mod.rs::compaction_run_guard_serializes_per_character_data_root`.
 */
import { afterEach, describe, expect, it } from "bun:test";
import { randomUUID } from "node:crypto";

import {
  _resetCompactionLocksForTest,
  tryBeginCompaction,
  withCompactionLock,
} from "../src/memory/compaction/lock.ts";

afterEach(() => {
  _resetCompactionLocksForTest();
});

describe("tryBeginCompaction", () => {
  it("serializes per character data root", () => {
    const dataDir = `/guard-data-${randomUUID()}`;
    const otherDataDir = `/guard-data-${randomUUID()}`;
    const character = "TestChar";
    const otherCharacter = "OtherChar";

    const first = tryBeginCompaction(dataDir, character);
    expect(first).toBeDefined();

    expect(
      tryBeginCompaction(dataDir, character),
      "second guard for same character data root must be rejected",
    ).toBeUndefined();

    const otherChar = tryBeginCompaction(dataDir, otherCharacter);
    expect(
      otherChar,
      "different characters in one data dir may compact independently",
    ).toBeDefined();
    otherChar?.release();

    const otherDir = tryBeginCompaction(otherDataDir, character);
    expect(
      otherDir,
      "same character name in another data dir may compact independently",
    ).toBeDefined();
    otherDir?.release();

    first?.release();
    expect(
      tryBeginCompaction(dataDir, character),
      "guard should release when dropped",
    ).toBeDefined();
  });

  it("release is idempotent", () => {
    const dataDir = `/guard-${randomUUID()}`;
    const guard = tryBeginCompaction(dataDir, "C")!;
    expect(guard).toBeDefined();
    guard.release();
    guard.release();
    expect(tryBeginCompaction(dataDir, "C")).toBeDefined();
  });
});

describe("withCompactionLock", () => {
  it("releases the lock after the body returns", async () => {
    const dataDir = `/wcl-${randomUUID()}`;
    await withCompactionLock(dataDir, "C", async () => "done");
    expect(tryBeginCompaction(dataDir, "C")).toBeDefined();
  });

  it("releases the lock when the body throws", async () => {
    const dataDir = `/wcl-throw-${randomUUID()}`;
    await expect(
      withCompactionLock(dataDir, "C", async () => {
        throw new Error("boom");
      }),
    ).rejects.toThrow("boom");
    expect(tryBeginCompaction(dataDir, "C")).toBeDefined();
  });

  it("throws when the lock is already held", async () => {
    const dataDir = `/wcl-held-${randomUUID()}`;
    const guard = tryBeginCompaction(dataDir, "C")!;
    await expect(
      withCompactionLock(dataDir, "C", async () => "should not run"),
    ).rejects.toThrow(/already running/);
    guard.release();
  });
});
