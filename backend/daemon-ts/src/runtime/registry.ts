/**
 * Daemon instance registry — `$SHORE_RUNTIME_DIR/instances.json`.
 *
 * Mirror of `backend/swp-server/src/registry.rs`. The CLI and other
 * clients discover live daemons by reading this file, so the schema is
 * FROZEN.
 *
 * Schema (JSON array):
 *   {
 *     id: string,                 // UUID or pinned --instance-id
 *     pid: number,                // PID of this daemon process
 *     addr: string,               // "host:port" the daemon listens on
 *     started_at: string,         // RFC3339 timestamp
 *     data_dir?: string,
 *     config_dir?: string
 *   }
 *
 * Locking: we use a sidecar lock file (`instances.json.lock`) opened
 * with `O_EXCL`-ish semantics via `fs.open(... 'wx')`. The Rust
 * version uses `fs2::FileExt::lock_exclusive`. The two are not
 * byte-compatible (Rust uses `flock(2)`, we don't), but for our use
 * case — short critical sections, single-host, low contention — a
 * lockfile poll is sufficient.
 */

import fs from "node:fs";
import path from "node:path";

export interface InstanceInfo {
  id: string;
  pid: number;
  addr: string;
  started_at: string;
  data_dir?: string;
  config_dir?: string;
}

export class Registry {
  constructor(private readonly registryPath: string) {}

  static atDefault(runtimeDir: string): Registry {
    return new Registry(path.join(runtimeDir, "instances.json"));
  }

  path(): string {
    return this.registryPath;
  }

  /** Register this daemon. Replaces any existing entry with the same id. */
  register(info: InstanceInfo): void {
    this.withLock((entries) => {
      const next = entries.filter((e) => e.id !== info.id);
      next.push(info);
      return next;
    });
  }

  /** Remove the entry with this id (called on shutdown). */
  unregister(id: string): void {
    this.withLock((entries) => entries.filter((e) => e.id !== id));
  }

  // ── internals ───────────────────────────────────────────────────

  private lockPath(): string {
    return `${this.registryPath}.lock`;
  }

  private readEntries(): InstanceInfo[] {
    try {
      const raw = fs.readFileSync(this.registryPath, "utf8");
      const trimmed = raw.trim();
      if (trimmed === "") return [];
      const parsed: unknown = JSON.parse(trimmed);
      if (!Array.isArray(parsed)) return [];
      return parsed.filter(isInstance);
    } catch (e) {
      if ((e as NodeJS.ErrnoException).code === "ENOENT") return [];
      throw e;
    }
  }

  private writeEntries(entries: InstanceInfo[]): void {
    fs.mkdirSync(path.dirname(this.registryPath), { recursive: true });
    const tmp = `${this.registryPath}.tmp.${process.pid}`;
    fs.writeFileSync(tmp, JSON.stringify(entries, null, 2) + "\n");
    fs.renameSync(tmp, this.registryPath);
  }

  /**
   * Lock-guarded read-modify-write. Prunes dead-PID entries on every pass,
   * matching the Rust `prune_dead_entries` behavior.
   */
  private withLock(mutate: (entries: InstanceInfo[]) => InstanceInfo[]): void {
    fs.mkdirSync(path.dirname(this.lockPath()), { recursive: true });

    // Acquire lock file via O_CREAT|O_EXCL.
    let fd: number | undefined;
    const start = Date.now();
    const deadlineMs = 5000;
    while (true) {
      try {
        fd = fs.openSync(this.lockPath(), fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_WRONLY, 0o644);
        break;
      } catch (e) {
        if ((e as NodeJS.ErrnoException).code !== "EEXIST") throw e;
        if (Date.now() - start > deadlineMs) {
          // Stale lock — break it.
          try {
            fs.unlinkSync(this.lockPath());
          } catch {
            // ignore; another process won the race
          }
          continue;
        }
        Bun.sleepSync(20);
      }
    }

    try {
      const before = this.readEntries();
      const live = before.filter((e) => isPidAlive(e.pid));
      const after = mutate(live);
      this.writeEntries(after);
    } finally {
      if (fd !== undefined) fs.closeSync(fd);
      try {
        fs.unlinkSync(this.lockPath());
      } catch {
        // ignore
      }
    }
  }
}

function isInstance(v: unknown): v is InstanceInfo {
  if (typeof v !== "object" || v === null) return false;
  const o = v as Record<string, unknown>;
  return typeof o["id"] === "string" && typeof o["pid"] === "number" && typeof o["addr"] === "string" && typeof o["started_at"] === "string";
}

function isPidAlive(pid: number): boolean {
  try {
    // Signal 0 is a no-op delivery — checks existence without sending.
    process.kill(pid, 0);
    return true;
  } catch (e) {
    const code = (e as NodeJS.ErrnoException).code;
    // EPERM means the process exists but we can't signal it — alive enough.
    return code === "EPERM";
  }
}
