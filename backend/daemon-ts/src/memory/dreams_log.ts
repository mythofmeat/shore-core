/**
 * Dreams audit log.
 *
 * Port of `backend/daemon/src/memory/dreams_log.rs`.
 *
 * Records what each memory-maintenance pass (compaction, dreaming) inspected
 * and changed. Lives in the character's data directory at
 * `<dataDir>/<character>/DREAMS.md` — outside the workspace so it never
 * bleeds into prompts or memory snapshots.
 */

import fs from "node:fs";
import path from "node:path";

const DREAMS_FILE = "DREAMS.md";
const DREAMS_HEADER = "# Dreams\n";

/** Canonical path of the dreams log for a character. */
export function dreamsLogPath(dataDir: string, character: string): string {
  return path.join(dataDir, character, DREAMS_FILE);
}

/**
 * Append a timestamped audit entry. Creates the parent directory and the
 * log file if they do not yet exist.
 *
 * `title` and `body` are inserted verbatim (body is trimmed). The timestamp
 * is formatted as `YYYY-MM-DD HH:MM` in the local timezone.
 */
export async function appendDreamEntry(
  dataDir: string,
  character: string,
  timestamp: Date,
  title: string,
  body: string,
): Promise<void> {
  const p = dreamsLogPath(dataDir, character);
  fs.mkdirSync(path.dirname(p), { recursive: true });

  let existing: string;
  try {
    existing = fs.readFileSync(p, "utf8");
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code !== "ENOENT") throw e;
    existing = DREAMS_HEADER;
  }

  let updated = existing.replace(/\s+$/u, "");
  if (updated.length > 0) updated += "\n\n";
  updated += `## ${formatTimestamp(timestamp)} - ${title}\n\n${body.trim()}\n`;

  fs.writeFileSync(p, updated);
}

/**
 * Read the most recent N dream entries (newest first), or an empty list
 * when the log doesn't exist yet.
 */
export async function recentDreamEntries(
  dataDir: string,
  character: string,
  limit: number,
): Promise<string[]> {
  const content = await readDreamsLog(dataDir, character);
  if (content === undefined) return [];

  const sections: string[] = [];
  for (const section of content.split("\n## ")) {
    const trimmed = section.trim();
    if (trimmed.length === 0) continue;
    if (trimmed.startsWith("# Dreams")) continue;
    sections.push(`## ${trimmed}`);
  }
  sections.sort((a, b) => b.localeCompare(a));
  return sections.slice(0, limit);
}

/** Read the full dreams log, or `undefined` when it doesn't exist yet. */
export async function readDreamsLog(
  dataDir: string,
  character: string,
): Promise<string | undefined> {
  try {
    return fs.readFileSync(dreamsLogPath(dataDir, character), "utf8");
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw e;
  }
}

function formatTimestamp(d: Date): string {
  const pad = (n: number, w = 2): string => String(n).padStart(w, "0");
  return (
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ` +
    `${pad(d.getHours())}:${pad(d.getMinutes())}`
  );
}
