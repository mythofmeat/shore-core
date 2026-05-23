/**
 * Atomic file write via temp file + rename.
 *
 * Mirror of `backend/daemon/src/engine/atomic.rs::atomic_write`. The temp
 * file is created in the same directory as the target so the final rename
 * stays on the same filesystem (rename is atomic only within one fs).
 *
 * The Rust impl uses `NamedTempFile` which generates a random name and
 * cleans up on drop. We pid-and-counter-tag the temp file ourselves and
 * rely on the rename to "move" it into place; if a partial write crashes,
 * the leftover temp will sit next to the target until the next write
 * cycle. Not ideal but matches the Rust behavior in failure modes.
 */

import fs from "node:fs";
import path from "node:path";

let counter = 0;

export function atomicWrite(targetPath: string, data: string | Uint8Array): void {
  const dir = path.dirname(targetPath);
  fs.mkdirSync(dir, { recursive: true });

  const tmpName = `.${path.basename(targetPath)}.tmp.${process.pid}.${counter++}`;
  const tmpPath = path.join(dir, tmpName);

  const fd = fs.openSync(tmpPath, "w", 0o644);
  try {
    fs.writeSync(fd, data as string);
    fs.fsyncSync(fd);
  } finally {
    fs.closeSync(fd);
  }

  try {
    fs.renameSync(tmpPath, targetPath);
  } catch (e) {
    try {
      fs.unlinkSync(tmpPath);
    } catch {
      // ignore
    }
    throw e;
  }
}
