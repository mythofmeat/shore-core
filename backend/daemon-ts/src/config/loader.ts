/**
 * Config loader — minimal subset for Phase 2 handshake parity.
 *
 * Loads `config.toml` and `conf.d/*.toml` from `$SHORE_CONFIG_DIR` and
 * deep-merges the parsed objects. Exposes only the fields the handshake
 * snapshot actually reads:
 *   - `app.defaults.model`   → handshake `config.active_model` fallback
 *
 * The Rust loader (`core/config/src/lib.rs` + `core/config/src/models.rs`)
 * does provider catalog resolution, effective-config merging, validation,
 * etc. The handshake snapshot only needs `defaults.model` and a catalog
 * "first chat model" fallback. The full model catalog is a category →
 * provider → model hierarchy (`qualified_name = "{category}.{provider}.{name}"`);
 * porting it is its own phase — when we need it for LLM calls, not for the
 * handshake. Until then `firstChatModel` returns undefined and the snapshot
 * relies on `defaults.model` (or null) for `active_model`.
 */

import fs from "node:fs";
import path from "node:path";
import { parse as parseToml } from "smol-toml";

export interface LoadedConfig {
  app: {
    defaults: {
      model: string | undefined;
      display_name: string | undefined;
    };
  };
}

/** Load config from a Shore config directory. Missing files are tolerated. */
export function loadConfig(configDir: string): LoadedConfig {
  const merged = mergeAll(readAllConfigTables(configDir));

  const defaultsTable = pickTable(merged, "defaults") ?? {};

  return {
    app: {
      defaults: {
        model: typeof defaultsTable["model"] === "string" ? defaultsTable["model"] : undefined,
        display_name:
          typeof defaultsTable["display_name"] === "string"
            ? defaultsTable["display_name"]
            : undefined,
      },
    },
  };
}

/**
 * Resolve the display name like the Rust impl
 * (`config.app.defaults.resolve_display_name()`): explicit config wins,
 * else fall back to `$USER`, else "user".
 */
export function resolveDisplayName(config: LoadedConfig): string {
  return config.app.defaults.display_name ?? process.env["USER"] ?? "user";
}

/**
 * First chat-kind model in catalog order. The Rust loader builds a sorted
 * catalog from `[chat.<provider>.<model>]` tables; until that catalog port
 * lands, this always returns undefined and the handshake falls back to
 * `defaults.model` (or null).
 */
export function firstChatModelQualifiedName(_config: LoadedConfig): string | undefined {
  return undefined;
}

// ── internals ──────────────────────────────────────────────────────────

function readAllConfigTables(configDir: string): Record<string, unknown>[] {
  const tables: Record<string, unknown>[] = [];

  const baseFile = path.join(configDir, "config.toml");
  const baseContent = tryReadText(baseFile);
  if (baseContent !== undefined) tables.push(parseTomlOrFail(baseContent, baseFile));

  const confDir = path.join(configDir, "conf.d");
  let extras: string[] = [];
  try {
    extras = fs.readdirSync(confDir).filter((n) => n.endsWith(".toml")).sort();
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code !== "ENOENT") throw e;
  }
  for (const name of extras) {
    const full = path.join(confDir, name);
    const content = tryReadText(full);
    if (content !== undefined) tables.push(parseTomlOrFail(content, full));
  }

  return tables;
}

function tryReadText(file: string): string | undefined {
  try {
    return fs.readFileSync(file, "utf8");
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw e;
  }
}

function parseTomlOrFail(content: string, sourcePath: string): Record<string, unknown> {
  try {
    return parseToml(content) as Record<string, unknown>;
  } catch (e) {
    throw new Error(`failed to parse TOML at ${sourcePath}: ${(e as Error).message}`);
  }
}

/**
 * Deep-merge top-level tables. The Rust loader treats conf.d files as
 * overlays on config.toml: later files override earlier ones for scalar
 * fields, nested tables merge recursively, arrays-of-tables (e.g. multiple
 * `[chat.anthropic.opus]` blocks) extend.
 */
function mergeAll(tables: Record<string, unknown>[]): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const t of tables) deepMerge(out, t);
  return out;
}

function deepMerge(target: Record<string, unknown>, src: Record<string, unknown>): void {
  for (const [key, value] of Object.entries(src)) {
    const prev = target[key];
    if (Array.isArray(prev) && Array.isArray(value)) {
      target[key] = [...prev, ...value];
    } else if (isPlainObject(prev) && isPlainObject(value)) {
      const nested = { ...prev };
      deepMerge(nested, value);
      target[key] = nested;
    } else {
      target[key] = value;
    }
  }
}

function pickTable(obj: Record<string, unknown>, key: string): Record<string, unknown> | undefined {
  const v = obj[key];
  return isPlainObject(v) ? v : undefined;
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}
