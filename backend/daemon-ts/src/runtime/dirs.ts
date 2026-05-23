/**
 * Shore directory resolution.
 *
 * Mirror of the Rust `shore_config::ShoreDirs::resolve()` logic in
 * `core/config/src/lib.rs`. We MUST resolve to the same paths the Rust
 * daemon and CLI use so the two daemons can coexist and clients can
 * discover us via the same `instances.json`.
 *
 * Precedence (highest first):
 *   1. `SHORE_<KIND>_DIR` — used verbatim, no "/shore" suffix.
 *   2. `XDG_<KIND>_HOME` — appended with "/shore".
 *   3. Platform defaults — appended with "/shore".
 *   4. Hardcoded fallback — appended with "/shore". For runtime: `os.tmpdir()`.
 */

import os from "node:os";
import path from "node:path";

export interface ShoreDirs {
  config: string;
  data: string;
  runtime: string;
  cache: string;
}

function expandHome(p: string): string {
  if (p === "~" || p.startsWith("~/")) {
    return path.join(os.homedir(), p.slice(2));
  }
  return p;
}

function resolveXdgDir(
  overrideVar: string,
  xdgVar: string,
  platformDefault: () => string | undefined,
  hardcodedFallback: string,
): string {
  const override = process.env[overrideVar];
  if (override) return override;

  const xdg = process.env[xdgVar];
  const base = xdg ?? platformDefault() ?? (hardcodedFallback === "" ? os.tmpdir() : expandHome(hardcodedFallback));
  return path.join(base, "shore");
}

function platformConfigDir(): string {
  // Mirror of `dirs::config_dir()`.
  if (process.platform === "darwin") return path.join(os.homedir(), "Library", "Application Support");
  if (process.platform === "win32") return process.env["APPDATA"] ?? path.join(os.homedir(), "AppData", "Roaming");
  return path.join(os.homedir(), ".config");
}

function platformDataDir(): string {
  if (process.platform === "darwin") return path.join(os.homedir(), "Library", "Application Support");
  if (process.platform === "win32") return process.env["APPDATA"] ?? path.join(os.homedir(), "AppData", "Roaming");
  return path.join(os.homedir(), ".local", "share");
}

function platformRuntimeDir(): string | undefined {
  // `dirs::runtime_dir()` returns None on macOS/Windows; only Linux has it.
  // We rely on the empty hardcoded fallback to drop to `os.tmpdir()`.
  if (process.platform === "linux") {
    const xdg = process.env["XDG_RUNTIME_DIR"];
    if (xdg) return xdg;
  }
  return undefined;
}

function platformCacheDir(): string {
  if (process.platform === "darwin") return path.join(os.homedir(), "Library", "Caches");
  if (process.platform === "win32") return process.env["LOCALAPPDATA"] ?? path.join(os.homedir(), "AppData", "Local");
  return path.join(os.homedir(), ".cache");
}

export function resolveShoreDirs(): ShoreDirs {
  return {
    config: resolveXdgDir("SHORE_CONFIG_DIR", "XDG_CONFIG_HOME", platformConfigDir, "~/.config"),
    data: resolveXdgDir("SHORE_DATA_DIR", "XDG_DATA_HOME", platformDataDir, "~/.local/share"),
    runtime: resolveXdgDir("SHORE_RUNTIME_DIR", "XDG_RUNTIME_DIR", platformRuntimeDir, ""),
    cache: resolveXdgDir("SHORE_CACHE_DIR", "XDG_CACHE_HOME", platformCacheDir, "~/.cache"),
  };
}
