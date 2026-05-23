/**
 * Character discovery + avatar loading.
 *
 * Mirror of `core/config/src/lib.rs::discover_characters` and
 * `backend/daemon/src/commands/navigation.rs::character_metadata`.
 *
 * A character is a directory under `$CONFIG_DIR/characters/<name>/` that
 * contains either `workspace/SOUL.md` (new layout) or `character.md`
 * (legacy). Returned names are sorted lexicographically.
 */

import fs from "node:fs";
import path from "node:path";

/** Wire-shape of a `CharacterInfo` frame element. */
export interface CharacterInfo {
  name: string;
  avatar?: CharacterAvatar;
}

export interface CharacterAvatar {
  mime_type: string;
  data: string;
}

const SOUL_FILE = "SOUL.md";
const LEGACY_CHARACTER_FILE = "character.md";
const CHARACTER_WORKSPACE_DIR = "workspace";

const AVATAR_CANDIDATES: ReadonlyArray<readonly [string, string]> = [
  ["avatar.png", "image/png"],
  ["avatar.jpg", "image/jpeg"],
  ["avatar.jpeg", "image/jpeg"],
  ["avatar.webp", "image/webp"],
];

/** Discover character names by walking `$CONFIG_DIR/characters/`. */
export function discoverCharacters(configDir: string): string[] {
  const charsDir = path.join(configDir, "characters");
  let entries: fs.Dirent[];
  try {
    entries = fs.readdirSync(charsDir, { withFileTypes: true });
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return [];
    throw e;
  }

  const names: string[] = [];
  for (const entry of entries) {
    if (!entry.isDirectory()) continue;
    const dir = path.join(charsDir, entry.name);
    if (isFile(path.join(dir, CHARACTER_WORKSPACE_DIR, SOUL_FILE)) || isFile(path.join(dir, LEGACY_CHARACTER_FILE))) {
      names.push(entry.name);
    }
  }
  names.sort();
  return names;
}

/** Build a CharacterInfo (name + optional avatar) for the wire. */
export function characterMetadata(configDir: string, name: string): CharacterInfo {
  const info: CharacterInfo = { name };
  const avatar = loadAvatar(configDir, name);
  if (avatar) info.avatar = avatar;
  return info;
}

function loadAvatar(configDir: string, name: string): CharacterAvatar | undefined {
  for (const [filename, mimeType] of AVATAR_CANDIDATES) {
    const file = path.join(configDir, "characters", name, filename);
    let bytes: Buffer;
    try {
      bytes = fs.readFileSync(file);
    } catch {
      continue;
    }
    if (bytes.length === 0) continue;
    return { mime_type: mimeType, data: bytes.toString("base64") };
  }
  return undefined;
}

function isFile(p: string): boolean {
  try {
    return fs.statSync(p).isFile();
  } catch {
    return false;
  }
}
