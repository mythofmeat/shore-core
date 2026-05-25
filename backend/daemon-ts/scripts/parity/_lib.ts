/**
 * Shared helpers for parity capture/check scripts.
 *
 * Every parity flow follows the same shape: spawn a daemon, find its
 * listen port, open a TCP connection, exchange newline-delimited JSON
 * frames, then either record them (capture) or diff them against a
 * baseline (check). The helpers here factor that out so per-flow scripts
 * are just the CLI + the flow-specific control flow.
 *
 * See `docs/DAEMON_TS_PARITY.md` for the tier breakdown and how to add
 * a new parity case.
 */

import type { Subprocess } from "bun";
import { chmodSync, cpSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// ── daemon process management ──────────────────────────────────────────

/**
 * Spawn a daemon process bound to an ephemeral port. The caller is
 * responsible for `proc.kill(...)` + `await proc.exited` in a finally.
 */
export function spawnDaemon(cmd: string[], env: Record<string, string | undefined>): Subprocess {
  return Bun.spawn({
    cmd: [...cmd, "--addr", "127.0.0.1:0"],
    env,
    stdout: "pipe",
    stderr: "pipe",
  });
}

/**
 * Watch the daemon's stdout/stderr for its resolved listen address.
 * Returns undefined on timeout. Skips port-0 matches (the Rust daemon
 * logs `bind_addr=127.0.0.1:0` before the resolved `addr=127.0.0.1:<port>`).
 */
export async function readListenAddr(
  streams: ReadableStream<Uint8Array>[],
  timeoutMs = 10_000,
): Promise<{ host: string; port: number } | undefined> {
  return new Promise((resolve) => {
    const deadline = setTimeout(() => finish(undefined), timeoutMs);
    let resolved = false;
    const finish = (v: { host: string; port: number } | undefined) => {
      if (resolved) return;
      resolved = true;
      clearTimeout(deadline);
      resolve(v);
    };
    for (const stream of streams) {
      void (async () => {
        const decoder = new TextDecoder();
        let acc = "";
        const reader = stream.getReader();
        try {
          while (!resolved) {
            const { value, done } = await reader.read();
            if (done) return;
            acc += decoder.decode(value, { stream: true });
            for (const m of acc.matchAll(/(\d+\.\d+\.\d+\.\d+):(\d+)/g)) {
              const port = Number(m[2]);
              if (port > 0) {
                finish({ host: m[1]!, port });
                return;
              }
            }
          }
        } finally {
          reader.releaseLock();
        }
      })();
    }
  });
}

// ── connection + framing ───────────────────────────────────────────────

/** Newline-delimited JSON frame queue over a TCP socket. */
export class FrameQueue {
  private buf = Buffer.alloc(0);
  private waiters: Array<{ resolve: (l: string) => void; reject: (e: Error) => void }> = [];
  private closed = false;
  private err: Error | undefined;

  push(chunk: Buffer | Uint8Array): void {
    const b = Buffer.from(chunk);
    this.buf = this.buf.length === 0 ? b : Buffer.concat([this.buf, b]);
    this.drain();
  }
  eof(): void {
    this.closed = true;
    this.drain();
  }
  error(e: Error): void {
    this.err = e;
    this.drain();
  }
  read(): Promise<string> {
    return new Promise((resolve, reject) => {
      this.waiters.push({ resolve, reject });
      this.drain();
    });
  }
  private drain(): void {
    while (this.waiters.length > 0) {
      const w = this.waiters[0]!;
      if (this.err) {
        this.waiters.shift();
        w.reject(this.err);
        continue;
      }
      const nl = this.buf.indexOf(0x0a);
      if (nl < 0) {
        if (this.closed) {
          this.waiters.shift();
          w.reject(new Error("connection closed"));
          continue;
        }
        return;
      }
      const line = this.buf.subarray(0, nl).toString("utf8");
      this.buf = this.buf.subarray(nl + 1);
      this.waiters.shift();
      w.resolve(line);
    }
  }
}

// Bun.connect's return type is awkward to import; the call sites only
// need `.write` and `.end`.
type Socket = { write: (s: string) => void; end: () => void };

/** Open a TCP connection to the daemon and attach a FrameQueue. */
export async function openConnection(
  addr: { host: string; port: number },
): Promise<{ sock: Socket; frames: FrameQueue }> {
  const frames = new FrameQueue();
  const sock = await Bun.connect({
    hostname: addr.host,
    port: addr.port,
    socket: {
      data: (_s, chunk) => frames.push(chunk),
      open: () => {},
      close: () => frames.eof(),
      error: (_s, e) => frames.error(e),
    },
  });
  return { sock: sock as unknown as Socket, frames };
}

/** Read one JSON frame from the queue, with a timeout. */
export async function readFrame(q: FrameQueue, timeoutMs = 5000): Promise<unknown> {
  const line = await Promise.race([
    q.read(),
    new Promise<string>((_, rej) => setTimeout(() => rej(new Error("read timeout")), timeoutMs)),
  ]);
  return JSON.parse(line);
}

// ── fixture + env ──────────────────────────────────────────────────────

/**
 * Copy `<fixtureDir>/config` and `<fixtureDir>/data` into a fresh tmp
 * dir so the daemon can mutate them without polluting committed
 * fixtures. Returns the resolved workspace paths the daemon should use.
 */
export function copyFixtureToTmp(
  fixtureDir: string,
  prefix = "shore-parity-fixture-",
): { workDir: string; configDir: string; dataDir: string } {
  const workDir = mkdtempSync(join(tmpdir(), prefix));
  cpSync(join(fixtureDir, "config"), join(workDir, "config"), { recursive: true });
  cpSync(join(fixtureDir, "data"), join(workDir, "data"), { recursive: true });
  return {
    workDir,
    configDir: join(workDir, "config"),
    dataDir: join(workDir, "data"),
  };
}

/**
 * Rewrite every `cache_ttl = "…"` line under `<configDir>/config.toml` to
 * the given value. Used by T3 parity checks to flip a fixture's cache
 * mode between `""` (no Anthropic cache_control on the wire) and `"1h"`
 * (cache markers emitted, label-strip + breakpoint-placement code paths
 * exercised). Throws if the config has no `cache_ttl` line — that means
 * the caller is patching a fixture that never opted into caching.
 */
export function setCacheTtl(configDir: string, value: string): void {
  const configPath = join(configDir, "config.toml");
  const raw = readFileSync(configPath, "utf8");
  const next = raw.replace(/^cache_ttl\s*=\s*".*"\s*$/gm, `cache_ttl = "${value}"`);
  if (next === raw) {
    throw new Error(`setCacheTtl: no cache_ttl line found in ${configPath}`);
  }
  writeFileSync(configPath, next);
}

/**
 * Build the env block for a parity daemon spawn. `runtimeDir` /
 * `cacheDir` default to fresh tmp dirs.
 *
 * When `notifyLog` is set, a `notify-send` intercept shim is dropped
 * into a tmp dir at the front of PATH so both daemons' fire-and-forget
 * notification calls land in the same JSON-lines log file instead of
 * the real desktop bus. See `installNotifySendShim`.
 */
export function buildDaemonEnv(opts: {
  configDir: string;
  dataDir: string;
  runtimeDir?: string;
  cacheDir?: string;
  prefix?: string;
  notifyLog?: string;
}): Record<string, string | undefined> {
  const prefix = opts.prefix ?? "shore-parity-";
  const base: Record<string, string | undefined> = {
    ...process.env,
    SHORE_RUNTIME_DIR: opts.runtimeDir ?? mkdtempSync(join(tmpdir(), `${prefix}runtime-`)),
    SHORE_CACHE_DIR: opts.cacheDir ?? mkdtempSync(join(tmpdir(), `${prefix}cache-`)),
    SHORE_CONFIG_DIR: opts.configDir,
    SHORE_DATA_DIR: opts.dataDir,
  };
  if (opts.notifyLog !== undefined) {
    const shimDir = installNotifySendShim();
    base["PATH"] = `${shimDir}:${process.env["PATH"] ?? ""}`;
    base["SHORE_PARITY_NOTIFY_LOG"] = opts.notifyLog;
  }
  return base;
}

/**
 * Drop a `notify-send` shim into a fresh tmp dir and return that dir,
 * intended for prepending to PATH. Each invocation appends one JSON
 * object to `$SHORE_PARITY_NOTIFY_LOG` recording the argv the daemon
 * tried to send to `notify-send`, then exits 0. The shim never blocks
 * — both daemons treat notify-send as fire-and-forget anyway.
 *
 * Python3 is used because it's available on every Linux dev/CI host
 * and gives us safe JSON escape with zero external deps. The shim is
 * a script (not a binary), so file-locking on append from concurrent
 * daemon processes is fine on a local FS — at worst lines interleave
 * if two notify calls happen in the same microsecond. Test scenarios
 * here serialize the two daemons so that's a non-issue.
 */
export function installNotifySendShim(): string {
  const shimDir = mkdtempSync(join(tmpdir(), "shore-parity-notifyshim-"));
  const shimPath = join(shimDir, "notify-send");
  const script = `#!/usr/bin/env python3
import json, os, sys
log = os.environ.get("SHORE_PARITY_NOTIFY_LOG")
if log:
    with open(log, "a") as f:
        f.write(json.dumps({"argv": sys.argv[1:]}) + "\\n")
`;
  writeFileSync(shimPath, script);
  chmodSync(shimPath, 0o755);
  return shimDir;
}

// ── structural diff ────────────────────────────────────────────────────

/**
 * Per-frame-type fuzzy-match list. Keys are SWP frame `type` values.
 * Values are dotted paths into the frame body, with `[*]` matching any
 * array index. Paths in this list are *type-checked* (both sides must
 * have the same typeof) but values are not compared. Use this for
 * fields that legitimately differ between Rust and TS (server name,
 * generated message ids, timestamps).
 */
export type FuzzyDiffs = Record<string, string[]>;

type PathMatcher = (path: string) => boolean;

/**
 * Compile a fuzzy path pattern like `messages[*].timestamp` into a
 * matcher function. `[*]` matches one array index segment.
 */
export function pathToMatcher(pattern: string): PathMatcher {
  const SENTINEL = "\x00IDX\x00";
  const withSentinel = pattern.replaceAll("[*]", SENTINEL);
  const escaped = withSentinel.replace(/[.+?^${}()|[\]\\*]/g, "\\$&");
  const rxSrc = escaped.replaceAll(SENTINEL, "\\[\\d+\\]");
  const rx = new RegExp(`^${rxSrc}$`);
  return (p) => rx.test(p);
}

/**
 * Structural deep-diff between two frames. Returns one string per
 * divergence, or an empty list on parity. Paths listed under the
 * frame's `type` in `fuzzy` are type-checked but value-skipped.
 */
export function compareFrames(
  a: Record<string, unknown>,
  b: Record<string, unknown>,
  fuzzy: FuzzyDiffs = {},
): string[] {
  const type = String(a["type"] ?? "");
  const matchers = (fuzzy[type] ?? []).map(pathToMatcher);
  const out: string[] = [];
  walk(a, b, "", out, matchers);
  return out;
}

function walk(
  a: unknown,
  b: unknown,
  path: string,
  out: string[],
  fuzzy: PathMatcher[],
): void {
  if (path && fuzzy.some((m) => m(path))) {
    if (typeof a !== typeof b) {
      out.push(`${path}: fuzzy type mismatch (${typeof a} vs ${typeof b})`);
    }
    return;
  }
  if (a === b) return;
  if (a === null || b === null || typeof a !== typeof b) {
    out.push(`${path || "<root>"}: ${JSON.stringify(a)} !== ${JSON.stringify(b)}`);
    return;
  }
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) {
      out.push(`${path || "<root>"}: array shape differs`);
      return;
    }
    for (let i = 0; i < a.length; i++) walk(a[i], b[i], `${path}[${i}]`, out, fuzzy);
    return;
  }
  if (typeof a === "object") {
    const ao = a as Record<string, unknown>;
    const bo = b as Record<string, unknown>;
    const keys = new Set([...Object.keys(ao), ...Object.keys(bo)]);
    for (const k of keys) {
      const sub = path ? `${path}.${k}` : k;
      if (fuzzy.some((m) => m(sub))) {
        if (typeof ao[k] !== typeof bo[k]) {
          out.push(`${sub}: fuzzy type mismatch (${typeof ao[k]} vs ${typeof bo[k]})`);
        }
        continue;
      }
      if (!(k in ao)) out.push(`${sub}: missing in baseline`);
      else if (!(k in bo)) out.push(`${sub}: missing in actual`);
      else walk(ao[k], bo[k], sub, out, fuzzy);
    }
    return;
  }
  if (a !== b) out.push(`${path || "<root>"}: ${JSON.stringify(a)} !== ${JSON.stringify(b)}`);
}

// ── process control ────────────────────────────────────────────────────

export function fail(msg: string): never {
  console.error(`FAIL: ${msg}`);
  process.exit(1);
}
