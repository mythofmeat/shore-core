/**
 * Single-flight compaction lock keyed on character data root.
 *
 * Port of `try_begin_compaction` + `CompactionRunGuard` from
 * `backend/daemon/src/memory/compaction/mod.rs`.
 *
 * Manual and idle-triggered compaction both mutate the same active
 * transcript, segment manifest, markdown memory files, and prompt-refresh
 * state. Keep them single-flight per character data root so a slow provider
 * response cannot overlap with another compaction pass against the same
 * pre-compaction active window. Tests may host separate daemon instances
 * for the same character name in one process, so the character name alone
 * is not a sufficient key.
 *
 * JS is single-threaded, so this is just a `Set<string>` of locked keys
 * with promise-based release — no `try_lock_owned` race semantics needed.
 * `tryBeginCompaction` returns `undefined` immediately when the key is
 * already held.
 */

import path from "node:path";

const LOCKED = new Set<string>();

/** A held compaction lock. Call `release()` (or use `using`) to release. */
export interface CompactionRunGuard {
  release(): void;
  readonly key: string;
}

function characterDataDirKey(dataDir: string, character: string): string {
  return path.resolve(path.join(dataDir, character));
}

/**
 * Try to acquire the compaction lock for a character data root. Returns
 * `undefined` if a pass is already in flight for the same key.
 */
export function tryBeginCompaction(
  dataDir: string,
  character: string,
): CompactionRunGuard | undefined {
  const key = characterDataDirKey(dataDir, character);
  if (LOCKED.has(key)) return undefined;
  LOCKED.add(key);
  let released = false;
  return {
    key,
    release(): void {
      if (released) return;
      released = true;
      LOCKED.delete(key);
    },
  };
}

/** Run `body` while holding the lock for the duration. Throws if locked. */
export async function withCompactionLock<T>(
  dataDir: string,
  character: string,
  body: () => Promise<T>,
): Promise<T> {
  const guard = tryBeginCompaction(dataDir, character);
  if (guard === undefined) {
    throw new Error(`compaction already running for ${character}`);
  }
  try {
    return await body();
  } finally {
    guard.release();
  }
}

/** Test-only: forcibly clear all held locks. */
export function _resetCompactionLocksForTest(): void {
  LOCKED.clear();
}
