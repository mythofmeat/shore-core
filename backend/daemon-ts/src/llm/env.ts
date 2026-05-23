/**
 * `.env` loader for the daemon's config directory.
 *
 * Mirror of the Rust loader in `core/config/src/lib.rs` (~line 306): on
 * startup, read `$SHORE_CONFIG_DIR/.env` and merge its key=value pairs
 * into `process.env` so provider clients can resolve API keys via
 * `process.env[<api_key_env>]` — same convention the Rust daemon uses.
 *
 * "override" semantics match `dotenvy::from_path_override`: keys in the
 * file win over pre-existing process env. The Rust code chose override
 * specifically so a user editing `.env` and hot-reloading sees the new
 * value (see backend/daemon/src/hot_reload.rs:140); we don't have hot
 * reload yet but the precedence must match.
 */

import fs from "node:fs";
import path from "node:path";

import { config as dotenvConfig } from "dotenv";

export interface LoadEnvResult {
  loaded: boolean;
  path: string;
  keys: string[];
}

export function loadConfigDotenv(configDir: string): LoadEnvResult {
  const envPath = path.join(configDir, ".env");
  if (!fs.existsSync(envPath)) {
    return { loaded: false, path: envPath, keys: [] };
  }
  const result = dotenvConfig({ path: envPath, override: true, quiet: true });
  if (result.error) {
    throw new Error(`failed to load .env at ${envPath}: ${result.error.message}`);
  }
  return { loaded: true, path: envPath, keys: Object.keys(result.parsed ?? {}) };
}
